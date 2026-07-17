use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Host header used in all raw HTTP requests to the Tailscale localapi.
/// Tailscale's daemon expects this exact value.
const LOCALAPI_HOST: &str = "local-tailscaled.sock";

/// Error type for socket/pipe operations that distinguishes "transport
/// unavailable" (socket/pipe missing — safe to fall back to CLI) from
/// "transport connected but request failed" (HTTP error — must propagate).
/// Used by macOS `try_socket_get`/`try_socket_get_to_file` and Windows
/// `try_pipe_get`, and consumed by `get_incoming_files`/`accept_file` to
/// decide whether the CLI fallback is appropriate.
#[derive(Debug)]
pub(crate) enum SocketGetError {
    /// `UnixStream::connect` / pipe open failed — the socket/pipe is missing
    /// or inaccessible. Falling back to the CLI is the intended behaviour.
    Connect(String),
    /// The socket/pipe connected but the HTTP request/response failed (HTTP
    /// error status, transport failure mid-response, malformed headers, disk
    /// write error, …). Must be propagated, not swallowed.
    Other(String),
}

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
    pub exit_node_option: Option<bool>,
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
    pub is_exit_node: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
    /// Peer that sent the file, when the Tailscale localapi exposes it.
    /// Accepts both camelCase (`peerName`) and PascalCase (`PeerName`).
    #[serde(default, alias = "PeerName")]
    pub peer_name: Option<String>,
}

// ============================================================
// Shared accept_file helper for CLI-based platforms (macOS/Windows)
// ============================================================

/// Shared accept_file logic for CLI-based platforms.
/// `run_get` executes the platform-specific `tailscale file get` command.
/// Sanitizes `name` to prevent path traversal attacks.
fn accept_file_with_getter(
    name: &str,
    save_dir: &str,
    run_get: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    // Sanitize filename to prevent path traversal
    let safe_name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid filename".to_string())?;

    // Snapshot directory before download to detect newly arrived files
    let dir_path = std::path::Path::new(save_dir);
    let before_entries: std::collections::HashSet<std::path::PathBuf> = {
        if dir_path.exists() {
            std::fs::read_dir(dir_path)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default()
        } else {
            std::collections::HashSet::new()
        }
    };

    // Run the platform-specific tailscale file get command
    run_get()?;

    // Check if target file appeared (may already exist from prior download)
    let save_path = dir_path.join(safe_name);
    if save_path.exists() {
        return Ok(save_path.to_string_lossy().to_string());
    }

    // Wait for file to arrive on disk (Tailscale may still be writing)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if save_path.exists() {
            return Ok(save_path.to_string_lossy().to_string());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // File didn't appear — check if any new file was downloaded
    if let Ok(after_entries) = std::fs::read_dir(dir_path) {
        let new_entries: Vec<_> = after_entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| !before_entries.contains(p))
            .collect();
        if new_entries.len() == 1 {
            return Ok(new_entries[0].to_string_lossy().to_string());
        }
    }

    Err(format!(
        "tailscale file get succeeded but '{}' did not appear in {}",
        safe_name, save_dir
    ))
}

/// Shared CLI auto-receive logic for macOS/Windows. Runs
/// `tailscale file get --wait=false --conflict=overwrite <save_dir>` (via the
/// platform-specific `run_get` closure), parses the "moved N/N files" output,
/// and returns a JSON array of the received files.
///
/// `platform_label` is used in log messages ("macOS" / "Windows").
fn cli_receive_files(
    save_dir: &str,
    platform_label: &str,
    run_get: impl FnOnce(&str) -> Result<std::process::Output, String>,
) -> Result<Vec<u8>, String> {
    let output = run_get(save_dir)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    log::debug!(
        "{} CLI auto-receive: exit={} stdout_len={} stderr_len={}",
        platform_label,
        output.status,
        stdout.len(),
        stderr.len()
    );
    // Parse "moved X/Y files" from stdout.
    let moved_line = stdout
        .lines()
        .find(|l| l.contains("moved") && l.contains("files"));
    let count = match moved_line {
        Some(line) => {
            let nums: Vec<&str> = line
                .split_whitespace()
                .filter(|w| w.chars().all(|c| c.is_ascii_digit() || c == '/'))
                .collect();
            if let Some(fraction) = nums.first() {
                let moved = fraction
                    .split('/')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok());
                log::debug!(
                    "{} CLI auto-receive: parsed '{}' → {} files",
                    platform_label,
                    line,
                    moved.unwrap_or(0)
                );
                moved.unwrap_or(0)
            } else {
                0
            }
        }
        None => {
            if !stdout.trim().is_empty() {
                log::debug!(
                    "{} CLI auto-receive: unexpected stdout: {:?}",
                    platform_label,
                    stdout.lines().take(3).collect::<Vec<_>>()
                );
            }
            0
        }
    };
    if count == 0 {
        return Ok(b"[]".to_vec());
    }
    // List the most recently modified files in save_dir matching the count.
    let entries = match std::fs::read_dir(save_dir) {
        Ok(entries) => {
            let mut files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .collect();
            files.sort_by(|a, b| {
                let ma = a.metadata().and_then(|m| m.modified()).ok();
                let mb = b.metadata().and_then(|m| m.modified()).ok();
                mb.cmp(&ma)
            });
            files.into_iter().take(count).collect::<Vec<_>>()
        }
        Err(e) => {
            log::debug!(
                "{} CLI auto-receive: can't read save_dir '{}': {}",
                platform_label,
                save_dir,
                e
            );
            return Ok(b"[]".to_vec());
        }
    };
    // Build [{name, size}] JSON using serde (correct escaping).
    let files: Vec<IncomingFile> = entries
        .iter()
        .map(|e| IncomingFile {
            name: e.file_name().to_string_lossy().to_string(),
            size: e.metadata().map(|m| m.len()).unwrap_or(0),
            peer_name: None,
        })
        .collect();
    log::debug!(
        "{} CLI auto-receive: returning {} file(s) already saved to '{}'",
        platform_label,
        files.len(),
        save_dir
    );
    let json = serde_json::to_string(&files)
        .map_err(|e| format!("Failed to serialize file list: {}", e))?;
    Ok(json.into_bytes())
}

// ============================================================
// Shared accept_file helper for CLI-based platforms (macOS/Windows)
// ============================================================

/// Short, unique timestamp suffix for collision resolution.
///
/// Combines wall-clock milliseconds with a monotonic counter so that rapid
/// successive calls never collide. The previous `nanos as u32` implementation
/// wrapped every ~4.29 seconds, causing silent file overwrites.
fn timestamp_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // ~44 bits of ms (272 years from epoch) + 20 bits of counter (~1M calls/ms)
    (ms << 20) | (n & 0xFFFFF)
}

/// Generate a unique save path to avoid overwriting existing files.
/// e.g. "file.txt" -> "file (1).txt" -> "file (2).txt"; after 999 conflicts,
/// a unique timestamp suffix is appended to guarantee uniqueness.
fn unique_save_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str());

    for i in 1..1000 {
        let new_name = match ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        let path = dir.join(&new_name);
        if !path.exists() {
            return path;
        }
    }
    // After 999 conflicts, append a unique timestamp suffix to guarantee
    // uniqueness instead of silently overwriting (which would lose data).
    let fallback_name = match ext {
        Some(e) => format!("{}-{:016x}.{}", stem, timestamp_tag(), e),
        None => format!("{}-{:016x}", stem, timestamp_tag()),
    };
    dir.join(fallback_name)
}

/// RFC 3986 percent-encoding (encode all non-unreserved characters).
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

// ============================================================
// Linux implementation — hyperlocal (Unix socket)
// ============================================================

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{unique_save_path, url_encode};
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
            .header("Host", super::LOCALAPI_HOST)
            .body(Full::new(Bytes::new()))
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
            .header("Host", super::LOCALAPI_HOST)
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

    /// Write file data to Unix socket in chunks via raw HTTP/1.1.
    /// Streams file from disk in 8KB chunks to avoid loading entire file in memory.
    /// Uses HTTP/1.1 with Content-Length and Connection: close so the daemon
    /// knows the exact body size up front (Content-Length requires HTTP/1.1).
    /// If the daemon rejects the transfer (peer offline, not found, etc.) it may
    /// send an HTTP error response and close the connection while we are still
    /// streaming the body, causing a broken-pipe write error. We catch that and
    /// attempt to read the daemon's error response before reporting.
    async fn stream_file_to_socket(api_path: &str, file_path: &str) -> Result<Vec<u8>, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|e| format!("Failed to stat file '{}': {}", file_path, e))?;
        let file_size = metadata.len();

        let mut file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("Failed to open file '{}': {}", file_path, e))?;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

        // Timeout: 60s base + 60s per MB for large files
        let timeout_secs = 60 + (file_size / (1024 * 1024)) * 60;
        let timeout = std::time::Duration::from_secs(timeout_secs.min(600));

        // Write HTTP/1.1 request with Content-Length
        let request = format!(
            "PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            api_path,
            super::LOCALAPI_HOST,
            file_size
        );
        tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
            .await
            .map_err(|_| "Timeout writing request headers to Tailscale daemon".to_string())?
            .map_err(|e| format!("Failed to write request: {}", e))?;

        // Stream file in 8KB chunks from disk to socket
        let mut buf = [0u8; 8192];
        loop {
            let n = tokio::time::timeout(timeout, file.read(&mut buf))
                .await
                .map_err(|_| "Timeout reading file data from disk".to_string())?
                .map_err(|e| format!("Failed to read file: {}", e))?;
            if n == 0 {
                break;
            }
            match tokio::time::timeout(timeout, stream.write_all(&buf[..n])).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // Write failed (likely broken pipe / connection reset). The
                    // Tailscale daemon may have rejected the transfer and sent
                    // an HTTP error response before closing the connection.
                    return Err(read_daemon_error(&mut stream, &e).await);
                }
                Err(_) => {
                    return Err("Timeout writing file data to Tailscale daemon".to_string());
                }
            }
        }

        // Read response
        let mut response = Vec::new();
        let mut reader = tokio::io::BufReader::new(&mut stream);
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read_to_end(&mut response),
        )
        .await
        .map_err(|_| "Timeout reading response from Tailscale daemon".to_string())?
        .map_err(|e| format!("Failed to read response: {}", e))?;

        // Parse HTTP response. We sent Connection: close, so read_to_end reads
        // until the daemon closes the connection, yielding the full body. The
        // Tailscale localapi returns small JSON responses for PUT requests and
        // does not use chunked Transfer-Encoding, so direct body parsing works.
        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "Invalid HTTP response from Tailscale daemon".to_string())?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let body = response[header_end + 4..].to_vec();

        if status_code != 200 {
            return Err(format!(
                "Tailscale API error ({}): {}",
                status_code,
                String::from_utf8_lossy(&body)
            ));
        }
        Ok(body)
    }

    /// Attempts to read and parse an HTTP error response from the Tailscale
    /// daemon after a write failure (broken pipe / connection reset) during
    /// file upload. The daemon often sends an HTTP error response (e.g. 400
    /// with a JSON body) before closing the connection; this surfaces that
    /// message instead of the opaque "Broken pipe (os error 32)".
    async fn read_daemon_error(
        stream: &mut tokio::net::UnixStream,
        write_err: &std::io::Error,
    ) -> String {
        use tokio::io::AsyncReadExt;

        let write_err_str = write_err.to_string();
        let is_broken_pipe = write_err_str.contains("Broken pipe")
            || write_err_str.contains("Connection reset")
            || write_err_str.contains("Connection reset by peer");

        // Try to read whatever the daemon sent before closing (short timeout —
        // the daemon has likely already closed the connection, so this returns
        // immediately in practice).
        let mut error_response = Vec::new();
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_end(&mut error_response),
        )
        .await;

        match read_result {
            Ok(Ok(_)) if !error_response.is_empty() => {
                parse_daemon_http_error(&error_response, &write_err_str, is_broken_pipe)
            }
            _ => {
                if is_broken_pipe {
                    "Tailscale daemon closed connection during file transfer \
                     (broken pipe). The peer may be offline or not accepting files."
                        .to_string()
                } else {
                    format!(
                        "Failed to write file data: {} \
                         (the peer may be offline or not accepting files)",
                        write_err_str
                    )
                }
            }
        }
    }

    /// Parses the daemon's raw HTTP error response into a human-readable error
    /// string. Extracts the status code and body when possible; falls back to
    /// including the raw response text if parsing fails.
    fn parse_daemon_http_error(response: &[u8], write_err: &str, is_broken_pipe: bool) -> String {
        let text = String::from_utf8_lossy(response);

        // Split headers from body at the first "\r\n\r\n".
        let (headers, body) = match text.find("\r\n\r\n") {
            Some(idx) => (&text[..idx], &text[idx + 4..]),
            None => (text.as_ref(), ""),
        };

        // Extract the HTTP status code from the status line, e.g.
        // "HTTP/1.1 400 Bad Request".
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: Option<u16> = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok());

        let body_trimmed = body.trim();

        // Linux socket-permission case: the localapi socket requires root or
        // operator privileges. The daemon returns 403 "file access denied" —
        // which misleadingly sounds like a receiver-side issue. Rewrite it to
        // the actionable guidance the Tailscale CLI itself prints.
        if status_code == Some(403) && body_trimmed.to_ascii_lowercase().contains("access denied") {
            return "Access denied to the Tailscale socket. \
                    Run this once: sudo tailscale set --operator=$USER"
                .to_string();
        }

        match status_code {
            Some(code) if code != 200 => {
                if !body_trimmed.is_empty() {
                    format!(
                        "Tailscale daemon rejected file transfer (HTTP {}): {}",
                        code, body_trimmed
                    )
                } else {
                    format!("Tailscale daemon rejected file transfer (HTTP {})", code)
                }
            }
            _ => {
                // Could not parse a useful status code; include the raw
                // daemon response so the user still gets a clue.
                if is_broken_pipe {
                    format!(
                        "Tailscale daemon closed connection during file transfer \
                         (broken pipe). Daemon response: {}",
                        text.trim()
                    )
                } else {
                    format!(
                        "Failed to write file data: {} | Daemon response: {}",
                        write_err,
                        text.trim()
                    )
                }
            }
        }
    }

    /// Stream a GET response from the Tailscale localapi directly to a file on disk.
    /// Avoids buffering the entire response in memory (fixes OOM for large incoming files).
    ///
    /// Uses HTTP/1.0 with `Connection: close`. HTTP/1.0 prevents the daemon from
    /// using chunked Transfer-Encoding, so the body is delivered as raw bytes
    /// terminated by connection close — no chunked-framing decoder is needed. The
    /// response headers are read first (up to the `\r\n\r\n` boundary), then the
    /// body is streamed to disk in fixed-size chunks. This is consistent with the
    /// macOS `try_socket_get`/`try_socket_delete` helpers. (The Linux upload path
    /// `stream_file_to_socket` uses HTTP/1.1 instead, because PUT uploads require
    /// `Content-Length`, which needs HTTP/1.1.)
    async fn stream_get_to_file(api_path: &str, save_path: &std::path::Path) -> Result<(), String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

        let request = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            api_path,
            super::LOCALAPI_HOST
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.write_all(request.as_bytes()),
        )
        .await
        .map_err(|_| "Timeout writing GET request to Tailscale daemon".to_string())?
        .map_err(|e| format!("Failed to write request: {}", e))?;

        // Read headers incrementally until we find \r\n\r\n
        let mut header_buf = Vec::new();
        let mut temp_buf = [0u8; 4096];
        let header_end = loop {
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                stream.read(&mut temp_buf),
            )
            .await
            .map_err(|_| "Timeout reading response headers".to_string())?
            .map_err(|e| format!("Failed to read response: {}", e))?;
            if n == 0 {
                return Err("Connection closed before headers received".to_string());
            }
            header_buf.extend_from_slice(&temp_buf[..n]);
            if let Some(pos) = header_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if header_buf.len() > 65536 {
                return Err("Response headers too large".to_string());
            }
        };

        // Check status code
        let headers = String::from_utf8_lossy(&header_buf[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            // Read the full error body until connection close (HTTP/1.0) so
            // diagnostics aren't truncated by the 4KB header read buffer.
            let mut err_body = header_buf[header_end + 4..].to_vec();
            loop {
                let n = stream
                    .read(&mut temp_buf)
                    .await
                    .map_err(|e| format!("Failed to read error body: {}", e))?;
                if n == 0 {
                    break;
                }
                err_body.extend_from_slice(&temp_buf[..n]);
                // Cap the captured error body to avoid unbounded memory growth.
                if err_body.len() > 65536 {
                    break;
                }
            }
            return Err(format!(
                "Tailscale API error ({}): {}",
                status_code,
                String::from_utf8_lossy(&err_body)
            ));
        }

        // Write any body bytes already buffered after headers
        let body_start = header_end + 4;
        let mut file = tokio::fs::File::create(save_path)
            .await
            .map_err(|e| format!("Failed to create file '{}': {}", save_path.display(), e))?;
        if body_start < header_buf.len() {
            file.write_all(&header_buf[body_start..])
                .await
                .map_err(|e| format!("Failed to write to file: {}", e))?;
        }

        // Stream remaining body to disk in chunks. No per-read timeout here —
        // the operation is bounded by the outer 120s timeout in `accept_file`,
        // matching the upload path's reliance on a single outer timeout.
        loop {
            let n = stream
                .read(&mut temp_buf)
                .await
                .map_err(|e| format!("Failed to read response body: {}", e))?;
            if n == 0 {
                break;
            }
            file.write_all(&temp_buf[..n])
                .await
                .map_err(|e| format!("Failed to write to file: {}", e))?;
        }

        Ok(())
    }

    /// Streams file from disk to socket instead of loading entire file into memory.
    /// Uses the peer's stable node ID (peer_id) for the localapi path.
    pub async fn send_file(
        peer_id: &str,
        _peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|e| format!("Failed to stat file '{}': {}", file_path, e))?;
        if !metadata.is_file() {
            return Err(format!("'{}' is not a regular file", file_path));
        }
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let api_path = format!(
            "/localapi/v0/file-put/{}/{}",
            url_encode(peer_id),
            url_encode(filename)
        );
        stream_file_to_socket(&api_path, file_path).await?;
        log::debug!("Sent {} to {}", filename, peer_id);
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files(_save_dir: &str) -> Result<Vec<u8>, String> {
        get_request("/localapi/v0/files/").await
    }

    /// Accept an incoming file. Streams response directly to disk instead of
    /// buffering in memory (fixes OOM for large incoming files).
    /// Sanitizes filename to prevent path traversal.
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        // Single outer timeout bounds the whole operation (matches the upload
        // path's adaptive timeout and macOS/Windows' 120s wrapper).
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let safe_name = std::path::Path::new(&name)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| "Invalid filename".to_string())?;
            let api_path = format!("/localapi/v0/files/{}", url_encode(&name));
            let save_path = unique_save_path(std::path::Path::new(&save_dir), safe_name);
            stream_get_to_file(&api_path, &save_path).await?;

            // Delete from pending after successful download. Surface failures
            // so stale entries don't silently linger in the pending list.
            let delete_path = format!("/localapi/v0/files/{}", url_encode(&name));
            if let Err(e) = delete_request(&delete_path).await {
                log::warn!(
                    "Failed to delete pending file '{}' from Tailscale: {}",
                    name,
                    e
                );
            }

            log::debug!("Accepted file '{}' to '{}'", name, save_path.display());
            Ok(save_path.to_string_lossy().to_string())
        })
        .await
        .map_err(|_| "accept_file timed out".to_string())?
    }

    #[cfg(test)]
    mod tests {
        use super::super::{prettify_name, unique_save_path, url_encode};
        use super::*;

        #[test]
        fn url_encode_plain() {
            assert_eq!(url_encode("hello"), "hello");
        }

        #[test]
        fn url_encode_spaces() {
            assert_eq!(url_encode("hello world"), "hello%20world");
        }

        #[test]
        fn url_encode_slashes() {
            assert_eq!(url_encode("path/to/file"), "path%2Fto%2Ffile");
        }

        #[test]
        fn url_encode_unreserved() {
            // - _ . ~ should NOT be encoded
            assert_eq!(url_encode("-_.~"), "-_.~");
        }

        #[test]
        fn url_encode_unicode() {
            // UTF-8 bytes for 'é' are 0xC3 0xA9
            assert_eq!(url_encode("é"), "%C3%A9");
        }

        #[test]
        fn prettify_basic() {
            assert_eq!(prettify_name("my-laptop"), "My Laptop");
        }

        #[test]
        fn prettify_abbreviations() {
            assert_eq!(prettify_name("pixel-10-pro-xl"), "Pixel 10 Pro XL");
            assert_eq!(prettify_name("home-nas"), "Home NAS");
        }

        #[test]
        fn prettify_underscores() {
            assert_eq!(prettify_name("my_device"), "My Device");
        }

        #[test]
        fn unique_save_path_no_conflict() {
            let dir = std::env::temp_dir().join("taildrop_test_no_conflict");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = unique_save_path(&dir, "test.txt");
            assert_eq!(path, dir.join("test.txt"));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn unique_save_path_with_conflict() {
            let dir = std::env::temp_dir().join("taildrop_test_conflict");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("test.txt"), "first").unwrap();
            std::fs::write(dir.join("test (1).txt"), "second").unwrap();
            let path = unique_save_path(&dir, "test.txt");
            assert_eq!(path, dir.join("test (2).txt"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

// ============================================================
// macOS implementation — CLI-based
// ============================================================

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    fn find_tailscale() -> Option<&'static str> {
        let candidates = [
            "/Applications/Tailscale.app/Contents/MacOS/tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ];
        candidates
            .iter()
            .find(|&&path| std::path::Path::new(path).exists())
            .copied()
    }

    /// Run the tailscale CLI via /bin/sh -c to ensure proper environment.
    /// The macOS Tailscale CLI binary inside the .app bundle relies on XPC
    /// and other macOS services that fail when the binary is exec'd directly
    /// from a .app launched by launchd (minimal environment). Running through
    /// a shell resolves this.
    fn tailscale_cmd(args: &[&str]) -> std::io::Result<std::process::Output> {
        // Reject arguments containing null bytes — these can't be passed through
        // shell interpolation and indicate malformed/malicious input.
        for arg in args {
            if arg.contains('\0') {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "argument contains null byte",
                ));
            }
        }
        let binary = find_tailscale().unwrap_or("tailscale");
        // Quote binary path and each argument to handle spaces/special chars
        let escaped_binary = format!("'{}'", binary.replace('\'', "'\\''"));
        let escaped_args: Vec<String> = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
            .collect();
        let shell_cmd = format!("{} {}", escaped_binary, escaped_args.join(" "));
        log::debug!("macOS shell cmd: /bin/sh -c {}", shell_cmd);
        Command::new("/bin/sh").arg("-c").arg(&shell_cmd).output()
    }

    const SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

    /// Try an HTTP/1.0 GET via the Tailscale Unix socket.
    /// Works when the socket is accessible (Homebrew/open-source installs).
    /// Fails gracefully for App Store installs with restricted permissions.
    /// Uses HTTP/1.0 which guarantees non-chunked responses and connection close,
    /// so read_to_end will read the complete response body without needing to
    /// parse chunked Transfer-Encoding.
    fn try_socket_get(path: &str) -> Result<Vec<u8>, super::SocketGetError> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("connect: {}", e)))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|e| super::SocketGetError::Other(format!("timeout: {}", e)))?;

        // Use HTTP/1.0 to guarantee non-chunked response and connection close
        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| super::SocketGetError::Other(format!("read: {}", e)))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| super::SocketGetError::Other("Invalid HTTP response".to_string()))?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            return Err(super::SocketGetError::Other(format!(
                "HTTP error: {}",
                status_line
            )));
        }

        Ok(response[header_end + 4..].to_vec())
    }

    /// CLI auto-receive fallback for when the Unix socket is unavailable (the
    /// macOS GUI install case). Delegates to the shared `cli_receive_files`
    /// helper with the macOS-specific `tailscale_cmd` invocation.
    fn try_cli_receive_files(save_dir: &str) -> Result<Vec<u8>, String> {
        super::cli_receive_files(save_dir, "macOS", |dir| {
            tailscale_cmd(&[
                "file",
                "get",
                "--wait=false",
                "--verbose",
                "--conflict=overwrite",
                dir,
            ])
            .map_err(|e| format!("Failed to run tailscale file get: {}", e))
        })
    }

    // SocketGetError is defined at crate root (super::SocketGetError).
    // It distinguishes Connect (socket unavailable → CLI fallback safe)
    // from Other (HTTP error → must propagate).

    /// Stream a GET response from the Tailscale Unix socket directly to disk.
    ///
    /// Uses HTTP/1.0 (non-chunked, connection close at end of body). The
    /// response headers are read first in small chunks until the `\r\n\r\n`
    /// boundary is found; any body bytes that arrived in the same read buffer
    /// are flushed to the file before continuing. The remaining body is then
    /// streamed to disk in 8 KB chunks, so files much larger than memory can
    /// be downloaded without OOM risk — mirroring the Linux
    /// [`stream_get_to_file`](super::stream_get_to_file) design.
    ///
    /// Returns [`SocketGetError::Connect`] only when `UnixStream::connect`
    /// fails (the App Store install case). All other failures — including
    /// HTTP 4xx/5xx responses — are returned as [`SocketGetError::Other`] so
    /// callers can propagate them instead of silently falling back to the CLI.
    fn try_socket_get_to_file(
        path: &str,
        save_path: &std::path::Path,
    ) -> Result<(), super::SocketGetError> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("connect: {}", e)))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|e| super::SocketGetError::Other(format!("timeout: {}", e)))?;

        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        // Read headers incrementally until we find the \r\n\r\n boundary.
        // Any body bytes that arrive in the same buffer must be written to
        // the file afterwards — they are the start of the response body, not
        // part of the headers.
        let mut header_buf: Vec<u8> = Vec::with_capacity(8192);
        let mut temp_buf = [0u8; 8192];
        let header_end = loop {
            let n = stream
                .read(&mut temp_buf)
                .map_err(|e| super::SocketGetError::Other(format!("read headers: {}", e)))?;
            if n == 0 {
                return Err(super::SocketGetError::Other(
                    "Connection closed before headers received".to_string(),
                ));
            }
            header_buf.extend_from_slice(&temp_buf[..n]);
            if let Some(pos) = header_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if header_buf.len() > 65536 {
                return Err(super::SocketGetError::Other(
                    "Response headers too large".to_string(),
                ));
            }
        };

        // Parse the HTTP status line.
        let headers = String::from_utf8_lossy(&header_buf[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            // Drain the rest of the error body until connection close so the
            // diagnostic isn't truncated by the 8 KB header buffer. The error
            // body is small (a short message from the daemon), but we cap it
            // to avoid unbounded memory growth on a misbehaving server.
            let mut err_body = header_buf[header_end + 4..].to_vec();
            loop {
                let n = stream
                    .read(&mut temp_buf)
                    .map_err(|e| super::SocketGetError::Other(format!("read error body: {}", e)))?;
                if n == 0 {
                    break;
                }
                err_body.extend_from_slice(&temp_buf[..n]);
                if err_body.len() > 65536 {
                    break;
                }
            }
            return Err(super::SocketGetError::Other(format!(
                "Tailscale API error ({}): {}",
                status_code,
                String::from_utf8_lossy(&err_body)
            )));
        }

        // Open the output file and flush any body bytes already sitting in the
        // header buffer (the chunk that contained the final \r\n\r\n often
        // also carries the start of the body).
        let body_start = header_end + 4;
        let mut file = std::fs::File::create(save_path).map_err(|e| {
            super::SocketGetError::Other(format!("create file '{}': {}", save_path.display(), e))
        })?;
        if body_start < header_buf.len() {
            file.write_all(&header_buf[body_start..])
                .map_err(|e| super::SocketGetError::Other(format!("write body to file: {}", e)))?;
        }

        // Stream the remaining body to disk in 8 KB chunks. HTTP/1.0 + the
        // read timeout above bounds the wait; the loop exits when the daemon
        // closes the connection at end of body.
        loop {
            let n = stream
                .read(&mut temp_buf)
                .map_err(|e| super::SocketGetError::Other(format!("read body: {}", e)))?;
            if n == 0 {
                break;
            }
            file.write_all(&temp_buf[..n])
                .map_err(|e| super::SocketGetError::Other(format!("write body to file: {}", e)))?;
        }

        Ok(())
    }

    /// Best-effort DELETE of a pending file via the Tailscale Unix socket.
    ///
    /// Returns `Ok(())` only when the daemon responds with HTTP 200. Any other
    /// status (or a transport error) is surfaced as `Err` so the caller's
    /// `log::warn!` fires — matching the Linux [`delete_request`](super::delete_request)
    /// behaviour where HTTP errors are propagated, not silently discarded.
    fn try_socket_delete(path: &str) -> Result<(), String> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|e| format!("connect: {}", e))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| format!("timeout: {}", e))?;

        let req = format!(
            "DELETE {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {}", e))?;

        // DELETE responses are tiny (a short status line at most), so reading
        // the full response into memory is fine. HTTP/1.0 closes the
        // connection at end of body, so read_to_end captures everything
        // without needing to parse chunked encoding.
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| format!("read: {}", e))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "Invalid HTTP response from DELETE".to_string())?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            let body = String::from_utf8_lossy(&response[header_end + 4..]);
            return Err(format!(
                "Tailscale API error on DELETE ({}): {}",
                status_code, body
            ));
        }
        Ok(())
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(|| {
                let binary_path = find_tailscale().unwrap_or("tailscale");
                log::debug!("macOS fetch_status_json: binary={}", binary_path);

                let output = tailscale_cmd(&["status", "--json"]).map_err(|e| {
                    format!(
                        "Could not run tailscale CLI [tried: {}]: {}",
                        binary_path, e
                    )
                })?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                log::debug!(
                    "macOS CLI result: exit={} stdout_len={} stderr_len={}",
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
            }),
        )
        .await
        .map_err(|_| "fetch_status_json timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Send file to peer using tailscale CLI. Non-blocking via spawn_blocking.
    pub async fn send_file(
        _peer_id: &str,
        peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = (120 + (file_size / (1024 * 1024)) * 60).min(600);
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || {
                let output = tailscale_cmd(&["file", "cp", &file_path, &format!("{}:", peer_name)])
                    .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale file cp failed: {}", stderr));
                }
                Ok(format!("Sent file to {}", peer_name))
            }),
        )
        .await
        .map_err(|_| "send_file timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn get_incoming_files(save_dir: &str) -> Result<Vec<u8>, String> {
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || match try_socket_get("/localapi/v0/files/") {
                Ok(data) => {
                    log::debug!("macOS: socket file listing OK ({} bytes)", data.len());
                    Ok(data)
                }
                Err(super::SocketGetError::Connect(e)) => {
                    log::debug!("macOS: socket file listing failed (connect): {}", e);
                    log::debug!("macOS: falling back to CLI auto-receive to '{}'", save_dir);
                    try_cli_receive_files(&save_dir)
                }
                Err(super::SocketGetError::Other(e)) => {
                    log::debug!("macOS: socket file listing failed: {}", e);
                    Ok(b"[]".to_vec())
                }
            }),
        )
        .await
        .map_err(|_| "get_incoming_files timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Accept an incoming file. Tries the Unix socket first (streams large
    /// files efficiently), then falls back to the `tailscale file get` CLI —
    /// but **only** when the socket itself is unavailable (the App Store
    /// install case). HTTP-level errors from the daemon (404, 5xx, …) are
    /// propagated to the caller rather than masked by the CLI, which would
    /// otherwise download *every* pending file into `save_dir`. Uses the
    /// shared helper with path traversal sanitization for the CLI path.
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || {
                let safe_name = std::path::Path::new(&name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| "Invalid filename".to_string())?;
                let save_path = super::unique_save_path(std::path::Path::new(&save_dir), safe_name);
                let api_path = format!("/localapi/v0/files/{}", super::url_encode(&name));

                // Try the socket first (streams large files without buffering).
                match try_socket_get_to_file(&api_path, &save_path) {
                    Ok(()) => {
                        // Best-effort delete of the pending file from the daemon.
                        let delete_path =
                            format!("/localapi/v0/files/{}", super::url_encode(&name));
                        if let Err(e) = try_socket_delete(&delete_path) {
                            log::warn!(
                                "Failed to delete pending file '{}' from Tailscale: {}",
                                name,
                                e
                            );
                        }
                        Ok(save_path.to_string_lossy().to_string())
                    }
                    Err(super::SocketGetError::Connect(socket_err)) => {
                        // The Unix socket is missing/inaccessible (e.g. App
                        // Store install) — fall back to the `tailscale file
                        // get` CLI. This is the only case where falling back
                        // is safe: the socket itself is unavailable, so the
                        // daemon cannot be queried directly.
                        log::debug!(
                            "macOS: socket unavailable ({}), falling back to CLI",
                            socket_err
                        );
                        super::accept_file_with_getter(&name, &save_dir, || {
                            // --wait=false: don't block if the inbox is empty
                            // (the file may have already been consumed by the
                            // auto-receive poll on macOS).
                            let output = tailscale_cmd(&["file", "get", "--wait=false", &save_dir])
                                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
                            if !output.status.success() {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                return Err(format!("tailscale file get failed: {}", stderr));
                            }
                            Ok(())
                        })
                    }
                    Err(super::SocketGetError::Other(http_err)) => {
                        // The socket connected but the request failed (HTTP
                        // 4xx/5xx, transport failure mid-response, disk write
                        // error, …). Propagate the real error instead of
                        // falling back to the CLI — otherwise a transient
                        // daemon error would cause `tailscale file get` to
                        // download every pending file into save_dir.
                        log::debug!(
                            "macOS: socket accept failed with HTTP/transport error ({}), \
                             not falling back to CLI",
                            http_err
                        );
                        Err(http_err)
                    }
                }
            }),
        )
        .await
        .map_err(|_| "accept_file timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }
}

// ============================================================
// Windows implementation — CLI-based
// ============================================================

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    fn tailscale_cmd() -> Command {
        let candidates = [
            r"C:\Program Files\Tailscale\tailscale.exe",
            r"C:\Program Files (x86)\Tailscale\tailscale.exe",
        ];
        let binary = candidates
            .iter()
            .find(|&&path| std::path::Path::new(path).exists())
            .copied()
            .unwrap_or("tailscale");
        let mut cmd = Command::new(binary);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    /// CLI auto-receive fallback for when the named pipe is unavailable.
    /// Delegates to the shared `cli_receive_files` helper with the
    /// Windows-specific `tailscale_cmd` invocation.
    fn try_cli_receive_files(save_dir: &str) -> Result<Vec<u8>, String> {
        super::cli_receive_files(save_dir, "Windows", |dir| {
            tailscale_cmd()
                .args([
                    "file",
                    "get",
                    "--wait=false",
                    "--verbose",
                    "--conflict=overwrite",
                    dir,
                ])
                .output()
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))
        })
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(|| {
                let output = tailscale_cmd()
                    .args(["status", "--json"])
                    .output()
                    .map_err(|e| {
                        format!(
                            "Could not run tailscale CLI. Make sure Tailscale is installed and in your PATH: {}",
                            e
                        )
                    })?;
                log::debug!(
                    "Windows CLI result: exit={} stdout_len={} stderr_len={}",
                    output.status,
                    output.stdout.len(),
                    output.stderr.len()
                );
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale status failed: {}", stderr));
                }
                Ok(output.stdout)
            }),
        )
        .await
        .map_err(|_| "fetch_status_json timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Send file to peer using tailscale CLI. Non-blocking via spawn_blocking.
    pub async fn send_file(
        _peer_id: &str,
        peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = (120 + (file_size / (1024 * 1024)) * 60).min(600);
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || {
                let output = tailscale_cmd()
                    .args(["file", "cp", &file_path, &format!("{}:", peer_name)])
                    .output()
                    .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale file cp failed: {}", stderr));
                }
                let filename = std::path::Path::new(&file_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                log::debug!("Sent {} to {}", filename, peer_name);
                Ok(format!("Sent file to {}", peer_name))
            }),
        )
        .await
        .map_err(|_| "send_file timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Named pipe path for the Tailscale daemon's local API on Windows.
    const PIPE_PATH: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";

    /// Try an HTTP/1.0 GET via the Tailscale named pipe.
    /// Uses HTTP/1.0 which guarantees non-chunked responses and connection close,
    /// so read_to_end will read the complete response body without needing to
    /// parse chunked Transfer-Encoding.
    fn try_pipe_get(path: &str) -> Result<Vec<u8>, super::SocketGetError> {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};

        let mut pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("open pipe: {}", e)))?;

        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        pipe.write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        let mut response = Vec::new();
        pipe.read_to_end(&mut response)
            .map_err(|e| super::SocketGetError::Other(format!("read: {}", e)))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| super::SocketGetError::Other("Invalid HTTP response".to_string()))?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            return Err(super::SocketGetError::Other(format!(
                "HTTP error: {}",
                status_line
            )));
        }

        Ok(response[header_end + 4..].to_vec())
    }

    pub async fn get_incoming_files(save_dir: &str) -> Result<Vec<u8>, String> {
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || match try_pipe_get("/localapi/v0/files/") {
                Ok(data) => {
                    log::debug!("Windows: pipe file listing OK ({} bytes)", data.len());
                    Ok(data)
                }
                Err(super::SocketGetError::Connect(e)) => {
                    log::debug!("Windows: pipe file listing failed (connect): {}", e);
                    log::debug!(
                        "Windows: falling back to CLI auto-receive to '{}'",
                        save_dir
                    );
                    try_cli_receive_files(&save_dir)
                }
                Err(super::SocketGetError::Other(e)) => {
                    log::debug!("Windows: pipe file listing failed: {}", e);
                    Ok(b"[]".to_vec())
                }
            }),
        )
        .await
        .map_err(|_| "get_incoming_files timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Accept an incoming file. Uses shared helper with path traversal sanitization.
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || {
                super::accept_file_with_getter(&name, &save_dir, || {
                    // --wait=false: don't block if the inbox is empty.
                    let output = tailscale_cmd()
                        .args(["file", "get", "--wait=false", &save_dir])
                        .output()
                        .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!("tailscale file get failed: {}", stderr));
                    }
                    Ok(())
                })
            }),
        )
        .await
        .map_err(|_| "accept_file timed out".to_string())?
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
    name.split(['-', '_'])
        .map(|word| {
            if word.chars().all(|c| c.is_ascii_digit()) {
                return word.to_string();
            }
            // Common abbreviations that should be uppercase
            let upper = word.to_uppercase();
            match upper.as_str() {
                "XL" | "XS" | "SE" | "TV" | "PC" | "NAS" | "VM" | "VPN" | "USB" | "NUC" | "AI"
                | "IO" | "UK" | "US" | "EU" => upper,
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
    log::debug!("fetch_status: got {} bytes of JSON", body.len());
    let status: TailscaleStatus = serde_json::from_slice(&body).map_err(|e| {
        log::error!("fetch_status: PARSE ERROR: {}", e);
        // Log first 200 chars of body for diagnosis
        let preview = String::from_utf8_lossy(&body[..body.len().min(200)]);
        log::debug!("JSON preview: {}", preview);
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
            is_exit_node: self_node.exit_node_option.unwrap_or(false),
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
                is_exit_node: p.exit_node_option.unwrap_or(false),
            });
        }
    }

    peers.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    Ok(peers)
}

pub async fn send_file_to_peer(
    peer_id: &str,
    peer_name: &str,
    file_path: &str,
) -> Result<String, String> {
    platform::send_file(peer_id, peer_name, file_path).await
}

pub async fn fetch_incoming_files(save_dir: &str) -> Result<Vec<IncomingFile>, String> {
    let body = platform::get_incoming_files(save_dir).await?;
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    platform::accept_file(name, save_dir).await
}
