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
pub struct FloatStreamRef<'a> {
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
pub struct ComplexStreamRef<'a> {
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

pub trait ApplicationSpecific {
    // Can't default. https://github.com/rust-lang/rust/issues/29661
    type App: Serialize;
    type Start: Serialize;
    type Ready: Serialize;
    type End: Serialize;
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

/// No application specific messages required.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppEmpty {}

impl ApplicationSpecific for AppEmpty {
    type App = AppEmpty;
    type Start = AppEmpty;
    type Ready = AppEmpty;
    type End = AppEmpty;
}

/// Application specific ready data.
#[derive(Debug, Serialize, Deserialize)]
struct Ax25Ready {}

/// Messages going from main (UI) thread to worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(bound(
    serialize = "App::App: Serialize, App::Start: Serialize",
    deserialize = "App::App: Deserialize<'de>, App::Start: Deserialize<'de>",
))]
enum MainToWorker<App: ApplicationSpecific> {
    /// Start the graph with the selected input byte format.
    Start(App::Start),

    /// Application specific stuff.
    ApplicationSpecific(App::App),

    /// Raw DATA_STREAM protocol bytes received from the selected input source.
    DataStream(Vec<u8>),

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the worker.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),
}

impl<App: ApplicationSpecific> TryInto<wasm_bindgen::JsValue> for MainToWorker<App> {
    type Error = wasm_bindgen::JsValue;
    fn try_into(self) -> Result<wasm_bindgen::JsValue, Self::Error> {
        Ok(serde_wasm_bindgen::to_value(&self)?)
    }
}

impl<App> TryFrom<wasm_bindgen::JsValue> for MainToWorker<App>
where
    App: ApplicationSpecific,
    App::App: serde::de::DeserializeOwned,
    App::Start: serde::de::DeserializeOwned,
{
    type Error = wasm_bindgen::JsValue;
    fn try_from(js: wasm_bindgen::JsValue) -> Result<MainToWorker<App>, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(js)?)
    }
}

/// Messages from the worker to the main (UI) thread.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(bound(
    serialize = "App::App: Serialize, App::Ready: Serialize, App::End: Serialize",
    deserialize = "App::App: Deserialize<'de>, App::Ready: Deserialize<'de>, App::End: Deserialize<'de>",
))]
enum WorkerToMain<App: ApplicationSpecific = AppEmpty> {
    /// Worker notifying the main UI thread that the rustradio graph has
    /// successfully started.
    Ready(App::Ready),

    /// Application specific messages.
    ApplicationSpecific(App::App),

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the main thread.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),

    /// Raw DATA_STREAM protocol bytes to send to the selected input source.
    DataStream(Vec<u8>),

    /// At the end of execution, provide the result as a string.
    End(App::End),

    /// A worker log line to be emitted through the main thread logger.
    LogLine { level: log::Level, line: String },

    /// Float streams captured in the worker graph.
    ///
    /// TODO: This should be one receiver, multiple streams.
    FloatStreams(Vec<FloatStream>),

    /// Complex streams captured in the worker graph.
    /// TODO: This should be one receiver, multiple streams.
    ComplexStreams(Vec<ComplexStream>),

    /// Float PDU streams captured in the worker graph.
    ///
    /// TODO: this should only be the one packet per packet, right?
    FloatPduStreams(Vec<FloatPduStream>),
}

/// Borrowed version of WorkerToMain. Must serialize the same.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(bound(
    serialize = "App::App: Serialize, App::Ready: Serialize",
    deserialize = "App::App: Deserialize<'de>, App::Ready: Deserialize<'de>",
))]
pub enum WorkerToMainRef<'a, App: ApplicationSpecific = AppEmpty> {
    /// Worker notifying the main UI thread that the rustradio graph has
    /// successfully started.
    Ready(App::Ready),

    /// Application specific messages.
    ApplicationSpecific(App::App),

    /// Send a ping with a `performance.now()` timestamp.
    /// The timestamp will be reflected in the Pong.
    Ping(f64),

    /// Reply to a ping from the main thread.
    ///
    /// Original ping timestamp is returned.
    Pong(f64),

    /// Raw DATA_STREAM protocol bytes to send to the selected input source.
    DataStream(&'a [u8]),

    /// At the end of execution, provide the result as a string.
    Result(&'a str),

    /// A worker log line to be emitted through the main thread logger.
    LogLine { level: log::Level, line: &'a str },

    #[serde(skip_deserializing)]
    FloatStreams(Vec<FloatStreamRef<'a>>),

    #[serde(skip_deserializing)]
    ComplexStreams(Vec<ComplexStreamRef<'a>>),
}

impl<App: ApplicationSpecific> TryInto<wasm_bindgen::JsValue> for WorkerToMain<App> {
    type Error = wasm_bindgen::JsValue;
    fn try_into(self) -> Result<wasm_bindgen::JsValue, Self::Error> {
        Ok(serde_wasm_bindgen::to_value(&self)?)
    }
}

impl<App> TryFrom<wasm_bindgen::JsValue> for WorkerToMain<App>
where
    App: ApplicationSpecific,
    App::App: serde::de::DeserializeOwned,
    App::Ready: serde::de::DeserializeOwned,
    App::End: serde::de::DeserializeOwned,
{
    type Error = wasm_bindgen::JsValue;
    fn try_from(js: wasm_bindgen::JsValue) -> Result<WorkerToMain<App>, Self::Error> {
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
