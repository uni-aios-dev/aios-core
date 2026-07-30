use crate::crash_reporter::{CrashKind, CrashReporter};
use std::panic;
use std::sync::{Arc, Mutex};

pub struct PanicHandler {
    reporter: Arc<Mutex<CrashReporter>>,
    flight_recorder_getter: Arc<Mutex<Option<Box<dyn Fn() -> String + Send>>>>,
}

impl PanicHandler {
    pub fn new(app_name: &str, app_version: &str) -> Self {
        Self {
            reporter: Arc::new(Mutex::new(CrashReporter::new(app_name, app_version))),
            flight_recorder_getter: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_flight_recorder_getter<F>(&self, getter: F)
    where
        F: Fn() -> String + Send + 'static,
    {
        *self.flight_recorder_getter.lock().unwrap() = Some(Box::new(getter));
    }

    pub fn install(&self) {
        let reporter = self.reporter.clone();
        let fr_getter = self.flight_recorder_getter.clone();

        panic::set_hook(Box::new(move |info| {
            let panic_message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| {
                    info.payload()
                        .downcast_ref::<String>()
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "Unknown panic".to_string());

            let location = info
                .location()
                .map(|loc| format!("{}:{}", loc.file(), loc.line()))
                .unwrap_or_else(|| "unknown location".to_string());

            let message = format!("{panic_message} at {location}");

            let fr_dump = fr_getter
                .lock()
                .unwrap()
                .as_ref()
                .map(|f| f())
                .unwrap_or_default();

            reporter.lock().unwrap().generate_report(
                CrashKind::Panic,
                &std::thread::current()
                    .name()
                    .unwrap_or("unknown")
                    .to_string(),
                &message,
                "panic stack not captured",
                &fr_dump,
                false,
            );

            eprintln!("\nAIOS Panic: {message}");
            eprintln!("   Report saved. Check crash_reporter logs.\n");
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panic_handler_creation() {
        let handler = PanicHandler::new("aios-test", "0.1.0");
        assert_eq!(handler.reporter.lock().unwrap().report_count(), 0);
    }
}
