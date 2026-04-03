use log::info;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use web_sys::js_sys::Uint8Array;

mod mainthread;
mod wasm_graph;
mod wasm_source;
mod worker;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance)]
    fn now() -> f64;
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum MainToWorker {
    Data(Vec<u8>),
    Eof,
    Ping(f64),
    Pong(f64),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum WorkerToMain {
    Ready,
    Ping(f64),
    Pong(f64),
    Result(String),
}

#[wasm_bindgen]
#[derive(Serialize)]
pub struct Return {
    a: i32,
    b: i32,
    sum: i32,
    eval: String,
}

#[wasm_bindgen(start)]
pub async fn start() -> Result<(), JsValue> {
    console_log::init_with_level(log::Level::Debug).expect("Failed to init logging");
    info!("Logging initialized");

    info!("ruwasm: Starting at time {}", now());
    console_error_panic_hook::set_once();

    if web_sys::window().is_none() {
        info!("Worker: setting up");
        worker::setup().await
    } else {
        info!("Main: setting up");
        mainthread::setup().await
    }
}

#[wasm_bindgen]
pub fn git_version() -> String {
    rustradio::sys::initialize_rustradio();
    info!("git_version() called");
    env!("GIT_VERSION").to_string()
}

#[wasm_bindgen]
pub fn rustc_version() -> String {
    env!("RUSTC_VERSION").to_string()
}

#[wasm_bindgen]
pub fn compute(n: u32) -> u32 {
    info!("From rust: compute() called");
    (0..n).map(|x| x * x).sum()
}

#[wasm_bindgen]
pub fn add(a: i32, b: i32) -> String {
    info!("Hello world, adding {a} and {b}");
    serde_json::to_string(&Return {
        a,
        b,
        sum: a + b,
        eval: "console.log('hello world')".to_string(),
    })
    .unwrap()
}

pub(crate) fn uint8array_to_vec(arr: &Uint8Array) -> Vec<u8> {
    let mut buf = vec![0; arr.length() as usize];
    arr.copy_to(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
}
