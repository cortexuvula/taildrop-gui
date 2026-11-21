//! The copy engine (report §7.1 `transfer`, §7.4 v1 column).
//!
//! Shape: the plugin's record-stream architecture, expressed as a walker
//! driving a `CopySink` — the sink consumes items one at a time in
//! depth-first order exactly like the plugin's `sink.write(rec, prog)`
//! (bin/tailfiles:2420-2428), so ordering/stack rules port 1:1.
//!
//!   source walker ──(dir/file/link/skip, DFS)──► CopySink
//!     local:  held dir-fds, O_NOFOLLOW, dev/ino re-check, exact-size reads
//!     remote: SFTP lstat-then-open, exact-size enforcement, size caps
//!   CopySink ── exclusive random-tag staging ──► placement ──► cleanup
//!     local:  LocalSink  (full handle-bound invariant set)
//!     remote: RemoteSink (SFTP tier — ceilings documented below)
//!
//! Progress counts REAL acknowledged bytes, throttled to ~4 Hz (the plugin's
//! 0.25 s cadence). Cancellation is cooperative at chunk boundaries with
//! awaited cleanup on every exit path (M0-proven model).
//!
//! M1 ceilings (documented, from M0 + report §6.4):
//!   - Remote staging: random 64-bit tag, exclusive mkdir, stat-verify what
//!     attrs allow, cleanup removes ONLY our own tag dir. Full handle-bound
//!     remote invariants arrive with the M2 exec agents.
//!   - Remote placement: hardlink@openssh.com no-replace tier where
//!     advertised (collision fails the item into `conflicts`), else
//!     check-then-rename with mandatory conflict consent — a documented
//!     TOCTOU window. Never requires fsync (Tailscale SSH lacks it — M0).
//!   - Local destination: FULL invariant set (dir-fds, O_NOFOLLOW, dev/ino,
//!     renameat2 RENAME_NOREPLACE / renameatx_np RENAME_EXCL, fail closed on
//!     non-atomic filesystems).

pub mod place;
pub mod plan;

#[cfg(unix)]
pub mod local_side;
pub mod remote_side;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::browser::guards;
use crate::browser::transport::SftpTransport;
use crate::browser::{human_rate, BrowserError, CopyRequest, ErrorKind};

/// Chunk size for streaming reads/writes (M0-proven 256 KiB).
pub const CHUNK: usize = 256 * 1024;

/// One side of a copy, resolved by the commands layer.
pub enum Side<'a> {
    Local,
    Remote {
        transport: &'a dyn SftpTransport,
        /// Windows peers are browse+pull only in M1: refused as a copy
        /// destination by `preflight`; as a source, symlinks are
        /// skip+counted and junctions are undetectable over SFTP (M0 §5) —
        /// the depth cap bounds any junction loop.
        windows: bool,
    },
}

/// Destination sink interface — the port of the plugin's AgentSink/LocalSink
/// shared surface. Walkers drive it in DFS order; the sink enforces the
/// stack discipline (a child arriving after its parent was closed is a
/// protocol error, not a silent mkdir -p).
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait + Result output (known interaction)
pub trait CopySink: Send {
    /// Create a staging subdirectory (0700; final mode applied at dir_end).
    async fn dir_begin(&mut self, rel: &str, mode: u32, mtime: i64) -> Result<(), BrowserError>;
    /// Close the innermost directory, applying source mode/mtime.
    async fn dir_end(&mut self) -> Result<(), BrowserError>;
    /// Exclusively create a file (0600 while streaming).
    async fn file_begin(&mut self, rel: &str) -> Result<(), BrowserError>;
    async fn file_chunk(&mut self, data: &[u8]) -> Result<(), BrowserError>;
    /// Close the file, applying source mode/mtime.
    async fn file_end(&mut self, mode: u32, mtime: i64) -> Result<(), BrowserError>;
    /// Create a symlink inside staging. `Ok(false)` = destination can't hold
    /// links (skipped+counted, never a silent drop, never a hard failure).
    async fn symlink(&mut self, rel: &str, target: &str) -> Result<bool, BrowserError>;
    /// Move staged items into place. Returns (conflicts, errors) exactly as
    /// the plugin's place step: conflicts = existed-without-consent or
    /// kind-mismatch; errors = placement failures (non-atomic FS etc).
    async fn place(
        &mut self,
        top_names: &[String],
        replace: bool,
    ) -> Result<(Vec<String>, Vec<(String, String)>), BrowserError>;
    /// Remove the staging area through the sink's OWN references only
    /// (local: held fds + dev/ino-matched unlink; remote: our own random-tag
    /// dir). Runs on every exit path. Best effort — errors logged.
    async fn remove(&mut self) -> Result<(), BrowserError>;
}

/// Outcome of a copy attempt (before failure aggregation).
#[derive(Debug)]
pub struct CopyOutcome {
    pub skipped: u32,
    /// Real bytes streamed (outcome surface for job snapshots).
    #[allow(dead_code)]
    pub bytes: u64,
}

/// Throttled progress (4 Hz), mirroring the plugin's Progress class:
/// pct capped at 99 until done, average rate, current file name.
pub struct ProgressReporter {
    total: u64,
    done: u64,
    start: Instant,
    last_emit: Instant,
    file: String,
    emit: Arc<dyn Fn(u8, String, String) + Send + Sync>,
}

impl ProgressReporter {
    pub fn new(total: u64, emit: Arc<dyn Fn(u8, String, String) + Send + Sync>) -> Self {
        let now = Instant::now();
        Self {
            total,
            done: 0,
            start: now,
            last_emit: now,
            file: String::new(),
            emit,
        }
    }

    pub fn add(&mut self, n: u64, file: &str) {
        self.done += n;
        if !file.is_empty() {
            self.file = file.to_string();
        }
        if self.last_emit.elapsed().as_secs_f64() > 0.25 {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        self.last_emit = Instant::now();
        let elapsed = self
            .last_emit
            .duration_since(self.start)
            .as_secs_f64()
            .max(0.001);
        let pct = if self.total > 0 {
            (100u64
                .saturating_mul(self.done)
                .checked_div(self.total)
                .unwrap_or(0))
            .min(99) as u8
        } else {
            0
        };
        let rate = human_rate(self.done as f64 / elapsed);
        (self.emit)(pct, self.file.clone(), rate);
    }

    pub fn done_bytes(&self) -> u64 {
        self.done
    }
}

/// Pre-flight validation shared by every copy: guards, account pinning,
/// Windows-write refusal, control chars, unique single-parent selection,
/// copy-into-itself.
pub fn preflight(req: &CopyRequest, dst_windows: bool) -> Result<(), BrowserError> {
    if req.names.is_empty() {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "Nothing selected to copy",
        ));
    }
    // Same-host two-account refusal (bin/tailfiles:2824-2829): one connection
    // can't be two accounts; a saved-user change mid-run must not split them.
    if req.src_target == req.dst_target
        && req.src_target != crate::browser::LOCAL_TARGET
        && req.src_user != req.dst_user
    {
        return Err(BrowserError::new(
            ErrorKind::Account,
            format!(
                "{} is shown as two different accounts ({} and {}) — a copy needs one account \
                 for both sides",
                req.src_target,
                req.src_user.clone().unwrap_or_default(),
                req.dst_user.clone().unwrap_or_default()
            ),
        ));
    }
    // M2a: remote→remote streams THROUGH the app (two independent sessions,
    // no local disk) — the M1 B5 refusal is lifted. Same-host two-account
    // stays refused above (M1 invariant), Windows destinations below (M3
    // gate), and the copy-into-itself rules apply per target.
    // Remote sides need a pinned account before copying (pane loadedUser).
    for (target, user) in [
        (&req.src_target, &req.src_user),
        (&req.dst_target, &req.dst_user),
    ] {
        if target != crate::browser::LOCAL_TARGET && user.as_deref().unwrap_or("").is_empty() {
            return Err(BrowserError::new(
                ErrorKind::Account,
                format!("Can't tell which account to use on {target}"),
            ));
        }
    }
    // M1 gate: Windows peers are browse + pull only.
    if dst_windows {
        return Err(BrowserError::unsupported(
            "Writing to Windows peers is disabled until the M3 safety gate passes — \
             browse and pull only for now",
        ));
    }
    if let Some(reason) = guards::copy_precondition_problem(
        &req.src_target,
        &req.dst_target,
        &req.src_dir,
        &req.dst_dir,
        &req.names,
        false,
    ) {
        return Err(BrowserError::new(ErrorKind::Other, reason));
    }
    Ok(())
}

/// Destination existence pre-check — "ask the filesystem itself" so
/// case-insensitive volumes count case-variant clashes (bin/tailfiles:
/// 2391-2398). Only the early answer; placement enforces it atomically.
/// Skipped entirely when replace consent was given.
pub async fn existing_names(
    dst: &Side<'_>,
    dst_dir: &str,
    names: &[String],
) -> Result<Vec<String>, BrowserError> {
    let mut clash = Vec::new();
    for n in names {
        let exists = match dst {
            Side::Local => {
                #[cfg(unix)]
                {
                    std::fs::symlink_metadata(std::path::Path::new(dst_dir).join(n)).is_ok()
                }
                #[cfg(not(unix))]
                {
                    let _ = (dst_dir, n);
                    return Err(local_unsupported());
                }
            }
            Side::Remote { transport, .. } => {
                transport.lstat(&join_remote(dst_dir, n)).await?.is_some()
            }
        };
        if exists {
            clash.push(n.clone());
        }
    }
    Ok(clash)
}

/// POSIX-style remote path join (SFTP always uses `/` on the wire).
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", dir.trim_end_matches('/'), name)
    }
}

#[cfg(not(unix))]
pub fn local_unsupported() -> BrowserError {
    BrowserError::unsupported(
        "Local filesystem operations are not available on this platform in M1 \
         (the handle-bound local sink is Unix-only; fail closed, never weaker)",
    )
}

/// 16 hex chars from a CSPRNG — 64 bits of staging-tag randomness
/// (invariant §4.3; the plugin uses os.urandom(8)).
pub fn new_stage_tag() -> String {
    use rand::Rng as _;
    let n: u64 = rand::rng().random();
    format!("{n:016x}")
}

pub fn stage_dir_name(tag: &str) -> String {
    format!("{}{tag}", crate::browser::STAGE_PREFIX)
}

/// The full copy pipeline. Cleanup of the destination staging runs on EVERY
/// exit path (done/failed/cancelled) before this returns — cooperative
/// cancel with awaited cleanup, the M0-proven ordering. Failure aggregation
/// matches run_copy (bin/tailfiles:2430-2441).
pub async fn run_copy(
    req: &CopyRequest,
    src: &Side<'_>,
    dst: &Side<'_>,
    cancel: CancellationToken,
    emit: Arc<dyn Fn(u8, String, String) + Send + Sync>,
) -> Result<CopyOutcome, BrowserError> {
    // ---- 1. Pre-walk for the progress total (plugin: local_size / T record)
    let (total, remote_plan) = match src {
        Side::Local => {
            #[cfg(unix)]
            {
                (local_side::total_size(&req.src_dir, &req.names)?, None)
            }
            #[cfg(not(unix))]
            return Err(local_unsupported());
        }
        Side::Remote { transport, windows } => {
            let plan =
                remote_side::build_plan(*transport, &req.src_dir, &req.names, *windows).await?;
            (plan.total, Some(plan))
        }
    };
    if cancel.is_cancelled() {
        return Err(cancelled_err());
    }

    // ---- 2. Destination staging: exclusive, identity-checked ----
    let stage_tag = new_stage_tag();
    let mut sink: Box<dyn CopySink> = match dst {
        Side::Local => {
            #[cfg(unix)]
            {
                Box::new(local_side::LocalSink::open(&req.dst_dir, &stage_tag)?)
            }
            #[cfg(not(unix))]
            return Err(local_unsupported());
        }
        Side::Remote { transport, .. } => {
            Box::new(remote_side::RemoteSink::open(*transport, &req.dst_dir, &stage_tag).await?)
        }
    };

    // From here, EVERY exit path removes the stage before returning.
    let result = stream_place_aggregate(
        req,
        src,
        total,
        remote_plan.as_ref(),
        sink.as_mut(),
        &cancel,
        emit,
    )
    .await;
    if let Err(e) = sink.remove().await {
        log::warn!("browser copy: staging cleanup reported: {e}");
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn stream_place_aggregate(
    req: &CopyRequest,
    src: &Side<'_>,
    total: u64,
    remote_plan: Option<&plan::CopyPlan>,
    sink: &mut dyn CopySink,
    cancel: &CancellationToken,
    emit: Arc<dyn Fn(u8, String, String) + Send + Sync>,
) -> Result<CopyOutcome, BrowserError> {
    let mut prog = ProgressReporter::new(total, emit);

    // ---- 3. Stream in DFS order (walker drives the sink) ----
    let skipped = match src {
        Side::Local => {
            #[cfg(unix)]
            {
                local_side::stream_names(&req.src_dir, &req.names, sink, &mut prog, cancel).await?
            }
            #[cfg(not(unix))]
            return Err(local_unsupported());
        }
        Side::Remote { transport, .. } => {
            let plan = remote_plan.expect("remote plan built in run_copy");
            remote_side::stream_plan(*transport, &req.src_dir, plan, sink, &mut prog, cancel)
                .await?
        }
    };
    if cancel.is_cancelled() {
        return Err(cancelled_err());
    }

    // ---- 4. Placement ----
    let (conflicts, errors) = sink.place(&req.names, req.replace).await?;
    prog.flush();

    // ---- 5. Failure aggregation (run_copy, bin/tailfiles:2430-2441) ----
    if !errors.is_empty() {
        let shown: Vec<&str> = errors.iter().take(3).map(|(n, _)| n.as_str()).collect();
        return Err(BrowserError::new(
            ErrorKind::Other,
            format!(
                "Couldn't move {} into {} without a chance of replacing something (that disk \
                 can't rename atomically), so {} left out",
                shown.join(", "),
                req.dst_dir,
                if errors.len() == 1 {
                    "it was"
                } else {
                    "they were"
                }
            ),
        ));
    }
    if !conflicts.is_empty() {
        if req.replace {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!(
                    "Couldn't replace {}: a folder can only replace a folder, and a file a file \
                     or link",
                    conflicts[..conflicts.len().min(3)].join(", ")
                ),
            ));
        }
        let partial = conflicts.len() < req.names.len();
        return Err(BrowserError::exists(conflicts, &req.dst_dir, partial));
    }
    Ok(CopyOutcome {
        skipped: skipped + prog_skipped_adjust(),
        bytes: prog.done_bytes(),
    })
}

/// Skipped items are counted by the walkers; nothing extra to adjust here.
fn prog_skipped_adjust() -> u32 {
    0
}

fn cancelled_err() -> BrowserError {
    BrowserError::new(ErrorKind::Other, "Cancelled")
}

/// Convenience used by commands.rs: is this error the cancellation sentinel?
pub fn is_cancelled(e: &BrowserError) -> bool {
    e.message == "Cancelled"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_tag_is_16_hex_and_unique() {
        let a = new_stage_tag();
        let b = new_stage_tag();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(stage_dir_name(&a).starts_with(".tailfiles-incoming-"));
    }

    #[test]
    fn join_remote_rules() {
        assert_eq!(join_remote("/", "a"), "/a");
        assert_eq!(join_remote("", "a"), "/a");
        assert_eq!(join_remote("/x/y", "a"), "/x/y/a");
        assert_eq!(join_remote("/x/y/", "a"), "/x/y/a");
        assert_eq!(join_remote("/C:/Users/corte", "a"), "/C:/Users/corte/a");
    }

    #[test]
    fn preflight_refusals() {
        let base = CopyRequest {
            src_target: "local".into(),
            src_user: None,
            src_dir: "/a".into(),
            dst_target: "host".into(),
            dst_user: Some("u".into()),
            dst_dir: "/b".into(),
            names: vec!["x".into()],
            replace: false,
        };
        assert!(preflight(&base, false).is_ok());
        // Windows destination refused (M1 gate).
        let e = preflight(&base, true).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        // Missing account pin on a remote side.
        let mut r = base.clone();
        r.dst_user = None;
        assert_eq!(preflight(&r, false).unwrap_err().kind, ErrorKind::Account);
        // Same host, two accounts (refused even though M2a lifted the
        // remote→remote scope refusal — one connection can't be two accounts).
        let mut r = base.clone();
        r.src_target = "host".into();
        r.src_user = Some("u1".into());
        r.dst_user = Some("u2".into());
        assert_eq!(preflight(&r, false).unwrap_err().kind, ErrorKind::Account);
        // M2a: remote→remote between DIFFERENT hosts is now allowed at
        // preflight (the M1 B5 refusal is lifted; the copy streams through
        // the app on two independent sessions).
        let mut r = base.clone();
        r.src_target = "other-host".into();
        r.src_user = Some("u".into());
        assert!(
            preflight(&r, false).is_ok(),
            "remote→remote must pass preflight in M2a"
        );
        // A Windows DESTINATION is still refused for remote→remote too
        // (M3 gate — the refusal is destination-flavor based, direction
        // independent).
        assert_eq!(
            preflight(&r, true).unwrap_err().kind,
            ErrorKind::Unsupported
        );
        // Same host, SAME account, different folders: allowed (the plugin's
        // same-host rule; one connection serves both sides).
        let mut r = base.clone();
        r.src_target = "host".into();
        r.src_user = Some("u".into());
        r.dst_target = "host".into();
        r.dst_user = Some("u".into());
        r.src_dir = "/a".into();
        r.dst_dir = "/b".into();
        assert!(preflight(&r, false).is_ok());
        // Same host, same account, into itself.
        let mut r = base.clone();
        r.src_target = "host".into();
        r.src_user = Some("u".into());
        r.src_dir = "/a".into();
        r.dst_dir = "/a/x".into();
        assert!(preflight(&r, false).is_err());
        // Empty selection.
        let mut r = base.clone();
        r.names = vec![];
        assert!(preflight(&r, false).is_err());
    }

    #[tokio::test]
    async fn progress_throttles_and_caps_at_99() {
        use std::sync::Mutex;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let emit = Arc::new(move |pct: u8, _f: String, _r: String| {
            seen2.lock().unwrap().push(pct);
        });
        let mut p = ProgressReporter::new(1000, emit);
        // Force emissions by rewinding last_emit.
        for _ in 0..5 {
            p.last_emit = Instant::now() - std::time::Duration::from_secs(1);
            p.add(199, "f");
        }
        let v = seen.lock().unwrap().clone();
        assert!(!v.is_empty());
        assert!(v.iter().all(|x| *x <= 99), "pct must cap at 99: {v:?}");
        assert_eq!(*v.last().unwrap(), 99);
    }
}
