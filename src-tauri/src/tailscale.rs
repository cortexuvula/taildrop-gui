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
    pub display_name: String,
    pub machine_name: String,
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
}

// ============================================================
// Linux implementation — hyperlocal (Unix socket)
// ============================================================

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
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

    // Bug #2: accept file_path instead of data; Bug #3: use peer_id (stable ID) for localapi
    pub async fn send_file(peer_id: &str, _peer_name: &str, file_path: &str) -> Result<String, String> {
        let data = tokio::fs::read(file_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {}", file_path, e))?;
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let path = format!(
            "/localapi/v0/file-put/{}/{}",
            super::url_encode(peer_id),
            super::url_encode(filename)
        );
        put_request(&path, data).await?;
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        get_request("/localapi/v0/files/").await
    }

    // Bug #1: sanitize filename to prevent path traversal
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let path = format!("/localapi/v0/files/{}", super::url_encode(name));
        let data = get_request(&path).await?;

        let safe_name = std::path::Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "Invalid filename".to_string())?;
        let save_path = std::path::Path::new(save_dir).join(safe_name);
        tokio::fs::write(&save_path, &data)
            .await
            .map_err(|e| format!("Failed to save file: {}", e))?;

        delete_request(&path).await?;
        Ok(save_path.to_string_lossy().to_string())
    }
}

// ============================================================
// macOS implementation — CLI-based
// ============================================================

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process::Command;

    fn find_tailscale() -> Option<&'static str> {
        let candidates = [
            "/Applications/Tailscale.app/Contents/MacOS/tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path);
            }
        }
        None
    }

    /// Run the tailscale CLI via /bin/sh -c to ensure proper environment.
    /// The macOS Tailscale CLI binary inside the .app bundle relies on XPC
    /// and other macOS services that fail when the binary is exec'd directly
    /// from a .app launched by launchd (minimal environment). Running through
    /// a shell resolves this.
    fn tailscale_cmd(args: &[&str]) -> std::io::Result<std::process::Output> {
        let binary = find_tailscale().unwrap_or("tailscale");
        let escaped_args: Vec<String> = args.iter().map(|a| {
            // Single-quote each argument, escaping any embedded single quotes
            format!("'{}'", a.replace('\'', "'\\''"))
        }).collect();
        let shell_cmd = format!("{} {}", binary, escaped_args.join(" "));
        eprintln!("[taildrop] macOS shell cmd: /bin/sh -c {}", shell_cmd);
        Command::new("/bin/sh")
            .arg("-c")
            .arg(&shell_cmd)
            .output()
    }

    // Bug #9: wrap blocking CLI calls in spawn_blocking
    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(|| {
            let binary_path = find_tailscale().unwrap_or("tailscale");
            eprintln!("[taildrop] macOS fetch_status_json: binary={}", binary_path);

            let output = tailscale_cmd(&["status", "--json"])
                .map_err(|e| format!(
                    "Could not run tailscale CLI [tried: {}]: {}",
                    binary_path, e
                ))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            eprintln!(
                "[taildrop] macOS CLI result: exit={} stdout_len={} stderr_len={}",
                output.status,
                output.stdout.len(),
                output.stderr.len()
            );

            if !output.status.success() {
                return Err(format!(
                    "tailscale status failed [binary: {}] stderr: {} stdout: {}",
                    binary_path, stderr, stdout
                ));
            }
            if output.stdout.is_empty() {
                return Err(format!(
                    "tailscale returned empty output [binary: {}] stderr: {}",
                    binary_path, stderr
                ));
            }
            Ok(output.stdout)
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    // Bug #2: accept file_path (no more temp file dance)
    // Bug #6: temp file collision eliminated — uses real file path
    // Bug #9: non-blocking via spawn_blocking
    pub async fn send_file(_peer_id: &str, peer_name: &str, file_path: &str) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        tokio::task::spawn_blocking(move || {
            let output = tailscale_cmd(&["file", "cp", &file_path, &format!("{}:", peer_name)])
                .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file cp failed: {}", stderr));
            }
            Ok(format!("Sent file to {}", peer_name))
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        Ok(b"[]".to_vec())
    }

    // Bug #9: non-blocking via spawn_blocking
    pub async fn accept_file(_name: &str, save_dir: &str) -> Result<String, String> {
        let save_dir = save_dir.to_string();
        tokio::task::spawn_blocking(move || {
            let output = tailscale_cmd(&["file", "get", &save_dir])
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file get failed: {}", stderr));
            }
            Ok(format!("Files saved to {}", save_dir))
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }
}

// ============================================================
// Windows implementation — CLI-based
// ============================================================

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

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
        let mut cmd = Command::new("tailscale");
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    // Bug #9: non-blocking via spawn_blocking
    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(|| {
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
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    // Bug #2: accept file_path (no more temp file dance)
    // Bug #6: temp file collision eliminated
    // Bug #9: non-blocking via spawn_blocking
    pub async fn send_file(_peer_id: &str, peer_name: &str, file_path: &str) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        tokio::task::spawn_blocking(move || {
            let output = tailscale_cmd()
                .args(["file", "cp", &file_path, &format!("{}:", peer_name)])
                .output()
                .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file cp failed: {}", stderr));
            }
            Ok(format!("Sent file to {}", peer_name))
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        Ok(b"[]".to_vec())
    }

    // Bug #9: non-blocking via spawn_blocking
    pub async fn accept_file(_name: &str, save_dir: &str) -> Result<String, String> {
        let save_dir = save_dir.to_string();
        tokio::task::spawn_blocking(move || {
            let output = tailscale_cmd()
                .args(["file", "get", &save_dir])
                .output()
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file get failed: {}", stderr));
            }
            Ok(format!("Files saved to {}", save_dir))
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }
}

// ============================================================
// Public API (platform-agnostic)
// ============================================================

/// Extract the machine name from a Tailscale DNS name and prettify it.
/// e.g. "my-laptop.tail1234.ts.net." -> "My Laptop"
///      "pixel-10-pro-xl.tail1234.ts.net." -> "Pixel 10 Pro XL"
/// Raw machine name from DNS (e.g. "pixel-10-pro-xl.tail1234.ts.net." -> "pixel-10-pro-xl").
/// This is what the CLI expects for `tailscale file cp`.
fn raw_machine_name(dns_name: &str) -> Option<String> {
    let name = dns_name.split('.').next().filter(|s| !s.is_empty())?;
    Some(name.to_string())
}

/// Prettified display name from DNS (e.g. "pixel-10-pro-xl" -> "Pixel 10 Pro XL").
fn display_name_from_dns(dns_name: &str) -> Option<String> {
    let name = dns_name.split('.').next().filter(|s| !s.is_empty())?;
    Some(prettify_name(name))
}

fn prettify_name(name: &str) -> String {
    name.split(|c| c == '-' || c == '_')
        .map(|word| {
            if word.chars().all(|c| c.is_ascii_digit()) {
                return word.to_string();
            }
            // Common abbreviations that should be uppercase
            let upper = word.to_uppercase();
            match upper.as_str() {
                "XL" | "XS" | "SE" | "TV" | "PC" | "NAS" | "VM" | "VPN"
                | "USB" | "NUC" | "AI" | "IO" | "UK" | "US" | "EU" => upper,
                _ => {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
                        None => String::new(),
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn fetch_status() -> Result<Vec<Peer>, String> {
    let body = platform::fetch_status_json().await?;
    eprintln!("[taildrop] fetch_status: got {} bytes of JSON", body.len());
    let status: TailscaleStatus =
        serde_json::from_slice(&body).map_err(|e| {
            eprintln!("[taildrop] fetch_status: PARSE ERROR: {}", e);
            // Log first 200 chars of body for diagnosis
            let preview = String::from_utf8_lossy(&body[..body.len().min(200)]);
            eprintln!("[taildrop] JSON preview: {}", preview);
            format!("Failed to parse status: {}", e)
        })?;

    let mut peers = Vec::new();

    if let Some(self_node) = status.self_node {
        let dns = self_node.dns_name.unwrap_or_default();
        let host = self_node.host_name.unwrap_or_default();
        let machine = raw_machine_name(&dns).unwrap_or_else(|| host.clone());
        let display = display_name_from_dns(&dns).unwrap_or_else(|| host.clone());
        peers.push(Peer {
            id: self_node.id.unwrap_or_default(),
            public_key: self_node.public_key.unwrap_or_default(),
            hostname: host,
            dns_name: dns,
            display_name: display,
            machine_name: machine,
            os: self_node.os.unwrap_or_default(),
            ips: self_node.tailscale_ips.unwrap_or_default(),
            online: true,
            is_self: true,
        });
    }

    if let Some(peer_map) = status.peer {
        for (_key, p) in peer_map {
            let dns = p.dns_name.unwrap_or_default();
            let host = p.host_name.unwrap_or_default();
            let machine = raw_machine_name(&dns).unwrap_or_else(|| host.clone());
            let display = display_name_from_dns(&dns).unwrap_or_else(|| host.clone());
            peers.push(Peer {
                id: p.id.unwrap_or_default(),
                public_key: p.public_key.unwrap_or_default(),
                hostname: host,
                dns_name: dns,
                display_name: display,
                machine_name: machine,
                os: p.os.unwrap_or_default(),
                ips: p.tailscale_ips.unwrap_or_default(),
                online: p.online.unwrap_or(false),
                is_self: false,
            });
        }
    }

    peers.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));

    Ok(peers)
}

pub async fn send_file_to_peer(
    peer_id: &str,
    peer_name: &str,
    file_path: &str,
) -> Result<String, String> {
    platform::send_file(peer_id, peer_name, file_path).await
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

// Bug #8: proper RFC 3986 percent-encoding (encode all non-unreserved characters)
fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}
