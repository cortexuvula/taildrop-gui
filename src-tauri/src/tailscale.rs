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
mod transport {
    use super::*;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Buf;
    use hyper::Request;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

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

    pub async fn get_request(path: &str) -> Result<Vec<u8>, String> {
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

    pub async fn put_request(path: &str, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
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

    pub async fn delete_request(path: &str) -> Result<Vec<u8>, String> {
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
}

// ============================================================
// Windows implementation — raw HTTP over named pipe
// ============================================================

#[cfg(windows)]
mod transport {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ClientOptions;

    const PIPE_NAME: &str = r"\\.\pipe\ProtectedPrefix\Tailscale\tailscaled";

    async fn pipe_request(
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, String> {
        // Connect to Tailscale named pipe
        let mut pipe = ClientOptions::new()
            .open(PIPE_NAME)
            .map_err(|e| format!("Failed to open Tailscale pipe (is Tailscale running?): {}", e))?;

        let body_bytes = body.unwrap_or_default();

        // Build a minimal HTTP/1.0 request (no keep-alive, simpler parsing)
        let request = format!(
            "{} {} HTTP/1.0\r\nHost: local-tailscaled.sock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            method,
            path,
            body_bytes.len()
        );

        pipe.write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Failed to write request headers: {}", e))?;

        if !body_bytes.is_empty() {
            pipe.write_all(&body_bytes)
                .await
                .map_err(|e| format!("Failed to write request body: {}", e))?;
        }

        // Read full response
        let mut response = Vec::new();
        pipe.read_to_end(&mut response)
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        // Split headers / body at \r\n\r\n
        let sep = b"\r\n\r\n";
        let body_start = response
            .windows(4)
            .position(|w| w == sep)
            .ok_or_else(|| "Malformed HTTP response from Tailscale".to_string())?;

        let headers = String::from_utf8_lossy(&response[..body_start]);
        let status_line = headers.lines().next().unwrap_or("");

        // Extract status code from "HTTP/1.x NNN ..."
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if status_code < 200 || status_code >= 300 {
            let resp_body = String::from_utf8_lossy(&response[body_start + 4..]);
            return Err(format!(
                "Tailscale API error ({}): {}",
                status_code, resp_body
            ));
        }

        Ok(response[body_start + 4..].to_vec())
    }

    pub async fn get_request(path: &str) -> Result<Vec<u8>, String> {
        pipe_request("GET", path, None).await
    }

    pub async fn put_request(path: &str, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        pipe_request("PUT", path, Some(body_bytes)).await
    }

    pub async fn delete_request(path: &str) -> Result<Vec<u8>, String> {
        pipe_request("DELETE", path, None).await
    }
}

// ============================================================
// Public API (platform-agnostic — uses transport module above)
// ============================================================

pub async fn fetch_status() -> Result<Vec<Peer>, String> {
    let body = transport::get_request("/localapi/v0/status").await?;
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
    let path = format!(
        "/localapi/v0/file-put/{}?name={}",
        url_encode(peer_id),
        url_encode(filename)
    );
    transport::put_request(&path, data).await?;
    Ok(format!("Sent {} to {}", filename, peer_id))
}

pub async fn fetch_incoming_files() -> Result<Vec<IncomingFile>, String> {
    let body = transport::get_request("/localapi/v0/files/").await?;
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    let path = format!("/localapi/v0/files/{}", url_encode(name));
    let data = transport::get_request(&path).await?;

    let save_path = std::path::Path::new(save_dir).join(name);
    std::fs::write(&save_path, &data).map_err(|e| format!("Failed to save file: {}", e))?;

    transport::delete_request(&path).await?;
    Ok(save_path.to_string_lossy().to_string())
}

fn url_encode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('/', "%2F")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('&', "%26")
}
