use log::{Level, LevelFilter, Log, Metadata, Record};
use wasm_bindgen::JsCast;
use web_sys::DedicatedWorkerGlobalScope;

use crate::WorkerToMain;

struct WorkerLogger {
    level: LevelFilter,
}

impl Log for WorkerLogger {
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

        let Ok(msg) = (WorkerToMain::LogLine {
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

pub fn init_logging() -> Result<(), log::SetLoggerError> {
    static LOGGER: WorkerLogger = WorkerLogger {
        level: LevelFilter::Debug,
    };

    log::set_logger(&LOGGER)?;
    log::set_max_level(LevelFilter::Info);
    Ok(())
}
