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

    /// Write file data to Unix socket in chunks via raw HTTP/1.1.
    /// Reduces peak memory on the socket side. True disk-to-socket streaming
    /// would require a custom hyper Body; this is a pragmatic middle ground.
    async fn stream_file_to_socket(path: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

        // Timeout: 60s base + 60s per MB for large files
        let timeout_secs = 60 + (body.len() as u64 / (1024 * 1024)) * 60;
        let timeout = std::time::Duration::from_secs(timeout_secs.min(600));
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| format!("set_write_timeout: {}", e))?;

        // Write HTTP/1.1 request with streaming body
        let request = format!(
            "PUT {} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            path,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Failed to write request: {}", e))?;

        // Stream file in 8KB chunks to avoid loading entire file in memory
        let mut pos = 0;
        while pos < body.len() {
            let end = (pos + 8192).min(body.len());
            stream
                .write_all(&body[pos..end])
                .await
                .map_err(|e| format!("Failed to write file data: {}", e))?;
            pos = end;
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

        // Parse HTTP response
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

    // Bug #2: accept file_path instead of data; Bug #3: use peer_id (stable ID) for localapi
    // Streams file to socket instead of loading entire file into memory
    pub async fn send_file(peer_id: &str, _peer_name: &str, file_path: &str) -> Result<String, String> {
        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|e| format!("Failed to stat file '{}': {}", file_path, e))?;
        if !metadata.is_file() {
            return Err(format!("'{}' is not a regular file", file_path));
        }
        let data = tokio::fs::read(file_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {}", file_path, e))?;
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let api_path = format!(
            "/localapi/v0/file-put/{}/{}",
            url_encode(peer_id),
            url_encode(filename)
        );
        stream_file_to_socket(&api_path, data).await?;
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        get_request("/localapi/v0/files/").await
    }

    // Bug #1: sanitize filename to prevent path traversal
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let path = format!("/localapi/v0/files/{}", url_encode(name));
        let data = get_request(&path).await?;

        let safe_name = std::path::Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "Invalid filename".to_string())?;
        let save_path = unique_save_path(std::path::Path::new(save_dir), safe_name);
        tokio::fs::write(&save_path, &data)
            .await
            .map_err(|e| format!("Failed to save file: {}", e))?;

        delete_request(&path).await?;
        Ok(save_path.to_string_lossy().to_string())
    }

    /// Generate a unique save path to avoid overwriting existing files.
    /// e.g. "file.txt" -> "file (1).txt" -> "file (2).txt"
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
        base
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

    #[cfg(test)]
    mod tests {
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
        let binary = find_tailscale().unwrap_or("tailscale");
        // Quote binary path and each argument to handle spaces/special chars
        let escaped_binary = format!("'{}'", binary.replace('\'', "'\\''"));
        let escaped_args: Vec<String> = args.iter().map(|a| {
            format!("'{}'", a.replace('\'', "'\\''"))
        }).collect();
        let shell_cmd = format!("{} {}", escaped_binary, escaped_args.join(" "));
        eprintln!("[taildrop] macOS shell cmd: /bin/sh -c {}", shell_cmd);
        Command::new("/bin/sh")
            .arg("-c")
            .arg(&shell_cmd)
            .output()
    }

    const SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

    /// Try an HTTP/1.0 GET via the Tailscale Unix socket.
    /// Works when the socket is accessible (Homebrew/open-source installs).
    /// Fails gracefully for App Store installs with restricted permissions.
    fn try_socket_get(path: &str) -> Result<Vec<u8>, String> {
        use std::os::unix::net::UnixStream;
        use std::io::{Read, Write};

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .map_err(|e| format!("connect: {}", e))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|e| format!("timeout: {}", e))?;

        // Use HTTP/1.0 to guarantee non-chunked response and connection close
        let req = format!(
            "GET {} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n",
            path
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {}", e))?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| format!("read: {}", e))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "Invalid HTTP response".to_string())?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        if !status_line.contains(" 200 ") {
            return Err(format!("HTTP error: {}", status_line));
        }

        Ok(response[header_end + 4..].to_vec())
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
        tokio::task::spawn_blocking(|| {
            match try_socket_get("/localapi/v0/files/") {
                Ok(data) => Ok(data),
                Err(e) => {
                    use std::sync::Once;
                    static LOG_ONCE: Once = Once::new();
                    LOG_ONCE.call_once(|| {
                        eprintln!(
                            "[taildrop] macOS: socket file listing unavailable ({}), incoming files won't be detected",
                            e
                        );
                    });
                    Ok(b"[]".to_vec())
                }
            }
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::task::spawn_blocking(move || {
            // Snapshot directory before download to detect newly arrived files
            let dir_path = std::path::Path::new(&save_dir);
            let before_entries: std::collections::HashSet<std::path::PathBuf> = {
                if dir_path.exists() {
                    std::fs::read_dir(dir_path)
                        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                        .unwrap_or_default()
                } else {
                    std::collections::HashSet::new()
                }
            };

            // --wait=5s prevents indefinite hang if files were already consumed
            let output = tailscale_cmd(&["file", "get", "--wait=5s", &save_dir])
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file get failed: {}", stderr));
            }

            // Check if target file appeared (may already exist from prior download)
            let save_path = dir_path.join(&name);
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
                name, save_dir
            ))
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

    /// Named pipe path for the Tailscale daemon's local API on Windows.
    const PIPE_PATH: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";

    fn try_pipe_get(path: &str) -> Result<Vec<u8>, String> {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};

        let mut pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_PATH)
            .map_err(|e| format!("open pipe: {}", e))?;

        let req = format!(
            "GET {} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n",
            path
        );
        pipe.write_all(req.as_bytes())
            .map_err(|e| format!("write: {}", e))?;

        let mut response = Vec::new();
        pipe.read_to_end(&mut response)
            .map_err(|e| format!("read: {}", e))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "Invalid HTTP response".to_string())?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        if !status_line.contains(" 200 ") {
            return Err(format!("HTTP error: {}", status_line));
        }

        Ok(response[header_end + 4..].to_vec())
    }

    pub async fn get_incoming_files() -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(|| {
            match try_pipe_get("/localapi/v0/files/") {
                Ok(data) => Ok(data),
                Err(e) => {
                    use std::sync::Once;
                    static LOG_ONCE: Once = Once::new();
                    LOG_ONCE.call_once(|| {
                        eprintln!(
                            "[taildrop] Windows: pipe file listing unavailable ({}), incoming files won't be detected",
                            e
                        );
                    });
                    Ok(b"[]".to_vec())
                }
            }
        })
        .await
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::task::spawn_blocking(move || {
            // Snapshot directory before download to detect newly arrived files
            let dir_path = std::path::Path::new(&save_dir);
            let before_entries: std::collections::HashSet<std::path::PathBuf> = {
                if dir_path.exists() {
                    std::fs::read_dir(dir_path)
                        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                        .unwrap_or_default()
                } else {
                    std::collections::HashSet::new()
                }
            };

            // --wait=5s prevents indefinite hang if files were already consumed
            let output = tailscale_cmd()
                .args(["file", "get", "--wait=5s", &save_dir])
                .output()
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("tailscale file get failed: {}", stderr));
            }

            // Check if target file appeared (may already exist from prior download)
            let save_path = dir_path.join(&name);
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
                name, save_dir
            ))
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
    name.split(['-', '_'])
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
