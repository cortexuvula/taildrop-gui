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
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, ChannelMsg, Disconnect};
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
    /// under ~/.ssh (id_ed25519, id_ecdsa, id_rsa). RSA files are still
    /// DETECTED so auth can name them in a typed unsupported error (B7:
    /// the russh `rsa` feature is dropped per decisions.md #6), but they
    /// can no longer sign in-process.
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
            // Real sshHostKeys entries are FULL LINES: "<algorithm> <base64>"
            // (M0 evidence/fleet.json; the spike's probe.py splits on
            // whitespace for the same reason). Compare (algorithm, blob)
            // PAIRS — a bare blob can never equal a full line, and a blob
            // published under a different algorithm must not pass either.
            let mut offered = openssh.split_whitespace();
            let alg = offered.next().unwrap_or("");
            let blob = offered.next().unwrap_or("");
            let matched = !alg.is_empty()
                && !blob.is_empty()
                && self.tailscale_host_keys.iter().any(|entry| {
                    let mut parts = entry.split_whitespace();
                    parts.next() == Some(alg) && parts.next() == Some(blob)
                });
            if !matched {
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

/// Boxed agent client — russh's `dynamic()` erases the platform-specific
/// stream type so the auth flow below is shared across platforms.
type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// Connect to the local ssh-agent. `None` when no agent is reachable —
/// auth then falls through to the typed no-credential error; an absent
/// agent is never reported as an agent success.
///
/// Unix: the `SSH_AUTH_SOCK` socket (russh `connect_env`).
#[cfg(unix)]
async fn agent_connect() -> Option<DynAgent> {
    match AgentClient::connect_env().await {
        Ok(a) => Some(a.dynamic()),
        Err(_) => None, // no SSH_AUTH_SOCK / no agent
    }
}

/// Windows: the Windows-OpenSSH ssh-agent service listens on this named
/// pipe (russh exposes `connect_named_pipe` for exactly this). The service
/// is manual-start; when it is not running the pipe is absent and agent
/// auth is honestly skipped — key-file and tailnet-identity auth are
/// unaffected.
#[cfg(windows)]
async fn agent_connect() -> Option<DynAgent> {
    // russh retries ERROR_PIPE_BUSY every 50 ms with no limit of its own,
    // but the whole auth phase runs inside tokio::time::timeout(AUTH_TIMEOUT
    // = 25 s) — a permanently busy pipe ends in the typed auth-timeout
    // error, never a hang.
    match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(a) => Some(a.dynamic()),
        Err(_) => None, // ssh-agent service not running / pipe absent
    }
}

/// Any other platform: no ssh-agent transport is known — skip agent auth
/// rather than pretending it succeeded.
#[cfg(not(any(unix, windows)))]
async fn agent_connect() -> Option<DynAgent> {
    None
}

/// Try every ssh-agent identity (certificates skipped in M1). Ok(None) when
/// no agent / no identities; Ok(Some(method)) on success.
async fn agent_auth(
    ssh: &mut client::Handle<ClientHandler>,
    user: &str,
) -> Result<Option<String>, russh::Error> {
    let mut agent = match agent_connect().await {
        Some(a) => a,
        None => return Ok(None), // no agent reachable on this platform
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
impl std::fmt::Debug for RusshTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RusshTransport")
            .field("user", &self.user)
            .field("flavor", &self.flavor)
            .finish_non_exhaustive()
    }
}

pub struct RusshTransport {
    ssh: client::Handle<ClientHandler>,
    sftp: SftpSession,
    extensions: BTreeMap<String, String>,
    flavor: ServerFlavor,
    user: String,
    /// Diagnostics surface: which auth method succeeded (probe detail, M2).
    #[allow(dead_code)]
    auth_method: String,
    /// M2b: probed exec-agent capability, set once by the commands layer
    /// after connect (`set_agent_cap`); read by the copy engine's tier
    /// selection. `None` = not probed = the SFTP-only default.
    agent_cap: Mutex<Option<crate::browser::agent::AgentCap>>,
    win_agent: Mutex<Option<crate::browser::win_agent::WinAgent>>,
}

impl RusshTransport {
    pub fn set_win_agent(&self, agent: Option<crate::browser::win_agent::WinAgent>) {
        *self.win_agent.lock().unwrap_or_else(|p| p.into_inner()) = agent;
    }
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
    /// M2b: record the probed exec-agent capability for this transport.
    pub fn set_agent_cap(&self, cap: crate::browser::agent::AgentCap) {
        if let Ok(mut slot) = self.agent_cap.lock() {
            *slot = Some(cap);
        }
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
                sftp_status: None,
                agent_rc: None,
            };
        }
    }
    let message = format!("{context}: {e}");
    let kind = classify_message(&message);
    BrowserError {
        message,
        kind,
        names: None,
        sftp_status: None,
        agent_rc: None,
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
        //    B7 (decisions.md #6): the russh `rsa` feature is dropped
        //    (RUSTSEC-2023-0071, Marvin timing side channel, no fixed
        //    upgrade), so RSA key FILES can no longer sign in-process.
        //    Detect them and produce a clear typed error below instead of a
        //    confusing signature failure. RSA identities held by the
        //    ssh-agent still work (signing happens inside the agent, and
        //    russh's hash negotiation is not feature-gated).
        let mut loaded_any = false;
        let mut rsa_skipped: Vec<String> = Vec::new();
        for path in &key_files {
            let private = match keys::load_secret_key(path, None) {
                Ok(k) => k,
                Err(_) => continue, // encrypted or unreadable — agent's turn
            };
            if matches!(private.algorithm(), keys::Algorithm::Rsa { .. }) {
                rsa_skipped.push(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                );
                continue;
            }
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
        // B7: name the dropped-RSA-support blocker explicitly when RSA key
        // files were the only file credentials found.
        if !rsa_skipped.is_empty() {
            return Err(BrowserError::new(
                ErrorKind::Unsupported,
                format!(
                    "auth: RSA key file(s) {} found for {user}@{host}, but this build no \
                     longer supports in-process RSA signing (RUSTSEC-2023-0071, owner \
                     decision). Use an Ed25519 or ECDSA key file, or add the RSA key to \
                     your ssh-agent — agent-side signing still works.",
                    rsa_skipped.join(", ")
                ),
            ));
        }
        Err(BrowserError::new(
            ErrorKind::Auth,
            format!(
                "auth: no working credential for {user}@{host} — tried tailnet identity, \
                 key files (id_ed25519/id_ecdsa) and ssh-agent. Password auth is \
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
                    sftp_status: None,
                    agent_rc: None,
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
        agent_cap: Mutex::new(None),
        win_agent: Mutex::new(None),
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
                .with_sftp_status(status.status_code as u32)
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

    fn win_agent(&self) -> Option<crate::browser::win_agent::WinAgent> {
        self.win_agent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|a| a.proven())
    }
    fn agent_cap(&self) -> crate::browser::agent::AgentCap {
        self.agent_cap
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .unwrap_or(crate::browser::agent::AgentCap::SftpOnly)
    }

    async fn open_exec(
        &self,
        cmd: &str,
    ) -> Result<Box<dyn crate::browser::agent::ExecChannel>, BrowserError> {
        // A dedicated session channel per exec (never multiplexed with the
        // SFTP channel). Both failure shapes map to Unsupported so callers
        // treat them uniformly: the probe degrades to SftpOnly, and the
        // delete re-check falls back to the documented SFTP walk.
        let channel =
            self.ssh.channel_open_session().await.map_err(|e| {
                BrowserError::unsupported(format!("denied: exec channel open: {e}"))
            })?;
        channel
            .exec(true, cmd)
            .await
            .map_err(|e| BrowserError::unsupported(format!("denied: exec request: {e}")))?;
        Ok(Box::new(SshExecChannel::new(channel)))
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

/// M2b: the real exec-channel adapter — one SSH exec session driven as an
/// `agent::ExecChannel`. A single internal demux over `channel.wait()`:
/// Data → stdout buffer, ExtendedData(ext 1) → capped stderr capture,
/// ExitStatus → exit slot, want_reply Failure → typed "exec refused".
/// `make_writer()` is 'static (russh 0.65.2, channels/mod.rs:429 — verified
/// against the vendored source), so writes and reads could interleave; the
/// agent protocol is strictly request/reply, so no concurrent pumping is
/// needed and none is done.
struct SshExecChannel {
    channel: russh::Channel<client::Msg>,
    writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    stdout: std::collections::VecDeque<u8>,
    stderr: Arc<Mutex<Vec<u8>>>,
    exit: Option<u32>,
    /// Read-side EOF: no further stdout can arrive on this channel. RFC
    /// 4254 §5.3 leaves the channel OPEN after EOF — a process status may
    /// still be delivered afterwards — so EOF must not end the search for
    /// it. Only the channel's completion (Close per §6.10, or stream
    /// exhaustion) is terminal for `wait_exit`. The live M3-peer symptom
    /// (2026-11-23: correct stdout, no observed status) had its wire
    /// ordering UNMEASURED; the production-seam loopback tests in
    /// `exec_status_loopback` are what establish the client-side
    /// EOF-before-status defect.
    stdout_eof: bool,
    /// Channel completion: Close received, or the channel's message stream
    /// is exhausted. Only this ends the search for an exit status.
    closed: bool,
}

impl SshExecChannel {
    fn new(channel: russh::Channel<client::Msg>) -> Self {
        let writer = Box::new(channel.make_writer());
        Self {
            channel,
            writer,
            stdout: std::collections::VecDeque::new(),
            stderr: Arc::new(Mutex::new(Vec::new())),
            exit: None,
            stdout_eof: false,
            closed: false,
        }
    }

    /// Consume one channel message. `Ok(true)` = keep pumping;
    /// `Ok(false)` = channel exhausted (no more messages will arrive).
    async fn pump(&mut self) -> Result<bool, BrowserError> {
        let Some(msg) = self.channel.wait().await else {
            self.stdout_eof = true;
            self.closed = true;
            return Ok(false);
        };
        match msg {
            ChannelMsg::Data { data } => self.stdout.extend(data.iter().copied()),
            ChannelMsg::ExtendedData { data, ext } => {
                if ext == 1 {
                    // stderr, capped — diagnostics only, never unbounded.
                    let mut g = self.stderr.lock().unwrap_or_else(|p| p.into_inner());
                    let room = crate::browser::agent::STDERR_CAP.saturating_sub(g.len());
                    g.extend(data.iter().take(room).copied());
                }
            }
            ChannelMsg::ExitStatus { exit_status } => self.exit = Some(exit_status),
            ChannelMsg::Eof => self.stdout_eof = true,
            ChannelMsg::Close => {
                self.stdout_eof = true;
                self.closed = true;
            }
            // want_reply=true: the server's refusal of the exec request.
            ChannelMsg::Failure => {
                return Err(BrowserError::unsupported(
                    "the server refused the remote exec request",
                ))
            }
            _ => {}
        }
        Ok(true)
    }
}

#[async_trait]
impl crate::browser::agent::ExecChannel for SshExecChannel {
    async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        AsyncWriteExt::write_all(&mut self.writer, data)
            .await
            .map_err(|e| {
                BrowserError::new(
                    classify_message(&e.to_string()),
                    format!("agent stream write: {e}"),
                )
            })?;
        AsyncWriteExt::flush(&mut self.writer).await.map_err(|e| {
            BrowserError::new(
                classify_message(&e.to_string()),
                format!("agent stream flush: {e}"),
            )
        })
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
        loop {
            if !self.stdout.is_empty() {
                let n = buf.len().min(self.stdout.len());
                for (slot, b) in buf.iter_mut().zip(self.stdout.drain(..n)) {
                    *slot = b;
                }
                return Ok(n);
            }
            if self.stdout_eof {
                return Ok(0);
            }
            self.pump().await?;
        }
    }

    async fn close_stdin(&mut self) -> Result<(), BrowserError> {
        // SSH channel EOF = "no more stdin" for the remote process — the
        // agent's self-clean arming, and the plugin cleanup ordering's
        // first step (close the child's stdin, then wait bounded).
        self.channel
            .eof()
            .await
            .map_err(|e| BrowserError::other(format!("agent stdin close: {e}")))
    }

    async fn wait_exit(&mut self, timeout: Duration) -> Option<u32> {
        // Wait for the process status until the CHANNEL completes (Close or
        // message-stream exhaustion), not until stdout EOF: a status that
        // follows EOF must still be observed. A channel that completes
        // without ever sending a status reports None (unknown), and a
        // silent channel is bounded by the caller's deadline — never a
        // guess, never a busy loop (pump pends when nothing arrives).
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.exit.is_some() || self.closed {
                return self.exit;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                // Timeout: report what we have (None = capability/exit
                // unknown).
                return self.exit;
            }
            match tokio::time::timeout(left, self.pump()).await {
                Ok(Ok(true)) => continue,
                Ok(Ok(false)) | Ok(Err(_)) | Err(_) => return self.exit,
            }
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|g| String::from_utf8_lossy(&g).into_owned())
            .unwrap_or_default()
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
    /// N1 (B7 follow-up): with the russh `rsa` feature dropped
    /// (RUSTSEC-2023-0071, decisions.md #6), both RSA key-file shapes
    /// fail SAFE through the auth key-file loop — no panic, no silent
    /// misbehavior:
    /// - OpenSSH-format RSA still DECODES (ssh-key parses it under `alloc`,
    ///   not `rsa`), hits the `Algorithm::Rsa` detection in the loop and
    ///   yields the NAMED typed Unsupported error.
    /// - Legacy PEM RSA does NOT decode (russh keys format layer returns
    ///   UnsupportedKeyType with the feature off) → the loop's load-error
    ///   `continue` → the generic Auth error.
    ///
    /// Keys below are throwaway test-only material (2048-bit, no passphrase,
    /// generated for this fixture; authenticate nothing).
    #[test]
    fn rsa_key_files_fail_safe_without_the_rsa_feature() {
        let dir = std::env::temp_dir().join(format!("m1-n1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. OpenSSH-format RSA: decodes, and the detection predicate that
        //    drives the typed Unsupported error fires.
        let ossh_path = dir.join("id_rsa_ossh");
        std::fs::write(&ossh_path, OPENSSH_FORMAT_RSA_TEST_KEY).unwrap();
        let parsed = keys::load_secret_key(&ossh_path, None)
            .expect("OpenSSH-format RSA still decodes without the rsa feature");
        assert!(
            matches!(parsed.algorithm(), keys::Algorithm::Rsa { .. }),
            "Algorithm::Rsa detection must fire → typed Unsupported path"
        );

        // 2. Legacy PEM RSA: load FAILS (UnsupportedKeyType with the feature
        //    off) → the auth loop `continue`s → generic Auth error, no panic.
        let pem_path = dir.join("id_rsa_pem");
        std::fs::write(&pem_path, LEGACY_PEM_RSA_TEST_KEY).unwrap();
        let err = keys::load_secret_key(&pem_path, None)
            .expect_err("legacy PEM RSA must NOT decode without the rsa feature");
        assert!(
            err.to_string().to_lowercase().contains("unsupported")
                || err.to_string().to_lowercase().contains("rsa"),
            "unexpected load error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    const OPENSSH_FORMAT_RSA_TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAvlcbDHN9FOYVbs9NEzX/JKUQGcvil3Sa/JOQMcJDoUcEdzHVxxdZ
WztDGy+ctLJt9NwRcXQwM/seYSSSQGX2Up3rQ6HgVCH5xVINrFBgTZC2ZLMS84OAl/lDMB
U7Mx5ItdLVHlo8+La7DJzPsxcoPvOwKBkowViQT2ROB+gf94ujaitfWEMtFKEZmYrKnNyg
UeRR15gi+Onx+L8E/Q6ZZgoqoFFITrPCmmP+xVjaH0oq2jYtrOM7dt1NazbeSxElFa6hQa
nAI5Qk2nydsLmNr2AEQKYSRlI71CqRdWcpzVghRZOhszMNA2fec7AlI0Q1Ye7VoEuhpkBY
aW6zwnQETQAAA8A0qNSnNKjUpwAAAAdzc2gtcnNhAAABAQC+VxsMc30U5hVuz00TNf8kpR
AZy+KXdJr8k5AxwkOhRwR3MdXHF1lbO0MbL5y0sm303BFxdDAz+x5hJJJAZfZSnetDoeBU
IfnFUg2sUGBNkLZksxLzg4CX+UMwFTszHki10tUeWjz4trsMnM+zFyg+87AoGSjBWJBPZE
4H6B/3i6NqK19YQy0UoRmZisqc3KBR5FHXmCL46fH4vwT9DplmCiqgUUhOs8KaY/7FWNof
SiraNi2s4zt23U1rNt5LESUVrqFBqcAjlCTafJ2wuY2vYARAphJGUjvUKpF1ZynNWCFFk6
GzMw0DZ95zsCUjRDVh7tWgS6GmQFhpbrPCdARNAAAAAwEAAQAAAQAvHGYwfcE+0/gIFj0w
fRPqoQRMLj+mVxEpZOiyuC9/EpHTYr1T6r6OKyvf5bf2rZcznOigWS65HvWU+7VLRTFjn3
ykn8Njb4lfwoBp3vcHG8NZR01wgmLouqRbmVWuwFck0cs4GQOwV6ISbBmobRnkAvLd6eRB
fscxL1V09Yo7/aDFNjQDrYRThDpV5BqWlXvwRZi1V+NrUBIcWbehFFKt9Dy3aBhx06hHb5
8zjpE56aSCViWhyHm7MBSoeVojefpDpr7Jakix0/epkYYRduK119KbCzqTA3tEse1DtuqC
IGH5uIVl1fe+nh6lfUkPnf6BT5Vy0KL9rpZQMR0MWRttAAAAgQD4Rq2GXZxvxvn0oj6eEo
edKweoME5cLV3h3QB+dHb6/aAOOBjmsxjqPLWWAtElgAT8jdTEyLd8i6nWHCpl6isPwsyk
bEtIog7z1wv4Rvdome45or08gJF01XoGI3YgraNb4lHWWSIigyQTPL27wBCl3j9jaj7Nfb
/b7W3pIELoXAAAAIEA+x0goQfjmP4TH0FLegVhjDAQfx6fj6jFp/2H23T8Tn1x1BfsZtXT
/1goRZii6z3v0AWSGiQ46pEHoFcDUpWQd/qgtGXVrknioABWN6CSaRTh5M05chThAfuSup
qmMUqF60rq3u/V4hGFkT9ArRmRkF/Xblw0aDlIlm9Al7Pp2w8AAACBAMILP0c1gQU+ncs3
vsdPFmmooQFgB6qy5dlCcUGSHAy5G7p8Yw1pFyfqN/UIcoABC5iN7VpJJBkA0ytPenno2w
AwI5ZGq+ccZL9dBvuAzj167564GC6nTSzm64w2FNk/Bfln+S8d5knaQq2bhkv53hs16wtA
JNTbjxt2+j8BtNrjAAAACm0xLW4xLXRlc3Q=
-----END OPENSSH PRIVATE KEY-----";

    const LEGACY_PEM_RSA_TEST_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAlecraqYTmFr2/D/mNe87sTPVDjZxo8ADBueFuwJMvscs8kBb
3EajVwWkfpZQHF1XXUFlal3jHwHRDJWZJCJm6/Iac72MSVKfn2i877lOILs5CzbK
zdxBlw1aG11pONfYqOHlmyRLiVXiFaHoDFqCkKNtXrf2ksqFd7so5b+nV9Evv14t
d/uQTJhYFLTP/fBZYxAeY8cf32Vv7cb/JxNUxtzYbopkyI8nvVM6e7B13W/bceyg
C12OkTek8s8uEyO3cWubmIBi4U+AZ9AtIdM/46SpZ7V7AfmnvyBUH+kLJ95Q9wLM
fiSxlhddonxZlQcFd3W9npR9TZrAtLMN8a42cQIDAQABAoIBAFVaXzpGffnOMbvH
lok3QsHPRYzmKyaV+iNASMEGujQWrNzzEFsITZELQXJinBLKgygySFCcTOom2qVT
uPL+M6Q9Ihqs3ugRB5p4xLuqw3WXpQ9hj3BP7XjYvWm/cl5PaKogleAUdn4oTKcM
nVgLYTSCGZBJJzjF3+EgzFSlH9+7+VlDa4uTRUbtViKKWZyN55qbu2xtARXS48D5
y3ukgCAaFjxASZdv+bZUc4hQzfyAycAI5prZ1tQVs811EGyQMlubIJLvGvdzzOG7
Cjmp423+LR8I+j3nSzsjPQmEZqh62zOxIV8ceWkziA4gigNv8xlTJb/l0lUIMJlT
1LH6itECgYEAx2WSVQaDPmwychheWQ8niGfG1oQIs4/noka9a61Ksrz7l6WUPGBr
CV/9XOmDQ3K6zrO/1qbURgjCkEHypLePsDyXQTEFFgMUAGJiWd/ZOe7TtPdy9RJw
XVb0cbY9D87ytebOXlg2SvehChurggJ4A/CdZV/bpv4DG6Q7rk+ecFsCgYEAwHTS
GLe6Vni7+XhJbc1kRTEoeXWZEKohV7zzz907LLIGa4XElhBtiy5XTp+68WY+IZCv
vvd7P9IpO9r6WjPPv3LdNyfK5Pw08HcXaa+opvuVTbJhB3djP8xCqNXyAsckRjKB
EQnDiLCKIVo/PM28bm68eH6+2+OtAqf+iRylriMCgYAJ/qBgHKx1c4zvxSLtieHs
Z29DD2k0PvFYY/1YqV4GKYze5BDTGe4jkSktABNxTQ3qEH58Lu6S+Btwia207fKI
3fCi9ucPXGYjQss1QBAuZDc+aezcNell9FbaYE0dPr1Rutj7R6apCVKMJCjrol4L
rILnMu/apf7X4tqO+TSy4wKBgQCbZXhC9w7hRWlaPNLlcwN0jwXEVLz75LM5edtP
yq0+IJzocsFR/f/FcuySEo1K36hpR79wLbn5xUMADoc5ED69sgyGvX2pOS+xvidM
ccZ21gsvCkooUjD0UYS9eMqJjaxvddYbIMwecZWG6qzG9sQgUw+hJ44KVKLc0zLG
GTh6CwKBgFmJ0tIBN0a4sQe5H9qeYfSPbpeJ1sIL+olscXP0+q+YwsI9N8MrMGqM
ZUL/jkhM15B4OWsgkIS9a2a6+670bW1uL5660HbtwSxvAt7VhCa6IsULxDyx0Jx4
Wdlj8alBZoITtFBHPGIPvHqqAjV8fNbDomLxImXrVYxsgRa3DssZ
-----END RSA PRIVATE KEY-----";
}

/// Loopback exec-channel regression harness (M3-EXEC-STATUS-REPAIR-20261123).
///
/// A real russh server over 127.0.0.1 emits scripted ChannelMsg sequences;
/// the PRODUCTION `connect()` (auth + SFTP subsystem) and the production
/// `open_exec()` adapter are driven exactly as callers drive them
/// (write/EOF, read to stdout EOF, then `wait_exit`). This reproduces the
/// client-side defect deterministically: the adapter treated stdout-side
/// EOF as terminal, so an ExitStatus delivered after EOF was never pumped.
/// Attribution note: the archived live-peer probes observed correct stdout
/// with NO exit status — they did not trace message order, and the peers'
/// actual wire ordering remains unmeasured. The loopback proves the
/// EOF-before-status mechanism is a real client failure mode (and the
/// repaired client's handling of both orderings); it does not measure what
/// either peer's sshd sent.
#[cfg(test)]
mod exec_status_loopback {
    use super::*;
    use crate::browser::agent::ExecChannel;
    use crate::browser::transport::SftpTransport;
    use russh::server::{
        self, Auth, ChannelOpenHandle, Msg as SrvMsg, Server as _, Session as SrvSession,
        VerifiedPublicKey,
    };
    use russh::{Channel, ChannelId};
    use russh_sftp::server::Handler as SftpHandler;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Clone)]
    enum Seq {
        EofThenStatus(u32),
        StatusBeforeEof(u32),
        EofCloseNoStatus,
        StderrThenStatus(u32),
        Refuse,
        Silent,
    }

    struct MinimalSftp;
    impl SftpHandler for MinimalSftp {
        type Error = StatusCode;
        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }
    }

    async fn run_seq(ch: Channel<SrvMsg>, seq: Seq) {
        match seq {
            Seq::EofThenStatus(rc) => {
                let _ = ch.data_bytes(&b"out"[..]).await;
                let _ = ch.eof().await;
                let _ = ch.exit_status(rc).await;
                let _ = ch.close().await;
            }
            Seq::StatusBeforeEof(rc) => {
                let _ = ch.data_bytes(&b"out"[..]).await;
                let _ = ch.exit_status(rc).await;
                let _ = ch.eof().await;
                let _ = ch.close().await;
            }
            Seq::EofCloseNoStatus => {
                let _ = ch.data_bytes(&b"out"[..]).await;
                let _ = ch.eof().await;
                let _ = ch.close().await;
            }
            Seq::StderrThenStatus(rc) => {
                let _ = ch.extended_data_bytes(1, &b"err"[..]).await;
                let _ = ch.data_bytes(&b"out"[..]).await;
                let _ = ch.eof().await;
                let _ = ch.exit_status(rc).await;
                let _ = ch.close().await;
            }
            Seq::Refuse | Seq::Silent => {}
        }
    }

    #[derive(Clone)]
    struct LoopServer {
        scripts: Arc<HashMap<String, Seq>>,
        channels: Arc<Mutex<HashMap<ChannelId, Channel<SrvMsg>>>>,
    }

    struct LoopHandler {
        scripts: Arc<HashMap<String, Seq>>,
        channels: Arc<Mutex<HashMap<ChannelId, Channel<SrvMsg>>>>,
    }

    impl server::Handler for LoopHandler {
        type Error = russh::Error;

        async fn auth_publickey(
            &mut self,
            _user: &str,
            _key: VerifiedPublicKey<'_>,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<SrvMsg>,
            reply: ChannelOpenHandle,
            _session: &mut SrvSession,
        ) -> Result<(), Self::Error> {
            self.channels
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn subsystem_request(
            &mut self,
            channel_id: ChannelId,
            name: &str,
            session: &mut SrvSession,
        ) -> Result<(), Self::Error> {
            if name != "sftp" {
                return session.channel_failure(channel_id);
            }
            session.channel_success(channel_id)?;
            if let Some(ch) = self
                .channels
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&channel_id)
            {
                tokio::spawn(async move {
                    russh_sftp::server::run(ch.into_stream(), MinimalSftp).await;
                });
            }
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel_id: ChannelId,
            data: &[u8],
            session: &mut SrvSession,
        ) -> Result<(), Self::Error> {
            let cmd = String::from_utf8_lossy(data).into_owned();
            let mut guard = self.channels.lock().unwrap_or_else(|p| p.into_inner());
            let Some(ch) = guard.remove(&channel_id) else {
                return session.channel_failure(channel_id);
            };
            drop(guard);
            match self.scripts.get(&cmd) {
                Some(Seq::Refuse) => session.channel_failure(channel_id),
                Some(seq) => {
                    session.channel_success(channel_id)?;
                    let seq = seq.clone();
                    tokio::spawn(async move {
                        run_seq(ch, seq).await;
                    });
                    Ok(())
                }
                None => session.channel_failure(channel_id),
            }
        }
    }

    impl server::Server for LoopServer {
        type Handler = LoopHandler;
        fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> LoopHandler {
            LoopHandler {
                scripts: self.scripts.clone(),
                channels: self.channels.clone(),
            }
        }
    }

    async fn loop_setup(scripts: HashMap<String, Seq>) -> (u16, PathBuf, PathBuf) {
        use russh::keys::ssh_key::private::Ed25519Keypair;
        use russh::keys::PrivateKey;
        let host_key = PrivateKey::from(Ed25519Keypair::from_seed(&[1u8; 32]));
        let client_key = PrivateKey::from(Ed25519Keypair::from_seed(&[2u8; 32]));
        let dir = std::env::temp_dir().join(format!(
            "m3-exec-loop-{}",
            crate::browser::transfer::new_stage_tag()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("id_ed25519");
        client_key
            .write_openssh_file(&key_path, russh::keys::ssh_key::LineEnding::LF)
            .unwrap();
        let pin = host_key
            .public_key()
            .to_openssh()
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(
            dir.join("pins.openssh.json"),
            serde_json::to_string(&serde_json::json!({ "127.0.0.1": pin })).unwrap(),
        )
        .unwrap();
        let mut config = server::Config::default();
        config.keys.push(host_key);
        config.inactivity_timeout = None;
        let config = Arc::new(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut server = LoopServer {
            scripts: Arc::new(scripts),
            channels: Arc::new(Mutex::new(HashMap::new())),
        };
        tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                let handler = server.new_client(socket.peer_addr().ok());
                let _ = server::run_stream(config, socket, handler).await;
            }
        });
        (port, key_path, dir)
    }

    async fn connect_loop(port: u16, key: PathBuf, dir: PathBuf) -> RusshTransport {
        connect(ConnectParams {
            host: "127.0.0.1".into(),
            port,
            user: "u".into(),
            flavor: ServerFlavor::Openssh,
            trust: Arc::new(TrustStore::new(&dir)),
            tailscale_host_keys: Vec::new(),
            key_files: vec![key],
        })
        .await
        .unwrap()
    }

    /// Production consumption pattern (win_agent::capture / agent flows):
    /// close stdin, read stdout to EOF, THEN ask for the exit status.
    async fn drain(ch: &mut dyn ExecChannel) -> Vec<u8> {
        ch.close_stdin().await.unwrap();
        let mut out = Vec::new();
        loop {
            let mut b = [0u8; 1024];
            let n = ch.read(&mut b).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&b[..n]);
        }
        out
    }

    fn one(script: &str, seq: Seq) -> HashMap<String, Seq> {
        HashMap::from([(script.to_string(), seq)])
    }

    #[tokio::test]
    async fn exec_status_after_stdout_eof_is_returned_zero() {
        let (port, key, dir) = loop_setup(one("s", Seq::EofThenStatus(0))).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        assert_eq!(drain(ch.as_mut()).await, b"out");
        assert_eq!(
            ch.wait_exit(Duration::from_secs(10)).await,
            Some(0),
            "status delivered after stdout EOF must be observed exactly"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_status_after_stdout_eof_is_returned_nonzero() {
        let (port, key, dir) = loop_setup(one("s", Seq::EofThenStatus(7))).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        assert_eq!(drain(ch.as_mut()).await, b"out");
        assert_eq!(
            ch.wait_exit(Duration::from_secs(10)).await,
            Some(7),
            "nonzero status delivered after stdout EOF must be observed exactly"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_status_before_eof_still_observed() {
        let (port, key, dir) = loop_setup(one("s", Seq::StatusBeforeEof(3))).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        assert_eq!(drain(ch.as_mut()).await, b"out");
        assert_eq!(ch.wait_exit(Duration::from_secs(10)).await, Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_eof_close_without_status_stays_none_without_busy_loop() {
        let (port, key, dir) = loop_setup(one("s", Seq::EofCloseNoStatus)).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        assert_eq!(drain(ch.as_mut()).await, b"out");
        let start = std::time::Instant::now();
        assert_eq!(ch.wait_exit(Duration::from_secs(10)).await, None);
        assert_eq!(ch.wait_exit(Duration::from_secs(10)).await, None);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "exhausted channel must not be polled in a busy loop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_silent_channel_wait_is_bounded_unknown() {
        let (port, key, dir) = loop_setup(one("s", Seq::Silent)).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        ch.close_stdin().await.unwrap();
        let start = std::time::Instant::now();
        assert_eq!(ch.wait_exit(Duration::from_millis(300)).await, None);
        assert!(start.elapsed() >= Duration::from_millis(250));
        assert!(start.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_stderr_preserved_and_status_after_eof_observed() {
        let (port, key, dir) = loop_setup(one("s", Seq::StderrThenStatus(2))).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        assert_eq!(drain(ch.as_mut()).await, b"out");
        assert_eq!(
            ch.wait_exit(Duration::from_secs(10)).await,
            Some(2),
            "stderr followed by status after EOF must still report the status"
        );
        assert_eq!(ch.stderr_tail(), "err");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn exec_refusal_is_typed_refusal() {
        let (port, key, dir) = loop_setup(one("s", Seq::Refuse)).await;
        let t = connect_loop(port, key, dir.clone()).await;
        let mut ch = t.open_exec("s").await.unwrap();
        let mut b = [0u8; 64];
        let err = ch.read(&mut b).await.unwrap_err();
        assert_eq!(err.kind, crate::browser::ErrorKind::Unsupported);
        assert!(err.message.contains("refused the remote exec request"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
