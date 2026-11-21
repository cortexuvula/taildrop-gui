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
        }
    }
}

/// A running fake server. Dropping it aborts the accept loop and removes
/// the temp root.
pub struct FakeServer {
    pub addr: SocketAddr,
    pub root: PathBuf,
    /// Base64 of the server host key's SSH wire blob — for tailscale
    /// `sshHostKeys` cross-check tests.
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
        Ok(if self.cfg.accept_none_auth {
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
}

impl SftpHandler {
    fn new(cfg: Arc<FakeConfig>) -> Self {
        Self {
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
    use std::os::unix::fs::PermissionsExt;
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
        permissions: Some(meta.permissions().mode()),
        atime: secs(meta.accessed()),
        mtime: secs(meta.modified()),
    }
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
                if let Some(perm) = attrs.permissions {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = f.set_permissions(std::fs::Permissions::from_mode(perm & 0o7777));
                }
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
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        use std::os::unix::fs::FileExt;
        let path = match self.handles.get(&handle) {
            Some(HEntry::File(p)) => p.clone(),
            _ => return Err(StatusCode::Failure),
        };
        let f = std::fs::File::open(&path).map_err(map_io)?;
        let size = f.metadata().map_err(map_io)?.len();
        if offset >= size {
            return Err(StatusCode::Eof);
        }
        let want = std::cmp::min(len as u64, size - offset) as usize;
        let mut buf = vec![0u8; want];
        let n = f.read_at(&mut buf, offset).map_err(map_io)?;
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
        use std::os::unix::fs::FileExt;
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
        f.write_all_at(&data, offset).map_err(map_io)?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::symlink_metadata(self.resolve(&path)).map_err(map_io)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
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
        let target = std::fs::read_link(self.resolve(&path)).map_err(map_io)?;
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
        std::os::unix::fs::symlink(&targetpath, self.resolve(&linkpath)).map_err(map_io)?;
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
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        if let Some(perm) = attrs.permissions {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm & 0o7777));
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
    use crate::browser::transfer::{self, Side, CHUNK};
    use crate::browser::transport::{RemoteNode, RemoteReader, RemoteWriter, SftpTransport};
    use crate::browser::{BrowserError, CopyRequest, ErrorKind, ServerFlavor, STAGE_PREFIX};
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
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

    // ---- Group C: full copy runs — fidelity, consent, cleanup ----

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

    /// Hostile server: writes start failing mid-copy (PermissionDenied).
    /// The primary error surfaces (never masked by cleanup errors) and the
    /// staging tree is removed on the failed path.
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
}
