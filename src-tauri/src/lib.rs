mod debug_log;
mod tailscale;

use tauri::Emitter;

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
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
        .to_string_lossy()
        .to_string()
}

/// Resolve the effective save directory: the given directory, or the default
/// download dir when empty (same fallback the accept/poll commands use).
fn effective_save_dir(save_dir: &str) -> std::path::PathBuf {
    let save_dir = save_dir.trim();
    if save_dir.is_empty() {
        dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
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
    let path = effective_save_dir(&save_dir);
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
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
            validate_save_dir,
            get_debug_logs,
            get_env_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
