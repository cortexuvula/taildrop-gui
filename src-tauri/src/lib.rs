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
    // Emit simulated progress events while the transfer is in flight.
    // The actual tailscale CLI / localapi doesn't report byte-level progress,
    // so we emit milestone percentages to give the user visual feedback.
    // Uses a cancellation token so the task checks before each emit —
    // preventing stale milestones from arriving after the real result.
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
    let save_dir = save_dir.trim().to_string();
    let dir = if save_dir.is_empty() {
        dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
            .to_string_lossy()
            .to_string()
    } else {
        save_dir
    };
    tailscale::fetch_incoming_files(&dir).await
}

#[tauri::command]
async fn accept_file(name: String, save_dir: String) -> Result<String, String> {
    let save_dir = save_dir.trim().to_string();
    let dir = if save_dir.is_empty() {
        dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
            .to_string_lossy()
            .to_string()
    } else {
        save_dir
    };
    tailscale::accept_incoming_file(&name, &dir).await
}

#[tauri::command]
fn get_default_download_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
        .to_string_lossy()
        .to_string()
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
            get_debug_logs,
            get_env_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
