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

/// Messages going from main (UI) thread to worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum MainToWorker {
    /// Data going to a WasmSource.
    ///
    /// TODO: allow for multiple incoming streams, by identifying them somehow.
    Data(Vec<u8>),

    /// Inform that stream has ended.
    ///
    /// TODO: allow for multiple incoming streams, by identifying them somehow.
    Eof,

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the worker.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),
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

/// Entry point for both worker and main thread.
///
/// This function is run for both, and it does common initialization and then
/// calls out to the respective special setups.
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
