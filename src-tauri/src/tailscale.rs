use hyper::body::Buf;
use hyper::Request;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use http_body_util::{BodyExt, Full};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Tailscale API Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleStatus {
    pub version: Option<String>,
    #[serde(rename = "Self")]
    pub self_node: Option<PeerStatus>,
    #[serde(rename = "Peer")]
    pub peer: Option<HashMap<String, PeerStatus>>,
    #[serde(rename = "MagicDNSSuffix")]
    pub magic_dns_suffix: Option<String>,
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
    #[serde(rename = "ShareeNode")]
    pub sharee_node: Option<bool>,
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

// --- Socket Path ---

fn get_socket_path() -> String {
    if cfg!(target_os = "macos") {
        // macOS: the socket is at a well-known path
        "/var/run/tailscale/tailscaled.sock".to_string()
    } else if cfg!(target_os = "linux") {
        // Linux: standard path
        "/var/run/tailscale/tailscaled.sock".to_string()
    } else {
        // Windows uses named pipes — handled separately
        "/var/run/tailscale/tailscaled.sock".to_string()
    }
}

// --- HTTP Client over Unix Socket ---

#[cfg(unix)]
async fn get_request(path: &str) -> Result<Vec<u8>, String> {
    let socket_path = get_socket_path();
    let url: hyper::Uri = hyperlocal::Uri::new(&socket_path, path)
        .into();

    let client: Client<hyperlocal::UnixConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new())
            .build(hyperlocal::UnixConnector);

    let req = Request::builder()
        .uri(url)
        .header("Host", "local-tailscaled.sock")
        .body(Full::new(Bytes::new()))
        .map_err(|e| format!("Failed to build request: {}", e))?;

    let resp = client
        .request(req)
        .await
        .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

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
        let body_str = String::from_utf8_lossy(&buf);
        return Err(format!("Tailscale API error ({}): {}", status, body_str));
    }

    Ok(buf)
}

#[cfg(unix)]
async fn put_request(path: &str, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let socket_path = get_socket_path();
    let url: hyper::Uri = hyperlocal::Uri::new(&socket_path, path)
        .into();

    let client: Client<hyperlocal::UnixConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new())
            .build(hyperlocal::UnixConnector);

    let req = Request::builder()
        .method(hyper::Method::PUT)
        .uri(url)
        .header("Host", "local-tailscaled.sock")
        .body(Full::new(Bytes::from(body_bytes)))
        .map_err(|e| format!("Failed to build request: {}", e))?;

    let resp = client
        .request(req)
        .await
        .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

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
        let body_str = String::from_utf8_lossy(&buf);
        return Err(format!("Tailscale API error ({}): {}", status, body_str));
    }

    Ok(buf)
}

#[cfg(unix)]
async fn delete_request(path: &str) -> Result<Vec<u8>, String> {
    let socket_path = get_socket_path();
    let url: hyper::Uri = hyperlocal::Uri::new(&socket_path, path)
        .into();

    let client: Client<hyperlocal::UnixConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new())
            .build(hyperlocal::UnixConnector);

    let req = Request::builder()
        .method(hyper::Method::DELETE)
        .uri(url)
        .header("Host", "local-tailscaled.sock")
        .body(Full::new(Bytes::new()))
        .map_err(|e| format!("Failed to build request: {}", e))?;

    let resp = client
        .request(req)
        .await
        .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

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
        let body_str = String::from_utf8_lossy(&buf);
        return Err(format!("Tailscale API error ({}): {}", status, body_str));
    }

    Ok(buf)
}

// Windows stubs — real implementation would use named pipes
#[cfg(not(unix))]
async fn get_request(path: &str) -> Result<Vec<u8>, String> {
    // On Windows, connect to \\.\pipe\ProtectedPrefix\Tailscale\tailscaled
    // For now, use the CLI fallback
    use std::process::Command;
    let output = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("Failed to run tailscale CLI: {}", e))?;
    if !output.status.success() {
        return Err(format!("tailscale CLI error: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout)
}

#[cfg(not(unix))]
async fn put_request(path: &str, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    Err("Windows named pipe PUT not yet implemented — use tailscale CLI".to_string())
}

#[cfg(not(unix))]
async fn delete_request(path: &str) -> Result<Vec<u8>, String> {
    Err("Windows named pipe DELETE not yet implemented — use tailscale CLI".to_string())
}

// --- Public API ---

pub async fn fetch_status() -> Result<Vec<Peer>, String> {
    let body = get_request("/localapi/v0/status").await?;
    let status: TailscaleStatus =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse status: {}", e))?;

    let mut peers = Vec::new();

    // Add self node
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

    // Add peer nodes
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

pub async fn send_file_to_peer(peer_id: &str, filename: &str, data: Vec<u8>) -> Result<String, String> {
    let path = format!(
        "/localapi/v0/file-put/{}",
        urlencoded(peer_id)
    );

    // The Tailscale LocalAPI expects the filename as a query param
    let full_path = format!("{}?name={}", path, urlencoded(filename));

    put_request(&full_path, data).await?;
    Ok(format!("Sent {} to {}", filename, peer_id))
}

pub async fn fetch_incoming_files() -> Result<Vec<IncomingFile>, String> {
    let body = get_request("/localapi/v0/files/").await?;
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    // GET the file content
    let path = format!("/localapi/v0/files/{}", urlencoded(name));
    let data = get_request(&path).await?;

    // Save to disk
    let save_path = std::path::Path::new(save_dir).join(name);
    std::fs::write(&save_path, &data)
        .map_err(|e| format!("Failed to save file: {}", e))?;

    // DELETE to clear from queue
    delete_request(&path).await?;

    Ok(save_path.to_string_lossy().to_string())
}

fn urlencoded(s: &str) -> String {
    // Simple percent-encoding for path segments
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('/', "%2F")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('&', "%26")
}
