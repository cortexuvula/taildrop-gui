//! russh + russh-sftp client — the M0-proven transport (GO decision).
//! Patterns ported from the M0 spike (`m0-spike/src/main.rs`): raw VERSION
//! extension capture on a dedicated channel, banner-based auth refusals,
//! per-flavor host-key pinning, bounded connect/auth/op timeouts.
//!
//! Auth is key-file + ssh-agent ONLY. There is no password and no
//! keyboard-interactive code path in this module, so an auth prompt can
//! never hang the UI (plugin BatchMode equivalent, invariant §4.6/§6.5).
//!
//! Shell-out fallback: NOT implemented in M1 (M0 verdict). The
//! `transport::SftpTransport` trait is the documented seam it would slot
//! into, with the exact non-interactive ssh options recorded in m0-report
//! §7 if it is ever needed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, Disconnect};
use russh_sftp::client::error::Error as SftpClientError;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::browser::transport::{
    classify_message, RemoteNode, RemoteReader, RemoteWriter, SftpTransport,
};
use crate::browser::trust::TrustStore;
use crate::browser::{BrowserError, ErrorKind, ServerFlavor};

/// Connect timeout for the SSH transport handshake (spike: 10 s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Overall budget for the (multi-method, non-interactive) auth phase.
const AUTH_TIMEOUT: Duration = Duration::from_secs(25);
/// Per-operation SFTP timeout once the session is up.
const OP_TIMEOUT_SECS: u64 = 30;
/// Raw VERSION exchange timeout (spike: 8 s).
const RAW_TIMEOUT_SECS: u64 = 10;
/// Banner sniff read timeout.
const BANNER_TIMEOUT: Duration = Duration::from_secs(8);
/// Banner lines are ≤255 chars per RFC 4253; refuse longer ones.
const BANNER_MAX: usize = 256;

/// Everything needed to open one transport. One connection per session —
/// no multiplexing, so the Windows sshd shared-session stall class cannot
/// occur (report §6.6.1).
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub flavor: ServerFlavor,
    pub trust: Arc<TrustStore>,
    /// `tailscale status` sshHostKeys for this peer (base64 key blobs).
    /// When non-empty and the flavor is tailscale-ssh, the negotiated key
    /// must match one of them (M0 cross-check) — fail closed on mismatch.
    pub tailscale_host_keys: Vec<String>,
    /// Private key files to try (unencrypted only). Empty = standard names
    /// under ~/.ssh (id_ed25519, id_ecdsa, id_rsa).
    pub key_files: Vec<PathBuf>,
}

/// Read the server's SSH identification string over a bare TCP connection
/// and classify the flavor. M0: capability hints cannot classify the
/// serving endpoint (this Mac advertises cap/ssh but serves OpenSSH), and
/// the trust store is per flavor — so the banner decides which pin file
/// applies. The sniff connection is discarded; russh then makes its own
/// connection, where the pinned host key (not the banner) is the trust
/// anchor. The residual sniff-vs-connect window is inside one tailnet hop
/// to the same pinned IP/name.
pub async fn sniff_banner(host: &str, port: u16) -> Result<(ServerFlavor, String), BrowserError> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| {
            BrowserError::new(
                ErrorKind::Unreachable,
                format!("unreachable: {host}:{port} did not answer (TCP timeout)"),
            )
        })?
        .map_err(|e| {
            BrowserError::new(
                ErrorKind::Unreachable,
                format!("unreachable: {host}:{port}: {e}"),
            )
        })?;
    let (mut rd, _) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let read = async {
        loop {
            let n = rd.read(&mut byte).await?;
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            if buf.len() > BANNER_MAX {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::time::timeout(BANNER_TIMEOUT, read)
        .await
        .map_err(|_| {
            BrowserError::new(
                ErrorKind::Unreachable,
                format!("unreachable: {host}:{port} sent no SSH banner"),
            )
        })?
        .map_err(|e| {
            BrowserError::new(
                ErrorKind::Unreachable,
                format!("unreachable: banner read from {host}:{port} failed: {e}"),
            )
        })?;
    let banner = String::from_utf8_lossy(&buf).trim_end().to_string();
    let flavor = ServerFlavor::from_banner(&banner).ok_or_else(|| {
        BrowserError::new(
            ErrorKind::Unsupported,
            format!("{host}:{port} is not an SSH server (banner: {banner:?})"),
        )
    })?;
    Ok((flavor, banner))
}

/// The account plain `ssh HOST` would use (~/.ssh/config, else the local
/// user). Port of ssh_config_user (bin/tailfiles:130-141): read-only,
/// non-interactive, bounded. Falls back to $USER / $USERNAME.
pub fn ssh_config_user(host: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .args(["-G", host])
        .stdin(std::process::Stdio::null())
        .output();
    if let Ok(proc) = out {
        let text = String::from_utf8_lossy(&proc.stdout);
        for line in text.lines() {
            if let Some(user) = line.strip_prefix("user ") {
                if !user.is_empty() {
                    return Some(user.to_string());
                }
            }
        }
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.is_empty())
}

fn default_key_files() -> Vec<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .filter(|p| p.exists())
        .collect()
}

/// russh client callbacks: host-key pinning (per flavor) + banner-based
/// auth refusals. Typed failures are recorded in `failure` and surfaced
/// even though russh reports the abort as a generic connection error.
struct ClientHandler {
    host: String,
    flavor: ServerFlavor,
    trust: Arc<TrustStore>,
    tailscale_host_keys: Vec<String>,
    failure: Arc<Mutex<Option<String>>>,
}

impl ClientHandler {
    fn record(&self, message: String) {
        if let Ok(mut slot) = self.failure.lock() {
            if slot.is_none() {
                *slot = Some(message);
            }
        }
    }
}

impl client::Handler for ClientHandler {
    type Error = BrowserError;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Certificate host keys are refused (spike behavior; no fleet host
        // presented one in M0).
        if key.certificate().is_some() {
            self.record("hostkey: certificate host keys are not supported".into());
            return Ok(false);
        }
        let pk = key.public_key();
        let openssh = pk.to_openssh().map_err(|e| {
            self.record(format!("hostkey: cannot encode offered key: {e}"));
            BrowserError::new(ErrorKind::Hostkey, format!("hostkey: {e}"))
        })?;
        let openssh = openssh.trim();
        // M0 cross-check: for Tailscale SSH, the negotiated key must appear
        // in `tailscale status` sshHostKeys (base64 blobs). The tailnet
        // already authenticated this machine; a key outside the published
        // set means something is between us and the peer.
        if self.flavor == ServerFlavor::TailscaleSsh && !self.tailscale_host_keys.is_empty() {
            let blob = openssh.split_whitespace().nth(1).unwrap_or("");
            if !blob.is_empty() && !self.tailscale_host_keys.iter().any(|h| h == blob) {
                self.record(
                    "hostkey: offered key does not match any tailscale status sshHostKeys entry"
                        .into(),
                );
                return Ok(false);
            }
        }
        match self
            .trust
            .check_and_record(self.flavor, &self.host, openssh)
        {
            Ok(is_new) => {
                log::info!(
                    "browser trust: {} {} policy={}",
                    self.flavor.as_str(),
                    self.host,
                    if is_new { "accept-new" } else { "pinned-match" }
                );
                Ok(true)
            }
            Err(e) => {
                self.record(format!("hostkey: {}", e.message));
                Ok(false)
            }
        }
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        log::debug!("browser auth-banner ({}): {:?}", self.host, banner);
        // Port of the spike: recognize Tailscale check-mode / policy banners
        // and refuse them typed instead of hanging or opening a browser.
        let refusal = if banner.contains("https://login.tailscale.com/")
            || banner.contains("check mode")
        {
            Some(format!("auth: interactive Tailscale check required — open {banner:?} in a browser once, then retry"))
        } else if banner.contains("tailnet policy does not permit") {
            Some(format!("auth: {banner}"))
        } else {
            None
        };
        if let Some(message) = refusal {
            self.record(message);
            return Err(BrowserError::new(
                ErrorKind::Auth,
                "auth: refused by server banner",
            ));
        }
        Ok(())
    }
}

// Glue note: russh implements `Signer` for `AgentClient` directly (with
// `AgentAuthError`), so agent identities are passed to
// `authenticate_publickey_with` without a custom adapter.

/// Try every ssh-agent identity (certificates skipped in M1). Ok(None) when
/// no agent / no identities; Ok(Some(method)) on success.
async fn agent_auth(
    ssh: &mut client::Handle<ClientHandler>,
    user: &str,
) -> Result<Option<String>, russh::Error> {
    let mut agent = match AgentClient::connect_env().await {
        Ok(a) => a.dynamic(),
        Err(_) => return Ok(None), // no SSH_AUTH_SOCK / no agent
    };
    // OpenSSH agents want session-bind before exposing destination-
    // constrained identities; best effort (plain agents work without it).
    if let Some(binding) = ssh.agent_session_binding() {
        let _ = agent.bind_session(binding, false).await;
    }
    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(_) => return Ok(None),
    };
    if identities.is_empty() {
        return Ok(None);
    }
    for id in &identities {
        let pubkey = match id {
            AgentIdentity::PublicKey { key, .. } => key.clone(),
            // Certificate client-auth: not in M1 (nothing in the fleet
            // presented one; keeps the trust story simple).
            AgentIdentity::Certificate { .. } => continue,
        };
        let hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
        match ssh
            .authenticate_publickey_with(user, pubkey, hash, &mut agent)
            .await
        {
            Ok(r) if r.success() => return Ok(Some("publickey-agent".to_string())),
            Ok(_) => continue,
            Err(e) => {
                // Per-identity failures fall through; a dead connection
                // surfaces in the final typed auth error.
                log::debug!("browser auth: agent identity rejected: {e}");
                continue;
            }
        }
    }
    Ok(None)
}

/// The live russh transport: one SSH connection, one SFTP session, plus the
/// raw VERSION extension map captured from the wire (M0: the high-level
/// session exposes only selected features; the raw map retains unknown
/// extension names too).
pub struct RusshTransport {
    ssh: client::Handle<ClientHandler>,
    sftp: SftpSession,
    extensions: BTreeMap<String, String>,
    flavor: ServerFlavor,
    user: String,
    /// Diagnostics surface: which auth method succeeded (probe detail, M2).
    #[allow(dead_code)]
    auth_method: String,
}

impl RusshTransport {
    pub fn flavor(&self) -> ServerFlavor {
        self.flavor
    }
    pub fn user(&self) -> &str {
        &self.user
    }
    #[allow(dead_code)]
    pub fn auth_method(&self) -> &str {
        &self.auth_method
    }
}

fn typed_or_recorded<E: std::fmt::Display>(
    failure: &Arc<Mutex<Option<String>>>,
    e: E,
    context: &str,
) -> BrowserError {
    if let Ok(slot) = failure.lock() {
        if let Some(message) = slot.as_ref() {
            let kind = classify_message(message);
            return BrowserError {
                message: message.clone(),
                kind,
                names: None,
            };
        }
    }
    let message = format!("{context}: {e}");
    let kind = classify_message(&message);
    BrowserError {
        message,
        kind,
        names: None,
    }
}

/// Open an authenticated SFTP transport. Never prompts: tailscale-ssh tries
/// `none` (tailnet identity) first, then unencrypted key files, then
/// ssh-agent identities. Password / keyboard-interactive are never attempted.
pub async fn connect(params: ConnectParams) -> Result<RusshTransport, BrowserError> {
    let ConnectParams {
        host,
        port,
        user,
        flavor,
        trust,
        tailscale_host_keys,
        key_files,
    } = params;
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cfg = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(20)),
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });
    let handler = ClientHandler {
        host: host.clone(),
        flavor,
        trust,
        tailscale_host_keys,
        failure: failure.clone(),
    };
    let mut ssh = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect(cfg, (host.as_str(), port), handler),
    )
    .await
    .map_err(|_| {
        BrowserError::new(
            ErrorKind::Unreachable,
            format!("unreachable: SSH connect to {host}:{port} timed out"),
        )
    })?
    .map_err(|e| typed_or_recorded(&failure, e, "unreachable: SSH connect"))?;

    let key_files = if key_files.is_empty() {
        default_key_files()
    } else {
        key_files
    };

    // Auth phase, bounded overall. Each method is non-interactive; failures
    // fall through to the next method, and the recorded banner refusal (if
    // any) wins the final error message.
    let auth_method = tokio::time::timeout(AUTH_TIMEOUT, async {
        // 1. Tailscale SSH: tailnet identity via `none` (M0-proven).
        if flavor == ServerFlavor::TailscaleSsh {
            if let Ok(result) = ssh.authenticate_none(user.clone()).await {
                if result.success() {
                    return Ok::<String, BrowserError>("tailnet-identity/none".to_string());
                }
            }
        }
        // 2. Unencrypted key files. An encrypted file fails to load locally
        //    (no passphrase prompt exists by design) — skip to the agent.
        let mut loaded_any = false;
        for path in &key_files {
            let private = match keys::load_secret_key(path, None) {
                Ok(k) => k,
                Err(_) => continue, // encrypted or unreadable — agent's turn
            };
            loaded_any = true;
            let hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
            let pk = PrivateKeyWithHashAlg::new(Arc::new(private), hash);
            match ssh.authenticate_publickey(user.clone(), pk).await {
                Ok(r) if r.success() => {
                    return Ok::<String, BrowserError>(format!(
                        "publickey-file:{}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ))
                }
                Ok(_) => continue,
                Err(e) => {
                    return Err(typed_or_recorded(
                        &failure,
                        e,
                        "auth: publickey attempt failed",
                    ))
                }
            }
        }
        // 3. ssh-agent identities (M0: not exercised — the agent was empty —
        //    but the key-file path proves the flow; agent signing is best
        //    effort and never blocks the key-file result).
        match agent_auth(&mut ssh, &user).await {
            Ok(Some(method)) => return Ok(method),
            Ok(None) => {}
            Err(e) => return Err(typed_or_recorded(&failure, e, "auth: agent attempt failed")),
        }
        let _ = loaded_any;
        Err(BrowserError::new(
            ErrorKind::Auth,
            format!(
                "auth: no working credential for {user}@{host} — tried tailnet identity, \
                 key files (id_ed25519/id_ecdsa/id_rsa) and ssh-agent. Password auth is \
                 never attempted; an encrypted key file needs the ssh-agent."
            ),
        ))
    })
    .await
    .map_err(|_| {
        BrowserError::new(
            ErrorKind::Auth,
            "auth: timed out (possible Tailscale check-mode; see banner in debug log)",
        )
    })??;

    // A banner refusal recorded during auth always wins (spike behavior):
    // russh may report success:false or a channel error either way.
    if let Ok(slot) = failure.lock() {
        if let Some(message) = slot.as_ref() {
            if message.starts_with("auth") || message.starts_with("hostkey") {
                let kind = classify_message(message);
                return Err(BrowserError {
                    message: message.clone(),
                    kind,
                    names: None,
                });
            }
        }
    }

    // Raw channel first: retain the complete VERSION extension map, not just
    // the features the high-level session knows (M0 pattern).
    let channel = ssh
        .channel_open_session()
        .await
        .map_err(|e| typed_or_recorded(&failure, e, "denied: channel open"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|_| subsystem_refused())?;
    let raw = RawSftpSession::new(channel.into_stream());
    raw.set_timeout(RAW_TIMEOUT_SECS);
    let version = raw.init().await.map_err(|_| subsystem_refused())?;
    let mut extensions = BTreeMap::new();
    for (k, v) in version.extensions {
        extensions.insert(k, v);
    }
    let _ = raw.close_session();

    let channel = ssh
        .channel_open_session()
        .await
        .map_err(|e| typed_or_recorded(&failure, e, "denied: channel open"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|_| subsystem_refused())?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|_| subsystem_refused())?;
    sftp.set_timeout(OP_TIMEOUT_SECS);

    log::info!(
        "browser session: {user}@{host} flavor={} auth={auth_method} extensions={:?}",
        flavor.as_str(),
        extensions.keys().collect::<Vec<_>>()
    );

    Ok(RusshTransport {
        ssh,
        sftp,
        extensions,
        flavor,
        user,
        auth_method,
    })
}

fn subsystem_refused() -> BrowserError {
    BrowserError::new(
        ErrorKind::Denied,
        "denied: the server refused the SFTP subsystem — a shell-only SSH grant does not \
         include SFTP; check the tailnet policy grant (\"sftp\": {}) or sshd_config",
    )
}

/// Map a russh-sftp client error into the typed taxonomy.
fn sftp_err(op: &str, e: SftpClientError) -> BrowserError {
    match &e {
        SftpClientError::Status(status) => {
            // error_message is a plain String in russh-sftp 3.0.1; fall back
            // to the status-code text when the server sent an empty one.
            let msg = if status.error_message.is_empty() {
                status.status_code.to_string()
            } else {
                status.error_message.clone()
            };
            let kind = match status.status_code {
                StatusCode::NoSuchFile => ErrorKind::Notfound,
                StatusCode::PermissionDenied => ErrorKind::Denied,
                StatusCode::OpUnsupported => ErrorKind::Unsupported,
                StatusCode::Failure => {
                    let lower = msg.to_lowercase();
                    if lower.contains("exist") {
                        ErrorKind::Exists
                    } else {
                        ErrorKind::Other
                    }
                }
                StatusCode::NoConnection | StatusCode::ConnectionLost => ErrorKind::Unreachable,
                _ => classify_message(&msg),
            };
            BrowserError::new(kind, format!("{op}: {msg}"))
        }
        SftpClientError::Timeout => BrowserError::new(
            ErrorKind::Unreachable,
            format!("{op}: timed out waiting for the server"),
        ),
        other => {
            let msg = other.to_string();
            BrowserError::new(classify_message(&msg), format!("{op}: {msg}"))
        }
    }
}

impl From<russh::Error> for BrowserError {
    fn from(e: russh::Error) -> Self {
        let msg = e.to_string();
        BrowserError::new(classify_message(&msg), msg)
    }
}

fn node_from(name: String, attrs: &FileAttributes) -> RemoteNode {
    let ft = attrs.file_type();
    RemoteNode {
        name,
        is_dir: ft == FileType::Dir,
        is_symlink: ft == FileType::Symlink,
        is_regular: ft == FileType::File,
        // Servers that omit permissions leave the type unknown; callers
        // treat unknown nodes cautiously (never recurse-delete them).
        is_unknown: ft == FileType::Other || attrs.permissions.is_none(),
        size: attrs.size.unwrap_or(0),
        mtime: attrs.mtime.unwrap_or(0) as i64,
        perm: attrs.permissions.unwrap_or(0),
    }
}

fn missing_if_not_found(op: &str, e: SftpClientError) -> Result<Option<RemoteNode>, BrowserError> {
    match &e {
        SftpClientError::Status(s) if s.status_code == StatusCode::NoSuchFile => Ok(None),
        _ => Err(sftp_err(op, e)),
    }
}

struct RusshReader {
    file: russh_sftp::client::fs::File,
}

#[async_trait]
impl RemoteReader for RusshReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
        AsyncReadExt::read(&mut self.file, buf)
            .await
            .map_err(|e| BrowserError::new(classify_message(&e.to_string()), format!("read: {e}")))
    }
    async fn close(self: Box<Self>) -> Result<(), BrowserError> {
        self.file
            .close()
            .await
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("close: {e}")))
    }
}

struct RusshWriter {
    file: russh_sftp::client::fs::File,
}

#[async_trait]
impl RemoteWriter for RusshWriter {
    async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        AsyncWriteExt::write_all(&mut self.file, data)
            .await
            .map_err(|e| BrowserError::new(classify_message(&e.to_string()), format!("write: {e}")))
    }
    async fn flush(&mut self) -> Result<(), BrowserError> {
        // The ACK barrier: flush drains pending writes so reported progress
        // reflects server-acknowledged bytes (M0 upload pattern).
        AsyncWriteExt::flush(&mut self.file)
            .await
            .map_err(|e| BrowserError::new(classify_message(&e.to_string()), format!("flush: {e}")))
    }
    async fn close(self: Box<Self>) -> Result<(), BrowserError> {
        self.file
            .close()
            .await
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("close: {e}")))
    }
}

#[async_trait]
impl SftpTransport for RusshTransport {
    async fn realpath(&self, path: &str) -> Result<String, BrowserError> {
        self.sftp
            .canonicalize(path)
            .await
            .map_err(|e| sftp_err("realpath", e))
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<RemoteNode>, BrowserError> {
        let entries: Vec<russh_sftp::client::fs::DirEntry> = self
            .sftp
            .read_dir(path)
            .await
            .map_err(|e| sftp_err("list", e))?
            .collect();
        let mut out = Vec::with_capacity(entries.len().min(crate::browser::MAX_LISTING_ENTRIES));
        for e in entries {
            let m = e.metadata();
            out.push(node_from(e.file_name(), &m));
        }
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
        match self.sftp.metadata(path).await {
            Ok(m) => Ok(Some(node_from(String::new(), &m))),
            Err(e) => missing_if_not_found("stat", e),
        }
    }

    async fn lstat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
        match self.sftp.symlink_metadata(path).await {
            Ok(m) => Ok(Some(node_from(String::new(), &m))),
            Err(e) => missing_if_not_found("lstat", e),
        }
    }

    async fn mkdir(&self, path: &str, perm: u32) -> Result<(), BrowserError> {
        self.sftp
            .create_dir(path)
            .await
            .map_err(|e| sftp_err("mkdir", e))?;
        // Best-effort mode application (SETSTAT is standard SFTP; servers
        // that ignore it still leave a usable directory).
        let _ = self
            .sftp
            .set_metadata(
                path,
                FileAttributes {
                    permissions: Some(perm),
                    ..Default::default()
                },
            )
            .await;
        Ok(())
    }

    async fn remove_file(&self, path: &str) -> Result<(), BrowserError> {
        self.sftp
            .remove_file(path)
            .await
            .map_err(|e| sftp_err("remove", e))
    }

    async fn remove_dir(&self, path: &str) -> Result<(), BrowserError> {
        self.sftp
            .remove_dir(path)
            .await
            .map_err(|e| sftp_err("rmdir", e))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
        // Vanilla SSH_FXP_RENAME. M0: replace-or-refuse DIFFERS by flavor —
        // callers must pre-check existence and never rely on server refusal.
        self.sftp
            .rename(from, to)
            .await
            .map_err(|e| sftp_err("rename", e))
    }

    async fn posix_rename(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
        // Only via the raw extension path; the high-level rename is vanilla.
        // Not used by M1 placement (hardlink tier or check-then-rename); kept
        // so the Replace flow can opt into atomic replacement where the
        // extension is advertised and consent is explicit.
        Err(BrowserError::unsupported(
            "posix_rename is not wired in M1 (use hardlink tier or consented replace)",
        ))
    }

    async fn hardlink(&self, from: &str, to: &str) -> Result<(), BrowserError> {
        match self.sftp.hardlink(from, to).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(BrowserError::unsupported(
                "hardlink@openssh.com is not advertised by this server",
            )),
            Err(e) => Err(sftp_err("hardlink", e)),
        }
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<(), BrowserError> {
        self.sftp
            .symlink(path, target)
            .await
            .map_err(|e| sftp_err("symlink", e))
    }

    async fn read_link(&self, path: &str) -> Result<String, BrowserError> {
        self.sftp
            .read_link(path)
            .await
            .map_err(|e| sftp_err("readlink", e))
    }

    async fn open_read(&self, path: &str) -> Result<Box<dyn RemoteReader>, BrowserError> {
        let file = self
            .sftp
            .open(path)
            .await
            .map_err(|e| sftp_err("open", e))?;
        Ok(Box::new(RusshReader { file }))
    }

    async fn create_exclusive(
        &self,
        path: &str,
        perm: u32,
    ) -> Result<Box<dyn RemoteWriter>, BrowserError> {
        let file = self
            .sftp
            .open_with_flags_and_attributes(
                path,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(perm),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| sftp_err("create", e))?;
        Ok(Box::new(RusshWriter { file }))
    }

    async fn set_attrs(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<i64>,
    ) -> Result<(), BrowserError> {
        self.sftp
            .set_metadata(
                path,
                FileAttributes {
                    permissions: mode,
                    mtime: mtime.map(|t| t.max(0) as u32),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| sftp_err("setstat", e))
    }

    fn has_hardlink(&self) -> bool {
        self.extensions.contains_key("hardlink@openssh.com")
    }

    fn has_posix_rename(&self) -> bool {
        self.extensions.contains_key("posix-rename@openssh.com")
    }

    fn extensions(&self) -> &BTreeMap<String, String> {
        &self.extensions
    }

    async fn close(self: Box<Self>) -> Result<(), BrowserError> {
        let _ = self.sftp.close().await;
        let _ = self
            .ssh
            .disconnect(Disconnect::ByApplication, "browser session done", "en")
            .await;
        Ok(())
    }
}

/// Resolve a `~`-prefixed key-file argument (test/config convenience).
/// `~` expansion for configured key paths (pure; unit-tested, config UI in M2).
#[allow(dead_code)]
pub fn expand_key_path(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    Path::new(p).to_path_buf()
}

/// Connection broker: holds the per-flavor trust store and the persisted
/// config, resolves the account pin, and opens transports. One connection
/// per `open()` call — no multiplexing, so the Windows sshd shared-session
/// stall class cannot occur by construction (report §6.6.1).
pub struct SessionCtx {
    pub trust: Arc<TrustStore>,
    pub config: Arc<crate::browser::ConfigStore>,
}

impl SessionCtx {
    /// `config_dir` is the app config dir; trust pins live under
    /// `browser-trust/`, the config JSON is `browser.json`.
    pub fn new(config_dir: &Path) -> Self {
        Self {
            trust: Arc::new(TrustStore::new(config_dir.join("browser-trust"))),
            config: Arc::new(crate::browser::ConfigStore::new(
                config_dir.join("browser.json"),
            )),
        }
    }

    /// Resolve the account for `host`, fixed for the whole call: the pinned
    /// `as_user` (what the pane showed and the user confirmed) wins, else the
    /// saved setting, else ssh's own config, else the local user. Port of
    /// `account()` (bin/tailfiles:143-168) — every child gets it explicitly
    /// so a mid-run config change can't switch accounts under a guard.
    pub fn resolve_user(&self, host: &str, as_user: Option<&str>) -> Result<String, BrowserError> {
        if let Some(u) = as_user {
            let u = u.trim();
            if !u.is_empty() {
                if !crate::browser::valid_user(u) {
                    return Err(BrowserError::new(
                        ErrorKind::Account,
                        format!("Can't tell which account to use on {host}"),
                    ));
                }
                return Ok(u.to_string());
            }
        }
        if let Some(u) = self.config.user_for(host) {
            return Ok(u);
        }
        if let Some(u) = ssh_config_user(host) {
            if crate::browser::valid_user(&u) {
                return Ok(u);
            }
        }
        Err(BrowserError::new(
            ErrorKind::Account,
            format!("Can't tell which account to use on {host}"),
        ))
    }

    /// Sniff the banner (flavor), then open an authenticated SFTP transport
    /// with per-flavor host-key pinning. `tailscale_host_keys` (from
    /// `tailscale status`) cross-checks the negotiated key for Tailscale SSH.
    pub async fn open(
        &self,
        host: &str,
        user: &str,
        tailscale_host_keys: Vec<String>,
    ) -> Result<RusshTransport, BrowserError> {
        let (flavor, _banner) = sniff_banner(host, 22).await?;
        connect(ConnectParams {
            host: host.to_string(),
            port: 22,
            user: user.to_string(),
            flavor,
            trust: Arc::clone(&self.trust),
            tailscale_host_keys,
            key_files: Vec::new(),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_classification() {
        // M0-observed banners.
        assert_eq!(
            ServerFlavor::from_banner("SSH-2.0-Tailscale"),
            Some(ServerFlavor::TailscaleSsh)
        );
        assert_eq!(
            ServerFlavor::from_banner("SSH-2.0-OpenSSH_10.3"),
            Some(ServerFlavor::Openssh)
        );
        assert_eq!(
            ServerFlavor::from_banner("SSH-2.0-OpenSSH_for_Windows_9.5"),
            Some(ServerFlavor::OpensshWindows)
        );
        assert_eq!(ServerFlavor::from_banner("HTTP/1.1 400 Bad Request"), None);
        assert_eq!(ServerFlavor::from_banner(""), None);
    }

    #[test]
    fn sftp_error_classification() {
        let e = SftpClientError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::NoSuchFile,
            error_message: "No such file".into(),
            language_tag: "en-US".into(),
        });
        assert_eq!(sftp_err("stat", e).kind, ErrorKind::Notfound);

        let e = SftpClientError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::PermissionDenied,
            error_message: "Permission denied".into(),
            language_tag: "en-US".into(),
        });
        assert_eq!(sftp_err("open", e).kind, ErrorKind::Denied);

        // Hardlink collisions come back as generic Failure; the message on
        // Tailscale says "file exists" (M0) — map that to Exists, otherwise
        // the placement layer re-checks existence itself.
        let e = SftpClientError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::Failure,
            error_message: "file exists".into(),
            language_tag: "en-US".into(),
        });
        assert_eq!(sftp_err("hardlink", e).kind, ErrorKind::Exists);

        let e = SftpClientError::Timeout;
        assert_eq!(sftp_err("read", e).kind, ErrorKind::Unreachable);
    }

    #[test]
    fn message_classification_port() {
        assert_eq!(
            classify_message("auth: tailnet policy does not permit"),
            ErrorKind::Auth
        );
        assert_eq!(
            classify_message("hostkey: changed key refused"),
            ErrorKind::Hostkey
        );
        assert_eq!(
            classify_message("unreachable: TCP timeout"),
            ErrorKind::Unreachable
        );
        assert_eq!(classify_message("No such file"), ErrorKind::Notfound);
        assert_eq!(classify_message("Permission denied"), ErrorKind::Denied);
        assert_eq!(
            classify_message("Operation unsupported"),
            ErrorKind::Unsupported
        );
        assert_eq!(classify_message("weird"), ErrorKind::Other);
    }

    #[test]
    fn node_mapping_from_attrs() {
        let dir = FileAttributes {
            permissions: Some(0o40755),
            size: Some(64),
            mtime: Some(1700000000),
            ..Default::default()
        };
        let n = node_from("d".into(), &dir);
        assert!(n.is_dir && !n.is_symlink && !n.is_unknown);
        assert_eq!(n.mtime, 1700000000);

        let reg = FileAttributes {
            permissions: Some(0o100644),
            size: Some(12),
            ..Default::default()
        };
        let n = node_from("f".into(), &reg);
        assert!(n.is_regular && !n.is_dir);

        let link = FileAttributes {
            permissions: Some(0o120777),
            ..Default::default()
        };
        assert!(node_from("l".into(), &link).is_symlink);

        // No permissions → unknown type (treated cautiously by callers).
        let none = FileAttributes::default();
        assert!(node_from("x".into(), &none).is_unknown);
    }
}
