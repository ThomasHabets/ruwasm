// This file is for stuff in the main (UI) thread.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use log::{debug, error, info, warn};
use rustradio::Float;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys;
use web_sys::js_sys::Uint8Array;
use web_sys::{BinaryType, Element, Event, File, HtmlInputElement, MessageEvent, WebSocket};

use crate::js_performance_now;
use crate::{Ax25Messages, MainToWorker, WorkerToMain};
use rustradio_ui::TaggedVec;

use rustradio_ui::mainthread::{post_message, send_message, start_worker, time_sink};

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
const ID_TIME_SINK: &str = "time-sink";
const ID_CONSTELLATION_SINK: &str = "constellation-sink";
const ID_SPECTRUM_SINK: &str = "spectrum-sink";
const ID_WATERFALL_SINK: &str = "waterfall-sink";
pub(crate) const ID_LOG_OUTPUT: &str = "log-output";
const RTLSDR_MIN_GAIN_TENTHS_DB: i32 = -100;
const RTLSDR_MAX_GAIN_TENTHS_DB: i32 = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputSource {
    None,
    File,
    WebSocket,
    RtlSdr,
}

thread_local! {
    static FILE: RefCell<Option<File>> = const { RefCell::new(None) };
    static FILE_POS: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    static PENDING_FILE_REQUESTS: RefCell<Vec<(String, usize)>> = const { RefCell::new(Vec::new()) };
    static INPUT_SOURCE: RefCell<InputSource> = const { RefCell::new(InputSource::None) };
    static WS_SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
    static TIME_SINK: RefCell<Option<time_sink::TimeSink>> = const { RefCell::new(None) };
    static CONSTELLATION_SINK: RefCell<Option<crate::constellation_sink::ConstellationSink>> =
        const { RefCell::new(None) };
    static SPECTRUM_SINK: RefCell<Option<crate::spectrum_sink::SpectrumSink>> =
        const { RefCell::new(None) };
    static WATERFALL_SINK: RefCell<Option<crate::spectrum_sink::WaterfallSink>> =
        const { RefCell::new(None) };
}

async fn read_data(start: u64, size: u64) -> Result<Vec<u8>, JsValue> {
    let file = FILE
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("no file set"))?;
    let blob = file.slice_with_f64_and_f64(start as f64, start.saturating_add(size) as f64)?;
    let js = JsFuture::from(blob.array_buffer()).await?;
    let buf: js_sys::ArrayBuffer = js.dyn_into()?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// Clear positions when a new file is selected.
fn reset_file_stream_state() {
    FILE_POS.with(|slot| slot.borrow_mut().clear());
}

/// Send the next requested range of the selected file to the worker.
async fn satisfy_file_request(stream: String, amount: usize) -> Result<(), JsValue> {
    if amount == 0 {
        return Err(JsValue::from_str(
            "worker requested a zero-length file chunk",
        ));
    }

    let pos = FILE_POS.with(|slot| *slot.borrow().get(&stream).unwrap_or(&0));
    let amount =
        u64::try_from(amount).map_err(|_| JsValue::from_str("file request size is too large"))?;
    let data = read_data(pos, amount).await?;
    let len = data.len() as u64;

    debug!(
        "Main: sending {} file byte(s) for stream {stream} at offset {pos} for stream {stream}",
        data.len()
    );
    send_message(MainToWorker::Bytes(
        stream.clone(),
        vec![TaggedVec {
            data,
            tags: Vec::new(),
        }],
    ))
    .await?;

    FILE_POS.with(|slot| {
        slot.borrow_mut().insert(stream, pos.saturating_add(len));
    });
    Ok(())
}

/// Queue requests made before the user has selected an input source.
fn store_pending_file_request(stream: String, amount: usize) {
    PENDING_FILE_REQUESTS.with(|slot| slot.borrow_mut().push((stream, amount)));
}

/// Satisfy requests that arrived while the file chooser was still active.
async fn flush_pending_file_requests() -> Result<(), JsValue> {
    let requests = PENDING_FILE_REQUESTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
    for (stream, amount) in requests {
        satisfy_file_request(stream, amount).await?;
    }
    Ok(())
}

/// Route a worker pull request to the selected file source.
async fn handle_request_data(stream: String, amount: usize) -> Result<(), JsValue> {
    match input_source() {
        InputSource::File => satisfy_file_request(stream, amount).await,
        InputSource::None => {
            info!("Main: waiting for a file selection before serving stream {stream}");
            store_pending_file_request(stream, amount);
            Ok(())
        }
        InputSource::WebSocket | InputSource::RtlSdr => Ok(()),
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

/// Borrow the application-owned time sink handle from main-thread callbacks.
fn with_time_sink<T>(
    f: impl FnOnce(&time_sink::TimeSink) -> rustradio::Result<T>,
) -> rustradio::Result<T> {
    TIME_SINK.with(|slot| {
        let sink = slot.borrow();
        let sink = sink
            .as_ref()
            .ok_or_else(|| rustradio::Error::msg("time sink has not been initialized"))?;
        f(sink)
    })
}

/// Borrow the application-owned constellation sink handle from main-thread
/// callbacks.
fn with_constellation_sink<T>(
    f: impl FnOnce(&crate::constellation_sink::ConstellationSink) -> rustradio::Result<T>,
) -> rustradio::Result<T> {
    CONSTELLATION_SINK.with(|slot| {
        let sink = slot.borrow();
        let sink = sink
            .as_ref()
            .ok_or_else(|| rustradio::Error::msg("constellation sink has not been initialized"))?;
        f(sink)
    })
}

/// Borrow the application-owned spectrum sink handle from main-thread
/// callbacks.
fn with_spectrum_sink<T>(
    f: impl FnOnce(&crate::spectrum_sink::SpectrumSink) -> rustradio::Result<T>,
) -> rustradio::Result<T> {
    SPECTRUM_SINK.with(|slot| {
        let sink = slot.borrow();
        let sink = sink
            .as_ref()
            .ok_or_else(|| rustradio::Error::msg("spectrum sink has not been initialized"))?;
        f(sink)
    })
}

/// Borrow the application-owned waterfall sink handle from main-thread
/// callbacks.
fn with_waterfall_sink<T>(
    f: impl FnOnce(&crate::spectrum_sink::WaterfallSink) -> rustradio::Result<T>,
) -> rustradio::Result<T> {
    WATERFALL_SINK.with(|slot| {
        let sink = slot.borrow();
        let sink = sink
            .as_ref()
            .ok_or_else(|| rustradio::Error::msg("waterfall sink has not been initialized"))?;
        f(sink)
    })
}

/// Handle message sent from the worker.
async fn worker_msg(msg: WorkerToMain) -> Result<(), JsValue> {
    match msg {
        WorkerToMain::Floats(name, streams) => match name.as_str() {
            "iq_mag" => with_time_sink(|sink| sink.update(streams))?,
            "iq_spectrum" => {
                with_spectrum_sink(|sink| sink.update(&streams))?;
                with_waterfall_sink(|sink| sink.update(&streams))?;
            }
            "audio_demod" => {
                assert_eq!(streams.len(), 1);
                rustradio_ui::browser_audio::enqueue(streams[0].data.iter().copied())?;
            }
            other => log::error!("Unknown float vec: {other}"),
        },
        WorkerToMain::RequestData(name, amount) => {
            handle_request_data(name, amount).await?;
        }
        WorkerToMain::Complexes(name, streams) => {
            assert_eq!(name, "iq_constellation");
            with_constellation_sink(|sink| sink.update(streams))?;
        }
        WorkerToMain::ApplicationSpecific(msg) => match msg {
            Ax25Messages::Decoded(x) => {
                set_content(ID_RESULT, &format!("Decoded: {x:?}"))?;
            }
        },
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
        WorkerToMain::Ping(t) => {
            post_message(&MainToWorker::Pong(t)).unwrap();
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
        send_message(MainToWorker::Bytes(
            "rtl-sdr".into(),
            vec![TaggedVec {
                data: bytes,
                tags: vec![],
            }],
        ))
        .await
        .map_err(|_| JsValue::from_str("failed to send to worker"))?;
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
            post_message(&MainToWorker::Ping(js_performance_now()))?;
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
            let volume = get_element(ID_AUDIO_VOLUME)?
                .dyn_into::<HtmlInputElement>()?
                .value()
                .parse::<f32>()
                .unwrap_or(0.25)
                .clamp(0.0, 1.0);
            rustradio_ui::browser_audio::set_volume(volume);
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
            with_time_sink(|sink| sink.set_sample_rate(crate::worker::VIZ_SAMPLE_RATE as f64))?;
            rustradio_ui::browser_audio::reset()?;
            post_message(&MainToWorker::Start(crate::Ax25Start {
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
    start_worker::<crate::Ax25MainToWorker, crate::Ax25WorkerToMain, _, _>(worker_msg);

    let time_sink = time_sink::TimeSink::mount_by_id(
        ID_TIME_SINK,
        time_sink::TimeSinkOptions {
            title: "Signal Strength".into(),
            subtitle: "Float stream amplitude over time".into(),
            y_label: "Amplitude".into(),
            sample_rate: crate::worker::VIZ_SAMPLE_RATE as f64,
            ..Default::default()
        },
    )?;
    TIME_SINK.with(|slot| {
        *slot.borrow_mut() = Some(time_sink);
    });

    let constellation_sink = crate::constellation_sink::ConstellationSink::mount_by_id(
        ID_CONSTELLATION_SINK,
        crate::constellation_sink::ConstellationSinkOptions {
            title: "Constellation".into(),
            subtitle: "1 ksps I/Q sample plane".into(),
            ..Default::default()
        },
    )?;
    CONSTELLATION_SINK.with(|slot| {
        *slot.borrow_mut() = Some(constellation_sink);
    });

    let spectrum_sample_rate = crate::worker::IF_SAMPLE_RATE as f32;
    let spectrum_sink = crate::spectrum_sink::SpectrumSink::mount_by_id(
        ID_SPECTRUM_SINK,
        crate::spectrum_sink::SpectrumSinkOptions {
            title: "Spectrum".into(),
            subtitle: "FFT power frame".into(),
            sample_rate: spectrum_sample_rate,
        },
    )?;
    SPECTRUM_SINK.with(|slot| {
        *slot.borrow_mut() = Some(spectrum_sink);
    });

    let waterfall_sink = crate::spectrum_sink::WaterfallSink::mount_by_id(
        ID_WATERFALL_SINK,
        crate::spectrum_sink::WaterfallSinkOptions {
            title: "Waterfall".into(),
            subtitle: "FFT power history".into(),
            sample_rate: spectrum_sample_rate,
            ..Default::default()
        },
    )?;
    WATERFALL_SINK.with(|slot| {
        *slot.borrow_mut() = Some(waterfall_sink);
    });

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
            reset_file_stream_state();
            spawn_local(async {
                if let Err(e) = flush_pending_file_requests().await {
                    error!("Main: failed to start file input: {e:?}");
                    let _ = set_content(ID_RESULT, "File input error.");
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
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    {
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            match websocket_message_bytes(event.data()) {
                Some(_data) => {
                    // TODO: re-implement.
                    /*
                    if let Err(e) = post_message(MainToWorker::DataStream(data)) {
                        close_websocket_after_error(&e);
                    }
                    */
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
            /*
            if let Err(e) = post_message(MainToWorker::DataStream(Vec::new())) {
                error!("Main: failed to send websocket disconnect to worker: {e:?}");
            }
            */
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
