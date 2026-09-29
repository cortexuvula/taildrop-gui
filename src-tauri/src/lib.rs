mod debug_log;
mod receipts;
mod tailscale;

use std::sync::RwLock;

use tauri::{Emitter, Manager};

/// Shared receive settings: written by the `set_receive_settings` IPC
/// command, read by the background receive task on every iteration.
struct ReceiveSettings {
    save_dir: String,
    /// Plumbed through for completeness; auto-accept itself stays
    /// frontend-driven (see useIncomingFiles), so the receive loop does not
    /// read this field yet.
    #[allow(dead_code)]
    auto_accept: bool,
}

type SharedReceiveSettings = RwLock<ReceiveSettings>;

/// TD-03: the receive loop must not run a single destructive iteration until
/// the frontend has hydrated persisted settings and pushed them via
/// `set_receive_settings`. The initial `ReceiveSettings` holds the *default*
/// download directory; on CLI-fallback platforms `fetch_incoming_files` IS
/// the download, so an ungated first poll can drain pending inbox files into
/// Downloads before the user's persisted custom destination arrives over
/// IPC. Set once, never cleared.
static RECEIVE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Idle interval for the background receive loop when nothing changed.
const RECEIVE_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(8);
/// Interval used for a few iterations after the incoming list changed, so an
/// active transfer burst is picked up quickly (mirrors the old frontend poll).
const RECEIVE_ACTIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How many iterations stay fast after a change.
const RECEIVE_FAST_ITERATIONS: u32 = 5;
/// Consecutive fetch failures before `incoming-files-error` is emitted — a
/// dead daemon must not masquerade as "no incoming files".
const RECEIVE_FAILURE_THRESHOLD: u32 = 3;

#[tauri::command]
async fn get_tailscale_status() -> Result<Vec<tailscale::Peer>, String> {
    log::debug!("get_tailscale_status: invoking fetch_status...");
    match tailscale::fetch_status().await {
        Ok(peers) => {
            log::info!(
                "get_tailscale_status: OK — {} peers (self={}, online={})",
                peers.len(),
                peers.iter().filter(|p| p.is_self).count(),
                peers.iter().filter(|p| p.online && !p.is_self).count()
            );
            Ok(peers)
        }
        Err(e) => {
            log::error!("get_tailscale_status: ERROR — {}", e);
            Err(e)
        }
    }
}

#[tauri::command]
async fn send_file(
    app: tauri::AppHandle,
    peer_id: String,
    peer_name: String,
    file_path: String,
    transfer_id: String,
) -> Result<String, String> {
    // SIMULATED PROGRESS — documented contract: neither the Tailscale
    // localapi nor the CLI reports byte-level progress for Taildrop sends,
    // so these milestone events (10/30/60/90 on a timer, then 100 on
    // success) are cosmetic feedback, NOT real transferred bytes. Do not
    // build logic on them (e.g. ETA or speed estimates). Uses a cancellation
    // token so the task checks before each emit — preventing stale
    // milestones from arriving after the real result.
    let cancel = tokio_util::sync::CancellationToken::new();
    let progress_handle = {
        let app = app.clone();
        let tid = transfer_id.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            for pct in [10u8, 30, 60, 90] {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(
                        (pct as u64) * 20,
                    )) => {
                        let _ = app.emit(
                            "transfer-progress",
                            serde_json::json!({ "transferId": tid, "progress": pct }),
                        );
                    }
                    _ = cancel.cancelled() => break,
                }
            }
        })
    };

    let result = tailscale::send_file_to_peer(&peer_id, &peer_name, &file_path).await;
    cancel.cancel();
    // Wait for the task to observe cancellation so no stale events arrive.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), progress_handle).await;
    // Emit 100% on success so the UI shows completion before status flips
    if result.is_ok() {
        let _ = app.emit(
            "transfer-progress",
            serde_json::json!({ "transferId": transfer_id, "progress": 100 }),
        );
    }
    result
}

#[tauri::command]
async fn get_incoming_files(save_dir: String) -> Result<Vec<tailscale::IncomingFile>, String> {
    let dir = effective_save_dir(&save_dir);
    tailscale::fetch_incoming_files(&dir.to_string_lossy()).await
}

#[tauri::command]
async fn accept_file(name: String, save_dir: String) -> Result<String, String> {
    let dir = effective_save_dir(&save_dir);
    tailscale::accept_incoming_file(&name, &dir.to_string_lossy()).await
}

#[tauri::command]
fn get_default_download_dir() -> String {
    default_download_dir().to_string_lossy().to_string()
}

/// Default save directory: the OS download dir, falling back to the home
/// directory, then the working directory.
fn default_download_dir() -> std::path::PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
}

/// Resolve the effective save directory: the given directory, or the default
/// download dir when empty (same fallback the accept/poll commands use).
fn effective_save_dir(save_dir: &str) -> std::path::PathBuf {
    let save_dir = save_dir.trim();
    if save_dir.is_empty() {
        default_download_dir()
    } else {
        std::path::PathBuf::from(save_dir)
    }
}

/// Validate the save directory before it is used: must be absolute, must
/// already exist (accept creates missing dirs on demand — validation is the
/// UI's early warning, not a mutation), and must be writable. Returns the
/// canonical path on success so callers can normalize what they display.
#[tauri::command]
async fn validate_save_dir(save_dir: String) -> Result<String, String> {
    validate_save_dir_path(&save_dir).await
}

/// Body of [`validate_save_dir`], shared with `set_receive_settings`.
async fn validate_save_dir_path(save_dir: &str) -> Result<String, String> {
    let path = effective_save_dir(save_dir);
    if !path.is_absolute() {
        return Err(format!(
            "'{}' is not an absolute path — pick a folder via Browse",
            path.display()
        ));
    }
    if !tokio::fs::metadata(&path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(format!("Directory '{}' does not exist", path.display()));
    }
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .map_err(|e| format!("Cannot resolve '{}': {}", path.display(), e))?;
    // Writability probe: create and remove a uniquely-named temp file.
    let probe = canonical.join(format!(".taildrop-write-probe-{}", timestamp_probe_tag()));
    match tokio::fs::File::create(&probe).await {
        Ok(_) => {
            let _ = tokio::fs::remove_file(&probe).await;
        }
        Err(e) => {
            return Err(format!(
                "Directory '{}' is not writable: {}",
                canonical.display(),
                e
            ));
        }
    }
    Ok(canonical.to_string_lossy().to_string())
}

/// Short unique tag for the writability probe filename (ms clock + counter).
fn timestamp_probe_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (ms << 20) | (n & 0xFFFFF)
}

/// Update the shared receive settings the background receive task reads.
/// The values are stored even when validation fails so behavior matches the
/// frontend-driven poll this replaces: an unusable dir surfaces as a
/// persistent `incoming-files-error` event, not as a silent revert.
#[tauri::command]
async fn set_receive_settings(
    state: tauri::State<'_, SharedReceiveSettings>,
    save_dir: String,
    auto_accept: bool,
) -> Result<(), String> {
    {
        let mut settings = state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        settings.save_dir = save_dir.clone();
        settings.auto_accept = auto_accept;
    }
    // TD-03: settings have arrived from the (hydrated) frontend — the receive
    // loop may start draining into the authoritative destination now.
    RECEIVE_READY.store(true, std::sync::atomic::Ordering::Release);
    validate_save_dir_path(&save_dir).await.map(|_| ())
}

/// TD-03: readiness probe for the frontend/tests — true once the receive
/// loop is permitted to poll. Purely informational; the loop enforces the
/// gate itself.
#[tauri::command]
fn receive_ready() -> bool {
    RECEIVE_READY.load(std::sync::atomic::Ordering::Acquire)
}

// ============================================================
// TD-05: transfer receipts
// ============================================================

/// Page through the receipt store (contract: get_recent_receipts).
#[tauri::command]
fn get_recent_receipts(since_seq: u64, limit: usize) -> receipts::ReceiptPage {
    receipts::page_public(since_seq, limit)
}

/// Recover preserved staging files into the current save dir (contract:
/// recover_staging_files). Moves reuse move_file_into_dir guarantees —
/// exclusive-create, never overwrites, collision-resolved names — and the
/// staging dir is removed only once verifiably empty. Each landed file
/// records a normal "saved" receipt with its actual landing path.
#[tauri::command]
async fn recover_staging_files(path: String) -> Result<Vec<String>, String> {
    let staging = receipts::validate_staging_path(&path)?;
    let save_dir = {
        let app = APP_HANDLE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .expect("recover_staging_files called before app setup");
        let state = app.state::<SharedReceiveSettings>();
        let settings = state.read().unwrap_or_else(|p| p.into_inner());
        effective_save_dir(&settings.save_dir)
    };
    let mut landed = Vec::new();
    let entries: Vec<_> = std::fs::read_dir(&staging)
        .map_err(|e| format!("Cannot read staging dir '{}': {}", staging.display(), e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    for file_path in entries {
        let name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "Invalid filename in staging".to_string())?
            .to_string();
        let dest = tailscale::move_file_for_recovery(&file_path, &save_dir, &name)?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        receipts::ReceiptStore::record_saved(&name, &dest.to_string_lossy(), size, None);
        landed.push(dest.to_string_lossy().to_string());
    }
    // Remove the staging dir only when provably empty (TD-01 guarantee).
    if tailscale::staging_is_empty(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
    }
    Ok(landed)
}

/// Explicitly discard a preserved staging directory (contract:
/// discard_staging_dir — user action only, never automatic).
#[tauri::command]
async fn discard_staging_dir(path: String) -> Result<(), String> {
    let staging = receipts::validate_staging_path(&path)?;
    std::fs::remove_dir_all(&staging).map_err(|e| {
        format!(
            "Failed to discard staging dir '{}': {}",
            staging.display(),
            e
        )
    })
}

/// Re-scan for preserved staging dirs on demand (contract:
/// staging_recovery_found event; the same scan runs at startup).
#[tauri::command]
fn staging_recovery_scan() -> Vec<receipts::StagingDir> {
    receipts::scan_staging_dirs()
}

/// Forward receipt-store broadcasts to the webview as `transfer-receipt`
/// events (contract). Laggy/absent receivers are fine — the frontend
/// re-syncs via get_recent_receipts.
async fn receipt_emitter(app: tauri::AppHandle) {
    let mut rx = receipts::ReceiptStore::subscribe_global();
    loop {
        match rx.recv().await {
            Ok(receipt) => {
                if let Err(e) = app.emit("transfer-receipt", &receipt) {
                    log::debug!("receipt emitter: emit failed: {}", e);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("receipt emitter lagged, skipped {}", skipped);
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Global app handle for commands that need state outside their Tauri
/// injection (set once at setup; the receipt recovery path reads receive
/// settings).
static APP_HANDLE: std::sync::Mutex<Option<tauri::AppHandle>> = std::sync::Mutex::new(None);

/// Background receive loop. On macOS/Windows without a Tailscale socket,
/// `fetch_incoming_files` IS the download (it runs `tailscale file get` into
/// the save dir), so running it from Rust keeps incoming files flowing while
/// the webview — and with it the old JS poll — is suspended in a minimized
/// window. On Linux the call is list-only and downloading stays in
/// `accept_file`.
///
/// Emits `incoming-files-changed` (payload: the current incoming-file list)
/// when the list changed versus the previous iteration, and on recovery from
/// an error state (even when empty — the frontend uses that to clear the
/// error). Emits `incoming-files-error` (payload: message) once at least
/// [`RECEIVE_FAILURE_THRESHOLD`] consecutive iterations failed. Both emits
/// are best-effort: the task is cancelled at app shutdown and must not
/// panic if the webview is already gone.
async fn receive_loop(app: tauri::AppHandle) {
    let mut prev_files: Vec<tailscale::IncomingFile> = Vec::new();
    let mut consecutive_failures: u32 = 0;
    let mut error_emitted = false;
    let mut fast_iterations: u32 = 0;

    loop {
        // TD-03: block until the frontend has pushed hydrated settings.
        // Sleep in short intervals rather than awaiting a notify so a crash
        // before the first set_receive_settings can't spin the CPU.
        while !RECEIVE_READY.load(std::sync::atomic::Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let save_dir = {
            let settings = app.state::<SharedReceiveSettings>();
            let settings = settings
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            settings.save_dir.clone()
        };
        let dir = effective_save_dir(&save_dir);

        match tailscale::fetch_incoming_files(&dir.to_string_lossy()).await {
            Ok(files) => {
                let changed = incoming_lists_differ(&prev_files, &files);
                if changed || error_emitted {
                    if let Err(e) = app.emit("incoming-files-changed", &files) {
                        log::debug!("receive loop: emit incoming-files-changed failed: {}", e);
                    }
                }
                if changed {
                    log::debug!(
                        "receive loop: incoming list changed ({} file(s)), event emitted",
                        files.len()
                    );
                    fast_iterations = RECEIVE_FAST_ITERATIONS;
                }
                prev_files = files;
                consecutive_failures = 0;
                error_emitted = false;
            }
            Err(msg) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                log::debug!(
                    "receive loop: fetch failed ({}): {}",
                    consecutive_failures,
                    msg
                );
                if consecutive_failures >= RECEIVE_FAILURE_THRESHOLD {
                    error_emitted = true;
                    if let Err(e) = app.emit("incoming-files-error", msg) {
                        log::debug!("receive loop: emit incoming-files-error failed: {}", e);
                    }
                }
            }
        }

        let interval = if fast_iterations > 0 {
            fast_iterations -= 1;
            RECEIVE_ACTIVE_INTERVAL
        } else {
            RECEIVE_IDLE_INTERVAL
        };
        tokio::time::sleep(interval).await;
    }
}

/// Compare incoming-file lists by (name, size) — the same identity the
/// frontend's notification dedup uses. Order-insensitive so a daemon-side
/// reshuffle doesn't spam `incoming-files-changed` events.
fn incoming_lists_differ(a: &[tailscale::IncomingFile], b: &[tailscale::IncomingFile]) -> bool {
    fn sorted_keys(files: &[tailscale::IncomingFile]) -> Vec<(&str, u64)> {
        let mut keys: Vec<(&str, u64)> = files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        keys.sort_unstable();
        keys
    }
    sorted_keys(a) != sorted_keys(b)
}

#[tauri::command]
fn get_debug_logs() -> Vec<debug_log::LogEntry> {
    debug_log::snapshot()
}

#[tauri::command]
fn get_env_info() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    debug_log::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(RwLock::new(ReceiveSettings {
            save_dir: default_download_dir().to_string_lossy().to_string(),
            auto_accept: false,
        }))
        .setup(|app| {
            // The receive loop must live in Rust: WKWebView suspends DOM
            // timers in a minimized window, which used to stop the JS poll
            // that drives the CLI download on socket-less macOS/Windows.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(receive_loop(handle));
            // TD-05: forward receipt-store broadcasts to the webview.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(receipt_emitter(handle));
            // TD-05: global handle for commands reading shared state outside
            // Tauri injection (staging recovery needs the save dir).
            *APP_HANDLE
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(app.handle().clone());
            // TD-05: discover preserved staging dirs from a previous crashed
            // run and notify (discover-and-notify — never auto-delete).
            let dirs = receipts::scan_staging_dirs();
            if !dirs.is_empty() {
                log::warn!(
                    "startup: {} preserved staging dir(s) found — emitting staging-recovery-found",
                    dirs.len()
                );
                let _ = app.emit("staging-recovery-found", &dirs);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
            validate_save_dir,
            set_receive_settings,
            receive_ready,
            get_recent_receipts,
            recover_staging_files,
            discard_staging_dir,
            staging_recovery_scan,
            get_debug_logs,
            get_env_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> tailscale::IncomingFile {
        tailscale::IncomingFile {
            name: name.to_string(),
            size,
            peer_name: None,
        }
    }

    #[test]
    fn lists_differ_detects_new_and_removed_files() {
        let a = [file("a.txt", 10)];
        assert!(incoming_lists_differ(
            &a,
            &[file("a.txt", 10), file("b.txt", 5)]
        ));
        assert!(incoming_lists_differ(
            &[file("a.txt", 10), file("b.txt", 5)],
            &a
        ));
    }

    #[test]
    fn lists_differ_detects_size_change() {
        assert!(incoming_lists_differ(
            &[file("a.txt", 10)],
            &[file("a.txt", 11)]
        ));
    }

    #[test]
    fn lists_differ_ignores_order_and_peer_name() {
        let a = [file("a.txt", 10), file("b.txt", 5)];
        let mut b = [file("b.txt", 5), file("a.txt", 10)];
        b[1].peer_name = Some("peer".to_string());
        assert!(!incoming_lists_differ(&a, &b));
    }

    #[test]
    fn lists_differ_counts_duplicates() {
        assert!(incoming_lists_differ(
            &[file("a.txt", 10)],
            &[file("a.txt", 10), file("a.txt", 10)]
        ));
    }
}
