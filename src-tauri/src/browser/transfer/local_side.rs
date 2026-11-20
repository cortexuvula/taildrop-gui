//! Unix local side: the FULL handle-bound invariant set (report §7.4 rows
//! 1-5, "Local full" — no SFTP ceiling here). Faithful Rust port of the
//! plugin's bound-handle local engine (bin/tailfiles:735-1013):
//!
//!   - destination dir held by fd from open to placement (open_top)
//!   - staging dir created exclusively (mkdirat 0700), opened O_NOFOLLOW,
//!     identity-checked (euid owner, mode 0700, empty), and held by fd from
//!     creation until removal — never looked up by name again while in use
//!   - files created O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW 0600, chmod/utime
//!     through the held fd on close
//!   - dirs opened via open_dir_at: lstat-isdir, open O_NOFOLLOW, then
//!     dev/ino re-check against the pre-open lstat (swap detection)
//!   - source files opened O_RDONLY|O_NOFOLLOW|O_NONBLOCK with the same
//!     dev/ino re-check; exactly st_size bytes are read (a file that shrinks
//!     mid-copy fails the copy)
//!   - placement: renameat2(RENAME_NOREPLACE) on Linux, renameatx_np with
//!     RENAME_EXCL on macOS; EINVAL/ENOSYS/EOPNOTSUPP fail CLOSED (the item
//!     is not moved); EEXIST drives the consent/merge/replace matrix from
//!     `place`
//!   - removal: empty the stage through the held fd (depth-bounded), then
//!     unlink the NAME only while lstat(name) still matches the held fd's
//!     dev/ino — a planted lookalike is never touched
//!
//! Non-Unix platforms: the whole module is #[cfg(unix)]; callers fail closed
//! with an `unsupported` error rather than shipping a weaker local engine.

use std::ffi::{CStr, CString, OsString};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStringExt;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::browser::guards;
use crate::browser::transfer::place::{
    place_action, rename_errno_outcome, ItemKind, PlaceAction, RenameOutcome,
};
use crate::browser::transfer::{stage_dir_name, CopySink, ProgressReporter, CHUNK};
use crate::browser::{BrowserError, ErrorKind, MAX_DEPTH};

// ---------------------------------------------------------------------------
// fd primitives
// ---------------------------------------------------------------------------

const O_DIR: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;

fn cstr(s: &str) -> Result<CString, BrowserError> {
    CString::new(s)
        .map_err(|_| BrowserError::new(ErrorKind::Other, "path contains an interior NUL byte"))
}

fn io_err_kind(e: &std::io::Error) -> ErrorKind {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => ErrorKind::Denied,
        Some(libc::ENOENT) | Some(libc::ENOTDIR) => ErrorKind::Notfound,
        Some(libc::EEXIST) => ErrorKind::Exists,
        _ => ErrorKind::Other,
    }
}

fn map_io(e: std::io::Error, what: &str) -> BrowserError {
    let kind = io_err_kind(&e);
    BrowserError::new(kind, format!("{what}: {e}"))
}

/// Open a directory the copy will read from / write into (plugin open_top).
pub fn open_top(path: &str) -> Result<OwnedFd, BrowserError> {
    let c = cstr(path)?;
    let fd = unsafe { libc::open(c.as_ptr(), O_DIR) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        let kind = match e.raw_os_error() {
            Some(libc::EACCES) => ErrorKind::Denied,
            _ => ErrorKind::Notfound,
        };
        return Err(BrowserError::new(
            kind,
            format!("Can't open {} ({e})", path),
        ));
    }
    // SAFETY: fd was just created by a successful open().
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

use std::os::fd::FromRawFd as _;

fn fstat_fd(fd: RawFd) -> std::io::Result<libc::stat> {
    let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

fn statat(fd: RawFd, name: &CStr, follow: bool) -> std::io::Result<libc::stat> {
    let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, flags) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

/// fd of folder `name` inside folder `pfd`, but only if it is a real folder
/// and the VERY one that was examined (dev/ino re-check) — plugin
/// open_dir_at. `Ok(None)` when the check fails; the caller treats that as
/// "changed while it was being copied".
fn open_dir_at(pfd: RawFd, name: &CStr) -> std::io::Result<Option<OwnedFd>> {
    let st = statat(pfd, name, false)?;
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Ok(None);
    }
    let fd = unsafe { libc::openat(pfd, name.as_ptr(), O_DIR | libc::O_NOFOLLOW) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful openat.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let fst = fstat_fd(owned.as_raw_fd())?;
    if (fst.st_dev, fst.st_ino) != (st.st_dev, st.st_ino) {
        return Ok(None);
    }
    Ok(Some(owned))
}

/// Names in the folder fd holds, rewound first (a listing of an fd that was
/// listed before can otherwise come back stale — plugin names_at). Raw
/// bytes: non-UTF-8 names are surfaced as None text and SKIPPED+COUNTED by
/// the walker (M0: never corrupt, never guess).
struct RawName {
    raw: CString,
    text: Option<String>,
}

fn names_at(fd: RawFd) -> std::io::Result<Vec<RawName>> {
    unsafe {
        libc::lseek(fd, 0, libc::SEEK_SET);
    }
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let dp = unsafe { libc::fdopendir(dup) };
    if dp.is_null() {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(dup) };
        return Err(e);
    }
    let mut out = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(dp) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let bytes = name.to_bytes().to_vec();
        // Interior NUL impossible from readdir; if it ever happens, drop.
        if let Ok(raw) = CString::new(bytes.clone()) {
            out.push(RawName {
                raw,
                text: String::from_utf8(bytes).ok(),
            });
        }
    }
    unsafe { libc::closedir(dp) }; // also closes dup
    out.sort_by(|a, b| a.raw.as_bytes().cmp(b.raw.as_bytes()));
    Ok(out)
}

fn readlink_at(fd: RawFd, name: &CStr) -> std::io::Result<String> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    let n = unsafe { libc::readlinkat(fd, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn set_times(fd: RawFd, mtime: i64) {
    let ts = libc::timespec {
        tv_sec: mtime as libc::time_t,
        tv_nsec: 0,
    };
    let times = [ts, ts];
    unsafe {
        libc::futimens(fd, times.as_ptr());
    }
}

// ---------------------------------------------------------------------------
// Pre-walk: total size for progress (plugin local_size)
// ---------------------------------------------------------------------------

/// Pre-walked byte total for the progress denominator (the plugin's `T`
/// record for local sources). Symlinks and specials contribute 0.
pub fn total_size(dir: &str, names: &[String]) -> Result<u64, BrowserError> {
    let pfd = open_top(dir)?;
    let mut total = 0u64;
    for n in names {
        let c = cstr(n)?;
        total += size_at(pfd.as_raw_fd(), &c, 0)?;
    }
    Ok(total)
}

fn size_at(pfd: RawFd, name: &CStr, depth: usize) -> Result<u64, BrowserError> {
    if depth > MAX_DEPTH {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "folders nested too deeply to copy",
        ));
    }
    let st = match statat(pfd, name, false) {
        Ok(st) => st,
        Err(_) => return Ok(0), // vanished; the streaming walk reports it
    };
    let fmt = st.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFREG {
        return Ok(st.st_size.max(0) as u64);
    }
    if fmt == libc::S_IFDIR {
        if let Some(fd) = open_dir_at(pfd, name).map_err(|e| map_io(e, "folder changed"))? {
            let mut total = 0u64;
            for entry in names_at(fd.as_raw_fd()).map_err(|e| map_io(e, "listing"))? {
                total += size_at(fd.as_raw_fd(), &entry.raw, depth + 1)?;
            }
            return Ok(total);
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Streaming walk (plugin local_walk / local_records)
// ---------------------------------------------------------------------------

/// Walk the selected names under `dir`, driving `sink` in depth-first order.
/// Returns the skipped count (specials, control-char names, non-UTF-8 names).
pub async fn stream_names(
    dir: &str,
    names: &[String],
    sink: &mut dyn CopySink,
    prog: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<u32, BrowserError> {
    let pfd = open_top(dir)?;
    let mut skipped = 0u32;
    for n in names {
        if let Some(why) = guards::control_char_problem(n) {
            return Err(BrowserError::new(ErrorKind::Other, format!("{n:?} {why}")));
        }
        let c = cstr(n)?;
        skipped += walk_at(pfd.as_raw_fd(), &c, n, sink, prog, cancel, 0).await?;
    }
    Ok(skipped)
}

/// Boxed return: recursive async walk (E0733) + provably Send for the
/// spawned copy task.
#[allow(clippy::too_many_arguments)]
fn walk_at<'a>(
    pfd: RawFd,
    name: &'a CStr,
    rel: &'a str,
    sink: &'a mut dyn CopySink,
    prog: &'a mut ProgressReporter,
    cancel: &'a CancellationToken,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, BrowserError>> + Send + 'a>> {
    Box::pin(async move {
        if cancel.is_cancelled() {
            return Err(BrowserError::new(ErrorKind::Other, "Cancelled"));
        }
        if depth > MAX_DEPTH {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!(
                    "{rel} has folders nested more than {MAX_DEPTH} deep, so it can't be copied"
                ),
            ));
        }
        let st = statat(pfd, name, false).map_err(|_| {
            BrowserError::new(
                ErrorKind::Notfound,
                format!("{rel} disappeared while it was being copied"),
            )
        })?;
        let fmt = st.st_mode & libc::S_IFMT;
        let mut skipped = 0u32;

        if fmt == libc::S_IFLNK {
            // Copied as a link, never followed.
            let target =
                readlink_at(pfd, name).map_err(|e| map_io(e, &format!("{rel} (readlink)")))?;
            if !sink.symlink(rel, &target).await? {
                skipped += 1;
            }
        } else if fmt == libc::S_IFDIR {
            let fd = open_dir_at(pfd, name)
                .map_err(|e| map_io(e, &format!("{rel} changed")))?
                .ok_or_else(|| {
                    BrowserError::new(
                        ErrorKind::Other,
                        format!("{rel} changed while it was being copied"),
                    )
                })?;
            sink.dir_begin(rel, st.st_mode as u32 & 0o7777, st.st_mtime as i64)
                .await?;
            let entries = names_at(fd.as_raw_fd()).map_err(|e| map_io(e, "listing"))?;
            for entry in entries {
                let text = match &entry.text {
                    Some(t) if guards::control_char_problem(t).is_none() => t.clone(),
                    // Unsafe or non-UTF-8 name: skipped and counted, exactly
                    // once, never corrupted or guessed (M0 rule).
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                let child_rel = format!("{rel}/{text}");
                skipped += walk_at(
                    fd.as_raw_fd(),
                    &entry.raw,
                    &child_rel,
                    &mut *sink,
                    &mut *prog,
                    cancel,
                    depth + 1,
                )
                .await?;
            }
            sink.dir_end().await?;
        } else if fmt == libc::S_IFREG {
            let fd = {
                let f = unsafe {
                    libc::openat(
                        pfd,
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                };
                if f < 0 {
                    return Err(BrowserError::new(
                        ErrorKind::Other,
                        format!("{rel} changed while it was being copied"),
                    ));
                }
                // SAFETY: successful openat.
                unsafe { OwnedFd::from_raw_fd(f) }
            };
            let fst = fstat_fd(fd.as_raw_fd()).map_err(|e| map_io(e, &format!("{rel} (fstat)")))?;
            if fst.st_mode & libc::S_IFMT != libc::S_IFREG
                || (fst.st_dev, fst.st_ino) != (st.st_dev, st.st_ino)
            {
                return Err(BrowserError::new(
                    ErrorKind::Other,
                    format!("{rel} changed while it was being copied"),
                ));
            }
            let size = fst.st_size.max(0) as u64;
            sink.file_begin(rel).await?;
            let r = stream_fd(fd.as_raw_fd(), size, rel, sink, prog, cancel).await;
            // Close (drop fd) on every path, then surface the error.
            drop(fd);
            r?;
            sink.file_end(fst.st_mode as u32 & 0o7777, fst.st_mtime as i64)
                .await?;
        } else {
            // Sockets, devices, pipes: skipped and counted (the plugin's S).
            skipped += 1;
        }
        Ok(skipped)
    })
}

/// Read exactly `size` bytes, chunk by chunk, feeding the sink. A short read
/// means the file shrank mid-copy: fail, never deliver partial content.
async fn stream_fd(
    fd: RawFd,
    size: u64,
    rel: &str,
    sink: &mut dyn CopySink,
    prog: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(), BrowserError> {
    let mut buf = vec![0u8; CHUNK];
    let mut left = size;
    while left > 0 {
        if cancel.is_cancelled() {
            return Err(BrowserError::new(ErrorKind::Other, "Cancelled"));
        }
        let want = std::cmp::min(left, CHUNK as u64) as usize;
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), want) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(map_io(e, &format!("reading {rel}")));
        }
        if n == 0 {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("{rel} got shorter while it was being copied"),
            ));
        }
        sink.file_chunk(&buf[..n as usize]).await?;
        prog.add(n as u64, rel);
        left -= n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rename seam (injectable for non-atomic-FS / EXDEV injection tests)
// ---------------------------------------------------------------------------

/// The atomic-rename primitives behind an injectable seam so the fail-closed
/// behavior on non-atomic filesystems and cross-device errors is testable
/// without such a filesystem (repo pattern: pure cores + seams).
pub trait RenameNoReplace: Send + Sync {
    /// Atomic no-replace rename relative to two dir fds. Err(errno).
    fn rename_noreplace(
        &self,
        sfd: RawFd,
        sname: &CStr,
        dfd: RawFd,
        dname: &CStr,
    ) -> Result<(), i32>;
    /// Atomic REPLACING rename (consented Replace of a file/link only).
    fn rename_replace(&self, sfd: RawFd, sname: &CStr, dfd: RawFd, dname: &CStr)
        -> Result<(), i32>;
}

pub struct RealRename;

#[cfg(target_os = "linux")]
impl RenameNoReplace for RealRename {
    fn rename_noreplace(
        &self,
        sfd: RawFd,
        sname: &CStr,
        dfd: RawFd,
        dname: &CStr,
    ) -> Result<(), i32> {
        renameat2(sfd, sname, dfd, dname, 1 /* RENAME_NOREPLACE */)
    }
    fn rename_replace(
        &self,
        sfd: RawFd,
        sname: &CStr,
        dfd: RawFd,
        dname: &CStr,
    ) -> Result<(), i32> {
        renameat2(sfd, sname, dfd, dname, 0)
    }
}

#[cfg(target_os = "linux")]
fn renameat2(sfd: RawFd, sname: &CStr, dfd: RawFd, dname: &CStr, flags: u32) -> Result<(), i32> {
    let r = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            sfd as libc::c_int,
            sname.as_ptr(),
            dfd as libc::c_int,
            dname.as_ptr(),
            flags as libc::c_uint,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

#[cfg(target_os = "macos")]
impl RenameNoReplace for RealRename {
    fn rename_noreplace(
        &self,
        sfd: RawFd,
        sname: &CStr,
        dfd: RawFd,
        dname: &CStr,
    ) -> Result<(), i32> {
        // renameatx_np(…, RENAME_EXCL=4): fail with EEXIST instead of
        // replacing (the macOS equivalent of RENAME_NOREPLACE).
        renameatx(sfd, sname, dfd, dname, libc::RENAME_EXCL)
    }
    fn rename_replace(
        &self,
        sfd: RawFd,
        sname: &CStr,
        dfd: RawFd,
        dname: &CStr,
    ) -> Result<(), i32> {
        renameatx(sfd, sname, dfd, dname, 0)
    }
}

#[cfg(target_os = "macos")]
fn renameatx(sfd: RawFd, sname: &CStr, dfd: RawFd, dname: &CStr, flags: u32) -> Result<(), i32> {
    let r = unsafe {
        libc::renameatx_np(
            sfd as libc::c_int,
            sname.as_ptr(),
            dfd as libc::c_int,
            dname.as_ptr(),
            flags,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
impl RenameNoReplace for RealRename {
    fn rename_noreplace(
        &self,
        _sfd: RawFd,
        _sname: &CStr,
        _dfd: RawFd,
        _dname: &CStr,
    ) -> Result<(), i32> {
        // No known atomic no-replace primitive: fail CLOSED (ENOSYS →
        // NonAtomic → item not moved), never fall back to a racy rename.
        Err(libc::ENOSYS)
    }
    fn rename_replace(
        &self,
        _sfd: RawFd,
        _sname: &CStr,
        _dfd: RawFd,
        _dname: &CStr,
    ) -> Result<(), i32> {
        Err(libc::ENOSYS)
    }
}

// ---------------------------------------------------------------------------
// LocalSink
// ---------------------------------------------------------------------------

struct StackEntry {
    rel: String,
    /// None for the root entry (its fd is `sfd`, owned separately and held
    /// until removal — the plugin's stack root does the same).
    fd: Option<OwnedFd>,
    mode: u32,
    mtime: i64,
}

/// Handle-bound local destination (plugin LocalSink). The staging folder is
/// held by fd from creation until removal and never looked up by name again
/// while in use; the name is unlinked at the end only while it still IS the
/// held folder (dev/ino match).
pub struct LocalSink {
    dfd: OwnedFd,
    stage_name: CString,
    sfd: OwnedFd,
    stack: Vec<StackEntry>,
    current: Option<OwnedFd>,
    renamer: Box<dyn RenameNoReplace>,
    removed: bool,
}

impl std::fmt::Debug for LocalSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSink").finish_non_exhaustive()
    }
}

impl LocalSink {
    pub fn open(dst_dir: &str, tag: &str) -> Result<Self, BrowserError> {
        Self::open_with(dst_dir, tag, Box::new(RealRename))
    }

    /// Test seam: inject the rename behavior (non-atomic FS, EXDEV, ...).
    pub fn open_with(
        dst_dir: &str,
        tag: &str,
        renamer: Box<dyn RenameNoReplace>,
    ) -> Result<Self, BrowserError> {
        let dfd = open_top(dst_dir)?;
        let stage_name = cstr(&stage_dir_name(tag))?;
        // Exclusive by mkdir semantics. EEXIST on a 64-bit random tag means
        // either a planted lookalike or an astronomical collision — either
        // way: refuse, never write into an existing folder.
        let r = unsafe { libc::mkdirat(dfd.as_raw_fd(), stage_name.as_ptr(), 0o700) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            let kind = if e.raw_os_error() == Some(libc::EACCES) {
                ErrorKind::Denied
            } else {
                io_err_kind(&e)
            };
            return Err(BrowserError::new(kind, format!("Can't write there ({e})")));
        }
        let sfd = open_dir_at(dfd.as_raw_fd(), &stage_name)
            .map_err(|e| map_io(e, "staging folder"))?
            .ok_or_else(|| {
                BrowserError::new(
                    ErrorKind::Other,
                    "The copy's new folder changed before anything was written",
                )
            })?;
        // Identity check at creation: owned by this euid, mode 0700, empty
        // (invariant §4.3 — a swapped or pre-planted stage is refused).
        let fst = fstat_fd(sfd.as_raw_fd()).map_err(|e| map_io(e, "staging folder (fstat)"))?;
        let empty = names_at(sfd.as_raw_fd())
            .map_err(|e| map_io(e, "staging folder"))?
            .is_empty();
        let euid = unsafe { libc::geteuid() };
        if fst.st_uid != euid || (fst.st_mode & 0o777) != 0o700 || !empty {
            return Err(BrowserError::new(
                ErrorKind::Other,
                "The copy's new folder was swapped for another one, so nothing was copied",
            ));
        }
        Ok(Self {
            dfd,
            stage_name,
            sfd,
            stack: vec![StackEntry {
                rel: String::new(),
                fd: None,
                mode: 0,
                mtime: 0,
            }],
            current: None,
            renamer,
            removed: false,
        })
    }

    #[allow(dead_code)] // staging diagnostics
    pub fn stage_path_str(&self) -> String {
        self.stage_name.to_string_lossy().into_owned()
    }

    fn top_fd(&self) -> RawFd {
        match self.stack.last().and_then(|e| e.fd.as_ref()) {
            Some(fd) => fd.as_raw_fd(),
            None => self.sfd.as_raw_fd(),
        }
    }

    /// Pop to the parent folder of `rel` (plugin LocalSink.parent). Out of
    /// order streams are a hard error, never an implicit mkdir -p.
    fn align(&mut self, rel: &str) -> Result<(RawFd, CString), BrowserError> {
        let (parent, name) = match rel.rsplit_once('/') {
            Some((p, n)) => (p, n),
            None => ("", rel),
        };
        if name.is_empty() || name == "." || name == ".." {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("Unexpected item in the copy: {rel}"),
            ));
        }
        while self.stack.len() > 1 && self.stack.last().map(|e| e.rel.as_str()) != Some(parent) {
            self.pop()?;
        }
        if self.stack.last().map(|e| e.rel.as_str()) != Some(parent) {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!("Unexpected item in the copy: {rel}"),
            ));
        }
        Ok((self.top_fd(), cstr(name)?))
    }

    fn pop(&mut self) -> Result<(), BrowserError> {
        if self.stack.len() <= 1 {
            return Ok(());
        }
        let entry = self.stack.pop().expect("len > 1");
        if let Some(fd) = entry.fd {
            // Final mode/date applied through the held fd, then closed —
            // plugin pop(): fchmod((mode & 0o777) | 0o700), utime, close.
            unsafe {
                libc::fchmod(
                    fd.as_raw_fd(),
                    ((entry.mode & 0o777) | 0o700) as libc::mode_t,
                );
            }
            set_times(fd.as_raw_fd(), entry.mtime);
        }
        Ok(())
    }

    /// place_local port: move everything staged into the destination folder.
    #[allow(clippy::too_many_arguments)] // port of the plugin's place_local signature
    fn place_in(
        &self,
        sfd: RawFd,
        dfd: RawFd,
        rel: &str,
        replace: bool,
        conflicts: &mut Vec<String>,
        errors: &mut Vec<(String, String)>,
        depth: usize,
    ) -> Result<(), BrowserError> {
        for entry in names_at(sfd).map_err(|e| map_io(e, "staged folder"))? {
            let name = match &entry.text {
                Some(t) => t.clone(),
                None => continue, // cannot happen (we created these names)
            };
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let mine = match statat(sfd, &entry.raw, false) {
                Ok(st) => st.st_mode,
                Err(_) => continue,
            };
            let outcome = match self
                .renamer
                .rename_noreplace(sfd, &entry.raw, dfd, &entry.raw)
            {
                Ok(()) => RenameOutcome::Placed,
                Err(errno) => rename_errno_outcome(errno),
            };
            let theirs = statat(dfd, &entry.raw, false).ok().map(|st| {
                if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
                    ItemKind::Dir
                } else {
                    ItemKind::FileOrLink
                }
            });
            let mine_kind = if mine & libc::S_IFMT == libc::S_IFDIR {
                ItemKind::Dir
            } else {
                ItemKind::FileOrLink
            };
            match place_action(outcome, replace, mine_kind, theirs, depth < MAX_DEPTH) {
                PlaceAction::Placed => {}
                PlaceAction::Conflict => conflicts.push(r),
                PlaceAction::FailNonAtomic => {
                    errors.push((r, "that disk can't rename atomically".to_string()))
                }
                PlaceAction::FailOther => errors.push((r, "rename failed".to_string())),
                PlaceAction::MergeDirs => {
                    let cs =
                        open_dir_at(sfd, &entry.raw).map_err(|e| map_io(e, "staged folder"))?;
                    let cd = open_dir_at(dfd, &entry.raw)
                        .map_err(|e| map_io(e, "destination folder"))?;
                    match (cs, cd) {
                        (Some(cs), Some(cd)) => {
                            self.place_in(
                                cs.as_raw_fd(),
                                cd.as_raw_fd(),
                                &r,
                                true,
                                conflicts,
                                errors,
                                depth + 1,
                            )?;
                        }
                        _ => conflicts.push(r),
                    }
                }
                PlaceAction::ReplaceAtomic => {
                    // One atomic rename replaces the file or the link itself
                    // (never follows it).
                    if self
                        .renamer
                        .rename_replace(sfd, &entry.raw, dfd, &entry.raw)
                        .is_err()
                    {
                        conflicts.push(r);
                    }
                }
            }
        }
        Ok(())
    }

    /// remove_at port: empty the stage through held fds only (depth-bounded),
    /// never by re-walking names from the parent.
    fn remove_at(fd: RawFd, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let entries = match names_at(fd) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries {
            let st = match statat(fd, &entry.raw, false) {
                Ok(st) => st,
                Err(_) => continue,
            };
            if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
                // Not the folder we examined (open failed/vanished) — leave it alone.
                if let Ok(Some(child)) = open_dir_at(fd, &entry.raw) {
                    Self::remove_at(child.as_raw_fd(), depth + 1);
                    unsafe {
                        libc::unlinkat(fd, entry.raw.as_ptr(), libc::AT_REMOVEDIR);
                    }
                }
            } else {
                unsafe {
                    libc::unlinkat(fd, entry.raw.as_ptr(), 0);
                }
            }
        }
    }

    /// LocalSink.remove port: empty through the held handle, then unlink the
    /// NAME only while lstat(name) still matches the held fd's dev/ino. A
    /// planted lookalike (attacker removed our stage and symlinked the name)
    /// is NEVER touched.
    fn do_remove(&mut self) -> Result<(), BrowserError> {
        if self.removed {
            return Ok(());
        }
        self.removed = true;
        // Drop any open file/dir stack entries first.
        self.current = None;
        while self.stack.len() > 1 {
            let _ = self.pop();
        }
        Self::remove_at(self.sfd.as_raw_fd(), 0);
        let mine = fstat_fd(self.sfd.as_raw_fd()).ok();
        let named = statat(self.dfd.as_raw_fd(), &self.stage_name, false).ok();
        if let (Some(mine), Some(named)) = (mine, named) {
            if (mine.st_dev, mine.st_ino) == (named.st_dev, named.st_ino) {
                unsafe {
                    libc::unlinkat(
                        self.dfd.as_raw_fd(),
                        self.stage_name.as_ptr(),
                        libc::AT_REMOVEDIR,
                    );
                }
            }
            // else: the name is no longer our folder — leave it alone.
        }
        Ok(())
    }
}

impl Drop for LocalSink {
    fn drop(&mut self) {
        // Last-resort cleanup on panic/unwind paths; run_copy always calls
        // remove() explicitly on every normal exit path.
        if !self.removed {
            let _ = self.do_remove();
        }
    }
}

#[async_trait]
impl CopySink for LocalSink {
    async fn dir_begin(&mut self, rel: &str, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        if self.stack.len() > MAX_DEPTH {
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!(
                    "{} has folders nested more than {MAX_DEPTH} deep, so it can't be copied",
                    rel.split('/').next().unwrap_or(rel)
                ),
            ));
        }
        let (pfd, name) = self.align(rel)?;
        if unsafe { libc::mkdirat(pfd, name.as_ptr(), 0o700) } != 0 {
            let e = std::io::Error::last_os_error();
            return Err(map_io(e, &format!("Couldn't write {rel}")));
        }
        let fd = open_dir_at(pfd, &name)
            .map_err(|e| map_io(e, rel))?
            .ok_or_else(|| {
                BrowserError::new(
                    ErrorKind::Other,
                    format!("{rel} changed while it was being written"),
                )
            })?;
        self.stack.push(StackEntry {
            rel: rel.to_string(),
            fd: Some(fd),
            mode,
            mtime,
        });
        Ok(())
    }

    async fn dir_end(&mut self) -> Result<(), BrowserError> {
        self.pop()
    }

    async fn file_begin(&mut self, rel: &str) -> Result<(), BrowserError> {
        let (pfd, name) = self.align(rel)?;
        let fd = unsafe {
            libc::openat(
                pfd,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(map_io(e, &format!("Couldn't write {rel}")));
        }
        // SAFETY: successful openat.
        self.current = Some(unsafe { OwnedFd::from_raw_fd(fd) });
        Ok(())
    }

    async fn file_chunk(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        let fd = self
            .current
            .as_ref()
            .ok_or_else(|| BrowserError::new(ErrorKind::Other, "no file open"))?
            .as_raw_fd();
        let mut off = 0usize;
        while off < data.len() {
            let n = unsafe { libc::write(fd, data[off..].as_ptr().cast(), data.len() - off) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(map_io(e, "writing staged file"));
            }
            off += n as usize;
        }
        Ok(())
    }

    async fn file_end(&mut self, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        let fd = self
            .current
            .take()
            .ok_or_else(|| BrowserError::new(ErrorKind::Other, "no file open"))?;
        // Source mode/date through the held fd, then closed (plugin F path).
        unsafe {
            libc::fchmod(fd.as_raw_fd(), (mode & 0o7777) as libc::mode_t);
        }
        set_times(fd.as_raw_fd(), mtime);
        Ok(())
    }

    async fn symlink(&mut self, rel: &str, target: &str) -> Result<bool, BrowserError> {
        let (pfd, name) = self.align(rel)?;
        let t = cstr(target)?;
        if unsafe { libc::symlinkat(t.as_ptr(), pfd, name.as_ptr()) } != 0 {
            let e = std::io::Error::last_os_error();
            return Err(map_io(e, &format!("Couldn't write link {rel}")));
        }
        Ok(true)
    }

    async fn place(
        &mut self,
        _top_names: &[String],
        replace: bool,
    ) -> Result<(Vec<String>, Vec<(String, String)>), BrowserError> {
        let mut conflicts = Vec::new();
        let mut errors = Vec::new();
        self.place_in(
            self.sfd.as_raw_fd(),
            self.dfd.as_raw_fd(),
            "",
            replace,
            &mut conflicts,
            &mut errors,
            0,
        )?;
        Ok((conflicts, errors))
    }

    async fn remove(&mut self) -> Result<(), BrowserError> {
        self.do_remove()
    }
}

/// Keep OsString import used (raw-name handling helper for tests/debug).
#[allow(dead_code)]
fn osstring_from_raw(raw: Vec<u8>) -> OsString {
    OsString::from_vec(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::transfer::{stage_dir_name, CopySink};
    use std::path::{Path, PathBuf};
    use tokio_util::sync::CancellationToken;

    /// Isolated temp dir, removed on drop (invariant: tests never share a
    /// fixed scratch path).
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> TmpDir {
            use rand::Rng as _;
            let n: u64 = rand::rng().random();
            let dir = std::env::temp_dir().join(format!(
                "taildrop-m1-local-{tag}-{}-{n:016x}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TmpDir(std::fs::canonicalize(&dir).expect("canonicalize"))
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Injected rename behavior (the documented test seam): every
    /// no-replace rename fails with `nr`, every replacing rename with `r`.
    struct FakeRename {
        nr: i32,
        r: i32,
    }
    impl RenameNoReplace for FakeRename {
        fn rename_noreplace(
            &self,
            _sfd: RawFd,
            _sname: &CStr,
            _dfd: RawFd,
            _dname: &CStr,
        ) -> Result<(), i32> {
            Err(self.nr)
        }
        fn rename_replace(
            &self,
            _sfd: RawFd,
            _sname: &CStr,
            _dfd: RawFd,
            _dname: &CStr,
        ) -> Result<(), i32> {
            Err(self.r)
        }
    }

    fn stage_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with(crate::browser::STAGE_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_mtime(path: &Path, secs: i64) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path cstr");
        let ts = libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        };
        let times = [ts, ts];
        unsafe {
            libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0);
        }
    }

    // ---- staging creation attacks ----

    /// A pre-planted SYMLINK at the exact stage name must be refused
    /// (mkdirat EEXIST) — and never followed, so the victim is untouched.
    #[test]
    fn preplanted_symlink_stage_is_refused() {
        let root = TmpDir::new("preplant-link");
        let dst = root.path().join("dst");
        let victim = root.path().join("victim");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("secret.txt"), b"secret").unwrap();
        let tag = "0123456789abcdef";
        std::os::unix::fs::symlink(&victim, dst.join(stage_dir_name(tag))).unwrap();

        let e = LocalSink::open(dst.to_str().unwrap(), tag)
            .expect_err("planted symlink at the stage name must be refused");
        assert_eq!(e.kind, ErrorKind::Exists);
        // Victim untouched, link still a link (never followed, never unlinked).
        assert_eq!(std::fs::read(victim.join("secret.txt")).unwrap(), b"secret");
        assert!(dst
            .join(stage_dir_name(tag))
            .symlink_metadata()
            .unwrap()
            .is_symlink());
    }

    /// A pre-planted non-empty DIRECTORY (even with plausible mode) must be
    /// refused — the sink never writes into an existing folder.
    #[test]
    fn preplanted_dir_stage_is_refused() {
        let root = TmpDir::new("preplant-dir");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let tag = "fedcba9876543210";
        let planted = dst.join(stage_dir_name(tag));
        std::fs::create_dir(&planted).unwrap();
        std::fs::write(planted.join("attacker.txt"), b"evil").unwrap();

        let e = LocalSink::open(dst.to_str().unwrap(), tag)
            .expect_err("planted directory must be refused");
        assert_eq!(e.kind, ErrorKind::Exists);
        assert_eq!(
            std::fs::read(planted.join("attacker.txt")).unwrap(),
            b"evil"
        );
    }

    /// Swap-before-unlink: after the sink opened its stage, an attacker
    /// replaces the stage NAME with a symlink to a victim dir. Cleanup must
    /// empty only the held fd (the orphaned original) and NEVER unlink the
    /// planted name — dev/ino identity guard.
    #[tokio::test]
    async fn swapped_stage_name_is_never_unlinked() {
        let root = TmpDir::new("swap-stage");
        let dst = root.path().join("dst");
        let victim = root.path().join("victim");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("secret.txt"), b"secret").unwrap();
        let tag = "aaaaaaaaaaaaaaaa";
        let mut sink = LocalSink::open(dst.to_str().unwrap(), tag).expect("open stage");
        // Stage some content through the normal sink path.
        sink.file_begin("a.txt").await.unwrap();
        sink.file_chunk(b"payload").await.unwrap();
        sink.file_end(0o644, 0).await.unwrap();

        // Attacker swaps the stage name for a symlink to the victim.
        let stage = dst.join(stage_dir_name(tag));
        std::fs::remove_dir_all(&stage).unwrap();
        std::os::unix::fs::symlink(&victim, &stage).unwrap();

        // Cleanup runs (as it does on every exit path): must not touch the
        // planted name or anything under the victim.
        sink.remove().await.unwrap();
        assert!(victim.join("secret.txt").exists(), "victim untouched");
        assert!(
            stage.symlink_metadata().unwrap().is_symlink(),
            "planted link was NOT unlinked (dev/ino guard)"
        );
    }

    // ---- rename seam injection: EXDEV + non-atomic fail closed ----

    async fn stage_one_file(dst: &str, tag: &str, errno_nr: i32) -> LocalSink {
        let mut sink = LocalSink::open_with(
            dst,
            tag,
            Box::new(FakeRename {
                nr: errno_nr,
                r: libc::EIO,
            }),
        )
        .expect("open_with fake renamer");
        sink.file_begin("a.txt").await.unwrap();
        sink.file_chunk(b"hello").await.unwrap();
        sink.file_end(0o644, 0).await.unwrap();
        sink
    }

    /// EXDEV (cross-device) must fail closed: the error is aggregated for
    /// the row, and NOTHING is placed by any fallback copy.
    #[tokio::test]
    async fn exdev_rename_fails_closed() {
        let root = TmpDir::new("exdev");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let tag = "bbbbbbbbbbbbbbbb";
        let mut sink = stage_one_file(dst.to_str().unwrap(), tag, libc::EXDEV).await;
        let (conflicts, errors) = sink
            .place(&["a.txt".to_string()], false)
            .await
            .expect("place aggregates, never throws for row errors");
        assert!(conflicts.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "a.txt");
        // Fail closed: no fallback copy landed the file.
        assert!(!dst.join("a.txt").exists());
        sink.remove().await.unwrap();
        assert!(stage_leftovers(&dst).is_empty());
    }

    /// A filesystem where renameat2/renameatx_np is unavailable
    /// (EINVAL/ENOSYS/EOPNOTSUPP) must fail closed with the non-atomic
    /// message — never silently downgrade to a replacing rename.
    #[tokio::test]
    async fn non_atomic_fs_fails_closed() {
        let root = TmpDir::new("nonatomic");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let tag = "cccccccccccccccc";
        let mut sink = stage_one_file(dst.to_str().unwrap(), tag, libc::EINVAL).await;
        let (conflicts, errors) = sink.place(&["a.txt".to_string()], false).await.unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].1.contains("atomically"),
            "expected non-atomic message, got {:?}",
            errors[0].1
        );
        assert!(!dst.join("a.txt").exists());
        sink.remove().await.unwrap();
        assert!(stage_leftovers(&dst).is_empty());
    }

    // ---- full local→local copy runs (RealRename, kernel semantics) ----

    use crate::browser::transfer::{self, Side};
    use crate::browser::CopyRequest;
    use std::sync::{Arc, Mutex};

    fn local_req(src: &str, dst: &str, names: &[&str], replace: bool) -> CopyRequest {
        CopyRequest {
            src_target: "local".to_string(),
            src_user: None,
            src_dir: src.to_string(),
            dst_target: "local2".to_string(),
            dst_user: None,
            dst_dir: dst.to_string(),
            names: names.iter().map(|s| s.to_string()).collect(),
            replace,
        }
    }

    fn noop_emit() -> Arc<dyn Fn(u8, String, String) + Send + Sync> {
        Arc::new(|_p, _r, _f| ())
    }

    #[tokio::test]
    async fn local_to_local_fidelity() {
        let root = TmpDir::new("l2l-fidelity");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("hello.txt"), b"hello world").unwrap();
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/inner.bin"), vec![5u8; 1000]).unwrap();
        std::os::unix::fs::symlink("hello.txt", src.join("link.txt")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            src.join("hello.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::set_permissions(src.join("sub"), std::fs::Permissions::from_mode(0o750)).unwrap();
        set_mtime(&src.join("hello.txt"), 1_600_000_000);

        let outcome = transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["hello.txt", "sub", "link.txt"],
                false,
            ),
            &Side::Local,
            &Side::Local,
            CancellationToken::new(),
            noop_emit(),
        )
        .await
        .expect("local→local copy succeeds");
        assert_eq!(outcome.skipped, 0);
        assert_eq!(outcome.bytes, 11 + 1000);

        // Content, links-as-links, modes, mtime.
        assert_eq!(
            std::fs::read(dst.join("hello.txt")).unwrap(),
            b"hello world"
        );
        assert_eq!(
            std::fs::read(dst.join("sub/inner.bin")).unwrap(),
            vec![5u8; 1000]
        );
        let lm = std::fs::symlink_metadata(dst.join("link.txt")).unwrap();
        assert!(lm.is_symlink());
        assert_eq!(
            std::fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_str()
                .unwrap(),
            "hello.txt"
        );
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
        let mt = std::fs::metadata(dst.join("hello.txt"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mt, 1_600_000_000);
        // Cleanup on the done path.
        assert!(stage_leftovers(&dst).is_empty());
    }

    #[tokio::test]
    async fn local_exact_name_conflict_refuses_then_replaces_with_consent() {
        let root = TmpDir::new("l2l-conflict");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("hello.txt"), b"NEW").unwrap();
        std::fs::write(dst.join("hello.txt"), b"OLD").unwrap();

        // Without consent: refused (renameat2 RENAME_NOREPLACE → EEXIST),
        // destination untouched, staging cleaned.
        let e = transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["hello.txt"],
                false,
            ),
            &Side::Local,
            &Side::Local,
            CancellationToken::new(),
            noop_emit(),
        )
        .await
        .expect_err("conflict without consent");
        assert_eq!(e.kind, ErrorKind::Exists);
        assert_eq!(std::fs::read(dst.join("hello.txt")).unwrap(), b"OLD");
        assert!(stage_leftovers(&dst).is_empty());

        // With explicit consent: replaced atomically.
        transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["hello.txt"],
                true,
            ),
            &Side::Local,
            &Side::Local,
            CancellationToken::new(),
            noop_emit(),
        )
        .await
        .expect("consented replace");
        assert_eq!(std::fs::read(dst.join("hello.txt")).unwrap(), b"NEW");
        assert!(stage_leftovers(&dst).is_empty());
    }

    /// Case-insensitive clash: the FILESYSTEM decides (renameat2 EEXIST),
    /// never a string comparison. Adaptive: on a case-sensitive volume the
    /// copy simply succeeds and both names coexist.
    #[tokio::test]
    async fn case_clash_asks_the_filesystem() {
        let root = TmpDir::new("l2l-case");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("file.txt"), b"lower").unwrap();
        std::fs::write(dst.join("File.txt"), b"upper").unwrap();
        // Ask the filesystem, not strings:
        let case_insensitive = dst.join("FILE.TXT").exists();

        let r = transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["file.txt"],
                false,
            ),
            &Side::Local,
            &Side::Local,
            CancellationToken::new(),
            noop_emit(),
        )
        .await;
        if case_insensitive {
            let e = r.expect_err("case-insensitive FS must refuse the clash");
            assert_eq!(e.kind, ErrorKind::Exists);
            assert_eq!(std::fs::read(dst.join("File.txt")).unwrap(), b"upper");
        } else {
            r.expect("case-sensitive FS places both names");
            assert_eq!(std::fs::read(dst.join("file.txt")).unwrap(), b"lower");
            assert_eq!(std::fs::read(dst.join("File.txt")).unwrap(), b"upper");
        }
        assert!(stage_leftovers(&dst).is_empty());
    }

    /// Mid-copy cancel on the LOCAL destination leaves no stage and no
    /// partially placed files (pre-cancelled token: refused before
    /// staging exists).
    #[tokio::test]
    async fn local_precancelled_copy_creates_nothing() {
        let root = TmpDir::new("l2l-cancel");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let e = transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["a.txt"],
                false,
            ),
            &Side::Local,
            &Side::Local,
            cancel,
            noop_emit(),
        )
        .await
        .expect_err("pre-cancelled");
        assert!(transfer::is_cancelled(&e));
        assert!(stage_leftovers(&dst).is_empty());
        assert!(!dst.join("a.txt").exists());
    }

    /// Progress events are real-byte based: a multi-chunk local copy emits
    /// through the same ProgressReporter path (smoke: completes and counts
    /// every byte exactly once).
    #[tokio::test]
    async fn local_copy_counts_real_bytes() {
        let root = TmpDir::new("l2l-bytes");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let big = vec![42u8; crate::browser::transfer::CHUNK * 3 + 17];
        std::fs::write(src.join("big.bin"), &big).unwrap();
        let pcts: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let p2 = pcts.clone();
        let emit: Arc<dyn Fn(u8, String, String) + Send + Sync> =
            Arc::new(move |p: u8, _r: String, _f: String| p2.lock().unwrap().push(p));
        let outcome = transfer::run_copy(
            &local_req(
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
                &["big.bin"],
                false,
            ),
            &Side::Local,
            &Side::Local,
            CancellationToken::new(),
            emit,
        )
        .await
        .unwrap();
        assert_eq!(outcome.bytes as usize, big.len());
        assert_eq!(std::fs::read(dst.join("big.bin")).unwrap(), big);
        assert!(stage_leftovers(&dst).is_empty());
    }
}
