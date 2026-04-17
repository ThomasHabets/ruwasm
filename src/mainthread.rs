// This file is for stuff in the main (UI) thread.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::rc::Rc;

use log::{debug, info};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys;
use web_sys::js_sys::Uint8Array;
use web_sys::{Element, Event, File, HtmlInputElement, MessageEvent, Worker};

use crate::FloatStream;
use crate::RECEIVER_SOURCE;
use crate::ReceiverId;
use crate::js_performance_now;
use crate::{MainToWorker, WorkerToMain};

const HTML_DISABLED: &str = "disabled";
const ID_RESULT: &str = "result";
const ID_START: &str = "btn-start";
const ID_SAMP_RATE: &str = "input-samp-rate";
const ID_FILE_INPUT: &str = "fileInput";
const ID_ADD: &str = "btn-add";
const ID_PING: &str = "btn-ping";
const ID_HTML: &str = "root-html";

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
    static LATEST_FLOAT_STREAMS: RefCell<Vec<FloatStream>> = const { RefCell::new(Vec::new()) };
    static FILE: RefCell<Option<File>> = const { RefCell::new(None) };
}

pub(crate) fn post_message(msg: MainToWorker) -> Result<(), JsValue> {
    let msg = msg.try_into()?;
    worker().post_message(&msg)
}

pub async fn sleep(duration: std::time::Duration) {
    let ms_u128 = duration.as_millis();
    let ms = u32::try_from(ms_u128).unwrap_or(u32::MAX);
    let timeout = i32::try_from(ms).unwrap_or(i32::MAX);

    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let cb = Closure::once_into_js(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });

        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), timeout)
            .unwrap();
    });

    let _ = JsFuture::from(promise).await;
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

/// Handle message sent from the worker.
async fn worker_msg(e: MessageEvent) -> Result<(), JsValue> {
    match e.data().try_into()? {
        WorkerToMain::ReqData(rcv, pos, size) => {
            info!("Main: Received WorkerToMain::ReqData");
            let data = loop {
                match read_data(pos, size).await {
                    Ok(o) => break o,
                    Err(e) => {
                        info!("Main: file err: {e:?}");
                        sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            };
            if data.is_empty() {
                post_message(MainToWorker::Eof(rcv)).unwrap();
            } else {
                info!("Main: Sending back data len {}", data.len());
                post_message(MainToWorker::Data(rcv, data)).unwrap();
            }
        }
        WorkerToMain::Ready => {
            info!("Main: Received WorkerToMain::Ready");
            worker_msg_ready().await?;
        }
        WorkerToMain::Result(s) => {
            set_content(ID_RESULT, &s)?;
            info!("Main: worker returned: {s}");
        }
        WorkerToMain::FloatStreams(streams) => {
            info!("Main: received {} float stream(s)", streams.len());
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
            crate::time_sink::set_sample_rate(samp_rate as f64);
            post_message(MainToWorker::Start { samp_rate })?;
            get_element(ID_FILE_INPUT)?
                .dyn_into::<HtmlInputElement>()?
                .set_disabled(false);
            get_element(ID_START)?
                .dyn_into::<web_sys::HtmlButtonElement>()?
                .set_disabled(true);
            get_element(ID_SAMP_RATE)?
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
        install_file_chunk_listener(RECEIVER_SOURCE, input, 64 * 1024)?; // 64 KiB chunks
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
            let worker = Worker::new_with_options("./wasm-mod.js", &opts).unwrap();

            let onmessage = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
                    spawn_local(async move {
                        if let Err(e) = worker_msg(e).await {
                            // TODO: Surface error on page.
                            info!("Inner receiver thing: {e:?}");
                        }
                    });
                    Ok(())
                },
            );

            worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();

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
    info!("Setting inner HTML of {id}");
    get_element(id)?.set_inner_html(content);
    Ok(())
}

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
            r#"<b>WASM loaded</b>
WASM code version: {}
WASM built by Rust version: {}"#,
            crate::git_version(),
            crate::rustc_version()
        ),
    )?;

    crate::time_sink::setup_graph_ui()?;

    Ok(())
}

fn install_file_chunk_listener(
    _id: ReceiverId,
    input: HtmlInputElement,
    _chunk_size: u64,
) -> Result<(), JsValue> {
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
            get_element(ID_FILE_INPUT)?
                .dyn_into::<web_sys::HtmlInputElement>()?
                .set_disabled(true);
            set_content(ID_RESULT, "Running rustradio on input…")?;
            FILE.with(|slot| {
                *slot.borrow_mut() = Some(file);
            });
            Ok(())
        }));

    info!("Adding event listener");
    input2.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    info!("Done Adding event listener");
    on_change.forget();
    Ok(())
}
