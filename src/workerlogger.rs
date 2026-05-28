//! Log provider that logs to the main UI thread.
//!
//! It does not log directly to the console, even though workers can, because
//! the main thread wants to prefix the lines.
use log::{Level, LevelFilter, Log, Metadata, Record};
use wasm_bindgen::JsCast;
use web_sys::DedicatedWorkerGlobalScope;

use crate::WorkerToMain;

struct WorkerLogger<A, R> {
    level: LevelFilter,
    _dummy_a: std::marker::PhantomData<A>,
    _dummy_b: std::marker::PhantomData<R>,
}

impl<A: serde::Serialize + Send + Sync, R: serde::Serialize + Send + Sync> Log
    for WorkerLogger<A, R>
{
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = record.args().to_string();
        let console_line = format!("[{}] {}", record.level(), line);

        match record.level() {
            Level::Error => web_sys::console::error_1(&console_line.clone().into()),
            Level::Warn => web_sys::console::warn_1(&console_line.clone().into()),
            Level::Info => web_sys::console::info_1(&console_line.clone().into()),
            Level::Debug => web_sys::console::log_1(&console_line.clone().into()),
            Level::Trace => web_sys::console::debug_1(&console_line.clone().into()),
        }

        let Ok(scope) = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>() else {
            return;
        };

        let Ok(msg) = (WorkerToMain::<A, R>::LogLine {
            level: record.level(),
            line,
        })
        .try_into() else {
            return;
        };

        let _ = scope.post_message(&msg);
    }

    fn flush(&self) {}
}

/// Initialize worker logger.
pub fn init_logging() -> Result<(), log::SetLoggerError> {
    static LOGGER: WorkerLogger<crate::ApplicationSpecific, crate::ReadyData> = WorkerLogger {
        // TODO: make configurable.
        level: LevelFilter::Info,
        _dummy_a: std::marker::PhantomData,
        _dummy_b: std::marker::PhantomData,
    };

    log::set_logger(&LOGGER)?;
    // TODO: make configurable, and consistent.
    log::set_max_level(LevelFilter::Info);
    Ok(())
}
