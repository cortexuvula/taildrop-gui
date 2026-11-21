//! Remote side over the SFTP transport trait: source walk (plan builder),
//! staged writes, and placement at the tier M0 proved.
//!
//! Placement tiers (M0 "Placement conclusion for v1"):
//!   - regular files, `hardlink@openssh.com` advertised: hardlink staged→
//!     final. The hardlink FAILS if the final name exists (verified in M0 on
//!     Tailscale SSH, macOS OpenSSH and Windows OpenSSH) — atomic no-replace
//!     for regular files on the same volume. Then unlink the staged name.
//!   - extension absent (or dirs/links, which hardlink never covers):
//!     check-then-rename. NOT race-free — only ever reached behind the
//!     conflict pre-check + explicit Replace consent, with the TOCTOU
//!     ceiling documented (report §6.4).
//!   - posix-rename REPLACES and vanilla rename semantics DIFFER by flavor
//!     (M0) — neither is used for no-replace placement.
//!   - fsync is never required (Tailscale SSH doesn't advertise it — M0).
//!
//! Staging: `.tailfiles-incoming-<16hex>` created with SFTP mkdir (fails if
//! the name exists — exclusive), then verified to be an empty directory
//! (attrs stat-verify-what's-available; SFTP attrs carry no uid, so the
//! owner check is the local-destination-only invariant §4.3 ceiling,
//! documented in report §6.4). Cleanup removes ONLY our own random-tag dir,
//! best effort, on every exit path.

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::browser::guards;
use crate::browser::listing::safe_remote_name;
use crate::browser::transfer::place::{remote_tier, PlaceAction, RemoteTier};
use crate::browser::transfer::plan::{CopyPlan, PlanBuilder, PlanItem, PlanKind, MAX_REMOTE_SIZE};
use crate::browser::transfer::{
    join_remote, stage_dir_name, CopySink, FileMeta, ProgressReporter, CHUNK,
};
use crate::browser::transport::SftpTransport;
use crate::browser::{BrowserError, ErrorKind, MAX_DEPTH};

// ---------------------------------------------------------------------------
// Source walk → CopyPlan
// ---------------------------------------------------------------------------

/// Walk the selected names on the remote and build the validated copy plan
/// (the pre-walk that yields the progress total — the plugin's `T` record).
/// Symlinks are recorded as links (never followed); specials and unsafe
/// names are skipped+counted; every size/name is cap-checked by the builder.
pub async fn build_plan(
    t: &dyn SftpTransport,
    src_dir: &str,
    names: &[String],
    windows: bool,
) -> Result<CopyPlan, BrowserError> {
    let mut b = PlanBuilder::new(names);
    for n in names {
        if let Some(why) = guards::control_char_problem(n) {
            return Err(BrowserError::new(ErrorKind::Other, format!("{n:?} {why}")));
        }
        walk_node(
            t,
            src_dir.to_string(),
            n.clone(),
            n.clone(),
            &mut b,
            windows,
            0,
        )
        .await?;
    }
    Ok(b.finish())
}

/// Boxed return: recursive async walk (E0733) + provably Send for the
/// spawned copy task. base/name/rel are owned so each recursion level can
/// pass freshly built child paths.
#[allow(clippy::too_many_arguments)]
fn walk_node<'a>(
    t: &'a dyn SftpTransport,
    base: String,
    name: String,
    rel: String,
    b: &'a mut PlanBuilder,
    windows: bool,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BrowserError>> + Send + 'a>> {
    Box::pin(async move {
        if depth > MAX_DEPTH {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!(
                    "{rel} has folders nested more than {MAX_DEPTH} deep, so it can't be copied"
                ),
            ));
        }
        let path = join_remote(&base, &name);
        let st = t
            .lstat(&path)
            .await?
            .ok_or_else(|| {
                BrowserError::new(
                    ErrorKind::Notfound,
                    format!("{rel} disappeared while it was being copied"),
                )
            })?
            .with_name(&name);
        if st.is_symlink {
            if windows {
                // Windows sources: directory symlinks (and junctions, which
                // SFTP can't even distinguish from dirs — M0 §5) are never
                // followed; links are skipped+counted like the C# agent does.
                b.push_skipped();
                return Ok(());
            }
            // Copied as a link, never followed. An unreadable target skips.
            match t.read_link(&path).await {
                Ok(target) => b.push(link_item(&rel, &target))?,
                Err(_) => b.push_skipped(),
            }
            return Ok(());
        }
        if st.is_dir {
            // B6-R (Windows sources only; semantics verified against
            // Win32-OpenSSH source in review R3 — raw SSH_FXP_STATUS carried
            // through BrowserError::sftp_status because ErrorKind merges
            // FAILURE and BAD_MESSAGE into `other`):
            //   generic FAILURE (4) → PLAIN directory (fileio_readlink's
            //     ERROR_NOT_A_REPARSE_POINT 4390 is unmapped by
            //     errno_from_Win32Error, so errno_to_portable defaults to
            //     SSH2_FX_FAILURE) → DESCEND.
            //   BAD_MESSAGE (5) → junction / non-symlink reparse point
            //     (IO_REPARSE_TAG_MOUNT_POINT → EINVAL; M0 room-6 evidence)
            //     → skip + count, NEVER descend (would copy outside the
            //     selected tree or loop on an ancestor junction).
            //   NAME (Ok) → real directory symlink → skip + count (link
            //     policy for Windows sources: never followed).
            //   NO_SUCH_FILE (2) → dir vanished since listing → skip +
            //     count (race tolerance).
            //   anything else → fail closed: skip + count.
            // POSIX sources NEVER probe: POSIX sftp-server answers
            // BAD_MESSAGE (EINVAL) for plain dirs — the probe would refuse
            // every folder — and POSIX lstat flags links reliably anyway.
            if windows {
                const SSH_FX_FAILURE: u32 = 4;
                match t.read_link(&path).await {
                    Err(e) if e.sftp_status == Some(SSH_FX_FAILURE) => {}
                    _ => {
                        b.push_skipped();
                        return Ok(());
                    }
                }
            }
            b.push(PlanItem {
                rel: rel.clone(),
                kind: PlanKind::Dir,
                size: 0,
                mode: st.perm & 0o7777,
                mtime: st.mtime,
            })?;
            // Single listing pass: partition into safe children vs skipped
            // (unsafe/non-UTF-8 names counted exactly once each — M0 rule).
            let raw = t.read_dir(&path).await?;
            let mut children: Vec<_> = Vec::with_capacity(raw.len());
            for c in raw {
                // B3: remote names — U+FFFD means the lossy decode guessed;
                // skipped and counted exactly once, never copied.
                if safe_remote_name(&c.name) {
                    children.push(c);
                } else {
                    b.push_skipped();
                }
            }
            children.sort_by(|a, b| a.name.cmp(&b.name));
            let child_base = path;
            for c in children {
                let child_rel = format!("{rel}/{}", c.name);
                walk_node(
                    t,
                    child_base.clone(),
                    c.name,
                    child_rel,
                    &mut *b,
                    windows,
                    depth + 1,
                )
                .await?;
            }
            return Ok(());
        }
        if st.is_regular || (!st.is_unknown && !st.is_dir && !st.is_symlink) {
            if st.size > MAX_REMOTE_SIZE {
                return Err(BrowserError::new(
                    ErrorKind::Other,
                    "The other machine sent a number that doesn't make sense, so the copy stopped",
                ));
            }
            b.push(PlanItem {
                rel: rel.clone(),
                kind: PlanKind::File,
                size: st.size,
                // FILES masked 0o777 (W1): setuid/setgid/sticky never travel.
                mode: st.perm & 0o777,
                mtime: st.mtime,
            })?;
            return Ok(());
        }
        // Sockets/devices/pipes/unknown: skipped and counted (the plugin's S).
        b.push_skipped();
        Ok(())
    })
}

fn link_item(rel: &str, target: &str) -> PlanItem {
    PlanItem {
        rel: rel.to_string(),
        kind: PlanKind::Link {
            target: target.to_string(),
        },
        size: 0,
        mode: 0,
        mtime: 0,
    }
}

/// Adapter so walk_node can treat a node as its own name context.
trait WithName {
    fn with_name(self, name: &str) -> Self;
}
impl WithName for crate::browser::transport::RemoteNode {
    fn with_name(mut self, name: &str) -> Self {
        if self.name.is_empty() {
            self.name = name.to_string();
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Streaming a plan into a sink
// ---------------------------------------------------------------------------

/// Stream the planned items into `sink` in DFS order, reading each file over
/// SFTP in chunks. Enforces the planned size exactly (a file that changed
/// mid-copy fails the copy — the SFTP ceiling for the plugin's dev/ino
/// re-check, documented in report §6.4). Returns the skipped count.
pub async fn stream_plan(
    t: &dyn SftpTransport,
    src_dir: &str,
    plan: &CopyPlan,
    sink: &mut dyn CopySink,
    prog: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<u32, BrowserError> {
    let mut open_dirs: Vec<String> = vec![String::new()]; // stack of dir rels
    for item in &plan.items {
        if cancel.is_cancelled() {
            return Err(BrowserError::new(ErrorKind::Other, "Cancelled"));
        }
        let parent = match item.rel.rsplit_once('/') {
            Some((p, _)) => p.to_string(),
            None => String::new(),
        };
        while open_dirs.last().map(String::as_str) != Some(parent.as_str()) {
            if open_dirs.len() <= 1 {
                return Err(BrowserError::new(
                    ErrorKind::Other,
                    format!("Unexpected item in the copy: {}", item.rel),
                ));
            }
            open_dirs.pop();
            sink.dir_end().await?;
        }
        let path = join_remote(src_dir, &item.rel);
        match &item.kind {
            PlanKind::Dir => {
                sink.dir_begin(&item.rel, item.mode, item.mtime).await?;
                open_dirs.push(item.rel.clone());
            }
            PlanKind::Link { target } => {
                if !sink.symlink(&item.rel, target).await? {
                    prog.add(0, "");
                }
            }
            PlanKind::File => {
                prog.add(0, &item.rel);
                // Re-stat right before open: detect a swapped/changed file
                // as well as SFTP allows without handles (§6.4 ceiling).
                let st = t.lstat(&path).await?.ok_or_else(|| {
                    BrowserError::new(
                        ErrorKind::Notfound,
                        format!("{} disappeared while it was being copied", item.rel),
                    )
                })?;
                if st.size != item.size {
                    return Err(BrowserError::new(
                        ErrorKind::Other,
                        format!("{} changed while it was being copied", item.rel),
                    ));
                }
                let mut reader = t.open_read(&path).await?;
                let r = sink_file(t, sink, &item.rel, item, &mut reader, prog, cancel).await;
                let close = reader.close().await;
                r?;
                close?;
                sink.file_end(item.mode, item.mtime).await?;
            }
        }
    }
    while open_dirs.len() > 1 {
        open_dirs.pop();
        sink.dir_end().await?;
    }
    Ok(plan.skipped)
}

#[allow(clippy::too_many_arguments)]
async fn sink_file(
    _t: &dyn SftpTransport,
    sink: &mut dyn CopySink,
    rel: &str,
    item: &PlanItem,
    reader: &mut Box<dyn crate::browser::transport::RemoteReader>,
    prog: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(), BrowserError> {
    sink.file_begin(
        rel,
        &FileMeta {
            size: item.size,
            mode: item.mode,
            mtime: item.mtime,
        },
    )
    .await?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < item.size {
        if cancel.is_cancelled() {
            return Err(BrowserError::new(ErrorKind::Other, "Cancelled"));
        }
        let want = std::cmp::min(buf.len() as u64, item.size - done) as usize;
        let n = reader.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("{rel} got shorter while it was being copied"),
            ));
        }
        sink.file_chunk(&buf[..n]).await?;
        prog.add(n as u64, rel);
        done += n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RemoteSink
// ---------------------------------------------------------------------------

/// SFTP staging sink: exclusive random-tag folder inside the destination,
/// staged writes, then tiered placement. Mirrors the plugin's AgentSink
/// semantics at the SFTP tier (ceilings documented at module head).
pub struct RemoteSink<'a> {
    t: &'a dyn SftpTransport,
    dst_dir: String,
    /// The random tag component, kept for cleanup diagnostics.
    #[allow(dead_code)]
    stage_name: String,
    stage: String,
    /// Stack of staged dir rel paths ("" = stage root), mirroring the sink
    /// discipline of LocalSink (out-of-order streams are a hard error).
    stack: Vec<(String, u32, i64)>,
    writer: Option<Box<dyn crate::browser::transport::RemoteWriter>>,
    writer_rel: String,
    removed: bool,
}

impl<'a> RemoteSink<'a> {
    pub async fn open(
        t: &'a dyn SftpTransport,
        dst_dir: &str,
        tag: &str,
    ) -> Result<Self, BrowserError> {
        let stage_name = stage_dir_name(tag);
        let stage = join_remote(dst_dir, &stage_name);
        // SFTP mkdir fails if the name exists — exclusivity. A planted
        // lookalike at the (64-bit random) name therefore blocks the copy
        // instead of receiving writes.
        t.mkdir(&stage, 0o700).await.map_err(|mut e| {
            if e.kind == ErrorKind::Exists {
                e.message = format!("Can't write there ({} already exists)", stage_name);
            }
            e
        })?;
        // Verify what the attrs allow: a directory, and empty. (SFTP attrs
        // carry no uid on most servers — owner verification is the local-only
        // invariant, §6.4 ceiling.)
        let st = t.lstat(&stage).await?;
        let ok_dir = st.as_ref().map(|s| s.is_dir).unwrap_or(false);
        if !ok_dir {
            let _ = t.remove_dir(&stage).await;
            return Err(BrowserError::new(
                ErrorKind::Other,
                "The copy's new folder changed before anything was written",
            ));
        }
        let entries = t.read_dir(&stage).await?;
        let non_empty = entries.iter().any(|e| e.name != "." && e.name != "..");
        if non_empty {
            return Err(BrowserError::new(
                ErrorKind::Other,
                "The copy's new folder was swapped for another one, so nothing was copied",
            ));
        }
        Ok(Self {
            t,
            dst_dir: dst_dir.to_string(),
            stage_name,
            stage,
            stack: vec![(String::new(), 0, 0)],
            writer: None,
            writer_rel: String::new(),
            removed: false,
        })
    }

    fn staged(&self, rel: &str) -> String {
        join_remote(&self.stage, rel)
    }

    async fn align(&mut self, rel: &str) -> Result<(), BrowserError> {
        let parent = match rel.rsplit_once('/') {
            Some((p, _)) => p.to_string(),
            None => String::new(),
        };
        while self.stack.len() > 1
            && self.stack.last().map(|e| e.0.as_str()) != Some(parent.as_str())
        {
            self.pop().await?;
        }
        if self.stack.last().map(|e| e.0.as_str()) != Some(parent.as_str()) {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("Unexpected item in the copy: {rel}"),
            ));
        }
        Ok(())
    }

    async fn pop(&mut self) -> Result<(), BrowserError> {
        if self.stack.len() <= 1 {
            return Ok(());
        }
        let (rel, mode, mtime) = self.stack.pop().expect("len > 1");
        // Apply source mode/date best effort (servers may ignore SETSTAT).
        let path = self.staged(&rel);
        if self
            .t
            .set_attrs(&path, Some(mode), Some(mtime))
            .await
            .is_err()
        {
            log::debug!("browser copy: setstat ignored for staged dir {rel}");
        }
        Ok(())
    }

    /// Place one staged top-level (or merged child) item whose destination
    /// does NOT exist yet, at the best available tier.
    async fn place_no_exists(
        &self,
        staged_rel: &str,
        dst_path: &str,
        staged_is_dir: bool,
        staged_is_file: bool,
        errors: &mut Vec<(String, String)>,
        conflicts: &mut Vec<String>,
    ) -> Result<(), BrowserError> {
        let src_path = self.staged(staged_rel);
        let tier = if staged_is_file && !staged_is_dir {
            remote_tier(self.t.has_hardlink())
        } else {
            // Dirs and links: hardlink never covers them (M0). Check-then-
            // rename behind the existence check, TOCTOU ceiling documented.
            RemoteTier::CheckThenRename
        };
        match tier {
            RemoteTier::HardlinkNoReplace => {
                match self.t.hardlink(&src_path, dst_path).await {
                    Ok(()) => {
                        // Unlink the staged name; the final name now holds the
                        // bytes (M0-proven flow, collision verified there).
                        if let Err(e) = self.t.remove_file(&src_path).await {
                            errors.push((staged_rel.to_string(), format!("staged unlink: {e}")));
                        }
                    }
                    // Advertised but actually unsupported (M0: advertisement
                    // does not prove operation semantics): DOCUMENTED downgrade
                    // to the check-then-rename tier. Consent semantics are
                    // unchanged — this path only runs where the caller already
                    // verified the destination absent. unsupported ≠ failed:
                    // a genuine hardlink failure still verifies-then-errors.
                    Err(e) if e.kind == ErrorKind::Unsupported => {
                        self.check_then_rename(&src_path, dst_path, staged_rel, errors, conflicts)
                            .await;
                    }
                    Err(_) => {
                        // VERIFY the collision like the spike does: if the
                        // destination now exists, it's a conflict; anything
                        // else is a placement error — never a silent rename
                        // fallback to a weaker tier mid-item.
                        if self.t.lstat(dst_path).await?.is_some() {
                            conflicts.push(staged_rel.to_string());
                        } else {
                            errors.push((
                                staged_rel.to_string(),
                                "hardlink placement refused by the server".to_string(),
                            ));
                        }
                    }
                }
            }
            RemoteTier::CheckThenRename => {
                self.check_then_rename(&src_path, dst_path, staged_rel, errors, conflicts)
                    .await;
            }
        }
        Ok(())
    }

    /// The check-then-rename placement tier: rename a staged item onto a
    /// destination the caller verified absent, with the documented TOCTOU
    /// ceiling (report §7.4 v1: servers differ on whether a vanilla rename
    /// replaces — Tailscale SSH replaced, macOS/Windows OpenSSH refused —
    /// so this tier NEVER assumes refusal; the pre-check + consent is the
    /// guard, and a mid-flight appearance of the destination is inside the
    /// documented ceiling).
    async fn check_then_rename(
        &self,
        src_path: &str,
        dst_path: &str,
        staged_rel: &str,
        errors: &mut Vec<(String, String)>,
        conflicts: &mut Vec<String>,
    ) {
        if let Err(e) = self.t.rename(src_path, dst_path).await {
            if self.t.lstat(dst_path).await.unwrap_or(None).is_some() {
                conflicts.push(staged_rel.to_string());
            } else {
                errors.push((staged_rel.to_string(), e.message));
            }
        }
    }

    /// Replace path (explicit consent): remove the destination item, then
    /// place. NOT atomic — the window and the crash-between-steps risk are
    /// the documented M1 ceiling; full parity arrives with the M2 agents.
    async fn place_replace(
        &self,
        staged_rel: &str,
        dst_path: &str,
        dst: &crate::browser::transport::RemoteNode,
        errors: &mut Vec<(String, String)>,
        conflicts: &mut Vec<String>,
    ) -> Result<(), BrowserError> {
        let staged = self
            .t
            .lstat(&self.staged(staged_rel))
            .await?
            .ok_or_else(|| BrowserError::other("staged item vanished"))?;
        let mine_dir = staged.is_dir;
        let theirs_dir = dst.is_dir;
        let action = match (mine_dir, theirs_dir) {
            (true, true) => PlaceAction::MergeDirs,
            (false, false) => PlaceAction::ReplaceAtomic,
            // A folder can only replace a folder, and a file a file or link.
            _ => PlaceAction::Conflict,
        };
        match action {
            PlaceAction::MergeDirs => {
                self.merge_dirs(
                    staged_rel.to_string(),
                    dst_path.to_string(),
                    conflicts,
                    errors,
                    0,
                )
                .await?;
            }
            PlaceAction::ReplaceAtomic => {
                // Remove the destination (file or link — never followed:
                // REMOVE on a link removes the link itself), then place.
                if let Err(e) = self.t.remove_file(dst_path).await {
                    errors.push((staged_rel.to_string(), format!("replace remove: {e}")));
                    return Ok(());
                }
                self.place_no_exists(staged_rel, dst_path, false, true, errors, conflicts)
                    .await?;
            }
            _ => conflicts.push(staged_rel.to_string()),
        }
        Ok(())
    }

    /// Recursive dir merge under replace consent (plugin's place_local merge
    /// branch, SFTP tier). Depth-capped. Boxed: recursive async fn (E0733).
    fn merge_dirs<'s>(
        &'s self,
        staged_rel: String,
        dst_path: String,
        conflicts: &'s mut Vec<String>,
        errors: &'s mut Vec<(String, String)>,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BrowserError>> + Send + 's>>
    {
        Box::pin(async move {
            if depth >= MAX_DEPTH {
                conflicts.push(staged_rel);
                return Ok(());
            }
            let src_path = self.staged(&staged_rel);
            let children = self.t.read_dir(&src_path).await?;
            for c in children {
                if !safe_remote_name(&c.name) {
                    continue;
                }
                let child_rel = format!("{staged_rel}/{}", c.name);
                let child_dst = join_remote(&dst_path, &c.name);
                match self.t.lstat(&child_dst).await? {
                    None => {
                        self.place_no_exists(
                            &child_rel,
                            &child_dst,
                            c.is_dir,
                            !c.is_dir && !c.is_symlink,
                            &mut *errors,
                            &mut *conflicts,
                        )
                        .await?;
                    }
                    Some(existing) => {
                        if existing.is_dir && c.is_dir {
                            self.merge_dirs(
                                child_rel,
                                child_dst,
                                &mut *conflicts,
                                &mut *errors,
                                depth + 1,
                            )
                            .await?;
                        } else if !existing.is_dir && !c.is_dir {
                            if let Err(e) = self.t.remove_file(&child_dst).await {
                                errors.push((child_rel, format!("replace remove: {e}")));
                                continue;
                            }
                            self.place_no_exists(
                                &child_rel,
                                &child_dst,
                                false,
                                true,
                                &mut *errors,
                                &mut *conflicts,
                            )
                            .await?;
                        } else {
                            conflicts.push(child_rel);
                        }
                    }
                }
            }
            // The merged staging dir itself is now empty — drop it.
            let _ = self.t.remove_dir(&src_path).await;
            Ok(())
        })
    }

    /// Recursively remove our own staging tree (cleanup on every exit path).
    /// ONLY ever operates inside the random-tag stage dir; never touches the
    /// destination by name beyond it. Best effort, depth-bounded.
    /// Boxed: recursive async fn (E0733).
    fn remove_tree<'s>(
        &'s self,
        path: String,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 's>> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                return;
            }
            let entries = match self.t.read_dir(&path).await {
                Ok(e) => e,
                Err(_) => return,
            };
            for e in entries {
                if !safe_remote_name(&e.name) {
                    continue;
                }
                let child = join_remote(&path, &e.name);
                if e.is_dir && !e.is_symlink {
                    self.remove_tree(child.clone(), depth + 1).await;
                    let _ = self.t.remove_dir(&child).await;
                } else {
                    // Files and links (links removed as links — REMOVE on a
                    // symlink never touches its target).
                    if self.t.remove_file(&child).await.is_err() {
                        // Windows-style link fallback: RMDIR removes link-only.
                        let _ = self.t.remove_dir(&child).await;
                    }
                }
            }
        })
    }
}

#[async_trait]
impl CopySink for RemoteSink<'_> {
    async fn dir_begin(&mut self, rel: &str, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        if self.stack.len() > MAX_DEPTH {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("{rel} has folders nested more than {MAX_DEPTH} deep"),
            ));
        }
        self.align(rel).await?;
        let path = self.staged(rel);
        self.t.mkdir(&path, 0o700).await?;
        self.stack.push((rel.to_string(), mode, mtime));
        Ok(())
    }

    async fn dir_end(&mut self) -> Result<(), BrowserError> {
        self.pop().await
    }

    async fn file_begin(&mut self, rel: &str, _meta: &FileMeta) -> Result<(), BrowserError> {
        // `_meta` is the exec-agent's up-front size channel; the SFTP sink
        // enforces the planned size in `stream_plan` and applies attrs at
        // file_end (best effort — servers may ignore SETSTAT).
        self.align(rel).await?;
        let path = self.staged(rel);
        // Exclusive create (CREATE|EXCLUDE): a case-variant or exact clash
        // inside staging fails here rather than clobbering.
        self.writer = Some(self.t.create_exclusive(&path, 0o600).await?);
        self.writer_rel = rel.to_string();
        Ok(())
    }

    async fn file_chunk(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| BrowserError::other("no staged file open"))?;
        w.write_all(data).await?;
        // ACK barrier: flush so reported progress = acknowledged bytes (M0).
        w.flush().await
    }

    async fn file_end(&mut self, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        let w = self
            .writer
            .take()
            .ok_or_else(|| BrowserError::other("no staged file open"))?;
        let rel = std::mem::take(&mut self.writer_rel);
        w.close().await?;
        // Source mode/date best effort (fidelity; servers may ignore).
        // FILES masked 0o777 (W1): setuid/setgid/sticky never propagate.
        let path = self.staged(&rel);
        if self
            .t
            .set_attrs(&path, Some(mode & 0o777), Some(mtime))
            .await
            .is_err()
        {
            log::debug!("browser copy: setstat ignored for staged file {rel}");
        }
        Ok(())
    }

    async fn symlink(&mut self, rel: &str, target: &str) -> Result<bool, BrowserError> {
        self.align(rel).await?;
        let path = self.staged(rel);
        match self.t.symlink(target, &path).await {
            Ok(()) => Ok(true),
            Err(e) => {
                // A destination that can't hold links skips+counts them
                // (visible in the done event), never fails the whole copy
                // and never silently drops.
                log::info!("browser copy: symlink {rel} skipped ({e})");
                Ok(false)
            }
        }
    }

    async fn place(
        &mut self,
        top_names: &[String],
        replace: bool,
    ) -> Result<(Vec<String>, Vec<(String, String)>), BrowserError> {
        // Close any open staged dir levels first (applies their attrs).
        while self.stack.len() > 1 {
            self.pop().await?;
        }
        let mut conflicts = Vec::new();
        let mut errors = Vec::new();
        for name in top_names {
            let dst_path = join_remote(&self.dst_dir, name);
            let staged = match self.t.lstat(&self.staged(name)).await? {
                Some(st) => st,
                None => continue, // nothing staged for this name
            };
            match self.t.lstat(&dst_path).await? {
                None => {
                    // Existence already checked; place at the best tier. The
                    // tier's own failure path re-checks and reports conflict.
                    let is_dir = staged.is_dir;
                    let is_file = !staged.is_dir && !staged.is_symlink;
                    self.place_no_exists(
                        name,
                        &dst_path,
                        is_dir,
                        is_file,
                        &mut errors,
                        &mut conflicts,
                    )
                    .await?;
                }
                Some(existing) => {
                    if !replace {
                        conflicts.push(name.clone());
                    } else {
                        self.place_replace(name, &dst_path, &existing, &mut errors, &mut conflicts)
                            .await?;
                    }
                }
            }
        }
        Ok((conflicts, errors))
    }

    async fn remove(&mut self) -> Result<(), BrowserError> {
        if self.removed {
            return Ok(());
        }
        self.removed = true;
        // Close anything still open (writer flushes are per-chunk already).
        if let Some(w) = self.writer.take() {
            let _ = w.close().await;
        }
        self.remove_tree(self.stage.clone(), 0).await;
        let _ = self.t.remove_dir(&self.stage).await;
        Ok(())
    }
}

/// Extension map helper for tests/probe: what a server advertised.
/// Human-readable extension summary (probe diagnostics; surfaced in M2 UI).
#[allow(dead_code)]
pub fn extensions_summary(exts: &BTreeMap<String, String>) -> String {
    exts.keys()
        .map(|k| format!("{k}={}", exts.get(k).unwrap_or(&String::new())))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    //! Platform-neutral remote-side coverage: source-walk probe semantics
    //! (B6-R), RemoteSink staging/placement, and a full stream_plan
    //! roundtrip. These drive ONLY the fake SFTP server — every filesystem
    //! effect lands inside the fixture's temp root — so they also run on
    //! Windows, where the Unix-only local engine (local_side.rs is
    //! `#[cfg(unix)]`, callers fail closed) makes the run_copy-based fake.rs
    //! tests inapplicable.

    use super::*;
    use crate::browser::fake::{temp_root, write_temp_key, FakeConfig, FakeServer};
    use crate::browser::STAGE_PREFIX;
    use std::sync::Arc;

    fn noop_emit() -> Arc<dyn Fn(u8, String, String) + Send + Sync> {
        Arc::new(|_pct, _rate, _file| ())
    }

    /// Staging dirs (anywhere one level under `dir`) that survived — the
    /// cleanup invariant says this must be empty after `sink.remove()`.
    fn stage_leftovers(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with(STAGE_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// B6-R probe semantics at the PLAN level (all platforms): plain Windows
    /// dirs answering generic SSH_FX_FAILURE descend recursively; junctions
    /// (BAD_MESSAGE) and vanished dirs (NO_SUCH_FILE) are skipped + counted,
    /// never descended — without needing a local sink.
    #[tokio::test]
    async fn windows_plan_probe_descends_plain_dirs_skips_reparse_and_vanished() {
        let mut cfg = FakeConfig::openssh(temp_root("planprobe-srv"));
        cfg.banner = "SSH-2.0-OpenSSH_for_Windows_8.1".to_string();
        cfg.simulate_junctions = true;
        cfg.vanish_readlink_for = Some("gone-dir".to_string());
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("real/nested")).unwrap();
        std::fs::write(srv.root.join("real/inside.txt"), b"inside").unwrap();
        std::fs::write(srv.root.join("real/nested/deep.txt"), b"deep").unwrap();
        std::fs::create_dir(srv.root.join("junction-escape")).unwrap();
        std::fs::write(srv.root.join("junction-escape/outside.txt"), b"outside").unwrap();
        std::fs::create_dir(srv.root.join("gone-dir")).unwrap();

        // Explicit fixture key: OpenSSH-flavor auth never falls back to
        // none-auth, so keyless connect() would depend on ambient developer
        // credentials that do not exist on CI runners.
        let (key, _g) = write_temp_key("planprobe");
        let t = srv
            .connect_as(&temp_root("trustPLANPROBE"), "fakeuser", &[key])
            .await;
        let plan = build_plan(
            &t,
            "/",
            &[
                "real".to_string(),
                "junction-escape".to_string(),
                "gone-dir".to_string(),
            ],
            true,
        )
        .await
        .expect("plan builds");
        let rels: Vec<&str> = plan.items.iter().map(|i| i.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec![
                "real",
                "real/inside.txt",
                "real/nested",
                "real/nested/deep.txt"
            ],
            "plain dirs answering FAILURE descend; junction/vanished never do"
        );
        assert_eq!(
            plan.skipped, 2,
            "junction + vanished dir skipped and counted, never descended"
        );
        assert!(
            !rels.iter().any(|r| r.contains("outside")),
            "junction contents never enter the plan"
        );
        assert_eq!(plan.total, 10);
        Box::new(t).close().await.unwrap();
    }

    /// B6-R: POSIX sources NEVER readlink-probe directories (a real POSIX
    /// sftp-server answers BAD_MESSAGE for plain dirs — probing would refuse
    /// every folder). Plan-level twin of the fake.rs pull test, running on
    /// all platforms.
    #[tokio::test]
    async fn posix_plan_never_probes_dir_readlink() {
        let cfg = FakeConfig::tailscale(temp_root("planposix-srv"));
        let probes = cfg.dir_readlinks.clone();
        let srv = FakeServer::start(cfg).await;
        std::fs::create_dir_all(srv.root.join("tree/sub")).unwrap();
        std::fs::write(srv.root.join("tree/a.txt"), b"aaa").unwrap();
        std::fs::write(srv.root.join("tree/sub/b.txt"), b"bbb").unwrap();
        let t = srv.connect(&temp_root("trustPLANPOSIX")).await;
        let plan = build_plan(&t, "/", &["tree".to_string()], false)
            .await
            .expect("plan builds");
        let rels: Vec<&str> = plan.items.iter().map(|i| i.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec!["tree", "tree/a.txt", "tree/sub", "tree/sub/b.txt"]
        );
        assert_eq!(plan.skipped, 0);
        assert_eq!(plan.total, 6);
        assert_eq!(
            probes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "POSIX sources must never readlink-probe directories"
        );
        Box::new(t).close().await.unwrap();
    }

    /// RemoteSink staging + tier placement driven directly (no local side):
    /// exclusive stage creation, planted-lookalike refusal, staged writes
    /// with attrs, hardlink-tier file placement, rename-tier dir placement,
    /// and cleanup — on all platforms.
    #[tokio::test]
    async fn remote_sink_stages_exclusively_places_and_cleans() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("sink-srv"))).await;
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let t = srv.connect(&temp_root("trustSINK")).await;

        // A planted lookalike at the exact stage name blocks the sink
        // (SFTP mkdir is exclusive) and the collision is named.
        let tag = crate::browser::transfer::new_stage_tag();
        let planted = join_remote("/dst", &stage_dir_name(&tag));
        t.mkdir(&planted, 0o700).await.unwrap();
        let e = match RemoteSink::open(&t, "/dst", &tag).await {
            Ok(_) => panic!("planted stage name must refuse"),
            Err(e) => e,
        };
        assert_eq!(e.kind, ErrorKind::Exists);
        assert!(e.message.contains("already exists"), "{e:?}");
        t.remove_dir(&planted).await.unwrap();

        let mut sink = RemoteSink::open(&t, "/dst", &tag)
            .await
            .expect("stage opens");
        sink.file_begin(
            "a.txt",
            &FileMeta {
                size: 7,
                mode: 0o644,
                mtime: 1_700_000_000,
            },
        )
        .await
        .unwrap();
        sink.file_chunk(b"payload").await.unwrap();
        sink.file_end(0o644, 1_700_000_000).await.unwrap();
        sink.dir_begin("d", 0o755, 1_700_000_001).await.unwrap();
        sink.file_begin(
            "d/b.txt",
            &FileMeta {
                size: 5,
                mode: 0o644,
                mtime: 1_700_000_002,
            },
        )
        .await
        .unwrap();
        sink.file_chunk(b"inner").await.unwrap();
        sink.file_end(0o644, 1_700_000_002).await.unwrap();
        sink.dir_end().await.unwrap();
        let (conflicts, errors) = sink
            .place(&["a.txt".to_string(), "d".to_string()], false)
            .await
            .unwrap();
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert!(errors.is_empty(), "{errors:?}");
        sink.remove().await.unwrap();

        // Placed through the hardlink tier (file) and rename tier (dir):
        // content exact, and the staged mtime survives the hardlink.
        assert_eq!(
            std::fs::read(srv.root.join("dst/a.txt")).unwrap(),
            b"payload"
        );
        assert_eq!(
            std::fs::read(srv.root.join("dst/d/b.txt")).unwrap(),
            b"inner"
        );
        let mt = std::fs::metadata(srv.root.join("dst/a.txt"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mt, 1_700_000_000, "SETSTAT mtime applied through placement");
        assert!(
            stage_leftovers(&srv.root.join("dst")).is_empty(),
            "cleanup removes the whole stage tree"
        );
        Box::new(t).close().await.unwrap();
    }

    /// Full engine roundtrip without a local side: walk a remote tree into
    /// a plan, STREAM it over the wire into a RemoteSink (chunking +
    /// exact-size enforcement in `stream_plan`), place, and verify byte
    /// fidelity across chunk boundaries — on all platforms.
    #[tokio::test]
    async fn engine_streams_plan_into_remote_sink_roundtrip() {
        let srv = FakeServer::start(FakeConfig::tailscale(temp_root("rt-srv"))).await;
        std::fs::create_dir_all(srv.root.join("src/docs")).unwrap();
        std::fs::write(srv.root.join("src/docs/readme.md"), b"# hi").unwrap();
        std::fs::write(srv.root.join("src/big.bin"), vec![7u8; CHUNK * 2 + 123]).unwrap();
        std::fs::create_dir_all(srv.root.join("dst")).unwrap();
        let t = srv.connect(&temp_root("trustRT")).await;

        let plan = build_plan(
            &t,
            "/src",
            &["docs".to_string(), "big.bin".to_string()],
            false,
        )
        .await
        .expect("plan builds");
        assert_eq!(plan.skipped, 0);
        assert_eq!(plan.total, 4 + (CHUNK * 2 + 123) as u64);

        let tag = crate::browser::transfer::new_stage_tag();
        let mut sink = RemoteSink::open(&t, "/dst", &tag)
            .await
            .expect("stage opens");
        let mut prog = ProgressReporter::new(plan.total, noop_emit());
        let skipped = stream_plan(
            &t,
            "/src",
            &plan,
            &mut sink,
            &mut prog,
            &CancellationToken::new(),
        )
        .await
        .expect("stream succeeds");
        assert_eq!(skipped, 0);
        assert_eq!(
            prog.done_bytes(),
            plan.total,
            "progress counted every acknowledged byte"
        );
        let (conflicts, errors) = sink
            .place(&["docs".to_string(), "big.bin".to_string()], false)
            .await
            .unwrap();
        assert!(
            conflicts.is_empty() && errors.is_empty(),
            "{conflicts:?} {errors:?}"
        );
        sink.remove().await.unwrap();

        assert_eq!(
            std::fs::read(srv.root.join("dst/docs/readme.md")).unwrap(),
            b"# hi"
        );
        assert_eq!(
            std::fs::read(srv.root.join("dst/big.bin")).unwrap(),
            vec![7u8; CHUNK * 2 + 123],
            "multi-chunk file streams with byte fidelity"
        );
        assert!(stage_leftovers(&srv.root.join("dst")).is_empty());
        Box::new(t).close().await.unwrap();
    }
}
