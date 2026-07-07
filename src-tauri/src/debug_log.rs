use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use log::{Level, Log, Metadata, Record};
use serde::Serialize;

const MAX_ENTRIES: usize = 500;
const CAPTURE_LEVEL: Level = Level::Debug;

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
        // These still reach stderr (above), but don't flood the DebugPanel.
        // Our own crates (taildrop_gui*, tailscale) are always captured.
        let target = record.target();
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
        if !is_ours && record.level() > Level::Warn {
            for prefix in NOISY_PREFIXES {
                if target.starts_with(prefix) {
                    return;
                }
            }
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
    let _ = log::set_logger(&*SINK);
    log::set_max_level(CAPTURE_LEVEL.to_level_filter());
}

/// Return a snapshot of the current buffer (oldest-first).
pub fn snapshot() -> Vec<LogEntry> {
    SINK.buffer
        .lock()
        .map(|buf| buf.iter().cloned().collect())
        .unwrap_or_default()
}
