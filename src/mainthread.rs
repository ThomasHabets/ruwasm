// This file is for stuff in the main (UI) thread.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::rc::Rc;

use log::info;
use serde_wasm_bindgen::{from_value, to_value};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::js_sys::Uint8Array;
use web_sys::{
    Element, Event, File, FileReader, HtmlInputElement, MessageEvent, ProgressEvent, Worker,
};

use crate::RECEIVER_SOURCE;
use crate::ReceiverId;
use crate::js_performance_now;
use crate::uint8array_to_vec;
use crate::{MainToWorker, WorkerToMain};

const HTML_DISABLED: &str = "disabled";
const ID_RESULT: &str = "result";
const ID_START: &str = "btn-start";
const ID_SAMP_RATE: &str = "input-samp-rate";
const ID_FILE_INPUT: &str = "fileInput";
const ID_ADD: &str = "btn-add";
const ID_PING: &str = "btn-ping";

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
}

/// Handle message sent from the worker.
async fn worker_msg(e: MessageEvent) -> Result<(), JsValue> {
    match from_value::<WorkerToMain>(e.data())? {
        WorkerToMain::Ready => {
            info!("Main: Received WorkerToMain::Ready");
            worker_msg_ready().await?;
        }
        WorkerToMain::Result(s) => {
            set_content(ID_RESULT, &s)?;
            info!("Main: worker returned: {s}");
        }
        WorkerToMain::Ping(t) => {
            worker()
                .post_message(&to_value(&MainToWorker::Pong(t)).unwrap())
                .unwrap();
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
            worker().post_message(&to_value(&MainToWorker::Ping(js_performance_now()))?)?;
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
            worker().post_message(&to_value(&MainToWorker::Start { samp_rate })?)?;
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
        info!("Initializing the worker");
        cell.get_or_init(|| {
            info!("Initializing the worker.2");
            let opts = web_sys::WorkerOptions::new();
            opts.set_type(web_sys::WorkerType::Module);
            let worker = Worker::new_with_options("./worker.js", &opts).unwrap();

            // TODO: magic value.
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
fn get_element(id: &str) -> Result<Element, JsValue> {
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

    Ok(())
}

fn install_file_chunk_listener(
    id: ReceiverId,
    input: HtmlInputElement,
    chunk_size: u64,
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
            if let Err(err) = read_file_in_chunks(id, file, chunk_size) {
                web_sys::console::error_1(&err);
            }
            Ok(())
        }));

    info!("Adding event listener");
    input2.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    info!("Done Adding event listener");
    on_change.forget();
    Ok(())
}

fn read_file_in_chunks(id: ReceiverId, file: File, chunk_size: u64) -> Result<(), JsValue> {
    let file_size = file.size() as u64;
    let offset = Rc::new(RefCell::new(0u64));
    let file = Rc::new(file);

    let read_next: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let read_next_clone = Rc::clone(&read_next);

    *read_next.borrow_mut() = Some(Box::new({
        let offset = Rc::clone(&offset);
        let file = Rc::clone(&file);

        move || {
            let start = *offset.borrow();
            if start >= file_size {
                post_eof(id);
                return;
            }

            let end = (start + chunk_size).min(file_size);
            let blob = match file.slice_with_f64_and_f64(start as f64, end as f64) {
                Ok(b) => b,
                Err(err) => {
                    web_sys::console::error_1(&err);
                    return;
                }
            };

            let reader = Rc::new(match FileReader::new() {
                Ok(r) => r,
                Err(err) => {
                    web_sys::console::error_1(&err);
                    return;
                }
            });

            let next_offset = Rc::clone(&offset);
            let next_read = Rc::clone(&read_next_clone);
            let file_name = file.name();
            let chunk_index = (start / chunk_size) as u32;
            let is_last = end == file_size;

            let onload = {
                let reader = Rc::clone(&reader);
                Closure::<dyn FnMut(ProgressEvent)>::wrap(Box::new(move |_e: ProgressEvent| {
                    let Ok(result) = reader.result() else {
                        web_sys::console::error_1(&JsValue::from_str(
                            "FileReader returned no result",
                        ));
                        return;
                    };

                    let bytes = Uint8Array::new(&result);
                    let v = uint8array_to_vec(&bytes);
                    post_chunk_message(
                        id,
                        &file_name,
                        chunk_index,
                        start,
                        end,
                        file_size,
                        is_last,
                        v,
                    );

                    *next_offset.borrow_mut() = end;

                    if let Some(next) = next_read.borrow().as_ref() {
                        next();
                    }
                }))
            };

            reader.set_onload(Some(onload.as_ref().unchecked_ref()));
            onload.forget();

            if let Err(err) = reader.read_as_array_buffer(&blob) {
                web_sys::console::error_1(&err);
            }
        }
    }));

    if let Some(f) = read_next.borrow().as_ref() {
        f();
    }
    info!("Read file in chunks done");
    Ok(())
}

fn post_eof(id: ReceiverId) {
    info!("Main: Post EOF");
    worker()
        .post_message(&to_value(&MainToWorker::Eof(id)).unwrap())
        .unwrap();
}

fn post_chunk_message(
    id: ReceiverId,
    _file_name: &str,
    _chunk_index: u32,
    start: u64,
    _end: u64,
    file_size: u64,
    _is_last: bool,
    data: Vec<u8>,
) {
    info!(
        "Main: Post chunk message of len {}. Percent: {}",
        data.len(),
        100 * start / file_size
    );
    worker()
        .post_message(&to_value(&MainToWorker::Data(id, data)).unwrap())
        .unwrap();
    //todo!()
}
