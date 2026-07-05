#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
// TODO: fix some of the above
//
use log::info;
use rustradio::Float;
use rustradio_ui::ApplicationSpecific;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

// This needs to be re-exported to JS, per
// <https://github.com/RReverser/wasm-bindgen-rayon>.
pub use wasm_bindgen_rayon::init_thread_pool;

mod constellation_sink;
mod mainthread;
mod spectrum_sink;
mod time_sink;
mod wasm_graph;
mod worker;

type MainToWorker = rustradio_ui::MainToWorker<Ax25MainToWorker>;
type WorkerToMain = rustradio_ui::WorkerToMain<Ax25WorkerToMain>;

pub(crate) const RECEIVER_SOURCE_ID: &str = "rtl-sdr";

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn js_performance_now() -> f64;
}

/// Application specific messages.
///
/// None, in this case.
#[derive(Debug, Serialize, Deserialize)]
enum Ax25Messages {
    Decoded(String),
}

/// Application specific startup parameters.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25Start {
    samp_rate: u64,
    offset: Float,
    rtlsdr: bool,
}

/// Application specific end result.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25End {
    s: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Ax25WorkerToMain {}

impl ApplicationSpecific for Ax25WorkerToMain {
    type App = Ax25Messages;
    type Start = Ax25Start;
    type Ready = Ax25Ready;
    type End = Ax25End;
}

#[derive(Debug, Serialize, Deserialize)]
struct Ax25MainToWorker {}

impl ApplicationSpecific for Ax25MainToWorker {
    type App = rustradio_ui::AppEmpty;
    type Start = Ax25Start;
    type Ready = Ax25Ready;
    type End = Ax25End;
}

/// Application specific ready data.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25Ready {}

/// Entry point for both worker and main thread.
///
/// This function is run for both, and it does common initialization and then
/// calls out to the respective special setups.
#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    if web_sys::window().is_none() {
        use rayon::prelude::*;
        // With shared memory, the worker and UI have the same logger.
        info!("Worker: Starting at time {}", js_performance_now());
        info!(
            "rayon test: {}",
            (0..10u64).into_par_iter().map(|v| v).sum::<u64>()
        );
        worker::setup().await
    } else {
        rustradio_ui::dom_logger::init_logging::<Ax25WorkerToMain>(
            crate::mainthread::ID_LOG_OUTPUT,
        )
        .expect("failed to init logging");
        info!("Main: Starting at time {}", js_performance_now());
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

/// Get the UTC timestamp of the current git commit.
#[wasm_bindgen]
#[must_use]
pub fn git_author_timestamp() -> String {
    // Wat? What's wrong with clippy?
    #[allow(clippy::manual_string_new)]
    env!("GIT_AUTHOR_TIMESTAMP").to_string()
}

/// Get the UTC timestamp of the current git commit after rebase etc taken into
/// account.
#[wasm_bindgen]
#[must_use]
pub fn git_commit_timestamp() -> String {
    // Wat? What's wrong with clippy?
    #[allow(clippy::manual_string_new)]
    env!("GIT_COMMIT_TIMESTAMP").to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_correct() {
        assert_eq!(add(3, 5), "Add results: 8");
    }
}
