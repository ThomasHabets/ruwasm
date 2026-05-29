#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
// TODO: fix some of the above
use log::info;
use rustradio::data_stream::DataStreamId;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use rustradio_ui::{ApplicationSpecific, MainToWorker, WorkerToMain, WorkerToMainRef};

mod complex_sink;
mod constellation_sink;
mod data_stream;
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

pub(crate) const RECEIVER_SOURCE_ID: &str = "rtl-sdr";

/// Return the DATA_STREAM id used by the single input source receiver.
pub(crate) fn receiver_source() -> DataStreamId {
    DataStreamId::new(RECEIVER_SOURCE_ID)
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn js_performance_now() -> f64;
}

/// Application specific messages.
///
/// None, in this case.
#[derive(Debug, Serialize, Deserialize)]
enum Ax25Messages {}

/// Application specific startup parameters.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25Start {
    samp_rate: u64,
    rtlsdr: bool,
}

/// Application specific end result.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25End {
    s: String,
}

/// Application specific end result.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25EndRef<'a> {
    s: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
struct Ax25Impl {}

impl ApplicationSpecific for Ax25Impl {
    type App = Ax25Messages;
    type Start = Ax25Start;
    type Ready = Ax25Ready;
    type End = Ax25End;
}

#[derive(Debug, Serialize, Deserialize)]
struct Ax25ImplRef<'a> {
    _dummy: std::marker::PhantomData<&'a u8>,
}

impl<'a> ApplicationSpecific for Ax25ImplRef<'a> {
    type App = Ax25Messages;
    type Start = Ax25Start;
    type Ready = Ax25Ready;
    type End = Ax25EndRef<'a>;
}

/// Application specific ready data.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25Ready {}

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
    use super::*;
    use rustradio_ui::AppEmpty;

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct TestAppMessage {
        name: String,
        payload: String,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct TestStart {
        sample_rate: u64,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct TestReady {
        channels: u8,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct TestAppMessageRef<'a> {
        name: &'a str,
        payload: &'a str,
    }

    #[derive(Debug)]
    struct TestApp;

    impl ApplicationSpecific for TestApp {
        type App = TestAppMessage;
        type Start = TestStart;
        type Ready = TestReady;
        type End = AppEmpty;
    }

    #[derive(Debug)]
    struct TestAppRef<'a>(std::marker::PhantomData<&'a ()>);

    impl<'a> ApplicationSpecific for TestAppRef<'a> {
        type App = TestAppMessageRef<'a>;
        type Start = TestStart;
        type Ready = TestReady;
        type End = AppEmpty;
    }

    fn expected_app_message() -> TestAppMessage {
        TestAppMessage {
            name: "test app message".to_string(),
            payload: "test payload".to_string(),
        }
    }

    fn assert_main_to_worker_app_message(msg: MainToWorker<TestApp>, expected: &TestAppMessage) {
        match msg {
            MainToWorker::ApplicationSpecific(app) => assert_eq!(app, *expected),
            other => panic!("expected MainToWorker::ApplicationSpecific, got {other:?}"),
        }
    }

    fn assert_worker_to_main_app_message(msg: WorkerToMain<TestApp>, expected: &TestAppMessage) {
        match msg {
            WorkerToMain::ApplicationSpecific(app) => assert_eq!(app, *expected),
            other => panic!("expected WorkerToMain::ApplicationSpecific, got {other:?}"),
        }
    }

    fn assert_main_to_worker_ref_app_message(
        msg: MainToWorker<TestAppRef<'_>>,
        expected: &TestAppMessage,
    ) {
        match msg {
            MainToWorker::ApplicationSpecific(app) => {
                assert_eq!(app.name, expected.name);
                assert_eq!(app.payload, expected.payload);
            }
            other => panic!("expected MainToWorker::ApplicationSpecific, got {other:?}"),
        }
    }

    fn assert_worker_to_main_ref_app_message(
        msg: WorkerToMainRef<'_, TestAppRef<'_>>,
        expected: &TestAppMessage,
    ) {
        match msg {
            WorkerToMainRef::ApplicationSpecific(app) => {
                assert_eq!(app.name, expected.name);
                assert_eq!(app.payload, expected.payload);
            }
            _ => panic!("expected WorkerToMainRef::ApplicationSpecific"),
        }
    }

    #[test]
    fn application_specific_main_to_worker_serializes_between_owned_and_ref_payloads() {
        let expected = expected_app_message();

        let owned_json = serde_json::to_value(MainToWorker::<TestApp>::ApplicationSpecific(
            expected.clone(),
        ))
        .unwrap();
        let ref_json = serde_json::to_value(MainToWorker::<TestAppRef<'_>>::ApplicationSpecific(
            TestAppMessageRef {
                name: "test app message",
                payload: "test payload",
            },
        ))
        .unwrap();

        assert_eq!(owned_json, ref_json);

        let decoded: MainToWorker<TestApp> = serde_json::from_value(ref_json).unwrap();
        assert_main_to_worker_app_message(decoded, &expected);
    }

    #[test]
    fn application_specific_worker_to_main_serializes_between_owned_and_ref_payloads() {
        let expected = expected_app_message();

        let owned_json = serde_json::to_value(WorkerToMain::<TestApp>::ApplicationSpecific(
            expected.clone(),
        ))
        .unwrap();
        let ref_json = serde_json::to_value(
            WorkerToMainRef::<TestAppRef<'_>>::ApplicationSpecific(TestAppMessageRef {
                name: "test app message",
                payload: "test payload",
            }),
        )
        .unwrap();

        assert_eq!(owned_json, ref_json);

        let decoded: WorkerToMain<TestApp> = serde_json::from_value(ref_json).unwrap();
        assert_worker_to_main_app_message(decoded, &expected);
    }

    #[test]
    fn application_specific_main_to_worker_deserializes_from_json_into_ref_payload() {
        let expected = expected_app_message();
        let json = serde_json::to_string(&MainToWorker::<TestApp>::ApplicationSpecific(
            expected.clone(),
        ))
        .unwrap();

        let decoded: MainToWorker<TestAppRef<'_>> = serde_json::from_str(&json).unwrap();

        assert_main_to_worker_ref_app_message(decoded, &expected);
    }

    #[test]
    fn application_specific_worker_to_main_deserializes_from_json_into_ref_payload() {
        let expected = expected_app_message();
        let json = serde_json::to_string(&WorkerToMain::<TestApp>::ApplicationSpecific(
            expected.clone(),
        ))
        .unwrap();

        let decoded: WorkerToMainRef<'_, TestAppRef<'_>> = serde_json::from_str(&json).unwrap();

        assert_worker_to_main_ref_app_message(decoded, &expected);
    }
}
