//! In-process fake SFTP server for tests (acceptance gate 6: NO live-fleet
//! operations — everything runs over loopback against a temp-dir-backed
//! server built from russh's server side + russh-sftp's server Handler).
//!
//! What it can impersonate (M0-measured fleet behaviors):
//! - Banners: `SSH-2.0-TailscaleSSH`, `SSH-2.0-OpenSSH_9.6`,
//!   `SSH-2.0-OpenSSH_for_Windows_8.1` — so sniff_banner + flavor-specific
//!   trust stores and auth flows run end to end.
//! - Extension advertisement: `hardlink@openssh.com`, `posix-rename@openssh.com`,
//!   plus an unknown custom name to prove the RAW VERSION capture keeps
//!   names russh-sftp's high-level session drops.
//! - Hostility knobs:
//!   * `hardlink_lies` — advertises hardlink but answers OpUnsupported
//!     (the unsupported≠failed distinction Codie's M0 review required).
//!   * `rename_replaces` — Tailscale SSH replaced an existing destination on
//!     vanilla RENAME; macOS/Windows OpenSSH refused. Both are configurable
//!     so placement code can be proven to never ASSUME either behavior.
//!   * `fail_writes_after` — mid-copy write failure to exercise cleanup on
//!     the failed exit path.
//!   * `accept_none_auth=false` — forces the key-file auth path (OpenSSH
//!     flavor) instead of tailnet identity.
//!
//! Filesystem: plain std::fs under a temp root. Paths from the wire are
//! rooted; symlinks/hardlinks are real OS ones, so placement semantics
//! (hard_link collision = AlreadyExists, rename replace-or-refuse) are the
//! kernel's, not a simulation.

#![cfg(test)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use russh::keys::{decode_secret_key, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, SshId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::browser::session::{sniff_banner, ConnectParams, RusshTransport};
use crate::browser::trust::TrustStore;

// ---------------------------------------------------------------------------
// Test-only key material. Generated specifically for this fixture with
// ssh-keygen -t ed25519; they authenticate nothing beyond loopback unit
// tests and appear in no production config. Two host keys so the
// host-key-change refusal path is testable.
// ---------------------------------------------------------------------------

/// Fake server host key #1.
pub const HOST_KEY_PEM: &str = r#"-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCgifCgf/hI+A+yZFRlyEtJM3FniMzh0JVprSx737ICGgAAAKDbv3eM2793
jAAAAAtzc2gtZWQyNTUxOQAAACCgifCgf/hI+A+yZFRlyEtJM3FniMzh0JVprSx737ICGg
AAAEDt6z3R7memBRs1EVVyKPQJERs1ubEk4DBf1h0IUmCD6KCJ8KB/+Ej4D7JkVGXIS0kz
cWeIzOHQlWmtLHvfsgIaAAAAFm0xLWZha2UtaG9zdC10ZXN0LW9ubHkBAgMEBQYH
-----END OPENSSH PRIVATE KEY-----"#;

/// Fake server host key #2 — for the changed-host-key refusal test.
pub const HOST_KEY_2_PEM: &str = r#"-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDCl6smvo8Por8AtUVlCNujPSm0A3bLJDXNZINNVZAnjwAAAKB38eoJd/Hq
CQAAAAtzc2gtZWQyNTUxOQAAACDCl6smvo8Por8AtUVlCNujPSm0A3bLJDXNZINNVZAnjw
AAAEDRdL7F4Xd0Xvl1irIqkHTJDM7tGbwk42YUNJljtVCT2sKXqya+jw+ivwC1RWUI26M9
KbQDdsskNc1kg01VkCePAAAAF20xLWZha2UtaG9zdDItdGVzdC1vbmx5AQIDBAUG
-----END OPENSSH PRIVATE KEY-----"#;

/// Client key for the OpenSSH-flavor key-file auth path.
pub const CLIENT_KEY_PEM: &str = r#"-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDV4aqlPiL59VF3ix1lTjMYU7RZky02fUcdKU3W8j7/bQAAAKDTYtU502LV
OQAAAAtzc2gtZWQyNTUxOQAAACDV4aqlPiL59VF3ix1lTjMYU7RZky02fUcdKU3W8j7/bQ
AAAEBMDCx3pwiLwDRBqCN1yxhsi5YC/lz7WAgGBiQeKZ96XdXhqqU+Ivn1UXeLHWVOMxhT
tFmTLTZ9Rx0pTdbyPv9tAAAAGG0xLWZha2UtY2xpZW50LXRlc3Qtb25seQECAwQF
-----END OPENSSH PRIVATE KEY-----"#;

fn decode_test_key(pem: &str) -> PrivateKey {
    decode_secret_key(pem, None).expect("test key decodes")
}

// ---------------------------------------------------------------------------
// Config + lifecycle
// ---------------------------------------------------------------------------

/// Knobs for one fake server instance.
pub struct FakeConfig {
    /// Temp dir served as `/`. Created by the caller (see `temp_root`).
    pub root: PathBuf,
    /// Full SSH identification string, e.g. `SSH-2.0-TailscaleSSH`.
    pub banner: String,
    /// Tailscale-flavor auth: accept `none`. When false the client must use
    /// a key file (OpenSSH-flavor path).
    pub accept_none_auth: bool,
    /// Advertise `hardlink@openssh.com` in the server VERSION reply.
    pub advertise_hardlink: bool,
    /// Advertise but answer OpUnsupported (M0: advertisement ≠ semantics).
    pub hardlink_lies: bool,
    /// Advertise `posix-rename@openssh.com`.
    pub advertise_posix_rename: bool,
    /// Vanilla RENAME over an existing destination: replace (Tailscale SSH,
    /// M0-measured) vs refuse with `file exists` (OpenSSH, M0-measured).
    pub rename_replaces: bool,
    /// Hostility: after this many written bytes, writes fail with
    /// PermissionDenied — exercises the failed-exit cleanup path.
    pub fail_writes_after: Option<u64>,
    /// Which embedded test host key the server presents.
    pub host_key_pem: &'static str,
    /// Hostility (B3): every READDIR gains a synthetic entry named
    /// "caf\u{FFFD}" — byte-for-byte what a real server's raw invalid-UTF-8
    /// name (`caf\xe9`) becomes after russh-sftp's LOSSY client decode
    /// (String::from_utf8_lossy). macOS libc refuses paths with invalid
    /// UTF-8 (EILSEQ), so the fixture injects the post-decode form directly:
    /// the client-visible wire bytes are identical to production. The name
    /// does not resolve (no such file), exactly like a lossy guess.
    pub non_utf8_names: bool,
    /// Hostility (C1): every READDIR gains a synthetic entry named
    /// "bad\nname" — a control-character name. Win32 filesystems can never
    /// hold one (0x01-0x1F forbidden in filenames), so the real-file
    /// control-char test is Unix-only; this knob injects the name directly
    /// into the wire response (same pattern as `non_utf8_names`), keeping
    /// the listing skip-and-count rule testable on EVERY platform.
    pub control_char_names: bool,
    /// Hostility (B6/B6-R): with a Windows banner, READLINK on any directory
    /// whose name contains "junction" answers BAD_MESSAGE — exactly what
    /// Win32-OpenSSH returns for IO_REPARSE_TAG_MOUNT_POINT (EINVAL →
    /// SSH2_FX_BAD_MESSAGE; M0 room-6 evidence). Plain dirs/files answer
    /// per-flavor WITHOUT this knob (see readlink handler): Windows →
    /// generic FAILURE, POSIX → BAD_MESSAGE.
    pub simulate_junctions: bool,
    /// Instrumentation (B6-R): counts READLINK calls whose target is a
    /// directory — the POSIX-never-probes assertion reads this.
    pub dir_readlinks: Arc<AtomicU64>,
    /// Hostility (B6-R): READLINK on the file/dir with this name answers
    /// NO_SUCH_FILE even though it exists — a directory vanishing between
    /// listing and probe (race tolerance).
    pub vanish_readlink_for: Option<String>,
    /// Hostility (W3): the FIRST lstat of the file with this name reports a
    /// size `delta` bytes larger than reality; later lstats tell the truth —
    /// simulates a file changing between plan and stream (size-changed check).
    pub lie_size_once: Option<(String, u64)>,
    /// Hostility (W3): reads of the file with this name immediately return
    /// EOF — simulates a file shrinking mid-copy ("got shorter" path).
    pub shrink_reads_for: Option<String>,
    /// Hostility (W3): every CLOSE fails — proves a primary read/stream error
    /// is never masked by the close error (spike limit #1).
    pub fail_close: bool,
    /// Hostility (M2a): reject EVERY auth method (none AND publickey, files
    /// and agent) — a deterministic one-side auth failure regardless of the
    /// ambient developer credentials (real ~/.ssh keys, ssh-agent identities)
    /// the production connect flow would otherwise try.
    pub reject_all_auth: bool,
}

impl FakeConfig {
    /// Tailscale-SSH-like defaults (the fleet's common case).
    pub fn tailscale(root: PathBuf) -> Self {
        Self {
            root,
            banner: "SSH-2.0-TailscaleSSH".to_string(),
            accept_none_auth: true,
            advertise_hardlink: true,
            hardlink_lies: false,
            advertise_posix_rename: false,
            rename_replaces: true, // M0: Tailscale SSH REPLACED destinations
            fail_writes_after: None,
            host_key_pem: HOST_KEY_PEM,
            non_utf8_names: false,
            control_char_names: false,
            simulate_junctions: false,
            dir_readlinks: Arc::new(AtomicU64::new(0)),
            vanish_readlink_for: None,
            lie_size_once: None,
            shrink_reads_for: None,
            fail_close: false,
            reject_all_auth: false,
        }
    }
    /// macOS/Windows OpenSSH-like defaults (M0: refused replacing renames).
    pub fn openssh(root: PathBuf) -> Self {
        Self {
            root,
            banner: "SSH-2.0-OpenSSH_9.6".to_string(),
            accept_none_auth: false,
            advertise_hardlink: true,
            hardlink_lies: false,
            advertise_posix_rename: true,
            rename_replaces: false,
            fail_writes_after: None,
            host_key_pem: HOST_KEY_PEM,
            non_utf8_names: false,
            control_char_names: false,
            simulate_junctions: false,
            dir_readlinks: Arc::new(AtomicU64::new(0)),
            vanish_readlink_for: None,
            lie_size_once: None,
            shrink_reads_for: None,
            fail_close: false,
            reject_all_auth: false,
        }
    }
}

/// A running fake server. Dropping it aborts the accept loop and removes
/// the temp root.
pub struct FakeServer {
    pub addr: SocketAddr,
    pub root: PathBuf,
    /// The server's host key in the REAL `tailscale status --json`
    /// sshHostKeys entry format: a full "<algorithm> <base64>" line
    /// (M0 evidence/fleet.json). B4 regression fixture.
    pub host_key_line: String,
    task: JoinHandle<()>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl FakeServer {
    /// Bind 127.0.0.1:0 and serve until dropped.
    pub async fn start(mut cfg: FakeConfig) -> FakeServer {
        // macOS resolves /var -> /private/var: canonicalize so realpath
        // results strip_prefix cleanly back to virtual paths.
        cfg.root = std::fs::canonicalize(&cfg.root).expect("canonicalize fake root");
        let host_key = decode_test_key(cfg.host_key_pem);
        // tailscale status --json sshHostKeys entries are FULL LINES:
        // "<algorithm> <base64>" (M0 evidence/fleet.json — verified format;
        // the old blob-only fixture encoded B4's bug).
        let host_key_line = host_key
            .public_key()
            .to_openssh()
            .expect("pubkey openssh encoding")
            .trim()
            .to_string();
        let ssh_cfg = Arc::new(russh::server::Config {
            server_id: SshId::Standard(Cow::Owned(cfg.banner.clone())),
            keys: vec![host_key],
            auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
            auth_rejection_time: std::time::Duration::from_secs(1),
            inactivity_timeout: Some(std::time::Duration::from_secs(120)),
            ..Default::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let root = cfg.root.clone();
        let cfg = Arc::new(cfg);
        let mut factory = FakeFactory(cfg.clone());
        let task = tokio::spawn(async move {
            let running = factory.run_on_socket(ssh_cfg, &listener);
            let _ = running.await;
        });
        FakeServer {
            addr,
            root,
            host_key_line,
            task,
        }
    }

    /// Sniff the banner, then run the REAL production connect() against this
    /// server — the full stack minus the network. `trust_dir` persists pins
    /// across calls so host-key-change tests can reuse it.
    pub async fn connect(&self, trust_dir: &Path) -> RusshTransport {
        self.connect_as(trust_dir, "fakeuser", &[]).await
    }

    pub async fn connect_as(
        &self,
        trust_dir: &Path,
        user: &str,
        key_files: &[PathBuf],
    ) -> RusshTransport {
        self.try_connect(trust_dir, user, key_files, Vec::new())
            .await
            .expect("connect to fake server")
    }

    /// Fallible connect for refusal-path tests (host-key change, cross-check
    /// mismatch, auth failure). Sniffs the banner, then runs the production
    /// `session::connect` end to end.
    pub async fn try_connect(
        &self,
        trust_dir: &Path,
        user: &str,
        key_files: &[PathBuf],
        tailscale_host_keys: Vec<String>,
    ) -> Result<RusshTransport, crate::browser::BrowserError> {
        let (flavor, _banner) = sniff_banner("127.0.0.1", self.addr.port())
            .await
            .expect("fake server banner");
        let params = ConnectParams {
            host: "127.0.0.1".to_string(),
            port: self.addr.port(),
            user: user.to_string(),
            flavor,
            trust: Arc::new(TrustStore::new(trust_dir)),
            tailscale_host_keys,
            key_files: key_files.to_vec(),
        };
        crate::browser::session::connect(params).await
    }
}

/// Random-suffixed temp dir (never a fixed name — invariant: random scratch
/// tags, ORCHESTRATION.md).
pub fn temp_root(tag: &str) -> PathBuf {
    use rand::Rng as _;
    let n: u64 = rand::rng().random();
    let dir = std::env::temp_dir().join(format!(
        "taildrop-m1-fake-{tag}-{}-{n:016x}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp root");
    dir
}

/// Generate an ephemeral Ed25519 key, write it OpenSSH-format 0600 to a
/// random temp path, return (path, guard that deletes it on drop).
pub fn write_temp_key(tag: &str) -> (PathBuf, KeyGuard) {
    use rand::Rng as _;
    let pem = CLIENT_KEY_PEM;
    let n: u64 = rand::rng().random();
    let path = std::env::temp_dir().join(format!(
        "taildrop-m1-key-{tag}-{}-{n:016x}",
        std::process::id()
    ));
    std::fs::write(&path, pem.as_bytes()).expect("write key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    (path.clone(), KeyGuard(path))
}

pub struct KeyGuard(pub PathBuf);
impl Drop for KeyGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// SSH server side (russh)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeFactory(Arc<FakeConfig>);

impl russh::server::Server for FakeFactory {
    type Handler = FakeSession;
    fn new_client(&mut self, _: Option<SocketAddr>) -> FakeSession {
        FakeSession {
            cfg: self.0.clone(),
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct FakeSession {
    cfg: Arc<FakeConfig>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for FakeSession {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(if self.cfg.accept_none_auth && !self.cfg.reject_all_auth {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if self.cfg.reject_all_auth {
            // M2a: deterministic one-side auth failure — refuse every
            // credential (ambient developer keys must not pass the test).
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }
        // Any key accepted: the CLIENT side (key loading, method order,
        // never-password) is what's under test, not real authorization.
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let channel = self
                .channels
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&channel_id)
                .expect("sftp subsystem on an unopened channel");
            session.channel_success(channel_id)?;
            let handler = SftpHandler::new(self.cfg.clone());
            russh_sftp::server::run(channel.into_stream(), handler).await;
        } else {
            // Shell/exec grants are refused like a tailnet policy without
            // the sftp tag — the client's subsystem_refused path sees it.
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SFTP server side (russh-sftp Handler over std::fs)
// ---------------------------------------------------------------------------

enum HEntry {
    File(PathBuf),
    Dir { files: Vec<File>, sent: bool },
}

struct SftpHandler {
    cfg: Arc<FakeConfig>,
    handles: HashMap<String, HEntry>,
    next_handle: u64,
    written: u64,
    /// One-shot size lie (W3): taken on the first matching lstat.
    size_lie: Option<(String, u64)>,
}

impl SftpHandler {
    fn new(cfg: Arc<FakeConfig>) -> Self {
        Self {
            size_lie: cfg.lie_size_once.clone(),
            cfg,
            handles: HashMap::new(),
            next_handle: 1,
            written: 0,
        }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        self.cfg.root.join(path.trim_start_matches('/'))
    }

    fn to_virt(&self, abs: &Path) -> String {
        match abs.strip_prefix(&self.cfg.root) {
            Ok(rel) if rel.as_os_str().is_empty() => "/".to_string(),
            Ok(rel) => format!("/{}", rel.display()),
            // Canonicalized path escaped the root (symlink out): present it
            // as-is; production guards, not the fake, decide trust.
            Err(_) => format!("/{}", abs.display()),
        }
    }

    fn alloc(&mut self, entry: HEntry) -> String {
        let h = format!("h{}", self.next_handle);
        self.next_handle += 1;
        self.handles.insert(h.clone(), entry);
        h
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    let secs = |t: io::Result<SystemTime>| {
        t.ok()
            .and_then(|s| s.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32)
    };
    FileAttributes {
        size: Some(meta.len()),
        uid: None,
        user: None,
        gid: None,
        group: None,
        permissions: Some(mode_of(meta)),
        atime: secs(meta.accessed()),
        mtime: secs(meta.modified()),
    }
}

/// Unix: the real st_mode (type + permission bits), exactly what OpenSSH
/// serves.
#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

/// Windows: there is no st_mode. Synthesize the OpenSSH-style mode the wire
/// protocol expects so `FileAttributes::file_type()` classifies entries —
/// the SFTP type bits are what the client trusts, and omitting permissions
/// would mark every node unknown. Permission bits follow the same readonly
/// convention as `listing::perm_of` (Win32-OpenSSH synthesizes from the ACL
/// the same way).
#[cfg(not(unix))]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    let ft = meta.file_type();
    let type_bits = if ft.is_dir() {
        0o040_000 // S_IFDIR
    } else if ft.is_symlink() {
        0o120_000 // S_IFLNK
    } else {
        0o100_000 // S_IFREG
    };
    let perm_bits = if ft.is_dir() {
        0o755 // dirs must appear traversable
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    type_bits | perm_bits
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

/// A Failure status with a CUSTOM message. Returned as Ok(Status) — the
/// protocol allows any status as a reply — because Err(StatusCode) can only
/// carry the code's generic text, and the client maps `Failure` + a message
/// containing "exist" to ErrorKind::Exists (M0: Tailscale says "file
/// exists" on hardlink collision).
fn failure(id: u32, msg: &str) -> Status {
    Status {
        id,
        status_code: StatusCode::Failure,
        error_message: msg.to_string(),
        language_tag: "en-US".to_string(),
    }
}

fn map_io(e: io::Error) -> StatusCode {
    match e.kind() {
        io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

/// Positional read — Unix keeps the real `pread` (`FileExt::read_at`);
/// Windows seeks+reads, equivalent here because every call opens its own
/// handle (no shared file offset to preserve).
fn pread(f: &std::fs::File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_at(buf, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let f = &mut &*f;
        f.seek(SeekFrom::Start(offset))?;
        f.read(buf)
    }
}

/// Positional write-all — same portability split as [`pread`].
fn pwrite_all(f: &std::fs::File, data: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(data, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Seek, SeekFrom, Write};
        let f = &mut &*f;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)
    }
}

/// Best-effort mode application on a freshly opened file. Unix: real chmod.
/// Windows: mode bits have no local equivalent (and setting readonly would
/// break remove_file — Windows refuses to delete read-only files), so the
/// fixture ignores them, like a server that ignores SETSTAT permissions.
#[cfg(unix)]
fn apply_open_perms(f: &std::fs::File, attrs: &FileAttributes) {
    if let Some(perm) = attrs.permissions {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(perm & 0o7777));
    }
}

#[cfg(not(unix))]
fn apply_open_perms(_f: &std::fs::File, _attrs: &FileAttributes) {}

/// SSH_FXP_EXTENDED data is a byte string; extension payloads are sequences
/// of SFTP strings (u32be length + bytes).
fn ext_string(data: &[u8]) -> Option<(String, &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if data.len() < 4 + len {
        return None;
    }
    let s = String::from_utf8_lossy(&data[4..4 + len]).to_string();
    Some((s, &data[4 + len..]))
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _client_exts: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        let mut extensions = HashMap::new();
        if self.cfg.advertise_hardlink {
            extensions.insert("hardlink@openssh.com".to_string(), "1".to_string());
        }
        if self.cfg.advertise_posix_rename {
            extensions.insert("posix-rename@openssh.com".to_string(), "1".to_string());
        }
        // A name russh-sftp's high-level client drops — proves the RAW
        // VERSION capture in session.rs keeps unknown extensions (M0).
        extensions.insert("fake-vendor@taildrop.test".to_string(), "7".to_string());
        Ok(Version {
            version: 3,
            extensions,
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename);
        let opts: std::fs::OpenOptions = pflags.into();
        match opts.open(&path) {
            Ok(f) => {
                apply_open_perms(&f, &attrs);
                let h = self.alloc(HEntry::File(path));
                Ok(Handle { id, handle: h })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // EXCLUDE collision. open() can only answer Handle-or-code,
                // so the message is the generic one; the client treats any
                // exclusive-create failure as refuse (never overwrite).
                let _ = id;
                Err(StatusCode::Failure)
            }
            Err(e) => Err(map_io(e)),
        }
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        if self.cfg.fail_close {
            // W3 hostility: close fails — the caller must still surface the
            // PRIMARY error, never this one (spike limit #1).
            return Err(StatusCode::Failure);
        }
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let path = match self.handles.get(&handle) {
            Some(HEntry::File(p)) => p.clone(),
            _ => return Err(StatusCode::Failure),
        };
        // W3 hostility: this file "shrank" — every read hits EOF at once.
        if self
            .cfg
            .shrink_reads_for
            .as_ref()
            .map(|n| {
                path.file_name()
                    .map(|f| f.to_string_lossy() == n.as_str())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        {
            return Ok(Data {
                id,
                data: Vec::new(),
            });
        }
        let f = std::fs::File::open(&path).map_err(map_io)?;
        let size = f.metadata().map_err(map_io)?.len();
        if offset >= size {
            return Err(StatusCode::Eof);
        }
        let want = std::cmp::min(len as u64, size - offset) as usize;
        let mut buf = vec![0u8; want];
        let n = pread(&f, &mut buf, offset).map_err(map_io)?;
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        if let Some(limit) = self.cfg.fail_writes_after {
            self.written += data.len() as u64;
            if self.written > limit {
                return Err(StatusCode::PermissionDenied);
            }
        }
        let path = match self.handles.get(&handle) {
            Some(HEntry::File(p)) => p.clone(),
            _ => return Err(StatusCode::Failure),
        };
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(map_io)?;
        pwrite_all(&f, &data, offset).map_err(map_io)?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let resolved = self.resolve(&path);
        let meta = std::fs::symlink_metadata(&resolved).map_err(map_io)?;
        let mut attrs = attrs_of(&meta);
        // W3 hostility: first lstat of the target name lies about the size.
        if let Some((_name, delta)) = self.size_lie.take_if(|(n, _)| {
            resolved
                .file_name()
                .map(|f| f.to_string_lossy() == n.as_str())
                .unwrap_or(false)
        }) {
            attrs.size = attrs.size.map(|sz| sz + delta);
        }
        Ok(Attrs { id, attrs })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.resolve(&path)).map_err(map_io)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let path = match self.handles.get(&handle) {
            Some(HEntry::File(p)) => p.clone(),
            _ => return Err(StatusCode::Failure),
        };
        let meta = std::fs::symlink_metadata(&path).map_err(map_io)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.apply_attrs(&self.resolve(&path), &attrs);
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if let Some(HEntry::File(p)) = self.handles.get(&handle) {
            let p = p.clone();
            self.apply_attrs(&p, &attrs);
        }
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let resolved = self.resolve(&path);
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&resolved).map_err(map_io)? {
            let entry = entry.map_err(map_io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = std::fs::symlink_metadata(entry.path()).map_err(map_io)?;
            files.push(File::new(name, attrs_of(&meta)));
        }
        // B3 fixture: serve a lossy-decoded non-UTF-8 name (see the
        // FakeConfig knob). Regular file, 17 bytes; resolves to nothing.
        if self.cfg.non_utf8_names {
            files.push(File::new(
                "caf\u{FFFD}".to_string(),
                FileAttributes {
                    size: Some(17),
                    uid: None,
                    user: None,
                    gid: None,
                    group: None,
                    permissions: Some(0o100644),
                    atime: Some(1_700_000_000),
                    mtime: Some(1_700_000_000),
                },
            ));
        }
        // C1 fixture: a control-character name — Win32 hosts cannot create
        // one on disk (0x01-0x1F forbidden), so it is injected synthetically
        // to keep the listing skip-and-count rule covered on all platforms.
        // Regular file, 1 byte; resolves to nothing.
        if self.cfg.control_char_names {
            files.push(File::new(
                "bad\nname".to_string(),
                FileAttributes {
                    size: Some(1),
                    uid: None,
                    user: None,
                    gid: None,
                    group: None,
                    permissions: Some(0o100644),
                    atime: Some(1_700_000_000),
                    mtime: Some(1_700_000_000),
                },
            ));
        }
        // Deterministic order (the client sorts, but stability helps diffs).
        files.sort_by(|a, b| a.filename.cmp(&b.filename));
        let h = self.alloc(HEntry::Dir { files, sent: false });
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(HEntry::Dir { files, sent }) => {
                if *sent {
                    return Err(StatusCode::Eof);
                }
                *sent = true;
                Ok(Name {
                    id,
                    files: files.clone(),
                })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.resolve(&filename)).map_err(map_io)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let resolved = self.resolve(&path);
        match std::fs::create_dir(&resolved) {
            Ok(()) => {
                self.apply_attrs(&resolved, &attrs);
                Ok(ok(id))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Custom "file exists" message — the client maps it to
                // ErrorKind::Exists (exclusive staging semantics).
                Ok(failure(id, "file exists"))
            }
            Err(e) => Err(map_io(e)),
        }
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.resolve(&path)).map_err(map_io)?;
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let resolved = self.resolve(&path);
        let canon = std::fs::canonicalize(&resolved).map_err(map_io)?;
        Ok(Name {
            id,
            files: vec![File::new(
                self.to_virt(&canon),
                attrs_of(&std::fs::symlink_metadata(&canon).map_err(map_io)?),
            )],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let from = self.resolve(&oldpath);
        let to = self.resolve(&newpath);
        if !self.cfg.rename_replaces && to.symlink_metadata().is_ok() {
            // OpenSSH flavor (M0-measured): vanilla rename REFUSES to
            // replace an existing destination.
            return Ok(failure(id, "file exists"));
        }
        std::fs::rename(&from, &to).map_err(map_io)?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let resolved = self.resolve(&path);
        // B6-R fixture — answers like the REAL servers, not like the probe
        // wishes them to (R3: Win32-OpenSSH source walk + the reviewer's
        // POSIX /usr/libexec/sftp-server measurements):
        //   symlink                        → NAME (both flavors)
        //   plain dir/file, Windows flavor → generic FAILURE
        //     (fileio_readlink: ERROR_NOT_A_REPARSE_POINT 4390 is unmapped by
        //      errno_from_Win32Error → errno_to_portable default → SSH2_FX_FAILURE)
        //   plain dir/file, POSIX flavor   → BAD_MESSAGE (EINVAL)
        //   junction (Windows + simulate_junctions + name contains "junction")
        //                                    → BAD_MESSAGE (MOUNT_POINT → EINVAL)
        //   missing path                   → NO_SUCH_FILE
        if self
            .cfg
            .vanish_readlink_for
            .as_ref()
            .map(|n| {
                resolved
                    .file_name()
                    .map(|f| f.to_string_lossy() == n.as_str())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        {
            return Err(StatusCode::NoSuchFile);
        }
        let windows_flavor = self.cfg.banner.contains("Windows");
        let target = match std::fs::read_link(&resolved) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StatusCode::NoSuchFile),
            // Exists, not a symlink: a plain dir or file. Host-agnostic
            // match — Unix read_link fails EINVAL (InvalidInput), Windows
            // fails ERROR_NOT_A_REPARSE_POINT (Uncategorized) — so the
            // branch keys on "the path still exists", not on the error kind.
            Err(_) if resolved.symlink_metadata().is_ok() => {
                if resolved.is_dir() {
                    self.cfg.dir_readlinks.fetch_add(1, Ordering::SeqCst);
                    if windows_flavor
                        && self.cfg.simulate_junctions
                        && resolved
                            .file_name()
                            .map(|n| n.to_string_lossy().contains("junction"))
                            .unwrap_or(false)
                    {
                        return Err(StatusCode::BadMessage);
                    }
                }
                return Err(if windows_flavor {
                    StatusCode::Failure
                } else {
                    StatusCode::BadMessage
                });
            }
            Err(e) => return Err(map_io(e)),
        };
        Ok(Name {
            id,
            files: vec![File::new(
                target.to_string_lossy().to_string(),
                FileAttributes::dummy(),
            )],
        })
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        let link = self.resolve(&linkpath);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&targetpath, &link).map_err(map_io)?;
        }
        #[cfg(windows)]
        {
            // Windows separates file and dir symlinks; pick by the target's
            // type (resolved against the link's parent, same view as POSIX).
            // Creation needs Developer Mode / SeCreateSymbolicLinkPrivilege;
            // when it fails the error travels back and the copy engine treats
            // it as "destination can't hold links" (skip+count) — honest.
            use std::os::windows::fs::{symlink_dir, symlink_file};
            let target = Path::new(&targetpath);
            let resolved = if target.is_absolute() {
                target.to_path_buf()
            } else {
                link.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
                    .join(target)
            };
            let r = if resolved.is_dir() {
                symlink_dir(&targetpath, &link)
            } else {
                symlink_file(&targetpath, &link)
            };
            r.map_err(map_io)?;
        }
        Ok(ok(id))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        match request.as_str() {
            "hardlink@openssh.com" => {
                if self.cfg.hardlink_lies {
                    // Advertised but unsupported — the exact M0 distinction:
                    // the client must treat this as UNSUPPORTED (documented
                    // downgrade), never as a silent placement failure.
                    return Err(StatusCode::OpUnsupported);
                }
                let (old, rest) = ext_string(&data).ok_or(StatusCode::BadMessage)?;
                let (new, _) = ext_string(rest).ok_or(StatusCode::BadMessage)?;
                match std::fs::hard_link(self.resolve(&old), self.resolve(&new)) {
                    Ok(()) => Ok(Packet::Status(ok(id))),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        // M0-proven on the fleet: second link to the same
                        // final name FAILS — atomic no-replace. Tailscale's
                        // message says "file exists" (mapped to Exists).
                        Ok(Packet::Status(failure(id, "file exists")))
                    }
                    Err(e) => Err(map_io(e)),
                }
            }
            "posix-rename@openssh.com" => {
                if !self.cfg.advertise_posix_rename {
                    return Err(StatusCode::OpUnsupported);
                }
                let (old, rest) = ext_string(&data).ok_or(StatusCode::BadMessage)?;
                let (new, _) = ext_string(rest).ok_or(StatusCode::BadMessage)?;
                // posix-rename@openssh.com REPLACES (M0) — unlike vanilla
                // rename on OpenSSH flavors.
                std::fs::rename(self.resolve(&old), self.resolve(&new)).map_err(map_io)?;
                Ok(Packet::Status(ok(id)))
            }
            _ => Err(StatusCode::OpUnsupported),
        }
    }
}

impl SftpHandler {
    /// Apply the subset of FileAttributes production uses: permissions and
    /// mtime (best effort, like a real server that may ignore SETSTAT).
    fn apply_attrs(&self, path: &Path, attrs: &FileAttributes) {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::PermissionsExt;
            if let Some(perm) = attrs.permissions {
                let _ =
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm & 0o7777));
            }
            if let Some(mtime) = attrs.mtime {
                let ts = libc::timespec {
                    tv_sec: i64::from(mtime),
                    tv_nsec: 0,
                };
                let times = [ts, ts];
                unsafe {
                    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap_or_default();
                    if !c.is_empty() {
                        libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0);
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            // Windows: SFTP mode bits have no local equivalent (and setting
            // readonly would break staging cleanup — Windows refuses to
            // delete read-only files), so permissions are ignored best
            // effort, like a server that ignores SETSTAT. mtime is portable
            // via std FileTimes.
            let _ = attrs.permissions;
            if let Some(mtime) = attrs.mtime {
                set_mtime_best_effort(path, u64::from(mtime));
            }
        }
    }
}

/// Windows-only best-effort mtime setter for the fake server (std
/// `FileTimes`; dirs are opened read-only — if the platform refuses, the
/// SETSTAT is ignored exactly like a real server may).
#[cfg(not(unix))]
fn set_mtime_best_effort(path: &Path, secs: u64) {
    let when = UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let times = std::fs::FileTimes::new()
        .set_accessed(when)
        .set_modified(when);
    let r = if path.is_dir() {
        std::fs::File::open(path).and_then(|f| f.set_times(times))
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_times(times))
    };
    let _ = r;
}

// ---------------------------------------------------------------------------
// Tests: transport, extension negotiation, trust, hostile-server behavior,
// placement tiers, cleanup ordering. ALL over loopback against the fake —
// no fleet, no network (acceptance gate 6).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::listing;
    use crate::browser::{ErrorKind, ServerFlavor};
    // Everything below drives `transfer::run_copy` through its local side,
    // which is Unix-only in M1 (whole module #[cfg(unix)], callers fail
    // closed) — imports used solely by those tests are gated with them.
    // SftpTransport stays ungated: the cross-platform transport tests call
    // its methods on the concrete RusshTransport (trait must be in scope).
    // M2a: the remote→remote group also runs on ALL platforms (no local
    // side involved), so the shared engine helpers are ungated too.
    use crate::browser::agent::{AgentCap, AgentCapCache, ExecChannel};
    use crate::browser::transfer::{self, Side, CHUNK};
    use crate::browser::transport::SftpTransport;
    use crate::browser::transport::{RemoteNode, RemoteReader, RemoteWriter};
    use crate::browser::{BrowserError, CopyRequest, STAGE_PREFIX};
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    // ---- helpers ----

    type Emit = Arc<dyn Fn(u8, String, String) + Send + Sync>;

    fn collector() -> (Emit, Arc<Mutex<Vec<u8>>>) {
        let v: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let v2 = v.clone();
        (
            Arc::new(move |pct: u8, _rate: String, _file: String| {
                v2.lock().unwrap().push(pct);
            }),
            v,
        )
    }

    #[cfg(unix)] // Unix-only: run_copy local side (see imports note)
    fn req(src_dir: &str, dst_dir: &str, names: &[&str], replace: bool) -> CopyRequest {
        CopyRequest {
            src_target: "local".to_string(),
            src_user: None,
            src_dir: src_dir.to_string(),
            dst_target: "fakehost".to_string(),
            dst_user: Some("fakeuser".to_string()),
            dst_dir: dst_dir.to_string(),
            names: names.iter().map(|s| s.to_string()).collect(),
            replace,
        }
    }

    /// Staging dirs (anywhere one level under `dir`) that survived — the
    /// cleanup invariant says this must ALWAYS be empty after run_copy
    /// returns, on every exit path (done / failed / cancelled).
    fn stage_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with(STAGE_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn write_remote_file(t: &RusshTransport, path: &str, bytes: &[u8]) {
        let mut w = t.create_exclusive(path, 0o644).await.expect("create");
        w.write_all(bytes).await.expect("write");
        w.flush().await.expect("flush");
        w.close().await.expect("close");
    }

    /// Transport wrapper that cancels the token after N written bytes —
    /// makes mid-copy cancellation deterministic (no sleeps, no races).
    /// M2a: also drives the remote→remote cancel test on ALL platforms
    /// (wraps the DESTINATION transport; no local side involved).
    struct CancelAfterBytes<'a> {
        inner: &'a RusshTransport,
        cancel: CancellationToken,
        threshold: u64,
        written: Arc<AtomicU64>,
    }

    struct CountingWriter {
        inner: Box<dyn RemoteWriter>,
        cancel: CancellationToken,
        threshold: u64,
        written: Arc<AtomicU64>,
    }

    #[async_trait]
    impl RemoteWriter for CountingWriter {
        async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError> {
            self.inner.write_all(data).await?;
            let n = self.written.fetch_add(data.len() as u64, Ordering::SeqCst) + data.len() as u64;
            if n >= self.threshold {
                self.cancel.cancel();
            }
            Ok(())
        }
        async fn flush(&mut self) -> Result<(), BrowserError> {
            self.inner.flush().await
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            self.inner.close().await
        }
    }

    #[async_trait]
    impl SftpTransport for CancelAfterBytes<'_> {
        async fn realpath(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.realpath(path).await
        }
        async fn read_dir(&self, path: &str) -> Result<Vec<RemoteNode>, BrowserError> {
            self.inner.read_dir(path).await
        }
        async fn stat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.stat(path).await
        }
        async fn lstat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.lstat(path).await
        }
        async fn mkdir(&self, path: &str, perm: u32) -> Result<(), BrowserError> {
            self.inner.mkdir(path, perm).await
        }
        async fn remove_file(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_file(path).await
        }
        async fn remove_dir(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_dir(path).await
        }
        async fn rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.rename(from, to).await
        }
        async fn posix_rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.posix_rename(from, to).await
        }
        async fn hardlink(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.hardlink(from, to).await
        }
        async fn symlink(&self, target: &str, path: &str) -> Result<(), BrowserError> {
            self.inner.symlink(target, path).await
        }
        async fn read_link(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.read_link(path).await
        }
        async fn open_read(&self, path: &str) -> Result<Box<dyn RemoteReader>, BrowserError> {
            self.inner.open_read(path).await
        }
        async fn create_exclusive(
            &self,
            path: &str,
            perm: u32,
        ) -> Result<Box<dyn RemoteWriter>, BrowserError> {
            let inner = self.inner.create_exclusive(path, perm).await?;
            Ok(Box::new(CountingWriter {
                inner,
                cancel: self.cancel.clone(),
                threshold: self.threshold,
                written: self.written.clone(),
            }))
        }
        async fn set_attrs(
            &self,
            path: &str,
            mode: Option<u32>,
            mtime: Option<i64>,
        ) -> Result<(), BrowserError> {
            self.inner.set_attrs(path, mode, mtime).await
        }
        fn has_hardlink(&self) -> bool {
            self.inner.has_hardlink()
        }
        fn has_posix_rename(&self) -> bool {
            self.inner.has_posix_rename()
        }
        fn extensions(&self) -> &BTreeMap<String, String> {
            self.inner.extensions()
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            Ok(()) // inner is borrowed; the test closes it
        }
    }

    // ---- Group A: flavor, auth, trust, extension negotiation ----

    #[tokio::test]
    async fn tailscale_flavor_extensions_and_home() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("tsA"))).await;
        let trust = temp_root("trustA");
        let t = srv.connect(&trust).await;
        assert!(matches!(t.flavor(), ServerFlavor::TailscaleSsh));
        let exts = t.extensions();
        // Advertised extension captured with its raw version string.
        assert_eq!(
            exts.get("hardlink@openssh.com").map(String::as_str),
            Some("1")
        );
        // M0 fact: russh-sftp's high-level session drops unknown extension
        // names — the RAW VERSION capture must keep them.
        assert_eq!(
            exts.get("fake-vendor@taildrop.test").map(String::as_str),
            Some("7")
        );
        // Exec-free home discovery (realpath(".")) through the real wire.
        assert_eq!(t.realpath(".").await.unwrap(), "/");
        assert_eq!(t.user(), "fakeuser");
        Box::new(t).close().await.unwrap();
        // Trust pin written for the tailscale flavor store.
        assert!(trust.join("pins.tailscale-ssh.json").exists());
    }

    #[tokio::test]
    async fn openssh_flavor_keyfile_auth() {
        let srv = FakeServer::start(FakeConfig::openssh(temp_root("osA"))).await;
        let trust = temp_root("trustB");
        let (key, _guard) = write_temp_key("osA");
        let t = srv.connect_as(&trust, "fakeuser", &[key]).await;
        assert!(matches!(t.flavor(), ServerFlavor::Openssh));
        assert!(t.has_posix_rename());
        assert!(t.has_hardlink());
        Box::new(t).close().await.unwrap();
        assert!(trust.join("pins.openssh.json").exists());
    }

    #[tokio::test]
    async fn host_key_change_is_refused() {
        let trust = temp_root("trustC");
        let srv1 = FakeServer::start(FakeConfig::tailscale(temp_root("hk1"))).await;
        let t = srv1.connect(&trust).await;
        Box::new(t).close().await.unwrap();
        drop(srv1);
        // Same host string ("127.0.0.1"), DIFFERENT host key: refuse-on-change.
        let mut cfg = FakeConfig::tailscale(temp_root("hk2"));
        cfg.host_key_pem = HOST_KEY_2_PEM;
        let srv2 = FakeServer::start(cfg).await;
        let err = srv2
            .try_connect(&trust, "fakeuser", &[], Vec::new())
            .await
            .expect_err("changed host key must be refused");
        assert_eq!(err.kind, ErrorKind::Hostkey);
    }

    #[tokio::test]
    async fn tailscale_ssh_host_keys_crosscheck() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("xc"))).await;
        let alg = srv.host_key_line.split_whitespace().next().unwrap();
        let blob = srv.host_key_line.split_whitespace().nth(1).unwrap();

        // Wrong published blob (full-line format): fail closed.
        let trust1 = temp_root("trustD1");
        let err = srv
            .try_connect(
                &trust1,
                "fakeuser",
                &[],
                vec![format!("{alg} AAAAnot-the-real-blob")],
            )
            .await
            .expect_err("mismatched sshHostKeys must refuse");
        assert_eq!(err.kind, ErrorKind::Hostkey);

        // B4 regression: a BARE-BLOB entry (the old, wrong format) must NOT
        // match — real entries are "<algorithm> <base64>" full lines.
        let trust2 = temp_root("trustD2");
        let err = srv
            .try_connect(&trust2, "fakeuser", &[], vec![blob.to_string()])
            .await
            .expect_err("bare-blob entry must not match a full-line format");
        assert_eq!(err.kind, ErrorKind::Hostkey);

        // Algorithm-confusion guard: right blob under a wrong algorithm.
        let trust3 = temp_root("trustD3");
        let err = srv
            .try_connect(&trust3, "fakeuser", &[], vec![format!("ssh-rsa {blob}")])
            .await
            .expect_err("wrong-algorithm entry must not match");
        assert_eq!(err.kind, ErrorKind::Hostkey);

        // Correct published set (real full-line format, alongside an
        // unrelated entry): accepted, and the pin lands.
        let trust4 = temp_root("trustD4");
        let t = srv
            .try_connect(
                &trust4,
                "fakeuser",
                &[],
                vec![
                    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAunrelated".to_string(),
                    srv.host_key_line.clone(),
                ],
            )
            .await
            .expect("matching full-line sshHostKeys entry accepted");
        Box::new(t).close().await.unwrap();
    }

    // ---- Group B: placement primitives over the wire ----

    #[tokio::test]
    async fn hardlink_no_replace_collision_is_verified() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("hl1"))).await;
        let trust = temp_root("trustE");
        let t = srv.connect(&trust).await;
        write_remote_file(&t, "/f1", b"hello").await;
        // First link staged->final succeeds (M0-proven flow)...
        t.hardlink("/f1", "/f2").await.unwrap();
        assert_eq!(std::fs::read(srv.root.join("f2")).unwrap(), b"hello");
        // ...and a SECOND link to the same final name FAILS as Exists —
        // the atomic no-replace property the placement tier relies on.
        let e = t.hardlink("/f1", "/f2").await.expect_err("collision");
        assert_eq!(e.kind, ErrorKind::Exists);
        // Destination content untouched by the refused link.
        assert_eq!(std::fs::read(srv.root.join("f2")).unwrap(), b"hello");
        // A genuine failure is NOT conflated with Exists: missing source
        // maps to Notfound (failed ≠ exists ≠ unsupported).
        let e2 = t
            .hardlink("/missing", "/f3")
            .await
            .expect_err("missing src");
        assert_eq!(e2.kind, ErrorKind::Notfound);
        // After removing the final name, linking succeeds again.
        t.remove_file("/f2").await.unwrap();
        t.hardlink("/f1", "/f2").await.unwrap();
        Box::new(t).close().await.unwrap();
    }

    #[tokio::test]
    async fn hardlink_not_advertised_is_unsupported() {
        let mut cfg = FakeConfig::tailscale(temp_root("hl2"));
        cfg.advertise_hardlink = false;
        let srv = FakeServer::start(cfg).await;
        let t = srv.connect(&temp_root("trustF")).await;
        assert!(!t.has_hardlink());
        let e = t.hardlink("/a", "/b").await.expect_err("must not send");
        assert_eq!(e.kind, ErrorKind::Unsupported);
        Box::new(t).close().await.unwrap();
    }

    /// The Codie M0 blocker: advertised-but-unsupported must surface as
    /// Unsupported (documented downgrade), NOT as a placement failure and
    /// NOT as a silent rename fallback.
    #[tokio::test]
    async fn hardlink_advertised_but_unsupported_is_distinguished_from_failure() {
        let mut cfg = FakeConfig::tailscale(temp_root("hl3"));
        cfg.hardlink_lies = true;
        let srv = FakeServer::start(cfg).await;
        let t = srv.connect(&temp_root("trustG")).await;
        assert!(t.has_hardlink()); // advertised...
        write_remote_file(&t, "/f1", b"x").await;
        let e = t
            .hardlink("/f1", "/f2")
            .await
            .expect_err("...but unsupported");
        assert_eq!(e.kind, ErrorKind::Unsupported); // ≠ Exists, ≠ Other
        Box::new(t).close().await.unwrap();
        // (The failed≠exists half — a genuine hardlink failure mapping to
        // Notfound, not Exists — lives in hardlink_no_replace_collision,
        // which runs a REAL-hardlink server.)
    }

    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn hardlink_lies_end_to_end_copy_uses_documented_fallback() {
        let mut cfg = FakeConfig::tailscale(temp_root("hl4srv"));
        cfg.hardlink_lies = true;
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let src = temp_root("hl4src");
        std::fs::write(src.join("a.txt"), b"payload").unwrap();
        let t = srv.connect(&temp_root("trustH")).await;
        let (emit, _pcts) = collector();
        let outcome = transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["a.txt"], false),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("documented check-then-rename fallback places the file");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(
            std::fs::read(srv.root.join("dst/a.txt")).unwrap(),
            b"payload"
        );
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// M0: vanilla RENAME semantics DIFFER by flavor. Both behaviors must
    /// pass through the same client code — placement never ASSUMES either.
    #[tokio::test]
    async fn rename_semantics_tailscale_replaces() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("rn1"))).await;
        let t = srv.connect(&temp_root("trustI")).await;
        write_remote_file(&t, "/a", b"new").await;
        write_remote_file(&t, "/b", b"old").await;
        t.rename("/a", "/b").await.unwrap(); // Tailscale-style: REPLACES
        assert_eq!(std::fs::read(srv.root.join("b")).unwrap(), b"new");
        assert!(!srv.root.join("a").exists());
        Box::new(t).close().await.unwrap();
    }

    #[tokio::test]
    async fn rename_semantics_openssh_refuses_exists() {
        let srv = FakeServer::start(FakeConfig::openssh(temp_root("rn2"))).await;
        let (key, _g) = write_temp_key("rn2");
        let t = srv
            .connect_as(&temp_root("trustJ"), "fakeuser", &[key])
            .await;
        write_remote_file(&t, "/a", b"new").await;
        write_remote_file(&t, "/b", b"old").await;
        let e = t.rename("/a", "/b").await.expect_err("OpenSSH refuses");
        assert_eq!(e.kind, ErrorKind::Exists);
        // Both files intact — the refusal changed nothing.
        assert_eq!(std::fs::read(srv.root.join("a")).unwrap(), b"new");
        assert_eq!(std::fs::read(srv.root.join("b")).unwrap(), b"old");
        Box::new(t).close().await.unwrap();
    }

    #[tokio::test]
    async fn exclusive_create_refuses_existing() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("ex1"))).await;
        let t = srv.connect(&temp_root("trustK")).await;
        write_remote_file(&t, "/x", b"original").await;
        let e = match t.create_exclusive("/x", 0o644).await {
            Ok(_) => panic!("EXCLUDE must refuse an existing file"),
            Err(e) => e,
        };
        assert_eq!(e.kind, ErrorKind::Other); // Failure w/o "exist" text (open)
        assert_eq!(std::fs::read(srv.root.join("x")).unwrap(), b"original");
        Box::new(t).close().await.unwrap();
    }

    // Unix-only: the fixture SETUP writes a real "bad\nname" file — Win32
    // forbids 0x01-0x1F in filenames (Os code 123 InvalidFilename), so the
    // setup itself panics on Windows hosts. The knob-based twin below keeps
    // the rule covered there.
    #[cfg(unix)]
    #[tokio::test]
    async fn listing_skips_control_char_name_exactly_once() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("ls1"))).await;
        std::fs::write(srv.root.join("good.txt"), b"g").unwrap();
        std::fs::write(srv.root.join("bad\nname"), b"b").unwrap();
        let t = srv.connect(&temp_root("trustL")).await;
        let l = listing::list_remote(&t, "/", "", "fakeuser").await.unwrap();
        assert_eq!(l.skipped, 1, "control-char name counted exactly once");
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["good.txt"]);
        assert_eq!(l.path, "/");
        Box::new(t).close().await.unwrap();
    }

    /// C1: the same skip-and-count rule as the Unix-only real-file test
    /// above, but the control-char name is injected synthetically into
    /// READDIR (FakeConfig knob) instead of being created on disk — Win32
    /// filesystems cannot hold 0x01-0x1F in filenames, so this twin is what
    /// keeps the rule tested on Windows hosts. Runs on ALL platforms.
    #[tokio::test]
    async fn listing_skips_synthetic_control_char_name_exactly_once() {
        let mut cfg = FakeConfig::tailscale(temp_root("lscc"));
        cfg.control_char_names = true;
        let srv = FakeServer::start(cfg).await;
        std::fs::write(srv.root.join("good.txt"), b"g").unwrap();
        let t = srv.connect(&temp_root("trustLCC")).await;
        let l = listing::list_remote(&t, "/", "", "fakeuser").await.unwrap();
        assert_eq!(l.skipped, 1, "control-char name counted exactly once");
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["good.txt"]);
        assert!(
            !names.iter().any(|n| n.chars().any(|c| c.is_control())),
            "control-char name never returned or guessed at"
        );
        assert_eq!(l.path, "/");
        Box::new(t).close().await.unwrap();
    }

    /// B3: a raw non-UTF-8 name served through the lossy decode must be
    /// skipped and counted exactly once — never rendered, never actionable.
    #[tokio::test]
    async fn listing_skips_non_utf8_name_exactly_once() {
        let mut cfg = FakeConfig::tailscale(temp_root("nu8list"));
        cfg.non_utf8_names = true;
        let srv = FakeServer::start(cfg).await;
        std::fs::write(srv.root.join("good.txt"), b"g").unwrap();
        let t = srv.connect(&temp_root("trustNU8L")).await;
        let l = listing::list_remote(&t, "/", "", "fakeuser").await.unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["good.txt"]);
        assert_eq!(l.skipped, 1);
        assert!(!names.iter().any(|n| n.contains('\u{FFFD}')));
        Box::new(t).close().await.unwrap();
    }

    /// B3 + W3: remote→local PULL through the real LocalSink — the walker
    /// skips the lossy-decoded name (counted exactly once in the outcome),
    /// copies the rest with fidelity, and no U+FFFD name materializes at
    /// the destination.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_skips_non_utf8_names_and_counts_them() {
        let mut cfg = FakeConfig::tailscale(temp_root("nu8srv"));
        cfg.non_utf8_names = true;
        let srv = FakeServer::start(cfg).await;
        // The knob injects the lossy name into every READDIR, so the walker
        // meets it inside "sub" alongside a normal sibling.
        std::fs::create_dir(srv.root.join("sub")).unwrap();
        std::fs::write(srv.root.join("sub/keep.txt"), b"keep").unwrap();

        let dst = temp_root("nu8dst");
        let t = srv.connect(&temp_root("trustNU8W")).await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &CopyRequest {
                src_target: "fakehost".into(),
                src_user: Some("fakeuser".into()),
                src_dir: "/".into(),
                dst_target: "local".into(),
                dst_user: None,
                dst_dir: dst.to_str().unwrap().to_string(),
                names: vec!["sub".into()],
                replace: false,
            },
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("pull succeeds");
        assert_eq!(outcome.skipped, 1, "lossy name counted exactly once");
        assert_eq!(std::fs::read(dst.join("sub/keep.txt")).unwrap(), b"keep");
        // The guessed name must not materialize anywhere in the destination.
        for e in std::fs::read_dir(dst.join("sub")).unwrap().flatten() {
            assert!(
                !e.file_name().to_string_lossy().contains('\u{FFFD}'),
                "no replacement-char name may be created"
            );
        }
        Box::new(t).close().await.unwrap();
    }

    // ---- Group C: full copy runs — fidelity, consent, cleanup ----

    // Unix-only: builds real Unix symlinks + chmod modes; used solely by
    // the Unix-gated run_copy fidelity tests.
    #[cfg(unix)]
    fn fidelity_source(tag: &str) -> PathBuf {
        let src = temp_root(tag);
        std::fs::write(src.join("hello.txt"), b"hello world").unwrap();
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/inner.bin"), vec![7u8; 300_000]).unwrap();
        std::os::unix::fs::symlink("hello.txt", src.join("link.txt")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            src.join("hello.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::set_permissions(src.join("sub"), std::fs::Permissions::from_mode(0o750)).unwrap();
        src
    }

    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_local_to_remote_fidelity_and_cleanup() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("cp1srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let src = fidelity_source("cp1src");
        let t = srv.connect(&temp_root("trustM")).await;
        let (emit, pcts) = collector();
        let outcome = transfer::run_copy(
            &req(
                src.to_str().unwrap(),
                "/dst",
                &["hello.txt", "sub", "link.txt"],
                false,
            ),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("copy succeeds");
        assert_eq!(outcome.skipped, 0);
        let dst = srv.root.join("dst");
        assert_eq!(
            std::fs::read(dst.join("hello.txt")).unwrap(),
            b"hello world"
        );
        assert_eq!(
            std::fs::read(dst.join("sub/inner.bin")).unwrap(),
            vec![7u8; 300_000]
        );
        // Symlink copied AS A LINK, never followed.
        let lt = std::fs::symlink_metadata(dst.join("link.txt")).unwrap();
        assert!(lt.is_symlink());
        assert_eq!(
            std::fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_str()
                .unwrap(),
            "hello.txt"
        );
        // Modes preserved.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dst.join("hello.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        assert_eq!(
            std::fs::metadata(dst.join("sub"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o750
        );
        // Cleanup on the done path: no staging dirs survive.
        assert!(stage_leftovers(&dst).is_empty());
        // Progress capped at 99 until done-flush; at least one emit happened.
        {
            let p = pcts.lock().unwrap();
            assert!(p.is_empty() || p.iter().all(|v| *v <= 100));
        }
        Box::new(t).close().await.unwrap();
    }

    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_onto_existing_without_consent_refuses_and_cleans() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("cp2srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        std::fs::write(srv.root.join("dst/hello.txt"), b"OLD").unwrap();
        let src = fidelity_source("cp2src");
        let t = srv.connect(&temp_root("trustN")).await;
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["hello.txt"], false),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("exists without consent must refuse");
        assert_eq!(e.kind, ErrorKind::Exists);
        // Destination untouched...
        assert_eq!(
            std::fs::read(srv.root.join("dst/hello.txt")).unwrap(),
            b"OLD"
        );
        // ...and staging cleaned on the failed path.
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }

    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_replace_consent_replaces_and_cleans() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("cp3srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        std::fs::write(srv.root.join("dst/hello.txt"), b"OLD").unwrap();
        let src = fidelity_source("cp3src");
        let t = srv.connect(&temp_root("trustO")).await;
        let (emit, _p) = collector();
        transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["hello.txt"], true),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("consented replace succeeds");
        assert_eq!(
            std::fs::read(srv.root.join("dst/hello.txt")).unwrap(),
            b"hello world"
        );
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// Deterministic mid-copy cancel: the counting writer cancels the token
    /// after the first chunk lands; the next chunk boundary must stop the
    /// stream, and cleanup must remove the staging tree before returning.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_midcopy_leaves_no_stage_and_no_destination() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("cp4srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let src = temp_root("cp4src");
        std::fs::write(src.join("big.bin"), vec![3u8; CHUNK * 8]).unwrap();
        let t = srv.connect(&temp_root("trustP")).await;
        let cancel = CancellationToken::new();
        let wrapper = CancelAfterBytes {
            inner: &t,
            cancel: cancel.clone(),
            threshold: CHUNK as u64, // cancel once the first chunk is written
            written: Arc::new(AtomicU64::new(0)),
        };
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["big.bin"], false),
            &Side::Local,
            &Side::Remote {
                transport: &wrapper,
                windows: false,
            },
            cancel,
            emit,
        )
        .await
        .expect_err("cancelled copy errors");
        assert!(transfer::is_cancelled(&e), "expected cancelled, got {e:?}");
        // Cleanup on the cancelled path: no staging, no half-placed file.
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        assert!(!srv.root.join("dst/big.bin").exists());
        Box::new(t).close().await.unwrap();
    }

    /// B6/A7: Windows-source pull — a junction (READLINK → BadMessage-like
    /// Failure) and a directory symlink are skipped + counted, NEVER
    /// followed; a real dir (clean "not a link" NoSuchFile answer) is
    /// descended and copied. Without this probe a junction would copy data
    /// from outside the selected tree or loop to the depth cap.
    #[tokio::test]
    #[cfg(unix)]
    async fn windows_pull_skips_junctions_never_follows() {
        let mut cfg = FakeConfig::openssh(temp_root("junc-srv"));
        cfg.banner = "SSH-2.0-OpenSSH_for_Windows_8.1".to_string();
        cfg.simulate_junctions = true;
        let srv = FakeServer::start(cfg).await;
        // A real dir with content, a "junction" dir, and a dir symlink.
        std::fs::create_dir_all(srv.root.join("real")).unwrap();
        std::fs::write(srv.root.join("real/inside.txt"), b"inside").unwrap();
        std::fs::create_dir(srv.root.join("junction-escape")).unwrap();
        std::fs::write(srv.root.join("junction-escape/outside.txt"), b"outside").unwrap();
        // Ancestor-pointing junction NESTED inside a descended dir: if the
        // probe ever followed it, the walk would loop to the depth cap
        // instead of finishing — it must skip like any other reparse point.
        std::fs::create_dir_all(srv.root.join("real/junction-back")).unwrap();
        std::os::unix::fs::symlink(srv.root.join("real"), srv.root.join("dirlink")).unwrap();

        let dst = temp_root("junc-dst");
        let (key, _g) = write_temp_key("junc");
        let t = srv
            .connect_as(&temp_root("trustJUNC"), "fakeuser", &[key])
            .await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &CopyRequest {
                src_target: "fakehost".into(),
                src_user: Some("fakeuser".into()),
                src_dir: "/".into(),
                dst_target: "local".into(),
                dst_user: None,
                dst_dir: dst.to_str().unwrap().to_string(),
                names: vec!["real".into(), "junction-escape".into(), "dirlink".into()],
                replace: false,
            },
            &Side::Remote {
                transport: &t,
                windows: true,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("windows pull succeeds");
        assert_eq!(
            outcome.skipped, 3,
            "junction + nested ancestor junction + dirlink skipped and counted"
        );
        assert_eq!(
            std::fs::read(dst.join("real/inside.txt")).unwrap(),
            b"inside"
        );
        assert!(
            !dst.join("junction-escape").exists(),
            "junction never followed"
        );
        assert!(
            !dst.join("real/junction-back").exists(),
            "ancestor junction never followed (no loop)"
        );
        assert!(!dst.join("dirlink").exists(), "dir symlink never followed");
        Box::new(t).close().await.unwrap();
    }

    /// B6-R REGRESSION: a PLAIN Windows directory answers READLINK with
    /// generic SSH_FX_FAILURE (Win32-OpenSSH: ERROR_NOT_A_REPARSE_POINT 4390
    /// is unmapped by errno_from_Win32Error, errno_to_portable defaults to
    /// FAILURE). The probe must DESCEND — before B6-R it skipped these, so
    /// Windows folder pulls copied nothing (R3 blocker). The fixture now
    /// answers Failure for plain dirs, exactly like the real server.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn windows_pull_descends_plain_dirs_answering_failure() {
        let mut cfg = FakeConfig::openssh(temp_root("b6r-plain-srv"));
        cfg.banner = "SSH-2.0-OpenSSH_for_Windows_8.1".to_string();
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("docs/reports")).unwrap();
        std::fs::write(srv.root.join("docs/readme.md"), b"# hi").unwrap();
        std::fs::write(srv.root.join("docs/reports/q3.txt"), b"numbers").unwrap();

        let dst = temp_root("b6r-plain-dst");
        // Explicit fixture key: OpenSSH-flavor auth never falls back to
        // none-auth, so keyless connect() would depend on ambient developer
        // credentials that do not exist on CI runners.
        let (key, _g) = write_temp_key("b6rp");
        let t = srv
            .connect_as(&temp_root("trustB6RP"), "fakeuser", &[key])
            .await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["docs"]),
            &Side::Remote {
                transport: &t,
                windows: true,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("plain Windows dirs must descend");
        assert_eq!(outcome.skipped, 0, "nothing may be skipped on a plain tree");
        assert_eq!(std::fs::read(dst.join("docs/readme.md")).unwrap(), b"# hi");
        assert_eq!(
            std::fs::read(dst.join("docs/reports/q3.txt")).unwrap(),
            b"numbers",
            "nested plain dirs descend recursively"
        );
        assert!(stage_leftovers(&dst).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// B6-R race tolerance: a directory that VANISHES between listing and
    /// probe (NO_SUCH_FILE) is skipped + counted — never a hard error, never
    /// a descend.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn windows_pull_tolerates_dir_vanished_before_probe() {
        let mut cfg = FakeConfig::openssh(temp_root("b6r-van-srv"));
        cfg.banner = "SSH-2.0-OpenSSH_for_Windows_8.1".to_string();
        cfg.vanish_readlink_for = Some("gone-dir".to_string());
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("gone-dir")).unwrap();
        std::fs::write(srv.root.join("gone-dir/inner.txt"), b"x").unwrap();
        std::fs::create_dir_all(srv.root.join("stays")).unwrap();
        std::fs::write(srv.root.join("stays/keep.txt"), b"kept").unwrap();

        let dst = temp_root("b6r-van-dst");
        let (key, _g) = write_temp_key("b6rv");
        let t = srv
            .connect_as(&temp_root("trustB6RV"), "fakeuser", &[key])
            .await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["gone-dir", "stays"]),
            &Side::Remote {
                transport: &t,
                windows: true,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("vanished dir is tolerated, not fatal");
        assert_eq!(outcome.skipped, 1, "vanished dir skipped + counted");
        assert!(!dst.join("gone-dir").exists());
        assert_eq!(std::fs::read(dst.join("stays/keep.txt")).unwrap(), b"kept");
        Box::new(t).close().await.unwrap();
    }

    /// B6-R: POSIX sources NEVER probe read_link on directories — the real
    /// POSIX sftp-server answers BAD_MESSAGE (EINVAL) for plain dirs, so a
    /// probing walker would refuse every folder. The fixture now encodes
    /// that real answer; this pull must still copy the whole tree, and the
    /// dir-readlink counter must stay at 0.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn posix_pull_never_probes_dir_readlink() {
        let cfg = FakeConfig::tailscale(temp_root("b6r-posix-srv"));
        let probes = cfg.dir_readlinks.clone();
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("tree/sub")).unwrap();
        std::fs::write(srv.root.join("tree/a.txt"), b"aaa").unwrap();
        std::fs::write(srv.root.join("tree/sub/b.txt"), b"bbb").unwrap();

        let dst = temp_root("b6r-posix-dst");
        let t = srv.connect(&temp_root("trustB6RX")).await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["tree"]),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("POSIX pull succeeds without probing");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(std::fs::read(dst.join("tree/a.txt")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(dst.join("tree/sub/b.txt")).unwrap(), b"bbb");
        assert_eq!(
            probes.load(Ordering::SeqCst),
            0,
            "POSIX sources must never readlink-probe directories"
        );
        Box::new(t).close().await.unwrap();
    }

    /// Hostile server: writes start failing mid-copy (PermissionDenied).
    /// The primary error surfaces (never masked by cleanup errors) and the
    /// staging tree is removed on the failed path.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn write_failure_midcopy_reports_primary_error_and_cleans() {
        let mut cfg = FakeConfig::tailscale(temp_root("cp5srv"));
        cfg.fail_writes_after = Some((CHUNK / 2) as u64);
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let src = temp_root("cp5src");
        std::fs::write(src.join("big.bin"), vec![9u8; CHUNK * 4]).unwrap();
        let t = srv.connect(&temp_root("trustQ")).await;
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["big.bin"], false),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("failing writes error the copy");
        assert_eq!(e.kind, ErrorKind::Denied); // primary error, not a cleanup error
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        assert!(!srv.root.join("dst/big.bin").exists());
        Box::new(t).close().await.unwrap();
    }

    /// Pre-cancelled token: refused before any staging exists.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn pre_cancelled_copy_creates_nothing() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("cp6srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let src = temp_root("cp6src");
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        let t = srv.connect(&temp_root("trustR")).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &req(src.to_str().unwrap(), "/dst", &["a.txt"], false),
            &Side::Local,
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            cancel,
            emit,
        )
        .await
        .expect_err("pre-cancelled copy refuses");
        assert!(transfer::is_cancelled(&e));
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        assert!(!srv.root.join("dst/a.txt").exists());
        Box::new(t).close().await.unwrap();
    }
    /// Transport wrapper that cancels the token after N bytes have been READ
    /// from remote files — deterministic remote-source cancellation at a
    /// chunk boundary (no sleeps, no races). Read-side twin of
    /// CancelAfterBytes.
    #[cfg(unix)] // Unix-only: run_copy local side (see imports note)
    struct CancelAfterReads<'a> {
        inner: &'a RusshTransport,
        cancel: CancellationToken,
        threshold: u64,
        read: Arc<AtomicU64>,
    }

    #[cfg(unix)]
    struct CountingReader {
        inner: Box<dyn RemoteReader>,
        cancel: CancellationToken,
        threshold: u64,
        read: Arc<AtomicU64>,
    }

    #[cfg(unix)]
    #[async_trait]
    impl RemoteReader for CountingReader {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
            let n = self.inner.read(buf).await?;
            let total = self.read.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
            if total >= self.threshold {
                self.cancel.cancel();
            }
            Ok(n)
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            self.inner.close().await
        }
    }

    #[cfg(unix)]
    #[async_trait]
    impl SftpTransport for CancelAfterReads<'_> {
        async fn realpath(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.realpath(path).await
        }
        async fn read_dir(&self, path: &str) -> Result<Vec<RemoteNode>, BrowserError> {
            self.inner.read_dir(path).await
        }
        async fn stat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.stat(path).await
        }
        async fn lstat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.lstat(path).await
        }
        async fn mkdir(&self, path: &str, perm: u32) -> Result<(), BrowserError> {
            self.inner.mkdir(path, perm).await
        }
        async fn remove_file(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_file(path).await
        }
        async fn remove_dir(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_dir(path).await
        }
        async fn rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.rename(from, to).await
        }
        async fn posix_rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.posix_rename(from, to).await
        }
        async fn hardlink(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.hardlink(from, to).await
        }
        async fn symlink(&self, target: &str, path: &str) -> Result<(), BrowserError> {
            self.inner.symlink(target, path).await
        }
        async fn read_link(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.read_link(path).await
        }
        async fn open_read(&self, path: &str) -> Result<Box<dyn RemoteReader>, BrowserError> {
            let inner = self.inner.open_read(path).await?;
            Ok(Box::new(CountingReader {
                inner,
                cancel: self.cancel.clone(),
                threshold: self.threshold,
                read: self.read.clone(),
            }))
        }
        async fn create_exclusive(
            &self,
            path: &str,
            perm: u32,
        ) -> Result<Box<dyn RemoteWriter>, BrowserError> {
            self.inner.create_exclusive(path, perm).await
        }
        async fn set_attrs(
            &self,
            path: &str,
            mode: Option<u32>,
            mtime: Option<i64>,
        ) -> Result<(), BrowserError> {
            self.inner.set_attrs(path, mode, mtime).await
        }
        fn has_hardlink(&self) -> bool {
            self.inner.has_hardlink()
        }
        fn has_posix_rename(&self) -> bool {
            self.inner.has_posix_rename()
        }
        fn extensions(&self) -> &BTreeMap<String, String> {
            self.inner.extensions()
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            Ok(()) // inner is borrowed; the test closes it
        }
    }

    #[cfg(unix)] // Unix-only: run_copy local side (see imports note)
    fn pull_req(src_dir: &str, dst_dir: &str, names: &[&str]) -> CopyRequest {
        CopyRequest {
            src_target: "fakehost".into(),
            src_user: Some("fakeuser".into()),
            src_dir: src_dir.into(),
            dst_target: "local".into(),
            dst_user: None,
            dst_dir: dst_dir.into(),
            names: names.iter().map(|n| n.to_string()).collect(),
            replace: false,
        }
    }

    // ---- Group W3: end-to-end PULL (fake server → real LocalSink) ----

    /// W3: a remote symlink is pulled AS A LINK (never followed) and regular
    /// files land with byte fidelity; local staging cleaned on done.
    #[tokio::test]
    #[cfg(unix)]
    async fn pull_copies_remote_symlink_as_link() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("pull-sym-srv"))).await;
        std::fs::write(srv.root.join("target.txt"), b"target-data").unwrap();
        std::os::unix::fs::symlink("target.txt", srv.root.join("link.txt")).unwrap();
        let dst = temp_root("pull-sym-dst");
        let t = srv.connect(&temp_root("trustPS")).await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["target.txt", "link.txt"]),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("pull succeeds");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(
            std::fs::read(dst.join("target.txt")).unwrap(),
            b"target-data"
        );
        let lm = std::fs::symlink_metadata(dst.join("link.txt")).unwrap();
        assert!(lm.is_symlink(), "link stays a link, never followed");
        assert_eq!(
            std::fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_str()
                .unwrap(),
            "target.txt"
        );
        assert!(stage_leftovers(&dst).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// W3: a file that changes size between plan and stream fails the copy
    /// with the "changed" error (§6.4 ceiling) — and the local stage is
    /// cleaned on the failed path.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_size_changed_fails_and_cleans() {
        let mut cfg = FakeConfig::tailscale(temp_root("pull-sz-srv"));
        // Walker's first lstat over-reports by 100 bytes; the pre-open
        // re-stat tells the truth → mismatch.
        cfg.lie_size_once = Some(("grows.txt".into(), 100));
        let srv = FakeServer::start(cfg).await;
        std::fs::write(srv.root.join("grows.txt"), vec![1u8; 50]).unwrap();
        let dst = temp_root("pull-sz-dst");
        let t = srv.connect(&temp_root("trustSZ")).await;
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["grows.txt"]),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("size-changed must fail the copy");
        assert!(
            e.message.contains("changed while it was being copied"),
            "unexpected error: {e:?}"
        );
        assert!(!dst.join("grows.txt").exists());
        assert!(stage_leftovers(&dst).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// W3 + spike limit #1: the PRIMARY read error ("got shorter") surfaces
    /// even though the reader's CLOSE also fails — close never masks it.
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_read_error_not_masked_by_close_error() {
        let mut cfg = FakeConfig::tailscale(temp_root("pull-rd-srv"));
        cfg.shrink_reads_for = Some("shrinks.bin".into());
        cfg.fail_close = true;
        let srv = FakeServer::start(cfg).await;
        std::fs::write(srv.root.join("shrinks.bin"), vec![2u8; 5000]).unwrap();
        let dst = temp_root("pull-rd-dst");
        let t = srv.connect(&temp_root("trustRD")).await;
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["shrinks.bin"]),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("shrunk file must fail the copy");
        assert!(
            e.message.contains("got shorter"),
            "primary read error must surface, got {e:?}"
        );
        assert!(!dst.join("shrinks.bin").exists());
        assert!(stage_leftovers(&dst).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// W3: cancelling a REMOTE-SOURCE copy at a chunk boundary leaves no
    /// local stage and no partial file (read-side twin of the push cancel).
    // Unix-only: drives run_copy through the local side, which is
    // #[cfg(unix)] and fails closed elsewhere (local_side.rs module head).
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_remote_source_leaves_no_stage() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("pull-cx-srv"))).await;
        std::fs::write(srv.root.join("big.bin"), vec![3u8; CHUNK * 4]).unwrap();
        let dst = temp_root("pull-cx-dst");
        let t = srv.connect(&temp_root("trustCX")).await;
        let cancel = CancellationToken::new();
        let wrapper = CancelAfterReads {
            inner: &t,
            cancel: cancel.clone(),
            threshold: CHUNK as u64, // cancel once the first chunk has been read
            read: Arc::new(AtomicU64::new(0)),
        };
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &pull_req("/", dst.to_str().unwrap(), &["big.bin"]),
            &Side::Remote {
                transport: &wrapper,
                windows: false,
            },
            &Side::Local,
            cancel,
            emit,
        )
        .await
        .expect_err("cancelled pull errors");
        assert!(transfer::is_cancelled(&e), "expected cancelled, got {e:?}");
        assert!(!dst.join("big.bin").exists());
        assert!(stage_leftovers(&dst).is_empty());
        Box::new(t).close().await.unwrap();
    }

    // ---- Group M2a: remote→remote (TWO fake servers, no local side) ----
    //
    // The copy streams THROUGH the app: a source session on server A feeds a
    // destination session on server B (report §7.5 M2). No `Side::Local` is
    // involved, so the whole group runs on EVERY platform — the Windows
    // local-engine gate (W4) does not apply.

    fn r2r_req(src_dir: &str, dst_dir: &str, names: &[&str], replace: bool) -> CopyRequest {
        CopyRequest {
            src_target: "host-a".into(),
            src_user: Some("fakeuser".into()),
            src_dir: src_dir.into(),
            dst_target: "host-b".into(),
            dst_user: Some("fakeuser".into()),
            dst_dir: dst_dir.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
            replace,
        }
    }

    /// Portable whole-second mtime setter (std FileTimes, atime+mtime).
    fn set_mtime_secs(path: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for mtime");
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
            .expect("set mtime");
    }

    fn mtime_secs(path: &Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// M2a: remote→remote FILE copy between two independent sessions —
    /// multi-chunk bytes land on server B with the source mtime, server A is
    /// untouched, no staging survives on either side, progress stays ≤99.
    #[tokio::test]
    async fn remote_to_remote_file_copy_two_sessions() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-f-a"))).await;
        let srv_b = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-f-b"))).await;
        let payload = vec![5u8; CHUNK * 2 + 77];
        std::fs::write(srv_a.root.join("big.bin"), &payload).unwrap();
        set_mtime_secs(&srv_a.root.join("big.bin"), 1_700_000_123);
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();

        let t_a = srv_a.connect(&temp_root("trustR2RF1")).await;
        let t_b = srv_b.connect(&temp_root("trustR2RF2")).await;
        let (emit, pcts) = collector();
        let outcome = transfer::run_copy(
            &r2r_req("/", "/dst", &["big.bin"], false),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &t_b,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("remote→remote copy succeeds");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(
            outcome.bytes,
            payload.len() as u64,
            "every acknowledged byte counted"
        );
        assert_eq!(
            std::fs::read(srv_b.root.join("dst/big.bin")).unwrap(),
            payload,
            "multi-chunk bytes land with fidelity"
        );
        assert_eq!(
            mtime_secs(&srv_b.root.join("dst/big.bin")),
            1_700_000_123,
            "source mtime travels through the relay"
        );
        // Source untouched; staging cleaned on BOTH servers.
        assert_eq!(
            std::fs::read(srv_a.root.join("big.bin")).unwrap().len(),
            CHUNK * 2 + 77
        );
        assert!(stage_leftovers(&srv_b.root.join("dst")).is_empty());
        assert!(stage_leftovers(&srv_a.root).is_empty());
        {
            let p = pcts.lock().unwrap();
            assert!(
                p.iter().all(|v| *v <= 99),
                "progress caps at 99 until done: {p:?}"
            );
        }
        Box::new(t_a).close().await.unwrap();
        Box::new(t_b).close().await.unwrap();
    }

    /// M2a: remote→remote FOLDER copy with fidelity — nested structure,
    /// multi-chunk bytes, per-file mtimes everywhere; on Unix additionally
    /// symlinks travel AS LINKS and mode bits are preserved (the fake
    /// applies SETSTAT modes on Unix; Windows ignores them by design).
    #[tokio::test]
    async fn remote_to_remote_folder_copy_fidelity() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-d-a"))).await;
        let srv_b = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-d-b"))).await;
        std::fs::create_dir_all(srv_a.root.join("tree/sub")).unwrap();
        std::fs::write(srv_a.root.join("tree/readme.md"), b"# hi").unwrap();
        set_mtime_secs(&srv_a.root.join("tree/readme.md"), 1_700_000_001);
        let inner = vec![7u8; CHUNK + 5];
        std::fs::write(srv_a.root.join("tree/sub/inner.bin"), &inner).unwrap();
        set_mtime_secs(&srv_a.root.join("tree/sub/inner.bin"), 1_700_000_002);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("readme.md", srv_a.root.join("tree/link.txt")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                srv_a.root.join("tree/readme.md"),
                std::fs::Permissions::from_mode(0o640),
            )
            .unwrap();
            std::fs::set_permissions(
                srv_a.root.join("tree/sub"),
                std::fs::Permissions::from_mode(0o750),
            )
            .unwrap();
        }
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();

        let t_a = srv_a.connect(&temp_root("trustR2RD1")).await;
        let t_b = srv_b.connect(&temp_root("trustR2RD2")).await;
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &r2r_req("/", "/dst", &["tree"], false),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &t_b,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("remote→remote folder copy succeeds");
        assert_eq!(outcome.skipped, 0);
        let dst = srv_b.root.join("dst");
        assert_eq!(std::fs::read(dst.join("tree/readme.md")).unwrap(), b"# hi");
        assert_eq!(
            std::fs::read(dst.join("tree/sub/inner.bin")).unwrap(),
            inner,
            "nested multi-chunk file streams with byte fidelity"
        );
        assert_eq!(mtime_secs(&dst.join("tree/readme.md")), 1_700_000_001);
        assert_eq!(mtime_secs(&dst.join("tree/sub/inner.bin")), 1_700_000_002);
        #[cfg(unix)]
        {
            // Symlink travelled AS A LINK, never followed.
            let lm = std::fs::symlink_metadata(dst.join("tree/link.txt")).unwrap();
            assert!(lm.is_symlink(), "link stays a link");
            assert_eq!(
                std::fs::read_link(dst.join("tree/link.txt"))
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "readme.md"
            );
            // Mode bits preserved through staging + placement.
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dst.join("tree/readme.md"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o640
            );
            assert_eq!(
                std::fs::metadata(dst.join("tree/sub"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o750
            );
        }
        assert!(stage_leftovers(&dst).is_empty());
        assert!(stage_leftovers(&srv_a.root).is_empty());
        Box::new(t_a).close().await.unwrap();
        Box::new(t_b).close().await.unwrap();
    }

    /// M2a: remote→remote conflict semantics — an existing destination name
    /// fails the copy with kind=exists + names (the row's Replace offer),
    /// the destination is untouched and the stage is removed; the SAME copy
    /// with replace consent then replaces it and cleans up.
    #[tokio::test]
    async fn remote_to_remote_conflict_refuses_then_replace_succeeds() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-c-a"))).await;
        let srv_b = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-c-b"))).await;
        std::fs::write(srv_a.root.join("hello.txt"), b"hello world").unwrap();
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();
        std::fs::write(srv_b.root.join("dst/hello.txt"), b"OLD").unwrap();

        let t_a = srv_a.connect(&temp_root("trustR2RC1")).await;
        let t_b = srv_b.connect(&temp_root("trustR2RC2")).await;
        let (emit1, _p1) = collector();
        let e = transfer::run_copy(
            &r2r_req("/", "/dst", &["hello.txt"], false),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &t_b,
                windows: false,
            },
            CancellationToken::new(),
            emit1,
        )
        .await
        .expect_err("conflict without consent must refuse");
        assert_eq!(e.kind, ErrorKind::Exists);
        assert_eq!(
            e.names.as_deref(),
            Some(&["hello.txt".to_string()][..]),
            "the exists error carries the colliding names for the Replace offer"
        );
        assert_eq!(
            std::fs::read(srv_b.root.join("dst/hello.txt")).unwrap(),
            b"OLD",
            "refused conflict leaves the destination untouched"
        );
        assert!(stage_leftovers(&srv_b.root.join("dst")).is_empty());

        // Replace consent: the identical copy with replace=true overwrites.
        let (emit2, _p2) = collector();
        transfer::run_copy(
            &r2r_req("/", "/dst", &["hello.txt"], true),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &t_b,
                windows: false,
            },
            CancellationToken::new(),
            emit2,
        )
        .await
        .expect("consented replace succeeds");
        assert_eq!(
            std::fs::read(srv_b.root.join("dst/hello.txt")).unwrap(),
            b"hello world"
        );
        assert!(stage_leftovers(&srv_b.root.join("dst")).is_empty());
        Box::new(t_a).close().await.unwrap();
        Box::new(t_b).close().await.unwrap();
    }

    /// M2a: remote→remote cancel at a chunk boundary — the DESTINATION
    /// wrapper cancels the token after the first staged chunk; the copy
    /// stops, the stage tree is removed on B, no partial file is placed,
    /// and the source is untouched.
    #[tokio::test]
    async fn remote_to_remote_cancel_leaves_no_stage_or_destination() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-x-a"))).await;
        let srv_b = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-x-b"))).await;
        let payload = vec![3u8; CHUNK * 8];
        std::fs::write(srv_a.root.join("big.bin"), &payload).unwrap();
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();
        let t_a = srv_a.connect(&temp_root("trustR2RX1")).await;
        let t_b = srv_b.connect(&temp_root("trustR2RX2")).await;
        let cancel = CancellationToken::new();
        let dst_wrapper = CancelAfterBytes {
            inner: &t_b,
            cancel: cancel.clone(),
            threshold: CHUNK as u64, // cancel once the first chunk is staged
            written: Arc::new(AtomicU64::new(0)),
        };
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &r2r_req("/", "/dst", &["big.bin"], false),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &dst_wrapper,
                windows: false,
            },
            cancel,
            emit,
        )
        .await
        .expect_err("cancelled copy errors");
        assert!(transfer::is_cancelled(&e), "expected cancelled, got {e:?}");
        assert!(stage_leftovers(&srv_b.root.join("dst")).is_empty());
        assert!(
            !srv_b.root.join("dst/big.bin").exists(),
            "no partial destination file"
        );
        assert_eq!(std::fs::read(srv_a.root.join("big.bin")).unwrap(), payload);
        Box::new(t_a).close().await.unwrap();
        Box::new(t_b).close().await.unwrap();
    }

    /// M2a: one-side AUTH FAILURE — the source session opens fine but the
    /// destination rejects every credential (`reject_all_auth`, immune to
    /// ambient developer keys/agent identities). No session ever exists on
    /// B, so nothing is staged and no partial destination can appear; the
    /// source session stays healthy and closes cleanly (the commands layer
    /// closes it on the failed dst open — tripwire below).
    #[tokio::test]
    async fn remote_to_remote_dst_auth_failure_leaves_nothing() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-au-a"))).await;
        let mut cfg_b = FakeConfig::tailscale(temp_root("r2r-au-b"));
        cfg_b.reject_all_auth = true;
        let srv_b = FakeServer::start(cfg_b).await;
        std::fs::write(srv_a.root.join("a.txt"), b"payload").unwrap();
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();

        let t_a = srv_a.connect(&temp_root("trustR2RAU1")).await;
        let err = srv_b
            .try_connect(&temp_root("trustR2RAU2"), "fakeuser", &[], Vec::new())
            .await
            .expect_err("destination auth must fail");
        assert_eq!(err.kind, ErrorKind::Auth, "typed auth refusal: {err:?}");
        // Destination untouched: no stage, no partial file, nothing at all.
        let staged: Vec<String> = std::fs::read_dir(srv_b.root.join("dst"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            staged.is_empty(),
            "destination must be untouched: {staged:?}"
        );
        // The source session is still usable and closes cleanly.
        assert!(t_a.lstat("/a.txt").await.unwrap().is_some());
        assert_eq!(std::fs::read(srv_a.root.join("a.txt")).unwrap(), b"payload");
        Box::new(t_a).close().await.unwrap();
    }

    /// Tripwire for the M2a execute_copy fix: when the DESTINATION side
    /// fails to open (auth etc.), the already-open SOURCE session must be
    /// closed before the error propagates — no leaked connections. Reverting
    /// the fix drops one (or both) of the `close_side(src_side)` calls.
    #[test]
    fn dst_open_failure_closes_source_tripwire() {
        let src = include_str!("commands.rs");
        let closes = src.matches("close_side(src_side).await").count();
        assert_eq!(
            closes, 2,
            "src session must close on dst-open failure AND on the normal path"
        );
    }

    /// M2a: destination-side write failure MID-COPY (hostile dst server) —
    /// the primary error surfaces, the stage tree is removed on B, and no
    /// partial destination file is placed. The "stage removed, no partial
    /// destination" cleanup invariant for a dst side that fails AFTER
    /// staging began (twin of the auth-failure test above, which fails
    /// BEFORE staging can begin).
    #[tokio::test]
    async fn remote_to_remote_dst_write_failure_cleans_stage() {
        let srv_a = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-w-a"))).await;
        let mut cfg_b = FakeConfig::tailscale(temp_root("r2r-w-b"));
        cfg_b.fail_writes_after = Some((CHUNK / 2) as u64);
        let srv_b = FakeServer::start(cfg_b).await;
        std::fs::write(srv_a.root.join("big.bin"), vec![9u8; CHUNK * 4]).unwrap();
        std::fs::create_dir_all(srv_b.root.join("dst")).unwrap();
        let t_a = srv_a.connect(&temp_root("trustR2RW1")).await;
        let t_b = srv_b.connect(&temp_root("trustR2RW2")).await;
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &r2r_req("/", "/dst", &["big.bin"], false),
            &Side::Remote {
                transport: &t_a,
                windows: false,
            },
            &Side::Remote {
                transport: &t_b,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("failing destination writes error the copy");
        assert_eq!(
            e.kind,
            ErrorKind::Denied,
            "primary write error, never a cleanup error: {e:?}"
        );
        assert!(stage_leftovers(&srv_b.root.join("dst")).is_empty());
        assert!(!srv_b.root.join("dst/big.bin").exists());
        Box::new(t_a).close().await.unwrap();
        Box::new(t_b).close().await.unwrap();
    }

    /// W-1 (Codie M2a review, throwaway probe promoted permanently):
    /// same-host SAME-account copy — ONE transport serves both sides,
    /// exactly the `dst_same_as_src` shape commands.rs takes when
    /// dst_target == src_target && dst_user == src_user (single connection,
    /// single close). `/a` → `/b` on one FakeServer: a file spanning three
    /// chunk boundaries (+9 bytes) and a nested folder. Asserts byte
    /// fidelity, mtime travel, source untouched, and stage cleanup on both
    /// "sides" (one filesystem here). All platforms — no local side.
    #[tokio::test]
    async fn remote_to_remote_same_host_one_transport() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("r2r-one"))).await;
        let payload = vec![3u8; CHUNK * 3 + 9];
        std::fs::create_dir_all(srv.root.join("a/folder/sub")).unwrap();
        std::fs::write(srv.root.join("a/big.bin"), &payload).unwrap();
        set_mtime_secs(&srv.root.join("a/big.bin"), 1_700_000_789);
        std::fs::write(srv.root.join("a/folder/sub/inner.txt"), b"inner").unwrap();
        std::fs::create_dir_all(srv.root.join("b")).unwrap();

        let t = srv.connect(&temp_root("trustR2RONE")).await;
        let req = CopyRequest {
            src_target: "host-a".into(),
            src_user: Some("fakeuser".into()),
            src_dir: "/a".into(),
            dst_target: "host-a".into(),
            dst_user: Some("fakeuser".into()),
            dst_dir: "/b".into(),
            names: vec!["big.bin".into(), "folder".into()],
            replace: false,
        };
        let (emit, _pcts) = collector();
        // BOTH sides borrow the same transport value — the engine must read
        // and stage over one connection without deadlocking or leaking.
        let side = Side::Remote {
            transport: &t,
            windows: false,
        };
        let outcome = transfer::run_copy(&req, &side, &side, CancellationToken::new(), emit)
            .await
            .expect("same-host same-account copy succeeds on ONE transport");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(
            outcome.bytes,
            payload.len() as u64 + 5, // big.bin + folder/sub/inner.txt
            "every acknowledged byte counted through the single connection"
        );
        assert_eq!(
            std::fs::read(srv.root.join("b/big.bin")).unwrap(),
            payload,
            "3×CHUNK+9 bytes land with fidelity"
        );
        assert_eq!(
            mtime_secs(&srv.root.join("b/big.bin")),
            1_700_000_789,
            "source mtime travels"
        );
        assert_eq!(
            std::fs::read(srv.root.join("b/folder/sub/inner.txt")).unwrap(),
            b"inner",
            "folder structure lands"
        );
        assert!(
            stage_leftovers(&srv.root.join("b")).is_empty(),
            "stage cleaned on the destination side"
        );
        assert!(
            stage_leftovers(&srv.root.join("a")).is_empty(),
            "no stage debris on the source side either"
        );
        assert_eq!(
            std::fs::read(srv.root.join("a/big.bin")).unwrap(),
            payload,
            "source untouched"
        );
        Box::new(t).close().await.unwrap();
    }

    // ---- M2b: exec-agent engine tier (loopback perl as the dst agent) ----

    /// Exec-capable transport double: every SFTP verb delegates to the real
    /// RusshTransport (the SOURCE walk), while the agent seams answer for
    /// the destination — `open_exec` spawns the vendored Perl agent as a
    /// LOCAL child (the loopback harness), so `dst_dir` is a real local
    /// temp path and placement/cleanup are the real perl's.
    #[cfg(unix)]
    struct ExecCapable<'a> {
        inner: &'a RusshTransport,
        cap: crate::browser::agent::AgentCap,
        exec_opens: Arc<AtomicU64>,
        /// Optional cancel-after-N-bytes hook on exec-stream writes (the
        /// mid-copy cancel test — the agent-tier twin of CancelAfterBytes).
        cancel: Option<(CancellationToken, u64, Arc<AtomicU64>)>,
        /// S-1: when set, `open_exec` runs THIS sh command instead of the
        /// requested one — the "perl too old" host double: it consumes
        /// stdin, then bails rc 9 with the vendored agent's exact stderr.
        shim: Option<&'a str>,
    }

    #[cfg(unix)]
    struct CountingExec {
        inner: Box<dyn crate::browser::agent::ExecChannel>,
        cancel: CancellationToken,
        threshold: u64,
        written: Arc<AtomicU64>,
    }

    #[cfg(unix)]
    #[async_trait]
    impl crate::browser::agent::ExecChannel for CountingExec {
        async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError> {
            self.inner.write_all(data).await?;
            let n = self.written.fetch_add(data.len() as u64, Ordering::SeqCst) + data.len() as u64;
            if n >= self.threshold {
                self.cancel.cancel();
            }
            Ok(())
        }
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
            self.inner.read(buf).await
        }
        async fn close_stdin(&mut self) -> Result<(), BrowserError> {
            self.inner.close_stdin().await
        }
        async fn wait_exit(&mut self, timeout: std::time::Duration) -> Option<u32> {
            self.inner.wait_exit(timeout).await
        }
        fn stderr_tail(&self) -> String {
            self.inner.stderr_tail()
        }
    }

    #[cfg(unix)]
    #[async_trait]
    impl SftpTransport for ExecCapable<'_> {
        async fn realpath(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.realpath(path).await
        }
        async fn read_dir(&self, path: &str) -> Result<Vec<RemoteNode>, BrowserError> {
            self.inner.read_dir(path).await
        }
        async fn stat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.stat(path).await
        }
        async fn lstat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError> {
            self.inner.lstat(path).await
        }
        async fn mkdir(&self, path: &str, perm: u32) -> Result<(), BrowserError> {
            self.inner.mkdir(path, perm).await
        }
        async fn remove_file(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_file(path).await
        }
        async fn remove_dir(&self, path: &str) -> Result<(), BrowserError> {
            self.inner.remove_dir(path).await
        }
        async fn rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.rename(from, to).await
        }
        async fn posix_rename(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.posix_rename(from, to).await
        }
        async fn hardlink(&self, from: &str, to: &str) -> Result<(), BrowserError> {
            self.inner.hardlink(from, to).await
        }
        async fn symlink(&self, target: &str, path: &str) -> Result<(), BrowserError> {
            self.inner.symlink(target, path).await
        }
        async fn read_link(&self, path: &str) -> Result<String, BrowserError> {
            self.inner.read_link(path).await
        }
        async fn open_read(&self, path: &str) -> Result<Box<dyn RemoteReader>, BrowserError> {
            self.inner.open_read(path).await
        }
        async fn create_exclusive(
            &self,
            path: &str,
            perm: u32,
        ) -> Result<Box<dyn RemoteWriter>, BrowserError> {
            self.inner.create_exclusive(path, perm).await
        }
        async fn set_attrs(
            &self,
            path: &str,
            mode: Option<u32>,
            mtime: Option<i64>,
        ) -> Result<(), BrowserError> {
            self.inner.set_attrs(path, mode, mtime).await
        }
        fn has_hardlink(&self) -> bool {
            self.inner.has_hardlink()
        }
        fn has_posix_rename(&self) -> bool {
            self.inner.has_posix_rename()
        }
        fn extensions(&self) -> &BTreeMap<String, String> {
            self.inner.extensions()
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            Ok(()) // inner is borrowed; the test closes it
        }
        fn agent_cap(&self) -> crate::browser::agent::AgentCap {
            self.cap
        }
        async fn open_exec(
            &self,
            cmd: &str,
        ) -> Result<Box<dyn crate::browser::agent::ExecChannel>, BrowserError> {
            self.exec_opens.fetch_add(1, Ordering::SeqCst);
            // S-1: the "perl too old" host double replaces the agent command.
            let cmd = self.shim.unwrap_or(cmd);
            let ch = crate::browser::agent::loopback::LocalExecChannel::spawn(cmd).await?;
            Ok(match &self.cancel {
                Some((tok, threshold, written)) => Box::new(CountingExec {
                    inner: ch,
                    cancel: tok.clone(),
                    threshold: *threshold,
                    written: Arc::clone(written),
                }),
                None => ch,
            })
        }
    }

    /// M2b engine: run_copy with a PerlExec destination must take the AGENT
    /// tier — bytes land through the REAL vendored perl (loopback child),
    /// mtime travels in the F header, the stage is the agent's own and is
    /// cleaned by the agent, and no SFTP staging was involved on the dst.
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_agent_tier_lands_bytes_via_loopback_perl() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("agent-eng-src"))).await;
        let payload = vec![5u8; CHUNK * 3 + 9];
        std::fs::create_dir_all(srv.root.join("src/docs")).unwrap();
        std::fs::write(srv.root.join("src/big.bin"), &payload).unwrap();
        set_mtime_secs(&srv.root.join("src/big.bin"), 1_700_000_321);
        std::fs::write(srv.root.join("src/docs/in.txt"), b"inner").unwrap();
        let dst = temp_root("agent-eng-dst"); // real LOCAL dir: perl writes here

        let t = srv.connect(&temp_root("trustAGENTENG")).await;
        let wrapper = ExecCapable {
            inner: &t,
            cap: crate::browser::agent::AgentCap::PerlExec,
            exec_opens: Arc::new(AtomicU64::new(0)),
            cancel: None,
            shim: None,
        };
        let (emit, _p) = collector();
        let outcome = transfer::run_copy(
            &r2r_req("/src", dst.to_str().unwrap(), &["big.bin", "docs"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &wrapper,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("agent-tier copy succeeds");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(outcome.bytes, payload.len() as u64 + 5);
        assert_eq!(
            std::fs::read(dst.join("big.bin")).unwrap(),
            payload,
            "3×CHUNK+9 bytes land through the real perl agent"
        );
        assert_eq!(mtime_secs(&dst.join("big.bin")), 1_700_000_321);
        assert_eq!(std::fs::read(dst.join("docs/in.txt")).unwrap(), b"inner");
        assert!(
            stage_leftovers(&dst).is_empty(),
            "the agent dropped its own stage"
        );
        assert!(
            wrapper.exec_opens.load(Ordering::SeqCst) >= 1,
            "the exec lane was used"
        );
        assert_eq!(
            std::fs::read(srv.root.join("src/big.bin")).unwrap(),
            payload,
            "source untouched"
        );
        assert!(stage_leftovers(&srv.root.join("src")).is_empty());
        Box::new(t).close().await.unwrap();
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// M2b engine, the brief's hard requirement: cancel MID-COPY on the
    /// agent tier — the stream ends without `Z`, run_copy's cleanup closes
    /// the agent's stdin, the agent removes its own stage through its held
    /// handles. No partial file, no stage leftovers, source untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_agent_tier_cancel_midcopy_leaves_nothing() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("agent-eng-x-src"))).await;
        let payload = vec![3u8; CHUNK * 8];
        std::fs::write(srv.root.join("big.bin"), &payload).unwrap();
        let dst = temp_root("agent-eng-x-dst");
        let t = srv.connect(&temp_root("trustAGENTX")).await;
        let cancel = CancellationToken::new();
        let wrapper = ExecCapable {
            inner: &t,
            cap: crate::browser::agent::AgentCap::PerlExec,
            exec_opens: Arc::new(AtomicU64::new(0)),
            // Cancel once the first real chunk has gone into the agent.
            cancel: Some((cancel.clone(), CHUNK as u64, Arc::new(AtomicU64::new(0)))),
            shim: None,
        };
        let (emit, _p) = collector();
        let e = transfer::run_copy(
            &r2r_req("/", dst.to_str().unwrap(), &["big.bin"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &wrapper,
                windows: false,
            },
            cancel,
            emit,
        )
        .await
        .expect_err("cancelled copy errors");
        assert!(transfer::is_cancelled(&e), "expected cancelled, got {e:?}");
        assert!(!dst.join("big.bin").exists(), "no partial destination file");
        assert!(
            stage_leftovers(&dst).is_empty(),
            "the agent removed its stage after its stdin ended"
        );
        assert_eq!(std::fs::read(srv.root.join("big.bin")).unwrap(), payload);
        Box::new(t).close().await.unwrap();
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// M2b engine: a host WITHOUT perl (ExecNoPerl) keeps the CURRENT SFTP
    /// tiers unchanged — the exec channel is never opened and the bytes
    /// land through RemoteSink (documented fallback, no silent downgrade).
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_sftp_tier_unchanged_without_perl() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("agent-eng-sftp"))).await;
        std::fs::create_dir_all(srv.root.join("src")).unwrap();
        std::fs::write(srv.root.join("src/a.txt"), b"aaa").unwrap();
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let t = srv.connect(&temp_root("trustAGENTSFTP")).await;
        let wrapper = ExecCapable {
            inner: &t,
            cap: crate::browser::agent::AgentCap::ExecNoPerl,
            exec_opens: Arc::new(AtomicU64::new(0)),
            cancel: None,
            shim: None,
        };
        let (emit, _p) = collector();
        transfer::run_copy(
            &r2r_req("/src", "/dst", &["a.txt"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &wrapper,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("SFTP-tier copy still succeeds for no-perl hosts");
        assert_eq!(std::fs::read(srv.root.join("dst/a.txt")).unwrap(), b"aaa");
        assert_eq!(
            wrapper.exec_opens.load(Ordering::SeqCst),
            0,
            "no-perl hosts must never open an exec channel"
        );
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// M2b engine: a WINDOWS destination never takes the agent tier even
    /// when the capability says PerlExec — run_copy's own `!windows` gate
    /// (the M3 gate; commands-layer preflight refuses separately).
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_windows_dst_never_uses_agent_tier() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("agent-eng-win"))).await;
        std::fs::create_dir_all(srv.root.join("src")).unwrap();
        std::fs::write(srv.root.join("src/a.txt"), b"aaa").unwrap();
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let t = srv.connect(&temp_root("trustAGENTWIN")).await;
        let wrapper = ExecCapable {
            inner: &t,
            cap: crate::browser::agent::AgentCap::PerlExec,
            exec_opens: Arc::new(AtomicU64::new(0)),
            cancel: None,
            shim: None,
        };
        let (emit, _p) = collector();
        transfer::run_copy(
            &r2r_req("/src", "/dst", &["a.txt"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &wrapper,
                windows: true,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("windows-dst copy stays on the SFTP tier");
        assert_eq!(std::fs::read(srv.root.join("dst/a.txt")).unwrap(), b"aaa");
        assert_eq!(
            wrapper.exec_opens.load(Ordering::SeqCst),
            0,
            "windows destinations must never open an exec channel"
        );
        Box::new(t).close().await.unwrap();
    }

    // ---- S-1: rc-9 "perl too old" capability self-heal ---------------------

    /// The "perl too old" host double: consumes stdin (so the bail surfaces
    /// at `place`, exactly like the real agent's rc-9 path), then exits 9
    /// with the vendored agent's exact stderr line.
    #[cfg(unix)]
    const RC9_SHIM: &str =
        "cat >/dev/null; printf 'perl here is too old (needs 5.22 or newer)\\n' >&2; exit 9";

    /// S-1 end-to-end (the brief's flow): first copy fails with the rc-9
    /// typed error → the self-heal downgrades the cached cap to ExecNoPerl →
    /// the SECOND copy lands the bytes through RemoteSink with ZERO
    /// agent-tier exec opens (mirrors `engine_sftp_tier_unchanged_without_perl`).
    /// The commands-layer hook itself (execute_copy + browser_delete calling
    /// `downgrade_on_perl_too_old`) is tripwire-pinned below.
    /// Pre-fix RED: without the self-heal the cap stayed PerlExec and the
    /// second copy re-opened + re-failed the agent tier
    /// (m2c-red-s1-demo.txt receipt).
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_rc9_selfheal_second_copy_takes_sftp_tier() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("agent-rc9-heal"))).await;
        std::fs::create_dir_all(srv.root.join("src")).unwrap();
        std::fs::write(srv.root.join("src/a.txt"), b"aaa").unwrap();
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let t = srv.connect(&temp_root("trustAGENTRC9")).await;

        // The bootstrap probe passed (exit 2 needs nothing modern)...
        let cache = AgentCapCache::new();
        cache.put("h", "u", AgentCap::PerlExec);

        // Copy 1: agent tier, the host's perl bails rc 9 "too old".
        let o1 = Arc::new(AtomicU64::new(0));
        let w1 = ExecCapable {
            inner: &t,
            cap: cache.get("h", "u").unwrap(),
            exec_opens: Arc::clone(&o1),
            cancel: None,
            shim: Some(RC9_SHIM),
        };
        let (emit, _p) = collector();
        let e1 = transfer::run_copy(
            &r2r_req("/src", "/dst", &["a.txt"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &w1,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect_err("the rc-9 copy fails with a typed error");
        assert_eq!(e1.kind, ErrorKind::Unsupported, "{e1:?}");
        assert!(
            crate::browser::agent::is_perl_too_old(&e1),
            "rc-9 marker rides the typed error: {e1:?}"
        );
        assert!(e1.message.contains("too old"), "{e1:?}");
        assert!(o1.load(Ordering::SeqCst) >= 1, "the agent tier was used");
        assert!(
            !srv.root.join("dst/a.txt").exists(),
            "the failed copy landed nothing"
        );
        assert!(
            stage_leftovers(&srv.root.join("dst")).is_empty(),
            "the failed agent run left no stage"
        );

        // The self-heal (the commands layer calls exactly this on any copy
        // error; the method gates on the rc-9 marker itself).
        cache.downgrade_on_perl_too_old("h", "u", &e1);
        assert_eq!(
            cache.get("h", "u"),
            Some(AgentCap::ExecNoPerl),
            "the cached cap self-healed to ExecNoPerl"
        );

        // Copy 2: SFTP tier, ZERO exec opens, bytes land unchanged.
        let o2 = Arc::new(AtomicU64::new(0));
        let w2 = ExecCapable {
            inner: &t,
            cap: cache.get("h", "u").unwrap(),
            exec_opens: Arc::clone(&o2),
            cancel: None,
            shim: None, // moot: the SFTP tier never execs
        };
        let (emit, _p) = collector();
        transfer::run_copy(
            &r2r_req("/src", "/dst", &["a.txt"], false),
            &Side::Remote {
                transport: &t,
                windows: false,
            },
            &Side::Remote {
                transport: &w2,
                windows: false,
            },
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("the second copy lands via the unchanged SFTP tiers");
        assert_eq!(std::fs::read(srv.root.join("dst/a.txt")).unwrap(), b"aaa");
        assert_eq!(
            o2.load(Ordering::SeqCst),
            0,
            "the self-healed host never re-opens the agent tier"
        );
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }

    /// S-1 + S-2 interaction: a downgraded entry SHORT-CIRCUITS
    /// `get_or_probe` — no re-probe (the bootstrap would just re-classify
    /// PerlExec and re-arm the failing tier). Recovery stays the explicit
    /// invalidation (forget-host / account change / restart).
    #[tokio::test]
    async fn downgraded_cap_short_circuits_get_or_probe() {
        let cache = AgentCapCache::new();
        cache.put("h", "u", AgentCap::PerlExec);
        let e9 = crate::browser::agent::classify_agent_failure(
            Some(9),
            "perl here is too old (needs 5.22 or newer)\n",
        );
        cache.downgrade_on_perl_too_old("h", "u", &e9);
        assert_eq!(cache.get("h", "u"), Some(AgentCap::ExecNoPerl));
        let t = GatedProbeTransport::new();
        assert_eq!(cache.get_or_probe("h", "u", &t).await, AgentCap::ExecNoPerl);
        assert_eq!(
            t.exec_opens(),
            0,
            "the self-healed entry is a cache hit — never a re-probe"
        );
    }

    // ---- S-2: single-flight probe doubles ----------------------------------

    /// An `ExecChannel` whose `wait_exit` blocks on the test's gate — the
    /// deterministic overlap the single-flight tests need (no sleeps): the
    /// probe is INSIDE `wait_exit` from `started` until `gate` fires, and
    /// then reports rc 2 (the bootstrap's positive signal → PerlExec).
    struct GatedChannel {
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ExecChannel for GatedChannel {
        async fn write_all(&mut self, _data: &[u8]) -> Result<(), BrowserError> {
            Ok(())
        }
        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, BrowserError> {
            Ok(0) // EOF: the probe never reads the reply stream
        }
        async fn close_stdin(&mut self) -> Result<(), BrowserError> {
            Ok(())
        }
        async fn wait_exit(&mut self, _timeout: Duration) -> Option<u32> {
            self.gate.notified().await;
            Some(2) // "perl ran and honored the framing contract"
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    /// Counting transport double for probe tests: `open_exec` increments the
    /// counter, signals `started`, and returns the gated channel. Every SFTP
    /// verb refuses — the probe lane must never touch SFTP.
    struct GatedProbeTransport {
        exts: std::collections::BTreeMap<String, String>,
        exec_opens: Arc<std::sync::atomic::AtomicU64>,
        started: Arc<tokio::sync::Notify>,
        gate: Arc<tokio::sync::Notify>,
    }

    impl GatedProbeTransport {
        fn new() -> Self {
            Self {
                exts: Default::default(),
                exec_opens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                started: Arc::new(tokio::sync::Notify::new()),
                gate: Arc::new(tokio::sync::Notify::new()),
            }
        }
        fn exec_opens(&self) -> u64 {
            self.exec_opens.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn no_sftp<T>() -> Result<T, BrowserError> {
            Err(BrowserError::other("no SFTP in the gated probe double"))
        }
    }

    #[async_trait]
    impl SftpTransport for GatedProbeTransport {
        async fn realpath(&self, _path: &str) -> Result<String, BrowserError> {
            Self::no_sftp()
        }
        async fn read_dir(
            &self,
            _path: &str,
        ) -> Result<Vec<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn stat(
            &self,
            _path: &str,
        ) -> Result<Option<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn lstat(
            &self,
            _path: &str,
        ) -> Result<Option<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn mkdir(&self, _path: &str, _perm: u32) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn remove_file(&self, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn remove_dir(&self, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn rename(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn posix_rename(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn hardlink(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn symlink(&self, _target: &str, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn read_link(&self, _path: &str) -> Result<String, BrowserError> {
            Self::no_sftp()
        }
        async fn open_read(
            &self,
            _path: &str,
        ) -> Result<Box<dyn crate::browser::transport::RemoteReader>, BrowserError> {
            Self::no_sftp()
        }
        async fn create_exclusive(
            &self,
            _path: &str,
            _perm: u32,
        ) -> Result<Box<dyn crate::browser::transport::RemoteWriter>, BrowserError> {
            Self::no_sftp()
        }
        async fn set_attrs(
            &self,
            _path: &str,
            _mode: Option<u32>,
            _mtime: Option<i64>,
        ) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        fn has_hardlink(&self) -> bool {
            false
        }
        fn has_posix_rename(&self) -> bool {
            false
        }
        fn extensions(&self) -> &std::collections::BTreeMap<String, String> {
            &self.exts
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            Ok(())
        }
        async fn open_exec(&self, _cmd: &str) -> Result<Box<dyn ExecChannel>, BrowserError> {
            self.exec_opens
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.started.notify_one();
            Ok(Box::new(GatedChannel {
                gate: Arc::clone(&self.gate),
            }))
        }
    }

    /// S-2: two concurrent first opens of the same host+user run EXACTLY
    /// ONE bounded probe (single-flight). Deterministic, no sleeps: the
    /// gated double parks caller 1 inside the probe's `wait_exit`; caller 2
    /// then parks on the in-flight cell (current-thread runtime: its task
    /// yields only inside `get_or_init`'s wait), and releasing the gate
    /// finishes the single real probe — both callers receive its result.
    /// Pre-fix RED: the old open_side get/miss/probe/put shape ran TWO
    /// probes here (m2c-red-s2-demo.txt receipt).
    #[tokio::test]
    async fn cap_cache_single_flights_concurrent_probes() {
        let cache = Arc::new(AgentCapCache::new());
        let t = Arc::new(GatedProbeTransport::new());
        let started = Arc::clone(&t.started);
        let gate = Arc::clone(&t.gate);

        let c1 = Arc::clone(&cache);
        let t1 = Arc::clone(&t);
        let h1 = tokio::spawn(async move { c1.get_or_probe("h", "u", t1.as_ref()).await });
        started.notified().await; // caller 1 is INSIDE the gated probe

        let entered = Arc::new(tokio::sync::Notify::new());
        let c2 = Arc::clone(&cache);
        let t2 = Arc::clone(&t);
        let e2 = Arc::clone(&entered);
        let h2 = tokio::spawn(async move {
            e2.notify_one();
            c2.get_or_probe("h", "u", t2.as_ref()).await
        });
        entered.notified().await; // h2 has yielded INSIDE get_or_init's wait
        assert_eq!(
            cache.get("h", "u"),
            None,
            "an in-flight cell reads as None (never a guess)"
        );

        gate.notify_one(); // release the single real probe (rc 2 → PerlExec)
        let (a, b) = (h1.await.unwrap(), h2.await.unwrap());
        assert_eq!(a, AgentCap::PerlExec, "the winner got the probe result");
        assert_eq!(b, AgentCap::PerlExec, "the waiter got the SAME result");
        assert_eq!(
            t.exec_opens(),
            1,
            "exactly one bounded probe under concurrency (S-2)"
        );
        assert_eq!(cache.get("h", "u"), Some(AgentCap::PerlExec));
    }

    /// Tripwire for the M2b wiring (repo pattern): tier selection in
    /// run_copy, the once-per-host/account probe + cache in open_side, the
    /// invalidation pair, and the delete re-check — so a refactor can't
    /// silently drop a gate even if a behavior test is reworked.
    #[test]
    fn agent_tier_wiring_tripwire() {
        let tsrc = include_str!("transfer/mod.rs");
        assert!(
            tsrc.contains("agent_cap() == crate::browser::agent::AgentCap::PerlExec"),
            "run_copy must gate the agent tier on the probed capability"
        );
        assert!(tsrc.contains("AgentSink::open"), "agent tier wired");
        let csrc = include_str!("commands.rs");
        assert!(
            csrc.contains(".get_or_probe(target, transport.user(), &transport)"),
            "open_side probes the capability once per host/account THROUGH the single-flight cache (S-2)"
        );
        let asrc = include_str!("agent.rs");
        assert!(
            asrc.contains("get_or_init(|| probe_agent_cap(t))"),
            "the cap cache coalesces concurrent probes into exactly one (S-2 single-flight)"
        );
        assert!(
            asrc.contains("pub fn classify_delete_failure"),
            "the delete-helper failure mapping is pure + shape-pinned (S-3)"
        );
        assert_eq!(
            csrc.matches("downgrade_on_perl_too_old").count(),
            2,
            "the rc-9 self-heal is wired at BOTH agent lanes: copy (execute_copy) and delete (browser_delete) (S-1)"
        );
        assert_eq!(
            csrc.matches("agent_caps.invalidate_host").count(),
            2,
            "capability cache invalidated on account change AND forget-host"
        );
        assert!(
            csrc.contains("agent::agent_delete(&transport, &resolved)"),
            "remote delete goes through the physical re-check"
        );
    }
}
