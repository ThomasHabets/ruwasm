use std::collections::VecDeque;

use log::{Level, LevelFilter, Log, Metadata, Record};
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, window};

const MAX_LOG_MESSAGES: usize = 1000;

struct DomConsoleLogger {
    level: LevelFilter,
    log_lines: std::sync::Mutex<VecDeque<String>>,
    element_id: &'static str,
}

impl Log for DomConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = format!("[{}] {}", record.level(), record.args());

        // Also log to browser console.
        match record.level() {
            Level::Error => web_sys::console::error_1(&line.clone().into()),
            Level::Warn => web_sys::console::warn_1(&line.clone().into()),
            Level::Info => web_sys::console::info_1(&line.clone().into()),
            Level::Debug => web_sys::console::log_1(&line.clone().into()),
            Level::Trace => web_sys::console::debug_1(&line.clone().into()),
        }

        // DOM sink.
        //
        // TODO: can we cache this JS object? Or what happens if it's GC'd?

        let Some(document) = window().and_then(|w| w.document()) else {
            return;
        };

        let Some(el) = document.get_element_by_id(self.element_id) else {
            return;
        };

        let Ok(el) = el.dyn_into::<HtmlElement>() else {
            return;
        };

        let mut lines = self.log_lines.lock().unwrap();
            lines.push_back(line);
            while lines.len() > MAX_LOG_MESSAGES {
                lines.pop_front();
            }

            let mut content = String::new();
            for line in lines.iter() {
                content.push_str(line);
                content.push('\n');
            }
            el.set_inner_text(&content);
            el.set_scroll_top(el.scroll_height());
    }

    fn flush(&self) {}
}

pub fn init_logging() -> Result<(), log::SetLoggerError> {
    static LOGGER: DomConsoleLogger = DomConsoleLogger {
        level: LevelFilter::Debug,
        element_id: "log-output",
        log_lines: std::sync::Mutex::new(VecDeque::new()),
    };

    log::set_logger(&LOGGER)?;
    log::set_max_level(LevelFilter::Debug);
    Ok(())
}
