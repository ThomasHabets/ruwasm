// This file is for stuff in the main (UI) thread.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use log::{debug, error, info, warn};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys;
use web_sys::js_sys::Uint8Array;
use web_sys::{
    BinaryType, Element, Event, File, HtmlInputElement, MessageEvent, WebSocket, Worker,
};

use crate::FloatStream;
use crate::RECEIVER_SOURCE;
use crate::js_performance_now;
use crate::{MainToWorker, WorkerToMain};
use crate::{ReceiverId, ReqData};

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

struct WebSocketSourceState {
    chunks: VecDeque<Vec<u8>>,
    eof: bool,
}

impl WebSocketSourceState {
    /// Build the per-stream buffer used between WebSocket callbacks and the
    /// worker's pull-based data requests.
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            eof: false,
        }
    }
}

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
    static LATEST_FLOAT_STREAMS: RefCell<Vec<FloatStream>> = const { RefCell::new(Vec::new()) };
    static FILE: RefCell<Option<File>> = const { RefCell::new(None) };
    static INPUT_SOURCE: RefCell<InputSource> = const { RefCell::new(InputSource::None) };
    static PENDING_DATA_REQUEST: RefCell<Option<ReqData>> = const { RefCell::new(None) };
    static WS_SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
    static WS_SOURCE_STATE: RefCell<WebSocketSourceState> = RefCell::new(WebSocketSourceState::new());
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

/// Route the worker's pull-based input request to the browser-side source
/// selected by the user, or park it until a source is selected.
async fn handle_data_request(req: ReqData) -> Result<(), JsValue> {
    match input_source() {
        InputSource::File => satisfy_file_request(req).await,
        InputSource::WebSocket => satisfy_websocket_request(req),
        InputSource::None => {
            info!("Main: waiting for an input source before satisfying data request");
            store_pending_request(req);
            Ok(())
        }
    }
}

/// Serve a receiver read from the selected capture file using the worker's
/// requested byte range.
async fn satisfy_file_request(req: ReqData) -> Result<(), JsValue> {
    let data = read_data(req.pos, req.size).await?;
    send_data_or_eof(req.receiver, data)
}

/// Serve a receiver read from queued WebSocket chunks, or remember the request
/// until the next chunk or close event can answer it.
fn satisfy_websocket_request(req: ReqData) -> Result<(), JsValue> {
    let msg = WS_SOURCE_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if let Some(data) = state.chunks.pop_front() {
            Some(MainToWorker::Data(req.receiver, data))
        } else if state.eof {
            Some(MainToWorker::Eof(req.receiver))
        } else {
            store_pending_request(req);
            None
        }
    });
    if let Some(msg) = msg {
        post_message(msg)?;
    }
    Ok(())
}

/// Convert browser input bytes into the data/EOF messages expected by the
/// worker-side source block.
fn send_data_or_eof(rcv: ReceiverId, data: Vec<u8>) -> Result<(), JsValue> {
    if data.is_empty() {
        post_message(MainToWorker::Eof(rcv))
    } else {
        debug!("Main: sending {} input byte(s) to worker", data.len());
        post_message(MainToWorker::Data(rcv, data))
    }
}

/// Read the active input source for request handlers and async browser
/// callbacks.
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

/// Keep the worker's outstanding request while the main thread waits for user
/// source selection or for a streaming WebSocket chunk.
fn store_pending_request(req: ReqData) {
    PENDING_DATA_REQUEST.with(|pending| *pending.borrow_mut() = Some(req));
}

/// Take the parked worker request so exactly one later source event satisfies
/// it.
fn take_pending_request() -> Option<ReqData> {
    PENDING_DATA_REQUEST.with(|pending| pending.borrow_mut().take())
}

/// Clear WebSocket buffering for a newly selected stream before callbacks begin
/// appending chunks.
fn reset_websocket_state() {
    WS_SOURCE_STATE.with(|slot| *slot.borrow_mut() = WebSocketSourceState::new());
}

/// Feed a WebSocket payload into the worker-facing stream, satisfying a parked
/// request immediately when the graph is waiting for bytes.
fn queue_websocket_data(data: Vec<u8>) -> Result<(), JsValue> {
    if data.is_empty() {
        return Ok(());
    }
    if input_source() != InputSource::WebSocket {
        return Ok(());
    }

    if let Some(req) = take_pending_request() {
        send_data_or_eof(req.receiver, data)
    } else {
        WS_SOURCE_STATE.with(|slot| slot.borrow_mut().chunks.push_back(data));
        Ok(())
    }
}

/// Mark the WebSocket input complete and wake any parked request with EOF so
/// the worker graph can drain.
fn mark_websocket_eof() -> Result<(), JsValue> {
    WS_SOURCE_STATE.with(|slot| slot.borrow_mut().eof = true);
    if let Some(req) = take_pending_request() {
        post_message(MainToWorker::Eof(req.receiver))?;
    }
    Ok(())
}

/// Handle message sent from the worker.
async fn worker_msg(e: MessageEvent) -> Result<(), JsValue> {
    match e.data().try_into()? {
        WorkerToMain::ReqData(req) => {
            info!("Main: received WorkerToMain::ReqData");
            handle_data_request(req).await?;
        }
        WorkerToMain::Ready => {
            info!("Main: Received WorkerToMain::Ready");
            worker_msg_ready().await?;
        }
        WorkerToMain::Result(s) => {
            set_content(ID_RESULT, &s)?;
            info!("Main: worker returned: {s}");
        }
        WorkerToMain::LogLine { level, line } => {
            log::log!(level, "[worker] {line}");
        }
        WorkerToMain::FloatStreams(streams) => {
            let lens: Vec<_> = streams.iter().map(|s| s.samples.len()).collect();
            let n = lens[0].min(10);
            info!(
                "Main: received {} float stream(s) lens {lens:?}. A few samples: {:?}",
                streams.len(),
                &streams[0].samples[..n]
            );
            for (n, s) in streams.iter().enumerate() {
                debug!("Stream {n} name {}", s.name);
            }
            crate::time_sink::update(streams)?;
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
            post_message(MainToWorker::Start { samp_rate, rtlsdr })?;
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
        install_file_chunk_listener(RECEIVER_SOURCE, input)?; // 64 KiB chunks
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
WASM code version: {}
WASM built by Rust version: {}",
            crate::git_version(),
            crate::rustc_version()
        ),
    )?;

    crate::time_sink::setup_graph_ui()?;

    Ok(())
}

fn install_file_chunk_listener(_id: ReceiverId, input: HtmlInputElement) -> Result<(), JsValue> {
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
            if let Some(req) = take_pending_request() {
                spawn_local(async move {
                    if let Err(e) = satisfy_file_request(req).await {
                        error!("Main: failed to read selected file: {e:?}");
                    }
                });
            }
            Ok(())
        }));

    info!("Adding event listener");
    input2.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    info!("Done Adding event listener");
    on_change.forget();
    Ok(())
}

/// Start the WebSocket input path from the UI button by claiming the source,
/// resetting stream state, and connecting the browser socket.
fn start_websocket_source(url: &str) -> Result<(), JsValue> {
    select_input_source(InputSource::WebSocket)?;
    reset_websocket_state();
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

/// Own the browser WebSocket and wire its lifecycle callbacks into the same
/// worker data/EOF flow used by file input.
fn connect_websocket(url: &str) -> Result<(), JsValue> {
    let ws = WebSocket::new(url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    {
        let url = url.to_string();
        let onopen = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            info!("Main: websocket input connected to {url}");
            let _ = set_content(ID_RESULT, "WebSocket input connected. Waiting for samples…");
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    {
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            match websocket_message_bytes(event.data()) {
                Some(data) => {
                    if let Err(e) = queue_websocket_data(data) {
                        error!("Main: failed to queue websocket input: {e:?}");
                    }
                }
                None => {
                    warn!("Main: ignoring websocket message with unsupported payload type");
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
            if let Err(e) = mark_websocket_eof() {
                error!("Main: failed to send websocket EOF to worker: {e:?}");
            }
            let _ = set_content(ID_RESULT, "WebSocket input closed.");
            WS_SOCKET.with(|slot| {
                slot.borrow_mut().take();
            });
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    WS_SOCKET.with(|slot| {
        *slot.borrow_mut() = Some(ws);
    });
    Ok(())
}

/// Normalize supported WebSocket payload shapes into byte chunks so binary and
/// text senders can both feed the receiver block.
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
