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
#[allow(dead_code)] // Only used on macOS/Windows; Linux uses hyper directly
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
    #[serde(alias = "Name")]
    pub name: String,
    #[serde(alias = "Size")]
    pub size: u64,
    /// Peer that sent the file, when the Tailscale localapi exposes it.
    /// Accepts both camelCase (`peerName`) and PascalCase (`PeerName`).
    #[serde(default, alias = "PeerName")]
    pub peer_name: Option<String>,
}

// ============================================================
// Shared accept_file helper for CLI-based platforms (macOS/Windows)
// ============================================================

/// Per-filename accept locks. `tailscale file get` drains the WHOLE daemon
/// inbox (it cannot fetch a single named file), so two concurrent accepts of
/// the same name could both claim the same content and destination. Locking
/// per name serializes that case; cross-name races are resolved safely by the
/// staging directory + exact-name fallback in `accept_file_with_getter`.
fn accept_lock(name: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, LazyLock, Mutex};
    static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut map = LOCKS.lock().unwrap_or_else(|p| p.into_inner());
    map.entry(name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Reserve a unique output path in `dir` with an exclusive create (`O_EXCL`),
/// retrying with the next unique suffix when the name is taken. This closes
/// the check-then-create race where `unique_save_path` picked a free name and
/// `File::create` then TRUNCATED whoever created it in between. The returned
/// file is empty; callers stream content into it and must remove it on failure.
fn reserve_unique_file(
    dir: &std::path::Path,
    name: &str,
) -> Result<(std::fs::File, std::path::PathBuf), String> {
    let mut candidate = dir.join(name);
    loop {
        match std::fs::File::create_new(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                candidate = unique_save_path(dir, name);
            }
            Err(e) => {
                return Err(format!(
                    "Failed to create file '{}': {}",
                    candidate.display(),
                    e
                ))
            }
        }
    }
}

/// Whether an I/O error means "rename across filesystems" (EXDEV on Unix,
/// ERROR_NOT_SAME_DEVICE on Windows) and needs the copy fallback.
#[cfg(unix)]
fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(18) // EXDEV
}

#[cfg(windows)]
fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
}

/// Move `src` into `dir` under `name`, NEVER overwriting an existing file.
/// The destination is first reserved with an exclusive create (so concurrent
/// movers get the next unique suffix), then `src` is renamed over our own
/// reservation — atomic on both Unix and Windows. Falls back to copy+delete
/// when the staging and save directories live on different filesystems.
/// Returns the path the content actually landed at.
fn move_file_into_dir(
    src: &std::path::Path,
    dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let (placeholder, dest) = reserve_unique_file(dir, name)?;
    // Windows cannot replace an open file — close our reservation first.
    // The name stays reserved on disk until the rename replaces it.
    drop(placeholder);
    match std::fs::rename(src, &dest) {
        Ok(()) => Ok(dest),
        Err(e) if is_cross_device(&e) => {
            let copy_result = (|| -> Result<(), String> {
                let mut input = std::fs::File::open(src)
                    .map_err(|e| format!("Failed to read '{}': {}", src.display(), e))?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&dest)
                    .map_err(|e| format!("Failed to open '{}': {}", dest.display(), e))?;
                std::io::copy(&mut input, &mut output)
                    .map_err(|e| format!("Failed to copy '{}': {}", src.display(), e))?;
                output
                    .sync_all()
                    .map_err(|e| format!("Failed to flush '{}': {}", dest.display(), e))?;
                Ok(())
            })();
            match copy_result {
                Ok(()) => {
                    let _ = std::fs::remove_file(src);
                    Ok(dest)
                }
                Err(err) => {
                    let _ = std::fs::remove_file(&dest);
                    Err(err)
                }
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(format!(
                "Failed to move '{}' into '{}': {}",
                name,
                dir.display(),
                e
            ))
        }
    }
}

/// Shared accept_file logic for CLI-based platforms.
///
/// `run_get` executes the platform-specific `tailscale file get` command with
/// the given target directory and must return only after the CLI finished.
/// Sanitizes `name` to prevent path traversal attacks.
///
/// The CLI cannot fetch a single named file — it drains every pending inbox
/// entry into the target directory. To keep that from ever touching the
/// user's save directory directly (where `--conflict` handling and half-
/// finished downloads could clobber existing files), the download runs into a
/// private staging directory first; every drained file is then moved into the
/// save dir with exclusive-create semantics, and the exact path the requested
/// file landed at is returned.
#[allow(dead_code)] // Only used on macOS/Windows
fn accept_file_with_getter(
    name: &str,
    save_dir: &str,
    run_get: impl FnOnce(&std::path::Path) -> Result<(), String>,
) -> Result<String, String> {
    // Sanitize filename to prevent path traversal
    let safe_name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid filename".to_string())?;

    let save_dir_path = std::path::Path::new(save_dir);
    std::fs::create_dir_all(save_dir_path).map_err(|e| {
        format!(
            "Cannot create save directory '{}': {}",
            save_dir_path.display(),
            e
        )
    })?;

    let lock = accept_lock(safe_name);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Private staging directory so the CLI's conflict renames ("name (1).ext")
    // and collateral downloads are detected by exact name instead of the old
    // "exactly one new entry" heuristic, and nothing in save_dir is touched
    // until each file is atomically moved in.
    let staging = std::env::temp_dir().join(format!("taildrop-accept-{:016x}", timestamp_tag()));
    std::fs::create_dir_all(&staging).map_err(|e| {
        format!(
            "Cannot create staging directory '{}': {}",
            staging.display(),
            e
        )
    })?;

    let result = (|| -> Result<String, String> {
        run_get(&staging)?;

        // Move every file the CLI drained out of staging into the save dir.
        // They were already removed from the daemon's inbox, so dropping them
        // here would lose data. Moves never overwrite existing files.
        let mut moved_names: Vec<String> = Vec::new();
        let mut requested_path: Option<std::path::PathBuf> = None;
        let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&staging)
            .map_err(|e| {
                format!(
                    "Cannot read staging directory '{}': {}",
                    staging.display(),
                    e
                )
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        for path in entries {
            if !path.is_file() {
                continue;
            }
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            match move_file_into_dir(&path, save_dir_path, &file_name) {
                Ok(dest) => {
                    if file_name == safe_name {
                        requested_path = Some(dest);
                    } else {
                        moved_names.push(file_name);
                    }
                }
                Err(e) => {
                    // Collateral files must not fail the accept; the requested
                    // one is surfaced below (it was never moved).
                    log::warn!(
                        "accept: failed to move '{}' into save dir: {}",
                        file_name,
                        e
                    );
                }
            }
        }
        if let Some(dest) = requested_path {
            return Ok(dest.to_string_lossy().to_string());
        }

        // The inbox didn't deliver the file this time. Either a concurrent
        // accept already moved it, or the auto-receive poll saved it before
        // the user clicked Accept. If it already sits in the save dir under
        // the exact name, return that path.
        let existing = save_dir_path.join(safe_name);
        if existing.is_file() {
            return Ok(existing.to_string_lossy().to_string());
        }

        let suffix = if moved_names.is_empty() {
            String::new()
        } else {
            format!(" (found instead: {})", moved_names.join(", "))
        };
        Err(format!(
            "tailscale file get succeeded but '{}' did not appear in {}{}",
            safe_name,
            save_dir_path.display(),
            suffix
        ))
    })();

    // Staging is empty by now (everything was moved); remove it best-effort.
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Shared CLI auto-receive logic for macOS/Windows. Runs
/// `tailscale file get --wait=false --verbose --conflict=rename <save_dir>`
/// (via the platform-specific `run_get` closure, which receives the full
/// argument vector), parses the "moved N/N files" output, and returns a JSON
/// array of the received files.
///
/// `--conflict=rename` is load-bearing: the CLI default must never be
/// `overwrite`, or a background poll would silently clobber same-named files
/// already in the user's save dir. With `rename`, a conflicting incoming file
/// is written as "name (1).ext" instead. (This is also the CLI's default
/// conflict policy — passed explicitly so it can't silently regress.)
///
/// `platform_label` is used in log messages ("macOS" / "Windows").
#[allow(dead_code)] // Only used on macOS/Windows
fn cli_receive_files(
    save_dir: &str,
    platform_label: &str,
    run_get: impl FnOnce(&[&str]) -> Result<std::process::Output, String>,
) -> Result<Vec<u8>, String> {
    // Ensure the save directory exists before running the CLI.
    if !std::path::Path::new(save_dir).exists() {
        if let Err(e) = std::fs::create_dir_all(save_dir) {
            log::warn!(
                "{} CLI auto-receive: failed to create save_dir '{}': {}",
                platform_label,
                save_dir,
                e
            );
            // Propagate: an unusable save dir must surface as an error in the
            // UI, not as a silent "no incoming files".
            return Err(format!(
                "Cannot create save directory '{}': {}",
                save_dir, e
            ));
        }
    }
    let args: Vec<&str> = vec![
        "file",
        "get",
        "--wait=false",
        "--verbose",
        "--conflict=rename",
        save_dir,
    ];
    let output = run_get(&args)?;
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

        // Timeout: 60s base + 60s per MB, capped at 600s. Saturating math so
        // the per-MB term can't overflow before the cap applies (the cap used
        // to bind only after the multiply).
        let timeout_secs = 60u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(540);
        let timeout = std::time::Duration::from_secs(timeout_secs);

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

    /// Async twin of the shared `reserve_unique_file`: reserves a unique
    /// output path with an exclusive create, retrying with the next unique
    /// suffix when the name is taken. Never truncates an existing file.
    async fn reserve_unique_file_async(
        dir: &std::path::Path,
        name: &str,
    ) -> Result<(tokio::fs::File, std::path::PathBuf), String> {
        let mut candidate = dir.join(name);
        loop {
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .await
            {
                Ok(file) => return Ok((file, candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    candidate = unique_save_path(dir, name);
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to create file '{}': {}",
                        candidate.display(),
                        e
                    ))
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
    ///
    /// `file` is the caller's exclusively-created destination (see
    /// `reserve_unique_file_async`); on failure the caller removes the partial.
    async fn stream_get_to_file(api_path: &str, file: &mut tokio::fs::File) -> Result<(), String> {
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
        match get_request("/localapi/v0/files/").await {
            Ok(data) => {
                log::debug!("Linux: incoming files listing OK ({} bytes)", data.len());
                Ok(data)
            }
            Err(e) => {
                log::debug!("Linux: incoming files listing failed: {}", e);
                Err(e)
            }
        }
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
            let dir_path = std::path::Path::new(&save_dir);
            tokio::fs::create_dir_all(dir_path).await.map_err(|e| {
                format!(
                    "Cannot create save directory '{}': {}",
                    dir_path.display(),
                    e
                )
            })?;
            let (mut file, save_path) = reserve_unique_file_async(dir_path, safe_name).await?;
            if let Err(e) = stream_get_to_file(&api_path, &mut file).await {
                // Remove the partial download so a half-written file doesn't
                // linger under the reserved name.
                let _ = tokio::fs::remove_file(&save_path).await;
                return Err(e);
            }

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

    /// Run the tailscale CLI by exec'ing the binary with an argument vector —
    /// no shell involved. `Command::args` passes each argument verbatim, so
    /// paths and peer names with spaces, quotes, or shell metacharacters need
    /// no escaping (arguments containing NUL bytes are rejected by the OS as
    /// `InvalidInput`). This matches the Windows implementation and removes
    /// the shell-injection surface the previous `/bin/sh -c` wrapper carried.
    /// (The child inherits the same environment either way — the shell
    /// intermediary added no launchd/XPC-relevant state.)
    fn tailscale_cmd(args: &[&str]) -> std::io::Result<std::process::Output> {
        let binary = find_tailscale().unwrap_or("tailscale");
        log::debug!("macOS exec: {} {:?}", binary, args);
        Command::new(binary).args(args).output()
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
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
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
        super::cli_receive_files(save_dir, "macOS", |args| {
            tailscale_cmd(args).map_err(|e| format!("Failed to run tailscale file get: {}", e))
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
    ///
    /// `file` is the caller's exclusively-reserved destination (see the
    /// shared `reserve_unique_file`); on failure the caller removes the
    /// partial file — including the empty reservation when falling back to
    /// the CLI, so the CLI path gets a clean shot at the original name.
    fn try_socket_get_to_file(
        path: &str,
        file: &mut std::fs::File,
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

        // Write any body bytes already sitting in the header buffer (the chunk
        // that contained the final \r\n\r\n often also carries the start of
        // the body) into the caller's reserved destination file.
        let body_start = header_end + 4;
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

        // Flush to disk before reporting success so the caller's path points
        // at durable content.
        file.sync_all()
            .map_err(|e| super::SocketGetError::Other(format!("sync file: {}", e)))?;

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
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s (saturating
        // math so the multiply can't overflow before the cap applies).
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = 120u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(480);
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
    /// files efficiently into an exclusively-reserved path), then falls back
    /// to the `tailscale file get` CLI — but **only** when the socket itself
    /// is unavailable (the App Store install case). HTTP-level errors from
    /// the daemon (404, 5xx, …) are propagated to the caller rather than
    /// masked by the CLI, which would otherwise download *every* pending
    /// file into `save_dir`. Uses the shared helpers with path traversal
    /// sanitization and staging-directory semantics for the CLI path.
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
                let dir_path = std::path::Path::new(&save_dir);
                std::fs::create_dir_all(dir_path).map_err(|e| {
                    format!(
                        "Cannot create save directory '{}': {}",
                        dir_path.display(),
                        e
                    )
                })?;
                let api_path = format!("/localapi/v0/files/{}", super::url_encode(&name));
                let (mut file, save_path) = super::reserve_unique_file(dir_path, safe_name)?;

                // Try the socket first (streams large files without buffering).
                match try_socket_get_to_file(&api_path, &mut file) {
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
                        // get` CLI. Remove our empty reservation first so the
                        // CLI path can claim the original name.
                        let _ = std::fs::remove_file(&save_path);
                        log::debug!(
                            "macOS: socket unavailable ({}), falling back to CLI",
                            socket_err
                        );
                        super::accept_file_with_getter(&name, &save_dir, |staging| {
                            // --wait=false: don't block if the inbox is empty
                            // (the file may have already been consumed by the
                            // auto-receive poll on macOS).
                            let output = tailscale_cmd(&[
                                "file",
                                "get",
                                "--wait=false",
                                &staging.to_string_lossy(),
                            ])
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
                        // error, …). Remove the partial download and propagate
                        // the real error instead of falling back to the CLI —
                        // otherwise a transient daemon error would cause
                        // `tailscale file get` to download every pending file
                        // into save_dir.
                        let _ = std::fs::remove_file(&save_path);
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
        super::cli_receive_files(save_dir, "Windows", |args| {
            tailscale_cmd()
                .args(args)
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
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s (saturating
        // math so the multiply can't overflow before the cap applies).
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = 120u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(480);
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
            // 401/403 means the pipe connected but the daemon rejected our
            // request due to permissions (non-admin process). The CLI bypasses
            // this because tailscale.exe has its own auth, so treat these as
            // Connect errors to trigger the CLI fallback.
            let err_type = if status_code == 401 || status_code == 403 {
                super::SocketGetError::Connect
            } else {
                super::SocketGetError::Other
            };
            return Err(err_type(format!("HTTP error: {}", status_line)));
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

    /// Accept an incoming file. Uses shared helper with path traversal
    /// sanitization and staging-directory semantics (downloads drain the
    /// whole inbox into a private staging dir, then move each file into the
    /// save dir without ever overwriting).
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || {
                super::accept_file_with_getter(&name, &save_dir, |staging| {
                    // --wait=false: don't block if the inbox is empty.
                    let output = tailscale_cmd()
                        .args(["file", "get", "--wait=false", &staging.to_string_lossy()])
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
    // The Tailscale daemon returns `null` (not `[]`) when no files are pending.
    // Handle that gracefully instead of failing the parse.
    let body_str = String::from_utf8_lossy(&body);
    if body_str.trim() == "null" || body_str.trim().is_empty() {
        return Ok(Vec::new());
    }
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    platform::accept_file(name, save_dir).await
}

// ============================================================
// Tests — crate-root, runs on all platforms
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- url_encode ---

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
        assert_eq!(url_encode("-_.~"), "-_.~");
    }

    #[test]
    fn url_encode_unicode() {
        assert_eq!(url_encode("é"), "%C3%A9");
    }

    // --- prettify_name ---

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
    fn prettify_more_abbreviations() {
        assert_eq!(prettify_name("macbook-pro-se"), "Macbook Pro SE");
        assert_eq!(prettify_name("server-tv"), "Server TV");
        assert_eq!(prettify_name("office-pc"), "Office PC");
        assert_eq!(prettify_name("dev-vm"), "Dev VM");
        assert_eq!(prettify_name("work-vpn"), "Work VPN");
        assert_eq!(prettify_name("mini-nuc"), "Mini NUC");
    }

    #[test]
    fn prettify_underscores() {
        assert_eq!(prettify_name("my_device"), "My Device");
    }

    #[test]
    fn prettify_digit_only_word() {
        assert_eq!(prettify_name("node-100"), "Node 100");
    }

    #[test]
    fn prettify_empty() {
        assert_eq!(prettify_name(""), "");
    }

    // --- raw_machine_name ---

    #[test]
    fn raw_machine_name_basic() {
        assert_eq!(
            raw_machine_name("pixel.tail1234.ts.net."),
            Some("pixel".to_string())
        );
    }

    #[test]
    fn raw_machine_name_no_dot() {
        assert_eq!(raw_machine_name("hostname"), Some("hostname".to_string()));
    }

    #[test]
    fn raw_machine_name_empty() {
        assert_eq!(raw_machine_name(""), None);
    }

    #[test]
    fn raw_machine_name_trailing_dot_only() {
        assert_eq!(raw_machine_name("."), None);
    }

    // --- display_name_from_dns ---

    #[test]
    fn display_name_from_dns_basic() {
        assert_eq!(
            display_name_from_dns("my-laptop.tail9999.ts.net."),
            Some("My Laptop".to_string())
        );
    }

    #[test]
    fn display_name_from_dns_none() {
        assert_eq!(display_name_from_dns(""), None);
    }

    // --- unique_save_path ---

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

    // --- timestamp_tag ---

    #[test]
    fn timestamp_tag_is_monotonic() {
        let a = timestamp_tag();
        let b = timestamp_tag();
        assert!(
            b >= a,
            "timestamp_tag should be monotonically non-decreasing"
        );
    }

    #[test]
    fn timestamp_tag_counter_increments() {
        // Rapid calls should produce different tags (counter increments).
        let a = timestamp_tag();
        let b = timestamp_tag();
        assert_ne!(a, b, "consecutive calls must produce different tags");
    }

    // --- accept_file_with_getter path traversal ---

    #[test]
    fn accept_file_rejects_path_traversal_dotdot() {
        // The function sanitizes name via Path::file_name(), so "../etc/passwd"
        // becomes just "passwd". We test that the function does NOT create a
        // file outside save_dir by checking it doesn't error on a safe name
        // but would error on a purely traversal-only name like "../".
        let dir = std::env::temp_dir().join("taildrop_test_traversal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // "../" has no file_name component → should error "Invalid filename"
        let result = accept_file_with_getter("../", dir.to_str().unwrap(), |_| Ok(()));
        assert!(result.is_err(), "path traversal should be rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_file_accepts_normal_filename() {
        let dir = std::env::temp_dir().join("taildrop_test_normal_name");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A normal filename with a no-op getter should find no file and error,
        // but the name itself should pass sanitization (not "Invalid filename").
        let result = accept_file_with_getter("photo.jpg", dir.to_str().unwrap(), |_| Ok(()));
        assert!(result.is_err(), "should fail because file doesn't appear");
        assert!(
            !result.unwrap_err().contains("Invalid filename"),
            "normal filename should pass sanitization"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- accept_file_with_getter P0 regression tests ---

    /// Helper: build a `run_get` fake that simulates `tailscale file get`
    /// delivering files into the (staging) directory it is handed.
    fn fake_cli_delivering(
        files: Vec<(&'static str, &'static str)>,
    ) -> impl FnOnce(&std::path::Path) -> Result<(), String> {
        move |staging: &std::path::Path| {
            for (name, content) in files {
                std::fs::write(staging.join(name), content).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }

    fn temp_test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("taildrop_test_{}", label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accept_returns_path_content_actually_landed_in() {
        let dir = temp_test_dir("actual_path");
        // A same-named file already exists — the move must not clobber it and
        // must return the path the NEW content landed in.
        std::fs::write(dir.join("report.pdf"), "old").unwrap();

        let result = accept_file_with_getter(
            "report.pdf",
            dir.to_str().unwrap(),
            fake_cli_delivering(vec![("report.pdf", "new")]),
        )
        .unwrap();

        let returned = std::path::PathBuf::from(&result);
        assert_eq!(
            returned,
            dir.join("report (1).pdf"),
            "new content must land in a distinct file"
        );
        assert_eq!(
            std::fs::read_to_string(returned).unwrap(),
            "new",
            "returned path must contain the new content"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("report.pdf")).unwrap(),
            "old",
            "pre-existing file must never be overwritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_concurrent_double_accept_yields_two_distinct_files() {
        let dir = temp_test_dir("double_accept");
        let dir_str = dir.to_str().unwrap().to_string();

        let (first, second) = std::thread::scope(|s| {
            let h1 = s.spawn(|| {
                accept_file_with_getter(
                    "report.pdf",
                    &dir_str,
                    fake_cli_delivering(vec![("report.pdf", "first")]),
                )
            });
            let h2 = s.spawn(|| {
                accept_file_with_getter(
                    "report.pdf",
                    &dir_str,
                    fake_cli_delivering(vec![("report.pdf", "second")]),
                )
            });
            (h1.join().unwrap(), h2.join().unwrap())
        });

        let p1 = std::path::PathBuf::from(first.expect("first accept should succeed"));
        let p2 = std::path::PathBuf::from(second.expect("second accept should succeed"));
        assert_ne!(p1, p2, "double accept must yield two distinct files");
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_moves_collateral_files_without_losing_them() {
        let dir = temp_test_dir("collateral");
        // The CLI drains the whole inbox: two pending files arrive even though
        // only one was accepted. Both must end up in the save dir (the old
        // single-new-entry heuristic lost track with 2+ pending files).
        let result = accept_file_with_getter(
            "wanted.txt",
            dir.to_str().unwrap(),
            fake_cli_delivering(vec![("wanted.txt", "wanted"), ("other.txt", "other")]),
        )
        .unwrap();

        assert_eq!(
            std::path::PathBuf::from(&result),
            dir.join("wanted.txt"),
            "must return the path of the requested file"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "other"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_falls_back_to_existing_file_when_inbox_empty() {
        let dir = temp_test_dir("already_received");
        // Auto-receive poll already saved the file; clicking Accept drains an
        // empty inbox. The existing exact-name file is returned.
        std::fs::write(dir.join("note.txt"), "already here").unwrap();
        let result =
            accept_file_with_getter("note.txt", dir.to_str().unwrap(), |_| Ok(())).unwrap();
        assert_eq!(std::path::PathBuf::from(&result), dir.join("note.txt"));
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "already here"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_reports_failure_when_file_never_appears() {
        let dir = temp_test_dir("never_appeared");
        let err =
            accept_file_with_getter("ghost.txt", dir.to_str().unwrap(), |_| Ok(())).unwrap_err();
        assert!(
            err.contains("ghost.txt"),
            "error should name the missing file: {}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_creates_missing_save_dir() {
        let root = temp_test_dir("create_save_dir");
        let save = root.join("nested/save");
        let result = accept_file_with_getter(
            "file.txt",
            save.to_str().unwrap(),
            fake_cli_delivering(vec![("file.txt", "hi")]),
        );
        assert!(
            result.is_ok(),
            "missing save dir should be created: {:?}",
            result
        );
        assert_eq!(
            std::fs::read_to_string(save.join("file.txt")).unwrap(),
            "hi"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- move_file_into_dir / reserve_unique_file ---

    #[test]
    fn move_file_into_dir_never_overwrites() {
        let dir = temp_test_dir("move_no_overwrite");
        std::fs::write(dir.join("a.txt"), "original").unwrap();
        let src = dir.join("src.txt");
        std::fs::write(&src, "incoming").unwrap();

        let dest = move_file_into_dir(&src, &dir, "a.txt").unwrap();
        assert_eq!(dest, dir.join("a (1).txt"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "original"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "incoming");
        assert!(!src.exists(), "source must be consumed by the move");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reserve_unique_file_does_not_truncate() {
        let dir = temp_test_dir("reserve_no_truncate");
        std::fs::write(dir.join("data.bin"), "payload").unwrap();
        let (file, path) = reserve_unique_file(&dir, "data.bin").unwrap();
        assert_eq!(path, dir.join("data (1).bin"), "must take the next suffix");
        drop(file);
        assert_eq!(
            std::fs::read_to_string(dir.join("data.bin")).unwrap(),
            "payload",
            "existing file must be untouched"
        );
        assert!(path.exists(), "reserved file must exist");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- cli_receive_files (P0: no --conflict=overwrite) ---

    /// A successful exit status, built cross-platform via the OS-specific
    /// `ExitStatusExt` (there is no portable constructor).
    fn success_status() -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
    }

    /// Fake CLI output: stdout is what `tailscale file get --verbose` prints.
    fn fake_cli_output(moved_line: &str) -> std::process::Output {
        std::process::Output {
            status: success_status(),
            stdout: format!("{}\n", moved_line).into_bytes(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn cli_receive_files_uses_rename_conflict_policy() {
        let dir = temp_test_dir("cli_rename_policy");
        let captured_args: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            *captured_args.lock().unwrap() = args.iter().map(|a| a.to_string()).collect();
            Ok(fake_cli_output("moved 0/0 files"))
        })
        .unwrap();
        assert_eq!(result, b"[]");

        let args = captured_args.into_inner().unwrap();
        assert!(
            !args.contains(&"--conflict=overwrite".to_string()),
            "poll must never overwrite: args = {:?}",
            args
        );
        assert!(
            args.contains(&"--conflict=rename".to_string()),
            "rename policy must be explicit: args = {:?}",
            args
        );
        assert!(args.contains(&dir.to_str().unwrap().to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_receive_files_propagates_save_dir_error() {
        // Make save_dir creation impossible: a regular file exists where the
        // directory should be created under it.
        let root = temp_test_dir("cli_save_dir_error");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, "not a dir").unwrap();
        let bad_dir = blocker.join("sub");

        let result = cli_receive_files(bad_dir.to_str().unwrap(), "test", |_| {
            Ok(fake_cli_output("moved 0/0 files"))
        });
        assert!(
            result.is_err(),
            "unusable save dir must surface an error, not an empty list"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cli_receive_files_reports_received_files() {
        let dir = temp_test_dir("cli_reports_files");
        // Simulate the CLI having written one file ("moved 1/1") and list it.
        std::fs::write(dir.join("got.txt"), "x").unwrap();
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            // The last arg is the save dir; pretend the CLI saved got.txt.
            assert_eq!(args.last().copied(), Some(dir.to_str().unwrap()));
            Ok(fake_cli_output("moved 1/1 files"))
        })
        .unwrap();
        let text = String::from_utf8(result).unwrap();
        assert!(
            text.contains("got.txt"),
            "should list received file: {}",
            text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
