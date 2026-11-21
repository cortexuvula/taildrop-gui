//! Two-pane file browser over pure SFTP on Tailscale SSH (M1).
//!
//! Architecture: report.md §7.1 (module tree), §7.2 (command surface),
//! §7.4 (safety-invariant table — the "v1" column is the acceptance
//! contract). Fleet facts from m0-report.md OVERRIDE the analysis:
//!
//! - Transport: russh 0.65.2 + russh-sftp 3.0.1 (pins proven by the M0 spike).
//! - Remote placement: `hardlink@openssh.com` no-replace tier where
//!   advertised (collision verified), else check-then-rename with mandatory
//!   conflict consent and a documented TOCTOU ceiling.
//! - Vanilla SFTP RENAME semantics DIFFER by server flavor — the placement
//!   path must never depend on rename-replaces-or-refuses behavior.
//! - Tailscale SSH does NOT advertise fsync@openssh.com — never require it.
//! - Windows peers: BROWSE + PULL ONLY in M1 (writes refused until the M3
//!   gate); client-side Windows name validation is mandatory regardless.
//!
//! The existing Taildrop flow (tailscale.rs, receipts.rs) is untouched; this
//! module tree is purely additive.

pub mod commands;
#[cfg(test)]
pub mod fake;
pub mod guards;
pub mod listing;
pub mod probe;
pub mod session;
pub mod transfer;
pub mod transport;
pub mod trust;
pub mod winnames;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The pane-target string used across IPC: `"local"` or a peer host
/// (MagicDNS short name or Tailscale IP).
pub const LOCAL_TARGET: &str = "local";

/// Staging-dir prefix, kept identical to the plugin's so residual folders
/// are recognizable across tools and never GC'd by name (invariant §4.2).
pub const STAGE_PREFIX: &str = ".tailfiles-incoming-";

// Resource caps on everything a remote can send (invariant §4.12).
/// Nesting depth cap for copy streams / walk plans.
pub const MAX_DEPTH: usize = 512;
/// Per-field (name/path) byte cap.
pub const MAX_FIELD_BYTES: usize = 64 * 1024;
/// Decimal digits allowed in a server-supplied number field.
/// Record/parser numeric-field cap (plugin port); exercised by guards tests.
#[allow(dead_code)]
pub const MAX_NUMBER_DIGITS: usize = 15;
/// Hard cap on listing entries returned for one folder.
pub const MAX_LISTING_ENTRIES: usize = 100_000;
/// Cap on symlink probes per listing (plugin: ≤200).
pub const MAX_LINK_PROBES: usize = 200;

/// Typed error taxonomy — port of the plugin's classification
/// (bin/tailfiles:221-236) plus `unsupported` for missing
/// extensions/capabilities (report §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    Auth,
    Hostkey,
    Unreachable,
    Notfound,
    Denied,
    Exists,
    Account,
    Unsupported,
    Other,
}

impl ErrorKind {
    #[allow(dead_code)] // wire-mapping helper for frontend diagnostics
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Auth => "auth",
            ErrorKind::Hostkey => "hostkey",
            ErrorKind::Unreachable => "unreachable",
            ErrorKind::Notfound => "notfound",
            ErrorKind::Denied => "denied",
            ErrorKind::Exists => "exists",
            ErrorKind::Account => "account",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Other => "other",
        }
    }
}

/// Error shape crossing the IPC boundary: `{message, kind, names?}` so the
/// UI can render targeted error panes (auth/unreachable/hostkey) and the
/// Replace flow (exists + names).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserError {
    pub message: String,
    pub kind: ErrorKind,
    /// Present only for `kind == exists`: the colliding names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    /// Raw SSH_FXP_STATUS code when the error came from an SFTP status
    /// packet (B6-R: `ErrorKind` merges SSH_FX_FAILURE and SSH_FX_BAD_MESSAGE
    /// into `other`, but the Windows junction probe must tell them apart —
    /// Win32-OpenSSH answers generic FAILURE for a PLAIN directory and
    /// BAD_MESSAGE for a reparse point). Values per draft-ietf-secsh-filexfer:
    /// 2=NO_SUCH_FILE, 3=PERMISSION_DENIED, 4=FAILURE, 5=BAD_MESSAGE,
    /// 8=OP_UNSUPPORTED.
    #[serde(
        default,
        rename = "sftpStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub sftp_status: Option<u32>,
}

impl BrowserError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            sftp_status: None,
            names: None,
        }
    }

    pub fn exists(names: Vec<String>, folder: &str, partial: bool) -> Self {
        Self {
            message: guards::exists_message(&names, folder, partial),
            kind: ErrorKind::Exists,
            names: Some(names),
            sftp_status: None,
        }
    }

    /// Attach the raw SSH_FXP_STATUS code (B6-R probe disambiguation).
    pub fn with_sftp_status(mut self, code: u32) -> Self {
        self.sftp_status = Some(code);
        self
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message)
    }
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BrowserError {}

impl From<std::io::Error> for BrowserError {
    fn from(e: std::io::Error) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::Notfound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::Denied,
            std::io::ErrorKind::AlreadyExists => ErrorKind::Exists,
            _ => ErrorKind::Other,
        };
        BrowserError::new(kind, e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
}

/// One listing row. Wire shape mirrors the plugin's `ls` entries
/// (`{name, type, link, size, mtime, perm}`) so the frontend logic ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EntryKind,
    pub link: bool,
    pub size: u64,
    pub mtime: i64,
    pub perm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    /// Names omitted because they cannot be read safely (control chars,
    /// duplicates, non-UTF-8) — counted exactly once per entry.
    pub skipped: u32,
    /// The account this listing was made under — the pane pins later
    /// actions to it (`loadedUser`, FilePane.qml:132-134).
    pub user: String,
    pub home: String,
}

/// SSH server flavor. Trust pins are stored per flavor (M0: capability
/// hints alone cannot classify the serving endpoint — this Mac advertises
/// cap/ssh but serves OpenSSH).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerFlavor {
    TailscaleSsh,
    Openssh,
    OpensshWindows,
}

impl ServerFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerFlavor::TailscaleSsh => "tailscale-ssh",
            ServerFlavor::Openssh => "openssh",
            ServerFlavor::OpensshWindows => "openssh-windows",
        }
    }

    /// Classify from the SSH identification banner (M0-observed banners:
    /// `SSH-2.0-Tailscale`, `SSH-2.0-OpenSSH_10.3`,
    /// `SSH-2.0-OpenSSH_for_Windows_9.5`).
    pub fn from_banner(banner: &str) -> Option<Self> {
        if !banner.starts_with("SSH-2.0-") {
            return None;
        }
        if banner.contains("Tailscale") {
            Some(ServerFlavor::TailscaleSsh)
        } else if banner.contains("Windows") {
            Some(ServerFlavor::OpensshWindows)
        } else if banner.contains("OpenSSH") {
            Some(ServerFlavor::Openssh)
        } else {
            // Unknown SSH-2.0 server: treat as plain OpenSSH flavor for pin
            // storage; the pin itself is what authenticates future connects.
            Some(ServerFlavor::Openssh)
        }
    }

    pub fn is_windows(self) -> bool {
        matches!(self, ServerFlavor::OpensshWindows)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeInfo {
    pub system: String,
    pub posix: bool,
    pub home: String,
    pub flavor: ServerFlavor,
    /// The account the probe authenticated as — pin for later actions.
    pub user: String,
    /// Raw VERSION extension map captured from the wire (M0: high-level
    /// session objects drop unknown names; advertisement alone does not
    /// prove operation semantics).
    pub extensions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPeer {
    /// Connect target: MagicDNS short name, else first Tailscale IP.
    pub host: String,
    pub name: String,
    pub dns: String,
    pub ip: String,
    pub os: String,
    pub online: bool,
    /// Saved per-host user from browser config ("" = unset).
    pub user: String,
    /// Capability HINT from CapMap/sshHostKeys. Field-presence test only —
    /// values can be null, and hints can misclassify (M0).
    pub tailscale_ssh: bool,
    /// False for phones/tablets/TVs — visible for Taildrop, not browsable.
    pub browsable: bool,
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRequest {
    pub src_target: String,
    /// Account pin captured from the source pane's `loadedUser`.
    pub src_user: Option<String>,
    pub src_dir: String,
    pub dst_target: String,
    /// Account pin captured from the destination pane's `loadedUser`.
    pub dst_user: Option<String>,
    pub dst_dir: String,
    /// Basenames, all inside `src_dir` (one-parent rule).
    pub names: Vec<String>,
    /// Explicit user consent to replace colliding items (Replace click).
    pub replace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Queue-row snapshot for `browser_copy_jobs` re-sync and event payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyJobSnapshot {
    pub id: u64,
    pub state: JobState,
    /// Human label: "3 items → /Downloads @ studio-mac".
    pub label: String,
    pub pct: u8,
    pub rate: String,
    /// Name of the file currently streaming ("" between files).
    pub file: String,
    pub skipped: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ErrorKind>,
    /// Colliding names when `kind == exists` — drives the row Replace button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    /// The original request, kept so a failed row's Replace re-enqueues the
    /// identical job with `replace: true` (App.qml:227-242 port).
    pub request: CopyRequest,
    pub created: u64,
}

/// `browser-copy-event` payload — mirrors the plugin's event stream shapes
/// (§1.3) with `jobId` added for the Rust-side registry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyEvent {
    pub job_id: u64,
    /// "start" | "progress" | "done"
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ErrorKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl CopyEvent {
    fn base(job_id: u64, event: &str) -> Self {
        Self {
            job_id,
            event: event.to_string(),
            count: None,
            pct: None,
            rate: None,
            file: None,
            ok: None,
            skipped: None,
            kind: None,
            names: None,
            error: None,
            cancelled: None,
        }
    }

    pub fn start(job_id: u64, count: u32) -> Self {
        let mut e = Self::base(job_id, "start");
        e.count = Some(count);
        e
    }

    pub fn progress(job_id: u64, pct: u8, rate: String, file: String) -> Self {
        let mut e = Self::base(job_id, "progress");
        e.pct = Some(pct);
        e.rate = Some(rate);
        e.file = Some(file);
        e
    }

    pub fn done_ok(job_id: u64, skipped: u32) -> Self {
        let mut e = Self::base(job_id, "done");
        e.ok = Some(true);
        e.skipped = Some(skipped);
        e
    }

    pub fn done_err(job_id: u64, err: &BrowserError) -> Self {
        let mut e = Self::base(job_id, "done");
        e.ok = Some(false);
        e.error = Some(err.message.clone());
        e.kind = Some(err.kind);
        e.names = err.names.clone();
        e
    }

    pub fn done_cancelled(job_id: u64) -> Self {
        let mut e = Self::base(job_id, "done");
        e.ok = Some(false);
        e.cancelled = Some(true);
        e.error = Some("Cancelled".to_string());
        e
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteOutcome {
    /// true = items went to the Trash (local); false = permanent (remote).
    pub trashed: bool,
    /// How many items were removed before any failure (partial progress).
    pub done: usize,
}

/// One pinned host key for the Known-hosts management dialog (M2a, M1
/// gap 4). `fingerprint` is the ssh-keygen-style SHA256 of the stored key
/// ("" when the stored pin can't be parsed — the pin still enforces
/// byte-exact at connect time regardless of display).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHostPin {
    pub host: String,
    pub flavor: ServerFlavor,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Persisted browser config (per-host users + last remote), atomic-written
/// JSON in the app config dir — port of save_config (bin/tailfiles:111-117).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrowserConfig {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostConfig>,
    #[serde(default)]
    pub last: String,
}

/// Format a byte count the way the plugin's Progress does ("12.3MB/s").
pub fn human_rate(bytes_per_sec: f64) -> String {
    // 1024-based human(), ported from bin/tailfiles:700-705.
    let mut n = bytes_per_sec.max(0.0);
    for unit in ["B", "KB", "MB", "GB"] {
        if n < 1024.0 {
            return if unit == "B" {
                format!("{n:.0}{unit}/s")
            } else {
                format!("{n:.1}{unit}/s")
            };
        }
        n /= 1024.0;
    }
    format!("{n:.1}TB/s")
}

/// Persisted browser config: atomic-written JSON in the app config dir
/// (port of save_config, bin/tailfiles:111-117). Corrupt files fall back to
/// defaults rather than failing (load_config behavior) — a broken config
/// must never brick browsing.
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[allow(dead_code)] // diagnostics: where the config lives
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> BrowserConfig {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => BrowserConfig::default(),
        }
    }

    /// Atomic write: tmp + rename (never leaves a half-written config).
    pub fn save(&self, cfg: &BrowserConfig) -> Result<(), BrowserError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| BrowserError::other(format!("config dir: {e}")))?;
        }
        let tmp = self.path.with_extension("tmp");
        let payload = serde_json::to_vec_pretty(cfg)
            .map_err(|e| BrowserError::other(format!("config encode: {e}")))?;
        std::fs::write(&tmp, &payload)
            .map_err(|e| BrowserError::other(format!("config write: {e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| BrowserError::other(format!("config commit: {e}")))?;
        Ok(())
    }

    /// Saved user for a host, validated against the plugin's USER_RE
    /// (bin/tailfiles:55) — a corrupt config value can never become an
    /// ssh account string.
    pub fn user_for(&self, host: &str) -> Option<String> {
        let cfg = self.load();
        cfg.hosts
            .get(host)
            .and_then(|h| h.user.clone())
            .filter(|u| valid_user(u))
    }

    pub fn set_user(&self, host: &str, user: Option<String>) -> Result<(), BrowserError> {
        let mut cfg = self.load();
        match user {
            Some(u) if valid_user(&u) => {
                cfg.hosts.entry(host.to_string()).or_default().user = Some(u);
            }
            Some(_) => {
                return Err(BrowserError::new(
                    ErrorKind::Account,
                    "That doesn't look like a usable account name",
                ))
            }
            None => {
                if let Some(entry) = cfg.hosts.get_mut(host) {
                    entry.user = None;
                }
            }
        }
        self.save(&cfg)
    }

    pub fn set_last(&self, host: &str) -> Result<(), BrowserError> {
        let mut cfg = self.load();
        cfg.last = host.to_string();
        self.save(&cfg)
    }
}

/// USER_RE port: `^[A-Za-z0-9._][A-Za-z0-9._@\-]*$`.
pub fn valid_user(user: &str) -> bool {
    let mut chars = user.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '.' || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '@' || c == '-')
}

/// HOST_RE port: `^[A-Za-z0-9][A-Za-z0-9._:-]*$`.
pub fn valid_host(host: &str) -> bool {
    let mut chars = host.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ':' || c == '-')
}

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    fn config_tmp(tag: &str) -> PathBuf {
        use rand::Rng as _;
        let n: u64 = rand::rng().random();
        let dir = std::env::temp_dir().join(format!(
            "taildrop-m2a-cfg-{tag}-{}-{n:016x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("browser.json")
    }

    /// M2a verification (brief item 8): the per-host account AND the
    /// last-remote selection persist across app restarts — modeled as a
    /// FRESH ConfigStore instance reading the same file (the store holds no
    /// in-memory cache, so a new instance == a restart).
    #[test]
    fn config_persists_across_instances() {
        let path = config_tmp("persist");
        {
            let store = ConfigStore::new(path.clone());
            store.set_user("studio", Some("corte".into())).unwrap();
            store.set_user("room-6", Some("admin".into())).unwrap();
            store.set_last("studio").unwrap();
        }
        // "Restart": a brand-new store instance must see everything.
        let reopened = ConfigStore::new(path.clone());
        let cfg = reopened.load();
        assert_eq!(cfg.last, "studio", "last-remote survives the restart");
        assert_eq!(
            reopened.user_for("studio").as_deref(),
            Some("corte"),
            "per-host account survives the restart"
        );
        assert_eq!(reopened.user_for("room-6").as_deref(), Some("admin"));
        assert_eq!(cfg.hosts.len(), 2);
        // A later last wins (reopen flow re-points at the newest machine).
        reopened.set_last("room-6").unwrap();
        assert_eq!(ConfigStore::new(path.clone()).load().last, "room-6");
        // Clearing an account persists too.
        reopened.set_user("studio", None).unwrap();
        assert_eq!(ConfigStore::new(path.clone()).user_for("studio"), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A corrupt config degrades to defaults rather than failing (load_config
    /// behavior) — a broken browser.json must never brick browsing.
    #[test]
    fn corrupt_config_falls_back_to_defaults() {
        let path = config_tmp("corrupt");
        std::fs::write(&path, b"{not json").unwrap();
        let store = ConfigStore::new(path.clone());
        let cfg = store.load();
        assert_eq!(cfg.last, "");
        assert!(cfg.hosts.is_empty());
        // Writing over the corrupt file recovers it.
        store.set_last("studio").unwrap();
        assert_eq!(ConfigStore::new(path.clone()).load().last, "studio");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A hostile/corrupt config value can never become an ssh account
    /// string: user_for filters through USER_RE even though the raw JSON
    /// holds the value.
    #[test]
    fn user_for_filters_invalid_accounts() {
        let path = config_tmp("filter");
        std::fs::write(
            &path,
            br#"{"hosts":{"good":{"user":"corte"},"bad":{"user":"../evil name"}},"last":""}"#,
        )
        .unwrap();
        let store = ConfigStore::new(path.clone());
        assert_eq!(store.user_for("good").as_deref(), Some("corte"));
        assert_eq!(
            store.user_for("bad"),
            None,
            "an invalid saved account is dropped, never used"
        );
        // set_user refuses the same shapes on the way in.
        assert!(store.set_user("x", Some("bad name".into())).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
