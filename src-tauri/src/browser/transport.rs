//! The SFTP transport seam (report §7.1). Every remote operation the browser
//! needs goes through this trait so three implementations stay
//! interchangeable:
//!   - `session::RusshTransport` — the real russh + russh-sftp client (M0 GO).
//!   - a fake in `browser::fake` (test-only) backed by a temp dir, so the
//!     copy engine, placement tiers, and listing can be exercised with no
//!     network and no live machine (acceptance gate 6).
//!   - a future shell-out fallback (documented, not implemented in M1).
//!
//! Paths are protocol data, never shell text: nothing here concatenates a
//! path into a command string. The trait surface mirrors the SFTP subsystem
//! one-to-one so the russh impl is a thin adapter.

use async_trait::async_trait;

use crate::browser::BrowserError;

/// Raw attributes for one remote node, as reported by the server. `name` is
/// the basename for `read_dir` entries and empty for `stat`/`lstat`.
/// `listing.rs` converts this into a UI `Entry` (skipping unsafe names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteNode {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_regular: bool,
    /// True when the server could not classify the node (special file, or a
    /// Windows junction that SFTP reports as an ordinary dir — M0 §5). Such
    /// nodes are treated cautiously by delete/copy.
    pub is_unknown: bool,
    pub size: u64,
    pub mtime: i64,
    pub perm: u32,
}

impl RemoteNode {
    #[allow(dead_code)] // test-fixture helper
    pub fn empty() -> Self {
        Self {
            name: String::new(),
            is_dir: false,
            is_symlink: false,
            is_regular: false,
            is_unknown: true,
            size: 0,
            mtime: 0,
            perm: 0,
        }
    }
}

/// A remote file opened for reading. Chunked so the copy engine can count
/// real bytes (progress) and check cancellation between chunks.
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait + Result output (known interaction)
pub trait RemoteReader: Send + Sync {
    /// Read into `buf` at the current position; returns bytes read (0 = EOF).
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError>;
    /// Close the handle. Always awaited before the engine reports success or
    /// cleans up (M0: cancellation is cooperative at real byte boundaries).
    async fn close(self: Box<Self>) -> Result<(), BrowserError>;
}

/// A remote file created exclusively (O_EXCL semantics) and opened for
/// writing. Exclusive create is what makes the staging tag collision-safe.
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait + Result output (known interaction)
pub trait RemoteWriter: Send + Sync {
    async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError>;
    /// Drain buffered writes so progress reflects acknowledged bytes (M0:
    /// upload is flushed before progress is reported).
    async fn flush(&mut self) -> Result<(), BrowserError>;
    async fn close(self: Box<Self>) -> Result<(), BrowserError>;
}

/// The full remote-filesystem surface the browser uses.
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait + Result output (known interaction)
pub trait SftpTransport: Send + Sync {
    /// Canonicalize a path (SFTP realpath). Used for home discovery
    /// (`realpath(".")`) and to resolve a delete target's parents.
    async fn realpath(&self, path: &str) -> Result<String, BrowserError>;

    /// List a directory. Returns raw nodes including `.`/`..` if the server
    /// sends them; `listing.rs` filters and counts skips.
    async fn read_dir(&self, path: &str) -> Result<Vec<RemoteNode>, BrowserError>;

    /// Stat following symlinks. `Ok(None)` when the path does not exist.
    async fn stat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError>;

    /// Stat without following symlinks. `Ok(None)` when absent.
    async fn lstat(&self, path: &str) -> Result<Option<RemoteNode>, BrowserError>;

    /// Create a directory (fails if it exists — SFTP mkdir is exclusive).
    async fn mkdir(&self, path: &str, perm: u32) -> Result<(), BrowserError>;

    /// Remove a regular file (or a link, as a link).
    async fn remove_file(&self, path: &str) -> Result<(), BrowserError>;

    /// Remove an empty directory (on Windows this also removes a junction /
    /// dir-symlink link-only — M0 §5).
    async fn remove_dir(&self, path: &str) -> Result<(), BrowserError>;

    /// Rename. Vanilla SFTP rename — semantics DIFFER by server flavor (M0:
    /// Tailscale replaced an existing dst, OpenSSH refused). Callers that
    /// need no-replace must NOT rely on this; use `hardlink` tier instead.
    async fn rename(&self, from: &str, to: &str) -> Result<(), BrowserError>;

    /// posix-rename@openssh.com — REPLACES the destination atomically. Only
    /// used with explicit Replace consent, never for no-replace placement.
    #[allow(dead_code)] // consented-Replace seam; placement never uses it
    async fn posix_rename(&self, from: &str, to: &str) -> Result<(), BrowserError>;

    /// hardlink@openssh.com — atomic no-replace for a regular file on the
    /// same volume. Fails if `to` exists (the collision the engine verifies).
    async fn hardlink(&self, from: &str, to: &str) -> Result<(), BrowserError>;

    /// Create a symlink at `path` pointing to `target`.
    async fn symlink(&self, target: &str, path: &str) -> Result<(), BrowserError>;

    /// Read a symlink's target.
    async fn read_link(&self, path: &str) -> Result<String, BrowserError>;

    /// Open an existing file for reading.
    async fn open_read(&self, path: &str) -> Result<Box<dyn RemoteReader>, BrowserError>;

    /// Create a file exclusively (refuses if it exists) with `perm`, open for
    /// writing. The exclusive create is what makes staging collision-safe.
    async fn create_exclusive(
        &self,
        path: &str,
        perm: u32,
    ) -> Result<Box<dyn RemoteWriter>, BrowserError>;

    /// Apply source mode/mtime to a path (fidelity: dates and modes kept
    /// both ways, best effort — servers that ignore SETSTAT still work).
    async fn set_attrs(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime: Option<i64>,
    ) -> Result<(), BrowserError>;

    /// True when the server advertised hardlink@openssh.com — selects the
    /// extension-no-replace placement tier (M0-proven on the fleet).
    fn has_hardlink(&self) -> bool;

    /// True when the server advertised posix-rename@openssh.com.
    #[allow(dead_code)] // consented-Replace seam; placement never uses it
    fn has_posix_rename(&self) -> bool;

    /// M2b: what the exec-agent probe found for this transport's server
    /// (set once per host/account by the commands layer; see
    /// `agent::probe_agent_cap`). Defaults to `SftpOnly` — hosts without a
    /// probed capability keep the CURRENT SFTP placement tiers UNCHANGED
    /// (documented fallback, never a silent downgrade).
    fn agent_cap(&self) -> crate::browser::agent::AgentCap {
        crate::browser::agent::AgentCap::SftpOnly
    }

    /// A proven Windows helper for THIS transport's host/account. No default writes.
    fn win_agent(&self) -> Option<crate::browser::win_agent::WinAgent> {
        None
    }

    /// M2b: open an exec channel running a FIXED command string (the
    /// vendored agent bootstrap or the POSIX delete re-check). Paths are
    /// never part of the command — they travel as stdin data (plugin
    /// invariant §4.9). Default: typed Unsupported refusal, so only the
    /// real russh transport opts in.
    async fn open_exec(
        &self,
        cmd: &str,
    ) -> Result<Box<dyn crate::browser::agent::ExecChannel>, BrowserError> {
        let _ = cmd;
        Err(BrowserError::unsupported(
            "this transport can't run remote helper commands (no exec channel)",
        ))
    }

    /// The raw VERSION extension map, captured from the wire at connect
    /// (M0: advertisement does not prove operation semantics, but selects
    /// the placement tier to attempt + verify).
    fn extensions(&self) -> &std::collections::BTreeMap<String, String>;

    /// Close the session (flush + disconnect). Best-effort.
    async fn close(self: Box<Self>) -> Result<(), BrowserError>;
}

/// Map an SFTP status/error string to the typed error taxonomy. Port of the
/// plugin's `classify` (bin/tailfiles:221-236) and the spike's `error_kind`,
/// keyed on russh-sftp's StatusCode where available.
pub fn classify_message(msg: &str) -> crate::browser::ErrorKind {
    use crate::browser::ErrorKind as K;
    let m = msg.to_lowercase();
    if m.contains("hostkey") || m.contains("host key") || m.contains("changed key") {
        K::Hostkey
    } else if m.contains("auth")
        || m.contains("tailnet policy")
        || m.contains("permission denied (")
    {
        K::Auth
    } else if m.contains("no such file") || m.contains("nosuchfile") || m.contains("not found") {
        K::Notfound
    } else if m.contains("permission denied") || m.contains("permissiondenied") {
        K::Denied
    } else if m.contains("unsupported") || m.contains("opunsupported") {
        K::Unsupported
    } else if m.contains("unreachable")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("connection refused")
        || m.contains("connection lost")
        || m.contains("no connection")
        || m.contains("connection reset")
        || m.contains("connection closed")
        || m.contains("no route to host")
        || m.contains("network is unreachable")
    {
        K::Unreachable
    } else if m.contains("already exists") || m.contains("file exists") {
        K::Exists
    } else {
        K::Other
    }
}
