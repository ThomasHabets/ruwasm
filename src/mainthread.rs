// This file is for stuff in the main (UI) thread.

use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use log::{debug, error, info, warn};
use rustradio::data_stream::{
    BytesReader, DataStreamId, PROTOCOL_VERSION, Packet, RequestData, SyncWriter,
};
use rustradio_ui::FloatStream;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys;
use web_sys::js_sys::Uint8Array;
use web_sys::{
    BinaryType, Element, Event, File, HtmlInputElement, MessageEvent, WebSocket, Worker,
};

use crate::js_performance_now;

//type MainToWorker<'a> = crate::MainToWorker<crate::ApplicationSpecific, crate::StartupParameters>;
type MainToWorker = crate::MainToWorker<crate::Ax25Impl>;
type WorkerToMain = crate::WorkerToMain<crate::Ax25Impl>;

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
const ID_HTML: &str = "root-html";

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputSource {
    None,
    File,
    WebSocket,
}

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
    static LATEST_FLOAT_STREAMS: RefCell<Vec<FloatStream>> = const { RefCell::new(Vec::new()) };
    static FILE: RefCell<Option<File>> = const { RefCell::new(None) };
    static FILE_DATA_STREAM_READER: RefCell<BytesReader> = RefCell::new(BytesReader::new());
    static FILE_DATA_STREAM_VERSION_SENT: Cell<bool> = const { Cell::new(false) };
    static FILE_DATA_STREAM_PEER_VERSION_SEEN: Cell<bool> = const { Cell::new(false) };
    static FILE_DATA_STREAM_POS: RefCell<HashMap<DataStreamId, u64>> = RefCell::new(HashMap::new());
    static INPUT_SOURCE: RefCell<InputSource> = const { RefCell::new(InputSource::None) };
    static PENDING_WORKER_DATA_STREAM: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static WS_SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
}

pub(crate) fn post_message(msg: MainToWorker) -> Result<(), JsValue> {
    let msg = msg.try_into()?;
    worker().post_message(&msg)
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
            set_content(ID_RESULT, &format!("{s:?}"))?;
            info!("Main: worker returned: {s:?}");
        }
        WorkerToMain::LogLine { level, line } => {
            log::log!(level, "[worker] {line}");
        }
        WorkerToMain::FloatStreams(streams) => {
            let lens: Vec<_> = streams.iter().map(|s| s.samples.len()).collect();
            let n = lens[0].min(10);
            debug!(
                "Main: received {} float stream(s) lens {lens:?}. A few samples: {:?}",
                streams.len(),
                &streams[0].samples[..n]
            );
            for (n, s) in streams.iter().enumerate() {
                debug!("Stream {n} name {}", s.name);
            }
            crate::time_sink::update(streams)?;
        }
        WorkerToMain::ComplexStreams(streams) => {
            let lens: Vec<_> = streams.iter().map(|s| s.samples.len()).collect();
            debug!(
                "Main: received {} complex stream(s) lens {lens:?}",
                streams.len()
            );
            crate::constellation_sink::update(streams)?;
        }
        WorkerToMain::FloatPduStreams(streams) => {
            let lens: Vec<_> = streams.iter().map(|s| s.samples.len()).collect();
            debug!(
                "Main: received {} float PDU stream(s) lens {lens:?}",
                streams.len()
            );
            crate::spectrum_sink::update(streams)?;
        }
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

/// Handle receiving a message from the worker saying it's ready.
///
/// This means we should enable UI controls.
#[allow(clippy::unused_async)]
async fn worker_msg_ready() -> Result<(), JsValue> {
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
            post_message(MainToWorker::Ping(js_performance_now()))?;
            Ok(())
        });
        let btn = get_element(ID_PING)?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        btn.remove_attribute(HTML_DISABLED)?;
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
            let rtlsdr = get_element(ID_RTLSDR_FORMAT)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .checked();
            // TODO: hard coded here.
            crate::time_sink::set_sample_rate(1000.0);
            post_message(MainToWorker::Start(crate::Ax25Start { samp_rate, rtlsdr }))?;
            get_element(ID_FILE_INPUT)?
                .dyn_into::<HtmlInputElement>()?
                .set_disabled(false);
            get_element(ID_WS_URL)?
                .dyn_into::<HtmlInputElement>()?
                .set_disabled(false);
            get_element(ID_WS_CONNECT)?
                .dyn_into::<web_sys::HtmlButtonElement>()?
                .set_disabled(false);
            get_element(ID_START)?
                .dyn_into::<web_sys::HtmlButtonElement>()?
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
fn worker() -> Worker {
    WORKER.with(|cell| {
        cell.get_or_init(|| {
            info!("Main: Starting the worker");
            let opts = web_sys::WorkerOptions::new();
            opts.set_type(web_sys::WorkerType::Module);
            opts.set_name("RustRadio worker");
            let worker = Worker::new_with_options("./wasm-mod.js", &opts).unwrap();

            // Set message handler.
            let onmessage = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
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
    get_element(ID_WS_URL)?
        .dyn_into::<HtmlInputElement>()?
        .set_disabled(true);
    get_element(ID_WS_CONNECT)?
        .dyn_into::<web_sys::HtmlButtonElement>()?
        .set_disabled(true);
    Ok(())
}
