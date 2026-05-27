#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
// TODO: fix some of the above
use log::info;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

mod complex_sink;
mod constellation_sink;
mod domlogger;
mod float_pdu_sink;
mod float_sink;
mod mainthread;
mod spectrum_sink;
mod time_sink;
mod wasm_graph;
mod wasm_source;
mod worker;
mod workerlogger;

const RECEIVER_SOURCE: ReceiverId = ReceiverId(0);

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn js_performance_now() -> f64;
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiverId(u64);

/// Stream of floats going between worker and UI thread.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, PartialOrd)]
pub struct FloatStream {
    pub name: String,
    pub tags: Vec<rustradio::stream::Tag>,
    pub samples: Vec<rustradio::Float>,
}

/// Borrow version of `FloatStream`.
///
/// Used to avoid copies when e.g. sending directly from a RustRadio stream.
///
/// Must serialize the same as `FloatStream`.
#[derive(Serialize)]
struct FloatStreamRef<'a> {
    name: &'a str,
    tags: Vec<rustradio::stream::Tag>,
    samples: &'a [rustradio::Float],
}

/// Stream of data between worker and main UI.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ComplexStream {
    pub name: String,
    pub tags: Vec<rustradio::stream::Tag>,
    pub samples: Vec<rustradio::Complex>,
}

/// Borrow version of `ComplexStream`.
///
/// Used to avoid copies when e.g. sending directly from a RustRadio stream.
///
/// Must serialize the same as `ComplexStream`.
#[derive(Serialize)]
struct ComplexStreamRef<'a> {
    name: &'a str,
    tags: Vec<rustradio::stream::Tag>,
    samples: &'a [rustradio::Complex],
}

/// Stream of PDUs of floats for sending between worker and main UI.
///
/// This is used by the frequency and waterfall sinks.
///
/// There's currently no borrow version of `FloatPduStream`, since PDUs are
/// generally passed by value anyway. If a need comes up, it can be added.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, PartialOrd)]
pub struct FloatPduStream {
    pub name: String,
    pub sample_rate: rustradio::Float,
    pub samples: Vec<rustradio::Float>,
}

/// Messages going from main (UI) thread to worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum MainToWorker {
    /// Start the graph with the selected input byte format.
    Start { samp_rate: u64, rtlsdr: bool },

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

impl TryFrom<wasm_bindgen::JsValue> for MainToWorker {
    type Error = wasm_bindgen::JsValue;
    fn try_from(js: wasm_bindgen::JsValue) -> Result<MainToWorker, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(js)?)
    }
}

/// Named payload for worker data requests, sent across the worker/main boundary
/// so the UI thread can route a receiver read to either file or stream input.
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
struct ReqData {
    receiver: ReceiverId,
    pos: u64,
    size: u64,
}

/// Messages from the worker to the main (UI) thread.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum WorkerToMain {
    /// Worker notifying the main UI thread that the rustradio graph has
    /// successfully started.
    Ready,

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the main thread.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),

    /// Request more data.
    ReqData(ReqData),

    /// At the end of execution, provide the result as a string.
    Result(String),

    /// A worker log line to be emitted through the main thread logger.
    LogLine { level: log::Level, line: String },

    /// Float streams captured in the worker graph.
    FloatStreams(Vec<FloatStream>),

    /// Complex streams captured in the worker graph.
    ComplexStreams(Vec<ComplexStream>),

    /// Float PDU streams captured in the worker graph.
    FloatPduStreams(Vec<FloatPduStream>),
}

/// Borrowed version of WorkerToMain. Must serialize the same.
#[derive(Serialize)]
#[serde(tag = "type", content = "data")]
enum WorkerToMainRef<'a> {
    FloatStreams(Vec<FloatStreamRef<'a>>),
    ComplexStreams(Vec<ComplexStreamRef<'a>>),
}

impl TryInto<wasm_bindgen::JsValue> for WorkerToMain {
    type Error = wasm_bindgen::JsValue;
    fn try_into(self) -> Result<wasm_bindgen::JsValue, Self::Error> {
        Ok(serde_wasm_bindgen::to_value(&self)?)
    }
}

impl TryFrom<wasm_bindgen::JsValue> for WorkerToMain {
    type Error = wasm_bindgen::JsValue;
    fn try_from(js: wasm_bindgen::JsValue) -> Result<WorkerToMain, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(js)?)
    }
}

/// Entry point for both worker and main thread.
///
/// This function is run for both, and it does common initialization and then
/// calls out to the respective special setups.
#[wasm_bindgen(start)]
pub async fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    if web_sys::window().is_none() {
        // Init logging.
        workerlogger::init_logging().expect("Failed to init worker logging");
        // console_log::init_with_level(log::Level::Debug).expect("Failed to init logging");
        info!("Worker logging initialized");
        info!("Worker: Starting at time {}", js_performance_now());

        worker::setup().await
    } else {
        info!("Main: Starting at time {}", js_performance_now());
        domlogger::init_logging().expect("failed to init logging");
        info!("Main logging initialized");
        mainthread::setup().await
    }
}

/// Get a descriptive git string for the current code.
#[wasm_bindgen]
#[must_use]
pub fn git_version() -> String {
    rustradio::sys::initialize_rustradio();
    info!("git_version() called");

    // Wat? What's wrong with clippy?
    #[allow(clippy::manual_string_new)]
    env!("GIT_VERSION").to_string()
}

/// Get the version of the Rust compiler that built this.
#[wasm_bindgen]
#[must_use]
pub fn rustc_version() -> String {
    env!("RUSTC_VERSION").to_string()
}

#[wasm_bindgen]
#[must_use]
pub fn add(a: i32, b: i32) -> String {
    info!("Hello world, adding {a} and {b}");
    format!("Add results: {}", a + b)
}

/*
use web_sys::js_sys::Uint8Array;
pub(crate) fn uint8array_to_vec(arr: &Uint8Array) -> Vec<u8> {
    let mut buf = vec![0; arr.length() as usize];
    arr.copy_to(&mut buf);
    buf
}
*/

#[cfg(test)]
mod tests {
    // TODO: add some tests.
}
