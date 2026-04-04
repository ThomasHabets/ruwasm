use log::info;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use web_sys::js_sys::Uint8Array;

mod mainthread;
mod wasm_graph;
mod wasm_source;
mod worker;

const RECEIVER_SOURCE: ReceiverId = ReceiverId(0);

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn js_performance_now() -> f64;
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiverId(u64);

/// Messages going from main (UI) thread to worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum MainToWorker {
    /// Start the graph.
    Start { samp_rate: u64 },

    /// Data going to a WasmSource or something.
    Data(ReceiverId, Vec<u8>),

    /// Inform that stream has ended.
    Eof(ReceiverId),

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the worker.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),
}

impl TryInto<wasm_bindgen::JsValue> for MainToWorker {
    type Error = wasm_bindgen::JsValue;
    fn try_into(self) -> Result<wasm_bindgen::JsValue, Self::Error> {
        Ok(serde_wasm_bindgen::to_value(&self)?)
    }
}

/// Messages from the worker to the main (UI) thread.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum WorkerToMain {
    Ready,

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the main thread.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),

    /// At the end of execution, provide the result as a string.
    Result(String),
}

impl TryInto<wasm_bindgen::JsValue> for WorkerToMain {
    type Error = wasm_bindgen::JsValue;
    fn try_into(self) -> Result<wasm_bindgen::JsValue, Self::Error> {
        Ok(serde_wasm_bindgen::to_value(&self)?)
    }
}

/// Entry point for both worker and main thread.
///
/// This function is run for both, and it does common initialization and then
/// calls out to the respective special setups.
#[wasm_bindgen(start)]
pub async fn start() -> Result<(), JsValue> {
    // Init logging.
    console_log::init_with_level(log::Level::Debug).expect("Failed to init logging");
    info!("Logging initialized (expect this message is once for UI thread and worker)");
    console_error_panic_hook::set_once();

    if web_sys::window().is_none() {
        info!("Worker: Starting at time {}", js_performance_now());
        worker::setup().await
    } else {
        info!("Main: Starting at time {}", js_performance_now());
        mainthread::setup().await
    }
}

/// Get a descriptive git string for the current code.
#[wasm_bindgen]
pub fn git_version() -> String {
    rustradio::sys::initialize_rustradio();
    info!("git_version() called");
    env!("GIT_VERSION").to_string()
}

/// Get the version of the Rust compiler that built this.
#[wasm_bindgen]
pub fn rustc_version() -> String {
    env!("RUSTC_VERSION").to_string()
}

#[wasm_bindgen]
pub fn add(a: i32, b: i32) -> String {
    info!("Hello world, adding {a} and {b}");
    format!("Add results: {}", a + b)
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
