use log::{Level, LevelFilter, Log, Metadata, Record};
use wasm_bindgen::JsCast;
use web_sys::{window, HtmlElement};

struct DomConsoleLogger {
    level: LevelFilter,
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

        let line = format!(
            "[{}] {}",
            record.level(),
            record.args()
        );

        // Also log to browser console.
        match record.level() {
            Level::Error => web_sys::console::error_1(&line.clone().into()),
            Level::Warn  => web_sys::console::warn_1(&line.clone().into()),
            Level::Info  => web_sys::console::info_1(&line.clone().into()),
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

        let old = el.inner_text();
        el.set_inner_text(&format!("{old}{line}\n"));
    }

    fn flush(&self) {}
}

pub fn init_logging() -> Result<(), log::SetLoggerError> {
    static LOGGER: DomConsoleLogger = DomConsoleLogger {
        level: LevelFilter::Debug,
        element_id: "log-output",
    };

    log::set_logger(&LOGGER)?;
    log::set_max_level(LevelFilter::Debug);
    Ok(())
}
