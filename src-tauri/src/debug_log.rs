use log::{Level, Log, Metadata, Record};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 500;

/// Debug records filenames and peer hostnames; retaining them in the
/// in-memory ring buffer of a release build (exposed un-gated through the
/// `get_debug_logs` IPC) is a privacy leak. Dev builds capture everything;
/// release builds stop at `Info`.
#[cfg(debug_assertions)]
const CAPTURE_LEVEL: Level = Level::Debug;
#[cfg(not(debug_assertions))]
const CAPTURE_LEVEL: Level = Level::Info;

#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub timestamp_ms: u128,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct InMemorySink {
    buffer: Mutex<VecDeque<LogEntry>>,
    /// Optional inner logger (env_logger) to forward to, preserving stderr
    /// output in `tauri dev`. None in release builds (env_logger is quiet).
    inner: Mutex<Option<Box<dyn Log>>>,
}

static SINK: LazyLock<InMemorySink> = LazyLock::new(|| InMemorySink {
    buffer: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)),
    inner: Mutex::new(None),
});

impl Log for InMemorySink {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= CAPTURE_LEVEL
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Forward to the inner logger (env_logger) so dev stderr still works.
        if let Ok(inner) = self.inner.lock() {
            if let Some(logger) = inner.as_ref() {
                logger.log(record);
            }
        }
        // Suppress chatty third-party debug/info from the in-app buffer.
        if !should_capture(record.target(), record.level()) {
            return;
        }
        // Append to the in-memory buffer.
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let entry = LogEntry {
            timestamp_ms,
            level: record.level().to_string(),
            target: record.target().to_string(),
            message: format!("{}", record.args()),
        };
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= MAX_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(entry);
        }
    }

    fn flush(&self) {
        if let Ok(inner) = self.inner.lock() {
            if let Some(logger) = inner.as_ref() {
                logger.flush();
            }
        }
    }
}

/// Initialize the in-memory sink as the global logger. Builds env_logger from
/// the default environment (RUST_LOG) and wraps it so both sinks receive every
/// record. Replaces the bare `env_logger::init()` call.
pub fn init() {
    // Build env_logger but don't install it as the global logger; instead wrap
    // it in our sink so we can capture + forward.
    let env_logger = env_logger::Builder::from_default_env().build();
    if let Ok(mut inner) = SINK.inner.lock() {
        *inner = Some(Box::new(env_logger));
    }
    // Called once from run() at startup; single-threaded at that point.
    // Force LazyLock init, then hand set_logger a reference to the inner value.
    if log::set_logger(&*SINK).is_err() {
        // Another logger is already installed (e.g. a Tauri plugin called
        // env_logger::init() first). Log to stderr directly since our sink
        // isn't active.
        eprintln!("debug_log: failed to install in-memory sink — another logger is already set");
    }
    log::set_max_level(CAPTURE_LEVEL.to_level_filter());
}

/// Return a snapshot of the current buffer (oldest-first).
pub fn snapshot() -> Vec<LogEntry> {
    SINK.buffer
        .lock()
        .map(|buf| buf.iter().cloned().collect())
        .unwrap_or_default()
}

/// Test whether a log record should be captured in the in-memory buffer.
/// Extracted from `Log::log` for testability.
fn should_capture(target: &str, level: Level) -> bool {
    const NOISY_PREFIXES: &[&str] = &[
        "reqwest",
        "rustls",
        "hyper",
        "tauri_plugin_updater::updater",
        "tonic",
        "h2",
        "tower",
    ];
    let is_ours = target.starts_with("taildrop_gui") || target == "tailscale";
    if level > Level::Warn {
        // Only filter debug/info from noisy third-party crates.
        if !is_ours {
            for prefix in NOISY_PREFIXES {
                if target.starts_with(prefix) {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_our_crates_at_debug() {
        assert!(should_capture("taildrop_gui_lib", Level::Debug));
        assert!(should_capture("tailscale", Level::Debug));
    }

    #[test]
    fn filters_noisy_crates_at_debug() {
        assert!(!should_capture("reqwest::connect", Level::Debug));
        assert!(!should_capture("hyper::client", Level::Info));
        assert!(!should_capture(
            "tauri_plugin_updater::updater",
            Level::Debug
        ));
    }

    #[test]
    fn captures_noisy_crates_at_warn() {
        assert!(should_capture("reqwest::connect", Level::Warn));
        assert!(should_capture("hyper::client", Level::Error));
    }

    #[test]
    fn captures_unknown_crates_at_debug() {
        assert!(should_capture("some_random_crate", Level::Debug));
    }

    #[test]
    fn capture_level_is_debug_only_in_dev_builds() {
        // Privacy guard: a release build must not retain debug-level records
        // (filenames, peer hostnames) in the ring buffer.
        #[cfg(debug_assertions)]
        assert_eq!(CAPTURE_LEVEL, Level::Debug);
        #[cfg(not(debug_assertions))]
        assert_eq!(CAPTURE_LEVEL, Level::Info);
    }
}
