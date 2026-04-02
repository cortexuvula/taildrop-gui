use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Tailscale API Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailscaleStatus {
    #[serde(rename = "Self")]
    pub self_node: Option<PeerStatus>,
    #[serde(rename = "Peer")]
    pub peer: Option<HashMap<String, PeerStatus>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PeerStatus {
    #[serde(rename = "ID")]
    pub id: Option<String>,
    pub public_key: Option<String>,
    pub host_name: Option<String>,
    #[serde(rename = "DNSName")]
    pub dns_name: Option<String>,
    #[serde(rename = "OS")]
    pub os: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    pub tailscale_ips: Option<Vec<String>>,
    pub online: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub public_key: String,
    pub hostname: String,
    pub dns_name: String,
    pub os: String,
    pub ips: Vec<String>,
    pub online: bool,
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingFile {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Size")]
    pub size: i64,
    #[serde(rename = "PartialPath")]
    pub partial_path: Option<String>,
    #[serde(rename = "Done")]
    pub done: Option<bool>,
}

// ============================================================
// Unix implementation — hyperlocal (Unix socket)
// ============================================================

#[cfg(unix)]
mod platform {
    use super::*;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Buf;
    use hyper::Request;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    #[cfg(target_os = "macos")]
    const SOCKET_PATH: &str = "/var/run/tailscaled/tailscaled.sock";
    #[cfg(not(target_os = "macos"))]
    const SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

    fn make_client() -> Client<hyperlocal::UnixConnector, Full<Bytes>> {
        Client::builder(TokioExecutor::new()).build(hyperlocal::UnixConnector)
    }

    async fn read_body(resp: hyper::Response<hyper::body::Incoming>) -> Result<Vec<u8>, String> {
        let status = resp.status();
        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?
            .aggregate();

        let mut buf = Vec::new();
        let mut reader = body.reader();
        std::io::Read::read_to_end(&mut reader, &mut buf)
            .map_err(|e| format!("Failed to read body bytes: {}", e))?;

        if !status.is_success() {
            return Err(format!(
                "Tailscale API error ({}): {}",
                status,
                String::from_utf8_lossy(&buf)
            ));
        }
        Ok(buf)
    }

    async fn get_request(path: &str) -> Result<Vec<u8>, String> {
        let url: hyper::Uri = hyperlocal::Uri::new(SOCKET_PATH, path).into();
        let req = Request::builder()
            .uri(url)
            .header("Host", "local-tailscaled.sock")
            .body(Full::new(Bytes::new()))
            .map_err(|e| format!("Failed to build request: {}", e))?;
        let resp = make_client()
            .request(req)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;
        read_body(resp).await
    }

    async fn put_request(path: &str, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let url: hyper::Uri = hyperlocal::Uri::new(SOCKET_PATH, path).into();
        let req = Request::builder()
            .method(hyper::Method::PUT)
            .uri(url)
            .header("Host", "local-tailscaled.sock")
            .body(Full::new(Bytes::from(body_bytes)))
            .map_err(|e| format!("Failed to build request: {}", e))?;
        let resp = make_client()
            .request(req)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;
        read_body(resp).await
    }

    async fn delete_request(path: &str) -> Result<Vec<u8>, String> {
        let url: hyper::Uri = hyperlocal::Uri::new(SOCKET_PATH, path).into();
        let req = Request::builder()
            .method(hyper::Method::DELETE)
            .uri(url)
            .header("Host", "local-tailscaled.sock")
            .body(Full::new(Bytes::new()))
            .map_err(|e| format!("Failed to build request: {}", e))?;
        let resp = make_client()
            .request(req)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;
        read_body(resp).await
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        get_request("/localapi/v0/status").await
    }

    pub async fn send_file(peer_id: &str, filename: &str, data: Vec<u8>) -> Result<String, String> {
        let path = format!(
            "/localapi/v0/file-put/{}?name={}",
            super::url_encode(peer_id),
            super::url_encode(filename)
        );
        put_request(&path, data).await?;
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        get_request("/localapi/v0/files/").await
    }

    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let path = format!("/localapi/v0/files/{}", super::url_encode(name));
        let data = get_request(&path).await?;

        let save_path = std::path::Path::new(save_dir).join(name);
        std::fs::write(&save_path, &data).map_err(|e| format!("Failed to save file: {}", e))?;

        delete_request(&path).await?;
        Ok(save_path.to_string_lossy().to_string())
    }
}

// ============================================================
// Windows implementation — CLI-based
// ============================================================
//
// The Tailscale named pipe (\\.\pipe\ProtectedPrefix\Tailscale\tailscaled)
// lives in a protected namespace requiring SYSTEM/admin privileges.
// Regular user-mode desktop apps cannot connect to it.
// Instead, we use the `tailscale.exe` CLI which has its own IPC to the daemon.

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    /// CREATE_NO_WINDOW flag — prevents console window flash when spawning CLI
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    /// Find the tailscale CLI — check common install paths then PATH
    fn tailscale_cmd() -> Command {
        let candidates = [
            r"C:\Program Files\Tailscale\tailscale.exe",
            r"C:\Program Files (x86)\Tailscale\tailscale.exe",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                let mut cmd = Command::new(path);
                cmd.creation_flags(CREATE_NO_WINDOW);
                return cmd;
            }
        }
        // Fall back to PATH
        let mut cmd = Command::new("tailscale");
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        let output = tailscale_cmd()
            .args(["status", "--json"])
            .output()
            .map_err(|e| format!(
                "Could not run tailscale CLI. Make sure Tailscale is installed and in your PATH: {}",
                e
            ))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tailscale status failed: {}", stderr));
        }
        Ok(output.stdout)
    }

    pub async fn send_file(peer_id: &str, filename: &str, data: Vec<u8>) -> Result<String, String> {
        // Write data to a temp file, then use `tailscale file cp`
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(filename);
        std::fs::write(&temp_path, &data)
            .map_err(|e| format!("Failed to write temp file: {}", e))?;

        // `tailscale file cp <file> <target>:`
        // The target can be a hostname or IP — peer_id from our API is the node key,
        // so we need to resolve it to a hostname first via status
        let output = tailscale_cmd()
            .args(["file", "cp", &temp_path.to_string_lossy(), &format!("{}:", peer_id)])
            .output()
            .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;

        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tailscale file cp failed: {}", stderr));
        }
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        // `tailscale file get` downloads files; there's no list-only command.
        // Return empty array — the UI will show files after they're accepted.
        Ok(b"[]".to_vec())
    }

    pub async fn accept_file(_name: &str, save_dir: &str) -> Result<String, String> {
        // `tailscale file get <directory>` accepts all waiting files into the directory
        let output = tailscale_cmd()
            .args(["file", "get", save_dir])
            .output()
            .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tailscale file get failed: {}", stderr));
        }
        Ok(format!("Files saved to {}", save_dir))
    }
}

// ============================================================
// Public API (platform-agnostic)
// ============================================================

pub async fn fetch_status() -> Result<Vec<Peer>, String> {
    let body = platform::fetch_status_json().await?;
    let status: TailscaleStatus =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse status: {}", e))?;

    let mut peers = Vec::new();

    if let Some(self_node) = status.self_node {
        peers.push(Peer {
            id: self_node.id.unwrap_or_default(),
            public_key: self_node.public_key.unwrap_or_default(),
            hostname: self_node.host_name.unwrap_or_default(),
            dns_name: self_node.dns_name.unwrap_or_default(),
            os: self_node.os.unwrap_or_default(),
            ips: self_node.tailscale_ips.unwrap_or_default(),
            online: true,
            is_self: true,
        });
    }

    if let Some(peer_map) = status.peer {
        for (_key, p) in peer_map {
            peers.push(Peer {
                id: p.id.unwrap_or_default(),
                public_key: p.public_key.unwrap_or_default(),
                hostname: p.host_name.unwrap_or_default(),
                dns_name: p.dns_name.unwrap_or_default(),
                os: p.os.unwrap_or_default(),
                ips: p.tailscale_ips.unwrap_or_default(),
                online: p.online.unwrap_or(false),
                is_self: false,
            });
        }
    }

    Ok(peers)
}

pub async fn send_file_to_peer(
    peer_id: &str,
    filename: &str,
    data: Vec<u8>,
) -> Result<String, String> {
    platform::send_file(peer_id, filename, data).await
}

pub async fn fetch_incoming_files() -> Result<Vec<IncomingFile>, String> {
    let body = platform::get_incoming_files().await?;
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    platform::accept_file(name, save_dir).await
}

fn url_encode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('/', "%2F")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('&', "%26")
}
