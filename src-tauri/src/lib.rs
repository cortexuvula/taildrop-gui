mod tailscale;

#[tauri::command]
async fn get_tailscale_status() -> Result<Vec<tailscale::Peer>, String> {
    tailscale::fetch_status().await
}

#[tauri::command]
async fn send_file(peer_id: String, filename: String, data: Vec<u8>) -> Result<String, String> {
    tailscale::send_file_to_peer(&peer_id, &filename, data).await
}

#[tauri::command]
async fn get_incoming_files() -> Result<Vec<tailscale::IncomingFile>, String> {
    tailscale::fetch_incoming_files().await
}

#[tauri::command]
async fn accept_file(name: String, save_dir: String) -> Result<String, String> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
