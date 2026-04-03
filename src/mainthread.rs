// This file is for stuff in the main thread.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use serde_wasm_bindgen::{from_value, to_value};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::js_sys::Uint8Array;
use web_sys::{
    Element, Event, File, FileReader, HtmlInputElement, MessageEvent, ProgressEvent, Worker,
};

use crate::log;
use crate::uint8array_to_vec;
use crate::{MainToWorker, WorkerToMain};

const HTML_DISABLED: &str = "disabled";
const ID_RESULT: &str = "result";
const ID_FILE_INPUT: &str = "fileInput";

/*
 * JS global variables.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = worker)]
    static WORKER: Worker;
}
*/

thread_local! {
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
}

async fn worker_msg(e: MessageEvent) -> Result<(), JsValue> {
    match from_value::<WorkerToMain>(e.data())? {
        WorkerToMain::Ready => {
            log("Received WorkerToMain::Ready");
            worker_msg_ready().await?;
        }
        WorkerToMain::Result(s) => {
            set_content(ID_RESULT, &s)?;
            web_sys::console::log_1(&format!("worker returned: {s}").into());
        }
        WorkerToMain::Ping => {}
        WorkerToMain::Pong => {}
    }
    Ok(())
}

async fn worker_msg_ready() -> Result<(), JsValue> {
    // Set up Add button.
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            web_sys::console::log_1(&"button clicked".into());
            set_content(ID_RESULT, &format!("Result of add: {}", crate::add(3, 5)))
        });
        let btn = get_element("btn-add")?.dyn_into::<web_sys::HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        btn.remove_attribute(HTML_DISABLED)?;
        handler.forget();
    }

    // Set up file input thing.
    {
        let input = get_element(ID_FILE_INPUT)?.dyn_into::<HtmlInputElement>()?;
        input.set_disabled(false);
        // TODO: make some sort of UI friendly bounded channel. Don't want it to
        // block.
        let (tx, _rx) = mpsc::channel();
        log("install");
        install_file_chunk_listener(input, tx, 64 * 1024)?; // 64 KiB chunks
    }
    Ok(())
}

fn worker() -> Worker {
    WORKER.with(|cell| {
        cell.get_or_init(|| {
            let opts = web_sys::WorkerOptions::new();
            opts.set_type(web_sys::WorkerType::Module);
            let worker = Worker::new_with_options("./worker.js", &opts).unwrap();

            // TODO: magic value.
            let onmessage = Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(
                move |e: MessageEvent| {
                    spawn_local(async move {
                        if let Err(e) = worker_msg(e).await {
                            // TODO: Surface error on page.
                            log(&format!("Inner receiver thing: {e:?}"));
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

fn set_content(id: &str, content: &str) -> Result<(), JsValue> {
    log(&format!("Setting inner HTML of {id}"));
    get_element(id)?.set_inner_html(content);
    Ok(())
}

pub(crate) async fn setup() -> Result<(), JsValue> {
    // Init the worker.
    worker();
    // TODO: wait for worker to be ready.

    /*
       let worker = Worker::new("./worker.js")?;

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let reply = js_sys::Uint8Array::new(&e.data());
        let mut buf = vec![0; reply.length() as usize];
        reply.copy_to(&mut buf);
        web_sys::console::log_1(&format!("main got: {:?}", buf).into());
    });

    worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
    */
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
    input: HtmlInputElement,
    tx: mpsc::Sender<Vec<u8>>,
    chunk_size: u64,
) -> Result<(), JsValue> {
    log("Adding listener");
    let input = Rc::new(input);
    let input2 = input.clone();
    let on_change =
        Closure::<dyn FnMut(Event) -> Result<(), JsValue>>::wrap(Box::new(move |_event: Event| {
            let tx = tx.clone();
            let Some(files) = input.files() else {
                return Ok(());
            };
            let Some(file) = files.get(0) else {
                return Ok(());
            };
            log("Read file now!");
            set_content(ID_RESULT, "Running rustradio on input…")?;
            if let Err(err) = read_file_in_chunks(file, tx, chunk_size) {
                web_sys::console::error_1(&err);
            }
            Ok(())
        }));

    log("Adding event listener");
    input2.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    log("Done Adding event listener");
    on_change.forget();
    Ok(())
}

fn read_file_in_chunks(
    file: File,
    tx: mpsc::Sender<Vec<u8>>,
    chunk_size: u64,
) -> Result<(), JsValue> {
    let file_size = file.size() as u64;
    let offset = Rc::new(RefCell::new(0u64));
    let file = Rc::new(file);

    let read_next: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let read_next_clone = Rc::clone(&read_next);

    let whole_file = Rc::new(RefCell::<Vec<u8>>::new(vec![]));
    let whole_file_inner = whole_file.clone();

    *read_next.borrow_mut() = Some(Box::new({
        let offset = Rc::clone(&offset);
        let file = Rc::clone(&file);

        move || {
            let start = *offset.borrow();
            if start >= file_size {
                post_eof();
                // TODO: stream it instead.
                //let a = crate::radio_wrap_1200(&whole_file.borrow()).unwrap();
                let bytes = whole_file.borrow();
                worker()
                    .post_message(&to_value(&MainToWorker::Data(bytes.to_vec())).unwrap())
                    .unwrap();
                //log(&format!("Output: {a}"));
                //set_content(ID_RESULT, &a).unwrap();
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
                let whole_file_inner = Rc::clone(&whole_file_inner);
                let _tx = tx.clone();
                Closure::<dyn FnMut(ProgressEvent)>::wrap(Box::new(move |_e: ProgressEvent| {
                    let Ok(result) = reader.result() else {
                        web_sys::console::error_1(&JsValue::from_str(
                            "FileReader returned no result",
                        ));
                        return;
                    };

                    let bytes = Uint8Array::new(&result);
                    let v = uint8array_to_vec(&bytes);
                    whole_file_inner.borrow_mut().extend(&v);
                    //tx.send(v).unwrap();
                    post_chunk_message(&file_name, chunk_index, start, end, is_last, &bytes);

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
    log("Read file in chunks done");
    Ok(())
}

fn post_eof() {
    log("Post EOF");
}

fn post_chunk_message(
    _file_name: &str,
    _chunk_index: u32,
    start: u64,
    end: u64,
    _is_last: bool,
    _bytes: &Uint8Array,
) {
    log(&format!(
        "Post chunk message not yet implemented, of len {}",
        end - start
    ));
    //todo!()
}
