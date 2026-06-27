// This file is for stuff in the main (UI) thread.
use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use log::{debug, error, info, warn};
use rustradio::Float;
use rustradio::data_stream::{
    BytesReader, DataStreamId, PROTOCOL_VERSION, Packet, RequestData, SyncWriter,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys;
use web_sys::js_sys::Uint8Array;
use web_sys::{
    AudioContext, BinaryType, Element, Event, File, GainNode, HtmlInputElement, MessageEvent,
    WebSocket, Worker,
};

use crate::js_performance_now;
use crate::{Ax25Messages, MainToWorker, WorkerToMain};
use rustradio_ui::SharedVecPtr;

const HTML_DISABLED: &str = "disabled";
const ID_RESULT: &str = "result";
const ID_START: &str = "btn-start";
const ID_SAMP_RATE: &str = "input-samp-rate";
const ID_RTLSDR_FORMAT: &str = "input-rtlsdr-format";
const ID_FILE_INPUT: &str = "fileInput";
const ID_WS_URL: &str = "input-ws-url";
const ID_WS_CONNECT: &str = "btn-ws-connect";
const ID_ADD: &str = "btn-add";
const ID_PING: &str = "btn-ping";
const ID_AUDIO_VOLUME: &str = "input-audio-volume";
const ID_HTML: &str = "root-html";
const ID_RTLSDR: &str = "btn-rtlsdr";
const ID_RTLSDR_FREQUENCY: &str = "input-rtlsdr-frequency";
const ID_OFFSET: &str = "input-offset";
const ID_RTLSDR_GAIN_AUTO: &str = "input-rtlsdr-gain-auto";
const ID_RTLSDR_GAIN: &str = "input-rtlsdr-gain";
const AUDIO_SAMPLE_RATE: f32 = 44_100.0;
const AUDIO_START_LATENCY_SECONDS: f64 = 0.08;
const AUDIO_TARGET_LATENCY_SECONDS: f64 = 0.10;
const AUDIO_MAX_LATENCY_SECONDS: f64 = 1.0;
const AUDIO_MAX_PLAYBACK_RATE: f32 = 1.02;
const RTLSDR_MIN_GAIN_TENTHS_DB: i32 = -100;
const RTLSDR_MAX_GAIN_TENTHS_DB: i32 = 500;

struct AudioPlayback {
    context: AudioContext,
    gain: GainNode,
    next_time: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputSource {
    None,
    File,
    WebSocket,
    RtlSdr,
}

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
    static FILE: RefCell<Option<File>> = const { RefCell::new(None) };
    static FILE_DATA_STREAM_READER: RefCell<BytesReader> = RefCell::new(BytesReader::new());
    static FILE_DATA_STREAM_VERSION_SENT: Cell<bool> = const { Cell::new(false) };
    static FILE_DATA_STREAM_PEER_VERSION_SEEN: Cell<bool> = const { Cell::new(false) };
    static FILE_DATA_STREAM_POS: RefCell<HashMap<DataStreamId, u64>> = RefCell::new(HashMap::new());
    static INPUT_SOURCE: RefCell<InputSource> = const { RefCell::new(InputSource::None) };
    static PENDING_WORKER_DATA_STREAM: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static AUDIO_PLAYBACK: RefCell<Option<AudioPlayback>> = const { RefCell::new(None) };
    static WS_SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
}

#[allow(clippy::enum_glob_use)]
fn forget_shared(msg: MainToWorker) {
    use rustradio_ui::MainToWorker::*;
    match msg {
        // Ignore all the messages that don't have shared data.
        Start(_) | ApplicationSpecific(_) | DataStream(_) | Ping(_) | Pong(_) => {}
        SharedByte(_, v) => {
            for e in v {
                e.forget();
            }
        }
    }
}

#[allow(clippy::enum_glob_use)]
fn drop_shared(msg: MainToWorker) {
    use rustradio_ui::MainToWorker::*;
    match msg {
        // Ignore all the messages that don't have shared data.
        Start(_) | ApplicationSpecific(_) | DataStream(_) | Ping(_) | Pong(_) => {}
        SharedByte(_, v) => {
            for e in v {
                let _ = e.into_vec();
            }
        }
    }
}
fn post_message_inner(msg: &MainToWorker) -> Result<(), JsValue> {
    let msg = serde_wasm_bindgen::to_value(msg)?;
    worker().post_message(&msg)
}

pub(crate) fn post_message(msg: MainToWorker) -> Result<(), JsValue> {
    match post_message_inner(&msg) {
        Ok(()) => {
            forget_shared(msg);
            Ok(())
        }
        Err(e) => {
            drop_shared(msg);
            Err(e)
        }
    }
}

/// Read and clamp the current Web Audio volume control value.
fn audio_volume() -> Result<f32, JsValue> {
    Ok(get_element(ID_AUDIO_VOLUME)?
        .dyn_into::<HtmlInputElement>()?
        .value()
        .parse::<f32>()
        .unwrap_or(0.25)
        .clamp(0.0, 1.0))
}

/// Lazily create the Web Audio graph and keep its gain node in sync.
fn ensure_audio_playback() -> Result<(), JsValue> {
    let volume = audio_volume()?;
    AUDIO_PLAYBACK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(audio) = slot.as_mut() {
            audio.gain.gain().set_value(volume);
            let _ = audio.context.resume()?;
            return Ok(());
        }

        let context = AudioContext::new()?;
        let gain = context.create_gain()?;
        gain.gain().set_value(volume);
        gain.connect_with_audio_node(context.destination().unchecked_ref())?;
        let _ = context.resume()?;
        let next_time = context.current_time() + AUDIO_START_LATENCY_SECONDS;
        *slot = Some(AudioPlayback {
            context,
            gain,
            next_time,
        });
        Ok(())
    })
}

/// Restart sample scheduling slightly in the future to avoid immediate underruns.
fn reset_audio_schedule() {
    AUDIO_PLAYBACK.with(|slot| {
        if let Some(audio) = slot.borrow_mut().as_mut() {
            audio.next_time = audio.context.current_time() + AUDIO_START_LATENCY_SECONDS;
        }
    });
}

/// Apply the current slider value to an already-created audio graph.
fn update_audio_volume() -> Result<(), JsValue> {
    let volume = audio_volume()?;
    AUDIO_PLAYBACK.with(|slot| {
        if let Some(audio) = slot.borrow().as_ref() {
            audio.gain.gain().set_value(volume);
        }
    });
    Ok(())
}

/// Return the bounded playback rate to use for the current queued latency.
fn audio_playback_rate(queued_seconds: f64) -> f32 {
    let excess = queued_seconds - AUDIO_TARGET_LATENCY_SECONDS;
    if excess <= 0.0 {
        return 1.0;
    }

    let correction_range = AUDIO_MAX_LATENCY_SECONDS - AUDIO_TARGET_LATENCY_SECONDS;
    let correction = (excess / correction_range).clamp(0.0, 1.0) as f32;
    1.0 + (AUDIO_MAX_PLAYBACK_RATE - 1.0) * correction
}

/// Queue one demodulated audio chunk for browser playback.
fn enqueue_audio_samples(mut samples: Vec<f32>) -> Result<(), JsValue> {
    if samples.is_empty() {
        return Ok(());
    }

    ensure_audio_playback()?;
    for sample in &mut samples {
        *sample = sample.clamp(-1.0, 1.0);
    }

    AUDIO_PLAYBACK.with(|slot| {
        let mut slot = slot.borrow_mut();
        let audio = slot
            .as_mut()
            .ok_or_else(|| JsValue::from_str("audio playback is not initialized"))?;
        let now = audio.context.current_time();
        let start_time = audio.next_time.max(now + AUDIO_START_LATENCY_SECONDS);
        let queued_seconds = (start_time - now).max(0.0);
        let playback_rate = audio_playback_rate(queued_seconds);
        let max_end_time = now + AUDIO_MAX_LATENCY_SECONDS;
        let available_seconds = (max_end_time - start_time).max(0.0);
        let max_samples =
            (available_seconds * f64::from(AUDIO_SAMPLE_RATE) * f64::from(playback_rate)).floor()
                as usize;
        let sample_offset = samples.len().saturating_sub(max_samples);
        if sample_offset == samples.len() {
            info!(
                "Main: dropping {} audio samples; queued audio is {:.0}ms",
                samples.len(),
                queued_seconds * 1000.0
            );
            return Ok(());
        }
        if sample_offset > 0 {
            info!(
                "Main: dropping {} audio samples to keep queued audio below {:.0}ms",
                sample_offset,
                AUDIO_MAX_LATENCY_SECONDS * 1000.0
            );
        }
        let samples = &samples[sample_offset..];
        let len =
            u32::try_from(samples.len()).map_err(|_| JsValue::from_str("audio chunk too large"))?;
        let buffer = audio.context.create_buffer(1, len, AUDIO_SAMPLE_RATE)?;
        // AudioBuffer.copyToChannel rejects views backed by shared Wasm memory.
        let channel = js_sys::Float32Array::new_with_length(len);
        channel.copy_from(samples);
        buffer.copy_to_channel_with_f32_array(&channel, 0)?;

        let source = audio.context.create_buffer_source()?;
        source.set_buffer(Some(&buffer));
        source.playback_rate().set_value(playback_rate);
        source.connect_with_audio_node(audio.gain.unchecked_ref())?;
        source.start_with_when(start_time)?;
        audio.next_time = start_time
            + samples.len() as f64 / f64::from(AUDIO_SAMPLE_RATE) / f64::from(playback_rate);
        Ok(())
    })
}

async fn read_data(start: u64, size: u64) -> Result<Vec<u8>, JsValue> {
    let file = FILE
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("no file set"))?;
    let blob = file.slice_with_f64_and_f64(start as f64, (start + size) as f64)?;
    let js = JsFuture::from(blob.array_buffer()).await?;
    let buf: js_sys::ArrayBuffer = js.dyn_into()?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// Convert file-supplier protocol errors into browser callback errors.
fn file_data_stream_error(err: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("DATA_STREAM file input error: {err}"))
}

/// Return whether the next file-supplier packet must include Version.
fn take_file_data_stream_needs_version() -> bool {
    FILE_DATA_STREAM_VERSION_SENT.with(|slot| {
        if slot.get() {
            false
        } else {
            slot.set(true);
            true
        }
    })
}

/// Send the file supplier's DATA_STREAM Version packet to the worker.
fn send_file_data_stream_version() -> Result<(), JsValue> {
    if !take_file_data_stream_needs_version() {
        return Ok(());
    }

    let mut packet = Vec::new();
    SyncWriter::new(&mut packet)
        .write_version()
        .map_err(file_data_stream_error)?;
    post_message(MainToWorker::DataStream(packet))
}

/// Send one DATA_STREAM Data packet from the selected file to the worker.
fn send_file_data_stream_data(stream_id: &DataStreamId, data: &[u8]) -> Result<(), JsValue> {
    let mut packet = Vec::new();
    {
        let mut writer = SyncWriter::new(&mut packet);
        if take_file_data_stream_needs_version() {
            writer.write_version().map_err(file_data_stream_error)?;
        }
        writer
            .write_data(stream_id, data)
            .map_err(file_data_stream_error)?;
    }
    debug!(
        "Main: sending {} file input byte(s) to worker over DATA_STREAM",
        data.len()
    );
    post_message(MainToWorker::DataStream(packet))
}

/// Clear all file-supplier protocol state for a new selected file.
fn reset_file_data_stream_state() {
    FILE_DATA_STREAM_READER.with(|slot| slot.borrow_mut().clear());
    FILE_DATA_STREAM_VERSION_SENT.with(|slot| slot.set(false));
    FILE_DATA_STREAM_PEER_VERSION_SEEN.with(|slot| slot.set(false));
    FILE_DATA_STREAM_POS.with(|slot| slot.borrow_mut().clear());
}

/// Feed worker protocol bytes into the file supplier parser.
fn append_file_data_stream_bytes(data: &[u8]) -> Result<Vec<Packet>, JsValue> {
    FILE_DATA_STREAM_READER.with(|slot| {
        let mut reader = slot.borrow_mut();
        reader.push_bytes(data);
        let mut packets = Vec::new();

        loop {
            let Some(packet) = reader.read_packet().map_err(file_data_stream_error)? else {
                break;
            };
            packets.push(packet);
        }

        Ok(packets)
    })
}

/// Satisfy one worker RequestData packet by reading bytes from the selected file.
async fn satisfy_file_data_request(req: RequestData) -> Result<(), JsValue> {
    let pos = FILE_DATA_STREAM_POS.with(|slot| {
        let positions = slot.borrow();
        *positions.get(&req.stream_id).unwrap_or(&0)
    });
    // TODO: Confirm that we're reading the right file.
    let data = read_data(pos, req.window as u64).await?;
    FILE_DATA_STREAM_POS.with(|slot| {
        slot.borrow_mut()
            .insert(req.stream_id.clone(), pos + data.len() as u64);
    });
    send_file_data_stream_data(&req.stream_id, &data)
}

/// Apply one worker DATA_STREAM packet to the file supplier state.
async fn handle_file_data_stream_packet(packet: Packet) -> Result<(), JsValue> {
    match packet {
        Packet::Version(PROTOCOL_VERSION) => {
            info!("DataStream protocol version accepted");
            FILE_DATA_STREAM_PEER_VERSION_SEEN.with(|slot| slot.set(true));
            Ok(())
        }
        Packet::Version(version) => Err(file_data_stream_error(format!(
            "unsupported protocol version {version}"
        ))),
        Packet::RequestData(req) => {
            if !FILE_DATA_STREAM_PEER_VERSION_SEEN.with(std::cell::Cell::get) {
                return Err(file_data_stream_error(
                    "peer sent RequestData before Version packet",
                ));
            }
            satisfy_file_data_request(req).await
        }
        Packet::Data(data) => Err(file_data_stream_error(format!(
            "unexpected Data packet for {} with {} byte(s)",
            data.stream_id,
            data.data.len()
        ))),
    }
}

/// Decode and handle all complete DATA_STREAM packets sent by the worker.
async fn handle_file_data_stream_bytes(data: &[u8]) -> Result<(), JsValue> {
    for packet in append_file_data_stream_bytes(data)? {
        handle_file_data_stream_packet(packet).await?;
    }
    Ok(())
}

/// Route worker DATA_STREAM bytes to the active input source.
async fn handle_worker_data_stream(data: Vec<u8>) -> Result<(), JsValue> {
    route_worker_data_stream(data).await
}

/// Hold worker protocol bytes until an input source can receive them.
fn store_pending_worker_data_stream(data: Vec<u8>) {
    PENDING_WORKER_DATA_STREAM.with(|slot| slot.borrow_mut().push(data));
}

/// Take all worker protocol bytes held while waiting for source setup.
fn take_pending_worker_data_stream() -> Vec<Vec<u8>> {
    PENDING_WORKER_DATA_STREAM.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Replay held worker protocol bytes into the now-active input source.
async fn flush_pending_worker_data_stream() -> Result<(), JsValue> {
    for data in take_pending_worker_data_stream() {
        route_worker_data_stream(data).await?;
    }
    Ok(())
}

/// Deliver worker protocol bytes or queue them until the source is ready.
async fn route_worker_data_stream(data: Vec<u8>) -> Result<(), JsValue> {
    match input_source() {
        InputSource::File => handle_file_data_stream_bytes(&data).await,
        InputSource::WebSocket if websocket_is_open() => websocket_send_bytes(&data),
        InputSource::WebSocket => {
            info!("Main: waiting for websocket open before routing DATA_STREAM bytes");
            store_pending_worker_data_stream(data);
            Ok(())
        }
        InputSource::RtlSdr => Ok(()), // TOOD: do something?
        InputSource::None => {
            info!("Main: waiting for an input source before routing DATA_STREAM bytes");
            store_pending_worker_data_stream(data);
            Ok(())
        }
    }
}

/// Read the active input source for async browser callbacks.
fn input_source() -> InputSource {
    INPUT_SOURCE.with(|source| *source.borrow())
}

/// Lock in the single source that will feed this graph run, so file and
/// WebSocket inputs cannot both satisfy the same receiver.
fn select_input_source(source: InputSource) -> Result<(), JsValue> {
    INPUT_SOURCE.with(|slot| {
        let mut active = slot.borrow_mut();
        if *active == InputSource::None {
            *active = source;
            Ok(())
        } else {
            Err(JsValue::from_str("input source already selected"))
        }
    })
}

/// Roll back source selection when source setup fails before it can feed the
/// worker.
fn clear_input_source() {
    INPUT_SOURCE.with(|slot| *slot.borrow_mut() = InputSource::None);
}

/// Return whether the browser WebSocket can currently accept bytes.
fn websocket_is_open() -> bool {
    WS_SOCKET.with(|slot| {
        let socket = slot.borrow();
        match socket.as_ref() {
            Some(ws) => ws.ready_state() == WebSocket::OPEN,
            None => false,
        }
    })
}

/// Send one serialized DATA_STREAM packet over the browser WebSocket.
fn websocket_send_bytes(data: &[u8]) -> Result<(), JsValue> {
    WS_SOCKET.with(|slot| {
        let ws = slot
            .borrow()
            .clone()
            .ok_or_else(|| JsValue::from_str("websocket is not connected"))?;
        ws.send_with_u8_array(data)
    })
}

/// Surface a WebSocket relay failure and close the socket so the worker sees
/// EOF through the normal close path.
fn close_websocket_after_error(err: &JsValue) {
    error!("Main: websocket DATA_STREAM error: {err:?}");
    let _ = set_content(ID_RESULT, "WebSocket DATA_STREAM protocol error.");
    WS_SOCKET.with(|slot| {
        if let Some(ws) = slot.borrow().as_ref() {
            let _ = ws.close();
        }
    });
}

/// Handle message sent from the worker.
async fn worker_msg(e: MessageEvent) -> Result<(), JsValue> {
    match e.data().try_into()? {
        WorkerToMain::SharedFloat(name, streams) => {
            let streams = streams
                .into_iter()
                .map(rustradio_ui::SharedVecPtr::into_vec)
                .collect();
            match name.as_str() {
                "iq_mag" => crate::time_sink::update(streams)?,
                "iq_spectrum" => crate::spectrum_sink::update(streams)?,
                "audio_demod" => {
                    for stream in streams {
                        enqueue_audio_samples(stream.data)?;
                    }
                }
                other => log::error!("Unknown float vec: {other}"),
            }
        }
        WorkerToMain::SharedComplex(name, streams) => {
            assert_eq!(name, "iq_constellation");
            let streams: Vec<_> = streams
                .into_iter()
                .map(rustradio_ui::SharedVecPtr::into_vec)
                .collect();
            crate::constellation_sink::update(streams)?;
        }
        /*
        Ax25Messages::FloatPduStreams(streams) => {
            let streams: Vec<_> = streams
                .into_iter()
                .map(|s| FloatPduStream {
                    name: s.name,
                    tags: s.tags,
                    sample_rate: 1000.0, // TODO
                    samples: SharedVec::new(s.vec, post_release_shared_buffer),
                })
                .collect();
            crate::spectrum_sink::update(streams)?;
        }
        */
        WorkerToMain::ApplicationSpecific(msg) => match msg {
            Ax25Messages::Decoded(x) => {
                set_content(ID_RESULT, &format!("Decoded: {x:?}"))?;
            }
        },
        WorkerToMain::DataStream(data) => {
            debug!(
                "Main: handling {} DATA_STREAM byte(s) from worker",
                data.len()
            );
            handle_worker_data_stream(data).await?;
        }
        WorkerToMain::Ready(_todo) => {
            info!("Main: Received WorkerToMain::Ready");
            worker_msg_ready().await?;
        }
        WorkerToMain::End(s) => {
            info!("Main: worker returned: {s:?}");
        }
        WorkerToMain::LogLine { level, line } => {
            log::log!(level, "[worker] {line}");
        }
        WorkerToMain::FloatStreams(_) | WorkerToMain::ComplexStreams(_) => {}
        WorkerToMain::Ping(t) => {
            post_message(MainToWorker::Pong(t)).unwrap();
        }
        WorkerToMain::Pong(from) => {
            let to = js_performance_now();
            info!("Main: Got Pong {from} -> {to}: {}", to - from);
            set_content(ID_RESULT, &format!("Ping RTT: {}", to - from))?;
        }
    }
    Ok(())
}

/// Convert the RTL-SDR gain controls into the device gain mode.
fn rtlsdr_gain_mode() -> Result<rtlsdr_pure::GainMode, JsValue> {
    if get_element(ID_RTLSDR_GAIN_AUTO)?
        .dyn_into::<HtmlInputElement>()?
        .checked()
    {
        return Ok(rtlsdr_pure::GainMode::Auto);
    }

    let gain_db: f64 = get_element(ID_RTLSDR_GAIN)?
        .dyn_into::<HtmlInputElement>()?
        .value()
        .parse()
        .map_err(|e| JsValue::from_str(&format!("parsing RTL-SDR tuner gain: {e}")))?;
    if !gain_db.is_finite() {
        return Err(JsValue::from_str("RTL-SDR tuner gain must be finite"));
    }

    let gain_tenths = (gain_db * 10.0).round();
    if (gain_tenths - gain_db * 10.0).abs() > 1.0e-6 {
        return Err(JsValue::from_str(
            "RTL-SDR tuner gain must be in 0.1 dB steps",
        ));
    }
    let gain_tenths = gain_tenths as i32;
    if !(RTLSDR_MIN_GAIN_TENTHS_DB..=RTLSDR_MAX_GAIN_TENTHS_DB).contains(&gain_tenths) {
        return Err(JsValue::from_str(&format!(
            "RTL-SDR tuner gain must be between {:.1} and {:.1} dB",
            RTLSDR_MIN_GAIN_TENTHS_DB as f32 / 10.0,
            RTLSDR_MAX_GAIN_TENTHS_DB as f32 / 10.0
        )));
    }

    Ok(rtlsdr_pure::GainMode::ManualTenthsDb(gain_tenths))
}

/// Enable RTL-SDR gain controls, leaving manual gain disabled while Auto is on.
fn set_rtlsdr_gain_controls_enabled(enabled: bool) -> Result<(), JsValue> {
    let auto = get_element(ID_RTLSDR_GAIN_AUTO)?.dyn_into::<HtmlInputElement>()?;
    auto.set_disabled(!enabled);
    update_rtlsdr_gain_control_state()
}

/// Mirror Auto gain state into the manual gain input's disabled state.
fn update_rtlsdr_gain_control_state() -> Result<(), JsValue> {
    let auto = get_element(ID_RTLSDR_GAIN_AUTO)?.dyn_into::<HtmlInputElement>()?;
    let gain = get_element(ID_RTLSDR_GAIN)?.dyn_into::<HtmlInputElement>()?;
    gain.set_disabled(auto.disabled() || auto.checked());
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_rtlsdr_source(mut sdr: rtlsdr_pure::RtlSdr) -> Result<(), JsValue> {
    let sample_rate: u32 = get_element(ID_SAMP_RATE)?
        .dyn_into::<web_sys::HtmlInputElement>()?
        .value()
        .parse()
        .map_err(|e| JsValue::from_str(&format!("parsing sample rate: {e}")))?;
    let gain_mode = rtlsdr_gain_mode()?;

    let freq: i32 = get_element(ID_RTLSDR_FREQUENCY)?
        .dyn_into::<web_sys::HtmlInputElement>()?
        .value()
        .parse()
        .map_err(|e| JsValue::from_str(&format!("parsing sample rate: {e}")))?;
    let offset: i32 = get_element(ID_OFFSET)?
        .dyn_into::<web_sys::HtmlInputElement>()?
        .value()
        .parse()
        .map_err(|e| JsValue::from_str(&format!("parsing sample rate: {e}")))?;
    let freq = u32::try_from(freq - offset).map_err(|_e| {
        JsValue::from_str(&format!("frequency {freq} can't subtract offset {offset}"))
    })?;

    if offset.unsigned_abs() > sample_rate / 2 - 25_000 {
        return Err(JsValue::from_str(&format!(
            "offset {offset} must be within half sample rate minus buffer {}",
            sample_rate / 2 - 25_000
        )));
    }

    select_input_source(InputSource::RtlSdr)?;
    disable_input_selectors()?;

    info!(
        "RTLSDR manufacturer: {}",
        sdr.manufacturer().unwrap_or("<unknown>")
    );
    info!("RTLSDR product: {}", sdr.product().unwrap_or("<unknown>"));
    info!("RTLSDR tuner: {:?}", sdr.tuner_kind());
    let actual_rate = sdr.set_sample_rate(sample_rate).await?;
    info!("sample rate: {actual_rate} Hz");

    if sdr.tuner_kind().is_supported() {
        sdr.set_tuner_gain(gain_mode).await?;
        info!("RTLSDR tuner gain: {gain_mode:?}");
        sdr.set_center_frequency(freq).await?;
        info!("center frequency: {freq} Hz");
    } else {
        info!("center frequency: skipped for unsupported tuner");
    }
    sdr.reset_buffer().await?;

    // Init stream.
    {
        let packet =
            rustradio::data_stream::PacketRef::Version(rustradio::data_stream::PROTOCOL_VERSION);
        let mut buf: Vec<u8> = Vec::new();
        let mut w: SyncWriter<&mut Vec<u8>> = rustradio::data_stream::SyncWriter::new(&mut buf);
        w.write_packet(packet)?;
        post_message(MainToWorker::DataStream(buf))?;
    }

    // Stream data.
    //
    // Two bytes per sample, so `bytes / sample_rate / 2` seconds.
    //
    // At 250ksps:
    // * 65536: 131ms
    // * 16384: 33ms
    let read_len = 16384usize;

    info!("Running the rtlsdr loop");
    let mut deadline = js_performance_now() + 1000.0f64; // One second. Basically infinite time.
    loop {
        let now = js_performance_now();
        if now > deadline {
            warn!(
                "Slow to read from RTLSDR! Missed it by {} ms",
                now - deadline
            );
        }
        let bytes = sdr.read_bytes(read_len).await?;
        deadline =
            js_performance_now() + 1_000.0f64 * ((bytes.len() / 2) as f64) / f64::from(actual_rate);
        assert!(bytes.len().is_multiple_of(2));
        log::trace!("Read {} bytes from rtlsdr", bytes.len());
        post_message(MainToWorker::SharedByte(
            "rtl-sdr".into(),
            vec![SharedVecPtr::new(bytes, &[])],
        ))?;
    }
}

#[wasm_bindgen]
pub fn wasm_memory_is_shared() -> bool {
    use js_sys::Reflect;

    let memory = wasm_bindgen::memory();

    let buffer = Reflect::get(&memory, &JsValue::from_str("buffer")).expect("memory.buffer");

    // SharedArrayBuffer exists only when cross-origin isolation permits it.
    let sab_ctor = js_sys::global().unchecked_into::<js_sys::Object>();

    let shared_array_buffer = Reflect::get(&sab_ctor, &JsValue::from_str("SharedArrayBuffer")).ok();

    match shared_array_buffer {
        Some(ctor) if !ctor.is_undefined() => buffer.is_instance_of::<js_sys::SharedArrayBuffer>(),
        _ => false,
    }
}

/// Handle receiving a message from the worker saying it's ready.
///
/// This means we should enable UI controls.
#[allow(clippy::unused_async)]
#[allow(clippy::too_many_lines)]
async fn worker_msg_ready() -> Result<(), JsValue> {
    // Set up RTLSDR button.
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            spawn_local(async move {
                // Get the RTLSDR.
                let sdr = match rtlsdr_pure::open_first().await {
                    Err(e) => {
                        warn!("Failed to open RTLSDR: {e}");
                        return;
                    }
                    Ok(sdr) => {
                        info!(
                            "opened {:04x}:{:04x} {}",
                            sdr.vendor_id(),
                            sdr.product_id(),
                            sdr.known_name().unwrap_or("RTL-SDR")
                        );
                        sdr
                    }
                };
                if let Err(e) = run_rtlsdr_source(sdr).await {
                    warn!("RTL SDR source failed: {e:?}");
                }
            });
            Ok(())
        });
        let btn = get_element(ID_RTLSDR)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }
    // Set up Add button.
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            info!("button clicked");
            spawn_local(async move {
                // Get the RTLSDR.
                match rtlsdr_pure::open_first().await {
                    Err(e) => warn!("Failed to open RTLSDR: {e}"),
                    Ok(sdr) => {
                        info!(
                            "opened {:04x}:{:04x} {}",
                            sdr.vendor_id(),
                            sdr.product_id(),
                            sdr.known_name().unwrap_or("RTL-SDR")
                        );
                    }
                }
            });
            set_content(ID_RESULT, &format!("Result of add: {}", crate::add(3, 5)))
        });
        let btn = get_element(ID_ADD)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        btn.remove_attribute(HTML_DISABLED)?;
        handler.forget();
    }

    // Set up Ping button.
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            info!("ping button clicked");
            post_message(MainToWorker::Ping(js_performance_now()))?;
            Ok(())
        });
        let btn = get_element(ID_PING)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        btn.remove_attribute(HTML_DISABLED)?;
        handler.forget();
    }

    // Set up audio volume control.
    {
        let input = get_element(ID_AUDIO_VOLUME)?.dyn_into::<HtmlInputElement>()?;
        let handler = Closure::<dyn FnMut(Event) -> Result<(), JsValue>>::new(move |_event| {
            update_audio_volume()?;
            Ok(())
        });
        input.add_event_listener_with_callback("input", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // Set up RTL-SDR gain controls.
    {
        let input = get_element(ID_RTLSDR_GAIN_AUTO)?.dyn_into::<HtmlInputElement>()?;
        let handler = Closure::<dyn FnMut(Event) -> Result<(), JsValue>>::new(move |_event| {
            update_rtlsdr_gain_control_state()?;
            Ok(())
        });
        input.add_event_listener_with_callback("change", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    // Set up Start button.
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            let samp_rate: u64 = get_element(ID_SAMP_RATE)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .value()
                .parse()
                .map_err(|e| JsValue::from_str(&format!("parsing sample rate: {e}")))?;
            let offset: Float = get_element(ID_OFFSET)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .value()
                .parse()
                .map_err(|e| JsValue::from_str(&format!("parsing offset rate: {e}")))?;
            let rtlsdr = get_element(ID_RTLSDR_FORMAT)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .checked();
            crate::time_sink::set_sample_rate(crate::worker::VIZ_SAMPLE_RATE as f64);
            ensure_audio_playback()?;
            reset_audio_schedule();
            post_message(MainToWorker::Start(crate::Ax25Start {
                samp_rate,
                offset,
                rtlsdr,
            }))?;
            get_element(ID_FILE_INPUT)?
                .dyn_into::<HtmlInputElement>()?
                .set_disabled(false);
            get_element(ID_WS_URL)?
                .dyn_into::<HtmlInputElement>()?
                .set_disabled(false);
            if rtlsdr {
                get_element(ID_RTLSDR_FREQUENCY)?
                    .dyn_into::<HtmlInputElement>()?
                    .set_disabled(false);
                set_rtlsdr_gain_controls_enabled(true)?;
                get_element(ID_RTLSDR)?
                    .dyn_into::<web_sys::HtmlButtonElement>()?
                    .set_disabled(false);
            }
            get_element(ID_WS_CONNECT)?
                .dyn_into::<web_sys::HtmlButtonElement>()?
                .set_disabled(false);
            get_element(ID_START)?
                .dyn_into::<web_sys::HtmlButtonElement>()?
                .set_disabled(true);
            get_element(ID_OFFSET)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .set_disabled(true);
            get_element(ID_SAMP_RATE)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .set_disabled(true);
            get_element(ID_RTLSDR_FORMAT)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .set_disabled(true);
            Ok(())
        });
        let btn = get_element(ID_START)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        btn.remove_attribute(HTML_DISABLED)?;
        handler.forget();
    }

    // Set up file input thing.
    {
        let input = get_element(ID_FILE_INPUT)?.dyn_into::<HtmlInputElement>()?;
        //input.set_disabled(false);
        // TODO: make some sort of UI friendly bounded channel. Don't want it to
        // block.
        info!("Main: installing file chunk handler");
        install_file_chunk_listener(input)?; // 64 KiB chunks
    }

    // Set up websocket input.
    {
        let btn = get_element(ID_WS_CONNECT)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            let url = get_element(ID_WS_URL)?
                .dyn_into::<HtmlInputElement>()?
                .value()
                .trim()
                .to_string();
            if url.is_empty() {
                return Err(JsValue::from_str("missing websocket URL"));
            }
            start_websocket_source(&url)?;
            Ok(())
        });
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }
    Ok(())
}

/// Give us the worker.
pub(crate) fn worker() -> Worker {
    WORKER.with(|cell| {
        cell.get_or_init(|| {
            info!("Main: Starting the worker");
            let opts = web_sys::WorkerOptions::new();
            opts.set_type(web_sys::WorkerType::Module);
            opts.set_name("RustRadio worker");
            let worker = Worker::new_with_options("./wasm-mod.js", &opts).unwrap();
            let mut bootstrapped = false;

            // Set message handler.
            let onmessage = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
                    if !bootstrapped {
                        bootstrapped = crate::start_worker::msg(&e);
                        return Ok(());
                    }
                    spawn_local(async move {
                        if let Err(e) = worker_msg(e).await {
                            error!("Main: Inner receiver thing: {e:?}");
                        }
                    });
                    Ok(())
                },
            );
            worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();

            // Set messageerror handler.
            let onmsgerr = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
                    error!("Main: Message Error: {e:?}");
                    Ok(())
                },
            );
            worker.set_onmessageerror(Some(onmsgerr.as_ref().unchecked_ref()));
            onmsgerr.forget();

            // Set error handler.
            let onerr = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
                    error!("Main: Worker error: {e:?}");
                    Ok(())
                },
            );
            worker.set_onerror(Some(onerr.as_ref().unchecked_ref()));
            onerr.forget();

            worker
        })
        .clone()
    })
}

/// Get HTML element by ID.
pub(crate) fn get_element(id: &str) -> Result<Element, JsValue> {
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;

    document
        .get_element_by_id(id)
        .ok_or(JsValue::from_str(&format!(
            "can't find element with id {id}"
        )))
}

/// Set content of element.
fn set_content(id: &str, content: &str) -> Result<(), JsValue> {
    debug!("Setting inner HTML of {id}");
    get_element(id)?.set_inner_html(content);
    Ok(())
}

#[allow(clippy::unused_async)]
pub(crate) async fn setup() -> Result<(), JsValue> {
    {
        let wgit = crate::git_version();
        let html_git_version = get_element(ID_HTML)?
            .dyn_into::<web_sys::HtmlElement>()?
            .dataset()
            .get("gitVersion")
            .ok_or(JsValue::from_str("No HTML git version set"))?;
        if html_git_version == wgit {
            log::info!("Git versions matched");
        } else {
            let err_str = format!(
                "Git version mismatch. HTML is {html_git_version}, Wasm is {wgit}. You may need to clear your caches."
            );
            set_content(ID_RESULT, &err_str)?;
            return Err(JsValue::from_str(&err_str));
        }
    }

    // Init the worker.
    worker();

    // Show some bootup message.
    set_content(
        ID_RESULT,
        &format!(
            r"<b>WASM loaded</b>
WASM version: {}
WASM author timestamp: {}
WASM commit timestamp: {}
WASM built by Rust version: {}",
            crate::git_version(),
            crate::git_author_timestamp(),
            crate::git_commit_timestamp(),
            crate::rustc_version()
        ),
    )?;

    crate::time_sink::setup_graph_ui()?;
    crate::constellation_sink::setup_graph_ui()?;
    crate::spectrum_sink::setup_graph_ui()?;

    Ok(())
}

fn install_file_chunk_listener(input: HtmlInputElement) -> Result<(), JsValue> {
    info!("Adding listener");
    let input = Rc::new(input);
    let input2 = input.clone();
    let on_change =
        Closure::<dyn FnMut(Event) -> Result<(), JsValue>>::wrap(Box::new(move |_event: Event| {
            let Some(files) = input.files() else {
                return Ok(());
            };
            let Some(file) = files.get(0) else {
                return Ok(());
            };
            info!("Read file now!");
            select_input_source(InputSource::File)?;
            disable_input_selectors()?;
            get_element(ID_FILE_INPUT)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .set_disabled(true);
            set_content(ID_RESULT, "Running rustradio on input…")?;
            FILE.with(|slot| {
                *slot.borrow_mut() = Some(file);
            });
            reset_file_data_stream_state();
            spawn_local(async {
                let result = async {
                    flush_pending_worker_data_stream().await?;
                    send_file_data_stream_version()
                }
                .await;
                if let Err(e) = result {
                    error!("Main: failed to start file DATA_STREAM input: {e:?}");
                    let _ = set_content(ID_RESULT, "File DATA_STREAM protocol error.");
                }
            });
            Ok(())
        }));

    info!("Adding event listener");
    input2.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    info!("Done Adding event listener");
    on_change.forget();
    Ok(())
}

/// Start the WebSocket input path from the UI button by claiming the source and
/// connecting the browser socket.
fn start_websocket_source(url: &str) -> Result<(), JsValue> {
    select_input_source(InputSource::WebSocket)?;
    match connect_websocket(url) {
        Ok(()) => {
            disable_input_selectors()?;
            set_content(ID_RESULT, "Connecting websocket input…")?;
            Ok(())
        }
        Err(err) => {
            clear_input_source();
            Err(err)
        }
    }
}

/// Own the browser WebSocket and relay DATA_STREAM bytes between the socket and
/// worker.
fn connect_websocket(url: &str) -> Result<(), JsValue> {
    let ws = WebSocket::new(url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);
    WS_SOCKET.with(|slot| {
        *slot.borrow_mut() = Some(ws.clone());
    });

    {
        let url = url.to_string();
        let onopen = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            info!("Main: websocket input connected to {url}");
            let _ = set_content(ID_RESULT, "WebSocket connected. Waiting for DATA_STREAM…");
            spawn_local(async {
                if let Err(e) = flush_pending_worker_data_stream().await {
                    close_websocket_after_error(&e);
                }
            });
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    {
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            match websocket_message_bytes(event.data()) {
                Some(data) => {
                    if let Err(e) = post_message(MainToWorker::DataStream(data)) {
                        close_websocket_after_error(&e);
                    }
                }
                None => {
                    warn!(
                        "Main: ignoring DATA_STREAM websocket message with unsupported payload type"
                    );
                }
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }

    {
        let onerror = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            error!("Main: websocket input error");
            let _ = set_content(ID_RESULT, "WebSocket input error.");
        });
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    }

    {
        let onclose = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            info!("Main: websocket input closed");
            if let Err(e) = post_message(MainToWorker::DataStream(Vec::new())) {
                error!("Main: failed to send websocket disconnect to worker: {e:?}");
            }
            let _ = set_content(ID_RESULT, "WebSocket input closed.");
            WS_SOCKET.with(|slot| {
                slot.borrow_mut().take();
            });
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    Ok(())
}

/// Normalize supported WebSocket payload shapes into protocol bytes.
#[allow(clippy::needless_pass_by_value)]
fn websocket_message_bytes(data: JsValue) -> Option<Vec<u8>> {
    if let Ok(buf) = data.clone().dyn_into::<js_sys::ArrayBuffer>() {
        return Some(Uint8Array::new(&buf).to_vec());
    }
    if let Ok(bytes) = data.clone().dyn_into::<Uint8Array>() {
        return Some(bytes.to_vec());
    }
    data.as_string().map(String::into_bytes)
}

/// Freeze input-picking controls after one source wins, mirroring the
/// single-source state tracked by INPUT_SOURCE.
fn disable_input_selectors() -> Result<(), JsValue> {
    get_element(ID_FILE_INPUT)?
        .dyn_into::<HtmlInputElement>()?
        .set_disabled(true);

    // RTL-SDR
    get_element(ID_RTLSDR_FREQUENCY)?
        .dyn_into::<HtmlInputElement>()?
        .set_disabled(true);
    set_rtlsdr_gain_controls_enabled(false)?;
    get_element(ID_RTLSDR)?
        .dyn_into::<web_sys::HtmlButtonElement>()?
        .set_disabled(true);

    // Websocket.
    get_element(ID_WS_URL)?
        .dyn_into::<HtmlInputElement>()?
        .set_disabled(true);
    get_element(ID_WS_CONNECT)?
        .dyn_into::<web_sys::HtmlButtonElement>()?
        .set_disabled(true);
    Ok(())
}
