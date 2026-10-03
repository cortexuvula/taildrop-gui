use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Host header used in all raw HTTP requests to the Tailscale localapi.
/// Tailscale's daemon expects this exact value.
const LOCALAPI_HOST: &str = "local-tailscaled.sock";

/// Error type for socket/pipe operations that distinguishes "transport
/// unavailable" (socket/pipe missing — safe to fall back to CLI) from
/// "transport connected but request failed" (HTTP error — must propagate).
/// Used by macOS `try_socket_get`/`try_socket_get_to_file` and Windows
/// `try_pipe_get`, and consumed by `get_incoming_files`/`accept_file` to
/// decide whether the CLI fallback is appropriate.
#[derive(Debug)]
#[allow(dead_code)] // Only used on macOS/Windows; Linux uses hyper directly
pub(crate) enum SocketGetError {
    /// `UnixStream::connect` / pipe open failed — the socket/pipe is missing
    /// or inaccessible. Falling back to the CLI is the intended behaviour.
    Connect(String),
    /// The socket/pipe connected but the HTTP request/response failed (HTTP
    /// error status, transport failure mid-response, malformed headers, disk
    /// write error, …). Must be propagated, not swallowed.
    Other(String),
}

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
    #[serde(alias = "Name")]
    pub name: String,
    #[serde(alias = "Size")]
    pub size: u64,
    /// Peer that sent the file, when the Tailscale localapi exposes it.
    /// Accepts both camelCase (`peerName`) and PascalCase (`PeerName`).
    #[serde(default, alias = "PeerName")]
    pub peer_name: Option<String>,
}

// ============================================================
// Shared accept_file helper for CLI-based platforms (macOS/Windows)
// ============================================================

/// Per-filename accept locks. `tailscale file get` drains the WHOLE daemon
/// inbox (it cannot fetch a single named file), so two concurrent accepts of
/// the same name could both claim the same content and destination. Locking
/// per name serializes that case; cross-name races are resolved safely by the
/// staging directory + exact-name fallback in `accept_file_with_getter`.
fn accept_lock(name: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, LazyLock, Mutex};
    static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut map = LOCKS.lock().unwrap_or_else(|p| p.into_inner());
    map.entry(name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Reserve a unique output path in `dir` with an exclusive create (`O_EXCL`),
/// retrying with the next unique suffix when the name is taken. This closes
/// the check-then-create race where `unique_save_path` picked a free name and
/// `File::create` then TRUNCATED whoever created it in between. The returned
/// file is empty; callers stream content into it and must remove it on failure.
fn reserve_unique_file(
    dir: &std::path::Path,
    name: &str,
) -> Result<(std::fs::File, std::path::PathBuf), String> {
    let mut candidate = dir.join(name);
    loop {
        match std::fs::File::create_new(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                candidate = unique_save_path(dir, name);
            }
            Err(e) => {
                return Err(format!(
                    "Failed to create file '{}': {}",
                    candidate.display(),
                    e
                ))
            }
        }
    }
}

/// Whether an I/O error means "rename across filesystems" (EXDEV on Unix,
/// ERROR_NOT_SAME_DEVICE on Windows) and needs the copy fallback.
#[cfg(unix)]
fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(18) // EXDEV
}

#[cfg(windows)]
fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
}

/// Move `src` into `dir` under `name`, NEVER overwriting an existing file.
/// The destination is first reserved with an exclusive create (so concurrent
/// movers get the next unique suffix), then `src` is renamed over our own
/// reservation — atomic on both Unix and Windows. Falls back to copy+delete
/// when the staging and save directories live on different filesystems.
/// Returns the path the content actually landed at.
fn move_file_into_dir(
    src: &std::path::Path,
    dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let (placeholder, dest) = reserve_unique_file(dir, name)?;
    // Windows cannot replace an open file — close our reservation first.
    // The name stays reserved on disk until the rename replaces it.
    drop(placeholder);
    match std::fs::rename(src, &dest) {
        Ok(()) => Ok(dest),
        Err(e) if is_cross_device(&e) => {
            let copy_result = (|| -> Result<(), String> {
                let mut input = std::fs::File::open(src)
                    .map_err(|e| format!("Failed to read '{}': {}", src.display(), e))?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&dest)
                    .map_err(|e| format!("Failed to open '{}': {}", dest.display(), e))?;
                std::io::copy(&mut input, &mut output)
                    .map_err(|e| format!("Failed to copy '{}': {}", src.display(), e))?;
                output
                    .sync_all()
                    .map_err(|e| format!("Failed to flush '{}': {}", dest.display(), e))?;
                Ok(())
            })();
            match copy_result {
                Ok(()) => {
                    let _ = std::fs::remove_file(src);
                    Ok(dest)
                }
                Err(err) => {
                    let _ = std::fs::remove_file(&dest);
                    Err(err)
                }
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(format!(
                "Failed to move '{}' into '{}': {}",
                name,
                dir.display(),
                e
            ))
        }
    }
}

/// Shared accept_file logic for CLI-based platforms.
///
/// `run_get` executes the platform-specific `tailscale file get` command with
/// the given target directory and must return only after the CLI finished.
/// Sanitizes `name` to prevent path traversal attacks.
///
/// The CLI cannot fetch a single named file — it drains every pending inbox
/// entry into the target directory. To keep that from ever touching the
/// user's save directory directly (where `--conflict` handling and half-
/// finished downloads could clobber existing files), the download runs into a
/// private staging directory first; every drained file is then moved into the
/// save dir with exclusive-create semantics, and the exact path the requested
/// file landed at is returned.
#[allow(dead_code)] // Only used on macOS/Windows
fn accept_file_with_getter(
    name: &str,
    save_dir: &str,
    run_get: impl FnOnce(&std::path::Path) -> Result<(), String>,
) -> Result<String, String> {
    accept_file_inner(name, save_dir, run_get, false)
}

/// TD05-A: acknowledgement fast path. Checks whether this exact file was
/// already received and recorded (within the duplicate-suppression window)
/// BEFORE invoking the CLI — acknowledging must not re-drain the inbox
/// (which would consume pending files the user hasn't acted on). Only when
/// no recorded landing matches does the caller fall through to a real
/// accept.
#[allow(dead_code)] // Only used on macOS/Windows
fn acknowledge_already_received(name: &str, save_dir: &str) -> Option<String> {
    let safe_name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())?;
    let existing = std::path::Path::new(save_dir).join(safe_name);
    if !existing.is_file() {
        return None;
    }
    let path_str = existing.to_string_lossy().to_string();
    if crate::receipts::ReceiptStore::saved_landing_recorded(safe_name, &path_str) {
        return Some(path_str);
    }
    None
}

#[allow(dead_code)] // Only used on macOS/Windows
fn accept_file_inner(
    name: &str,
    save_dir: &str,
    run_get: impl FnOnce(&std::path::Path) -> Result<(), String>,
    _retain_staging_for_salvage: bool,
) -> Result<String, String> {
    // Sanitize filename to prevent path traversal
    let safe_name = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid filename".to_string())?;

    let save_dir_path = std::path::Path::new(save_dir);
    std::fs::create_dir_all(save_dir_path).map_err(|e| {
        format!(
            "Cannot create save directory '{}': {}",
            save_dir_path.display(),
            e
        )
    })?;

    let lock = accept_lock(safe_name);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Private staging directory so the CLI's conflict renames ("name (1).ext")
    // and collateral downloads are detected by exact name instead of the old
    // "exactly one new entry" heuristic, and nothing in save_dir is touched
    // until each file is atomically moved in.
    let staging = staging_root().join(format!("taildrop-accept-{:016x}", timestamp_tag()));
    std::fs::create_dir_all(&staging).map_err(|e| {
        format!(
            "Cannot create staging directory '{}': {}",
            staging.display(),
            e
        )
    })?;

    let result = (|| -> Result<String, String> {
        if let Err(e) = run_get(&staging) {
            // TD-01: the CLI may have drained (and removed from the daemon
            // inbox) some files before exiting non-zero. Anything left in
            // staging is the only remaining copy — preserve it and tell the
            // user where it is instead of deleting it.
            let leftover = count_files_in_dir(&staging);
            if leftover > 0 {
                // TD-05: the CLI's stdout would tell us which files completed
                // before the failure, but the injected-getter contract gives
                // us Output only through run_get; files present in staging
                // after a failed run are treated as UNVERIFIED (a file
                // sitting there is not proof it downloaded completely —
                // repo-audititor). They stay recoverable via the staging
                // recovery path; no "saved" receipt is recorded for them.
                return Err(format!(
                    "tailscale file get failed: {}. {} file(s) it had already \
                     downloaded are preserved for recovery in '{}'",
                    e,
                    leftover,
                    staging.display()
                ));
            }
            return Err(e);
        }

        // Move every file the CLI drained out of staging into the save dir.
        // They were already removed from the daemon's inbox, so dropping them
        // here would lose data. Moves never overwrite existing files.
        let mut moved_names: Vec<String> = Vec::new();
        let mut requested_path: Option<std::path::PathBuf> = None;
        let mut move_failures: Vec<String> = Vec::new();
        let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&staging)
            .map_err(|e| {
                format!(
                    "Cannot read staging directory '{}': {}",
                    staging.display(),
                    e
                )
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        for path in entries {
            if !path.is_file() {
                continue;
            }
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            match move_file_into_dir(&path, save_dir_path, &file_name) {
                Ok(dest) => {
                    // TD-05/TD05-A: every landed file records its OWN receipt
                    // — collateral files too, and with NO suppression: a new
                    // landing is a new transfer even when an old receipt for
                    // the same name/path exists (receive → delete → receive
                    // again must produce two receipts). Identity is the
                    // landing event, not the filesystem path.
                    crate::receipts::ReceiptStore::record_saved(
                        &file_name,
                        &dest.to_string_lossy(),
                        size,
                        None,
                    );
                    if file_name == safe_name {
                        requested_path = Some(dest);
                    } else {
                        moved_names.push(file_name);
                    }
                }
                Err(e) => {
                    // TD-01: a failed move must not lose the file — it stays
                    // in staging and the user is told where to find it. The
                    // requested file's failure is surfaced as the overall
                    // error below; collateral failures are appended so the
                    // recovery message covers everything left behind.
                    move_failures.push(format!("{} ({})", file_name, e));
                }
            }
        }
        if !move_failures.is_empty() {
            // The staged copies were already consumed from the daemon inbox;
            // staging now holds their only remaining copy. Report every
            // failure and the recovery location instead of deleting them.
            return Err(format!(
                "Failed to move received file(s) into '{}': {}. The file(s) \
                 are preserved for recovery in '{}'",
                save_dir_path.display(),
                move_failures.join("; "),
                staging.display()
            ));
        }
        if let Some(dest) = requested_path {
            return Ok(dest.to_string_lossy().to_string());
        }

        // The inbox didn't deliver the file this time. Either a concurrent
        // accept already moved it, or the auto-receive poll saved it before
        // the user clicked Accept. If it already sits in the save dir under
        // the exact name, return that path.
        let existing = save_dir_path.join(safe_name);
        if existing.is_file() {
            // TD-05/TD05-A: the fallback fires only when the inbox delivered
            // NOTHING this call — this is an old landing, not a new transfer,
            // so it must not re-record. Suppression here is time-unconditional
            // (a delayed ack still matches) and cannot swallow a new receipt
            // because new landings record in the move loop above, never here.
            if !crate::receipts::ReceiptStore::saved_landing_recorded(
                safe_name,
                &existing.to_string_lossy(),
            ) {
                let size = std::fs::metadata(&existing).map(|m| m.len()).unwrap_or(0);
                crate::receipts::ReceiptStore::record_saved(
                    safe_name,
                    &existing.to_string_lossy(),
                    size,
                    None,
                );
            }
            return Ok(existing.to_string_lossy().to_string());
        }

        let suffix = if moved_names.is_empty() {
            String::new()
        } else {
            format!(" (found instead: {})", moved_names.join(", "))
        };
        Err(format!(
            "tailscale file get succeeded but '{}' did not appear in {}{}",
            safe_name,
            save_dir_path.display(),
            suffix
        ))
    })();

    // TD-01: only remove staging once it provably holds nothing that needs
    // recovery. On success every file was moved out; on failure the error
    // message above already pointed the user at the staging path, which MUST
    // survive unless it is empty. Removing it unconditionally destroyed the
    // receiver's last copy when the CLI or a move failed partway.
    if result.is_ok() || count_files_in_dir(&staging) == 0 {
        let _ = std::fs::remove_dir_all(&staging);
    } else {
        log::warn!(
            "accept: preserving staging directory '{}' for manual recovery",
            staging.display()
        );
    }
    result
}

/// Number of regular files directly inside `dir` (0 if it doesn't exist).
/// Used to decide whether a staging directory still holds files that must
/// survive cleanup (TD-01).
fn count_files_in_dir(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .count()
        })
        .unwrap_or(0)
}

// --- TD-05: pub wrappers for the receipt-recovery command layer ---

/// Move a preserved staging file into the save dir with the full
/// `move_file_into_dir` guarantees (exclusive-create, never overwrites,
/// collision-resolved). Returns the actual landing path.
pub fn move_file_for_recovery(
    src: &std::path::Path,
    dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    move_file_into_dir(src, dir, name)
}

/// Whether a staging directory holds no recoverable files.
pub fn staging_is_empty(dir: &std::path::Path) -> bool {
    count_files_in_dir(dir) == 0
}

/// Shared CLI auto-receive logic for macOS/Windows. Runs
/// `tailscale file get --wait=false --verbose --conflict=rename <save_dir>`
/// (via the platform-specific `run_get` closure, which receives the full
/// argument vector), parses the "moved N/N files" output, and returns a JSON
/// array of the received files.
///
/// `--conflict=rename` is load-bearing: the CLI default must never be
/// `overwrite`, or a background poll would silently clobber same-named files
/// already in the user's save dir. With `rename`, a conflicting incoming file
/// is written as "name (1).ext" instead. (This is also the CLI's default
/// conflict policy — passed explicitly so it can't silently regress.)
///
/// `platform_label` is used in log messages ("macOS" / "Windows").
///
/// TD-05: the drain runs into a private staging directory (same scheme as
/// `accept_file_with_getter`) so received-file identity is EXACT, not
/// inferred: every file that lands is one the CLI verifiably delivered this
/// invocation. The previous implementation picked the N newest files in
/// save_dir by modification time, which an unrelated browser download (or a
/// future-dated file) could hijack (repo-audititor). Receipts are recorded
/// per landed file here — the receive loop and the catch-up both route
/// through this function, so both get durable receipts without duplicated
/// recording logic.
#[allow(dead_code)] // Only used on macOS/Windows
fn cli_receive_files(
    save_dir: &str,
    platform_label: &str,
    run_get: impl FnOnce(&[&str]) -> Result<std::process::Output, String>,
) -> Result<Vec<u8>, String> {
    // Ensure the save directory exists before running the CLI.
    if !std::path::Path::new(save_dir).exists() {
        if let Err(e) = std::fs::create_dir_all(save_dir) {
            log::warn!(
                "{} CLI auto-receive: failed to create save_dir '{}': {}",
                platform_label,
                save_dir,
                e
            );
            // Propagate: an unusable save dir must surface as an error in the
            // UI, not as a silent "no incoming files".
            return Err(format!(
                "Cannot create save directory '{}': {}",
                save_dir, e
            ));
        }
    }
    // Private staging dir: the CLI drains here first, then each file moves
    // into save_dir with exclusive-create semantics.
    let staging = staging_root().join(format!("taildrop-accept-{:016x}", timestamp_tag()));
    if let Err(e) = std::fs::create_dir_all(&staging) {
        return Err(format!(
            "Cannot create staging directory '{}': {}",
            staging.display(),
            e
        ));
    }
    let drain_result =
        cli_drain_into_staging(save_dir, &staging, platform_label, |args| run_get(args));
    match drain_result {
        Ok(()) => {}
        Err(e) => {
            // TD-01/TD-05: preserve any files the CLI managed to deliver
            // before failing — they are unverified but recoverable; the
            // staging recovery path owns them. No saved receipts for them.
            if count_files_in_dir(&staging) > 0 {
                log::warn!(
                    "{} CLI auto-receive failed with staged file(s) preserved \
                     for recovery in '{}': {}",
                    platform_label,
                    staging.display(),
                    e
                );
            } else {
                let _ = std::fs::remove_dir_all(&staging);
            }
            return Err(e);
        }
    }

    // Move every staged file into the save dir; each landing is a verified
    // completed download → durable receipt with the actual collision-resolved
    // destination. The returned list (frontend "pending" display) contains
    // only these newly landed files.
    let save_dir_path = std::path::Path::new(save_dir);
    let mut landed: Vec<IncomingFile> = Vec::new();
    let mut move_failures: Vec<String> = Vec::new();
    let entries: Vec<std::path::PathBuf> = match std::fs::read_dir(&staging) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .collect(),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!(
                "Cannot list staged files in '{}': {}",
                staging.display(),
                e
            ));
        }
    };
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match move_file_into_dir(&path, save_dir_path, &name) {
            Ok(dest) => {
                // TD05-A: no suppression — each completed landing in this
                // drain is a distinct transfer and gets its own receipt.
                let dest_str = dest.to_string_lossy().to_string();
                crate::receipts::ReceiptStore::record_saved(&name, &dest_str, size, None);
                landed.push(IncomingFile {
                    name,
                    size,
                    peer_name: None,
                });
            }
            Err(e) => move_failures.push(format!("{} ({})", name, e)),
        }
    }
    // TD-01 cleanup gate: remove staging only when provably empty.
    if count_files_in_dir(&staging) == 0 {
        let _ = std::fs::remove_dir_all(&staging);
    } else {
        log::warn!(
            "{} CLI auto-receive: preserving staging '{}' for recovery ({} \
             failed move(s): {})",
            platform_label,
            staging.display(),
            move_failures.len(),
            move_failures.join("; ")
        );
    }
    if !move_failures.is_empty() {
        // Files that moved are recorded above (partial success preserved);
        // the failure still surfaces so the loop's error state can trigger.
        return Err(format!(
            "Failed to move received file(s) into '{}': {}; preserved for \
             recovery in '{}'",
            save_dir_path.display(),
            move_failures.join("; "),
            staging.display()
        ));
    }
    log::debug!(
        "{} CLI auto-receive: landed {} file(s) in '{}'",
        platform_label,
        landed.len(),
        save_dir
    );
    let json = serde_json::to_string(&landed)
        .map_err(|e| format!("Failed to serialize file list: {}", e))?;
    Ok(json.into_bytes())
}

/// Run the CLI drain into `staging`, propagating a non-zero exit as an error
/// (TD-04). Split from `cli_receive_files` so tests can exercise the staging
/// semantics independently.
fn cli_drain_into_staging(
    _save_dir: &str,
    staging: &std::path::Path,
    platform_label: &str,
    run_get: impl FnOnce(&[&str]) -> Result<std::process::Output, String>,
) -> Result<(), String> {
    let staging_str = staging.to_string_lossy().to_string();
    let args: Vec<&str> = vec![
        "file",
        "get",
        "--wait=false",
        "--verbose",
        "--conflict=rename",
        &staging_str,
    ];
    let output = run_get(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    log::debug!(
        "{} CLI auto-receive: exit={} stdout_len={} stderr_len={}",
        platform_label,
        output.status,
        stdout.len(),
        stderr.len()
    );
    // TD-04: a non-zero exit is a receive failure and must surface as an
    // error, not as "no incoming files". Otherwise the receive loop can
    // never reach its consecutive-failure threshold and the UI shows an
    // idle inbox while receives are actually failing.
    if !output.status.success() {
        return Err(format!(
            "tailscale file get failed (exit {}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(())
}

// ============================================================
// Shared accept_file helper for CLI-based platforms (macOS/Windows)
// ============================================================

// ============================================================
// Staging root isolation (RA-01)
// ============================================================
//
// Production code creates staging directories under `std::env::temp_dir()`.
// Tests that exercise staging paths must NOT pollute the shared temp dir —
// leftover `taildrop-accept-*` dirs from interrupted tests become false
// recovery data visible to `scan_staging_dirs()` at app startup.
//
// `staging_root()` returns the base directory for staging.  Production code
// gets `std::env::temp_dir()`; tests override it via `StagingRootGuard`
// which creates an isolated per-test directory and cleans up on drop.

use std::cell::RefCell;

thread_local! {
    static STAGING_ROOT_OVERRIDE: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
}

/// Return the root directory under which `taildrop-accept-*` staging dirs
/// are created and scanned.  Defaults to `std::env::temp_dir()`.
pub(crate) fn staging_root() -> std::path::PathBuf {
    STAGING_ROOT_OVERRIDE.with(|r| {
        r.borrow()
            .as_ref()
            .cloned()
            .unwrap_or_else(std::env::temp_dir)
    })
}

#[cfg(test)]
pub(crate) mod staging_guard {
    use super::STAGING_ROOT_OVERRIDE;
    use std::path::PathBuf;

    /// RAII guard that isolates staging dirs for a single test.
    ///
    /// Creates a unique directory under the system temp dir, points
    /// `staging_root()` at it for the current thread's scope, and
    /// removes the entire tree on drop (including on panic).
    pub(crate) struct StagingRootGuard {
        root: PathBuf,
    }

    impl StagingRootGuard {
        pub(crate) fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "taildrop_test_staging_{}_{:016x}",
                label,
                super::timestamp_tag()
            ));
            std::fs::create_dir_all(&root).unwrap_or_else(|e| {
                panic!(
                    "cannot create isolated staging root '{}': {}",
                    root.display(),
                    e
                )
            });
            STAGING_ROOT_OVERRIDE.with(|r| {
                *r.borrow_mut() = Some(root.clone());
            });
            Self { root }
        }

        /// The isolated staging root for this test.
        #[allow(dead_code)]
        pub(crate) fn root(&self) -> &std::path::Path {
            &self.root
        }
    }

    impl Drop for StagingRootGuard {
        fn drop(&mut self) {
            STAGING_ROOT_OVERRIDE.with(|r| {
                *r.borrow_mut() = None;
            });
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Short, unique timestamp suffix for collision resolution.
///
/// Combines wall-clock milliseconds with a monotonic counter so that rapid
/// successive calls never collide. The previous `nanos as u32` implementation
/// wrapped every ~4.29 seconds, causing silent file overwrites.
fn timestamp_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // ~44 bits of ms (272 years from epoch) + 20 bits of counter (~1M calls/ms)
    (ms << 20) | (n & 0xFFFFF)
}

/// Generate a unique save path to avoid overwriting existing files.
/// e.g. "file.txt" -> "file (1).txt" -> "file (2).txt"; after 999 conflicts,
/// a unique timestamp suffix is appended to guarantee uniqueness.
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
    // After 999 conflicts, append a unique timestamp suffix to guarantee
    // uniqueness instead of silently overwriting (which would lose data).
    let fallback_name = match ext {
        Some(e) => format!("{}-{:016x}.{}", stem, timestamp_tag(), e),
        None => format!("{}-{:016x}", stem, timestamp_tag()),
    };
    dir.join(fallback_name)
}

// ============================================================
// TD-07: cancellable child-process execution
// ============================================================

/// Run a child process to completion on the blocking pool with a hard
/// wall-clock cap, TERMINATING and REAPING the child when the cap expires —
/// unlike `tokio::time::timeout(.., spawn_blocking(.. Command::output()))`,
/// which drops the future on elapse and orphans both the blocked thread and
/// the still-running process (TD-07: "timeouts leave operations running").
///
/// Kill-then-wait: `kill()` is async-signal-safe and always issued first;
/// the subsequent `wait()` reaps the zombie so no resources leak. If the
/// child exited between cap-expiry and kill, `kill` reports `InvalidInput`
/// / "no such process", which is success for our purposes.
// Used in production only on macOS/Windows (CLI-based platforms); on Linux the
// socket API is used instead, so it is dead in the Linux lib build but still
// exercised by the shared tests below.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn run_command_with_cap(
    mut command: std::process::Command,
    cap: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::io::Read;

    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        format!(
            "Failed to run {}: {}",
            command.get_program().to_string_lossy(),
            e
        )
    })?;

    // Reopen-finding fix: drain both pipes on dedicated threads WHILE the
    // child runs. Reading only after exit deadlocks — a full pipe (64 KiB on
    // macOS) blocks the child's writes, so it can never exit and gets killed
    // as "timed out" while perfectly healthy. Each drain thread joins when
    // the child exits and the pipe write end closes.
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = std::time::Instant::now() + cap;
    // wait_timeout-style poll: std has no timed wait, so poll at a modest
    // interval — the cap granularity is seconds, so 50ms is negligible.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    log::warn!(
                        "TD-07: command {:?} exceeded {:?}s cap — terminating",
                        command.get_program(),
                        cap
                    );
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                                          // Drain threads see EOF after the kill closes the pipes;
                                          // join so no thread leaks.
                    let _ = stdout_handle.map(|h| h.join());
                    let _ = stderr_handle.map(|h| h.join());
                    return Err(format!(
                        "command '{}' timed out after {:?} (process terminated)",
                        command.get_program().to_string_lossy(),
                        cap
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Failed to wait for child: {}", e)),
        }
    };
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// RFC 3986 percent-encoding (encode all non-unreserved characters).
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

// ============================================================
// HTTP download framing (TD-02: Content-Length enforcement)
// ============================================================

/// Parse the `Content-Length` header from an HTTP response header block
/// (including the status line; header names matched case-insensitively).
///
/// The Tailscale daemon always advertises `Content-Length` on file downloads
/// (localapi `serveFiles` sets it from the file size). A missing, negative,
/// or non-numeric value is therefore an error: falling back to
/// trust-connection-close framing when the header is absent would silently
/// re-admit truncated downloads (TD-02) whenever we are not talking to the
/// daemon we think we are.
fn parse_content_length(headers: &str) -> Result<u64, String> {
    for line in headers.lines().skip(1) {
        let mut split = line.splitn(2, ':');
        let name = split.next().unwrap_or("").trim();
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = split.next().unwrap_or("").trim();
        return value
            .parse::<u64>()
            .map_err(|_| format!("Invalid Content-Length header: {:?}", value));
    }
    Err(
        "Response is missing Content-Length; refusing to trust connection-close framing for a file download"
            .to_string(),
    )
}

/// Verify that a completed HTTP download wrote exactly the number of bytes
/// the response advertised. A short body (connection closed early) is the
/// TD-02 truncated-download case and MUST be treated as an error *before*
/// the caller deletes the file from the daemon inbox. An over-long body
/// (trailing garbage) fails the same equality.
fn validate_received_length(advertised: u64, written: u64) -> Result<(), String> {
    if written == advertised {
        Ok(())
    } else {
        Err(format!(
            "Truncated download: received {} of {} advertised bytes before the connection closed",
            written, advertised
        ))
    }
}

/// Read an HTTP/1.0 response from `reader` and stream its body to `writer`,
/// enforcing the advertised `Content-Length` (TD-02).
///
/// The caller has already written the GET request. Non-200 statuses drain
/// the (bounded) error body into the returned message. Header reads are
/// bounded by a 30s timeout; body reads are intentionally unbounded here —
/// the caller's outer accept timeout cancels the whole future if the daemon
/// stalls mid-transfer (matching the pre-TD-02 upload-path semantics).
///
/// Wired into the Linux socket download path; also compiled and unit-tested
/// on macOS so the framing logic is exercised on every Unix CI target.
#[cfg(unix)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
async fn read_http_download_async<R, W>(reader: &mut R, writer: &mut W) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut header_buf: Vec<u8> = Vec::new();
    let mut temp_buf = [0u8; 4096];
    let header_end = loop {
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read(&mut temp_buf),
        )
        .await
        .map_err(|_| "Timeout reading response headers".to_string())?
        .map_err(|e| format!("Failed to read response: {}", e))?;
        if n == 0 {
            return Err("Connection closed before headers received".to_string());
        }
        header_buf.extend_from_slice(&temp_buf[..n]);
        if let Some(pos) = header_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if header_buf.len() > 65536 {
            return Err("Response headers too large".to_string());
        }
    };

    let headers = String::from_utf8_lossy(&header_buf[..header_end]).to_string();
    let status_line = headers.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status_code != 200 {
        // Drain the error body until connection close (HTTP/1.0) so
        // diagnostics aren't truncated by the header read buffer, capped to
        // avoid unbounded memory growth on a misbehaving server.
        let mut err_body = header_buf[header_end + 4..].to_vec();
        loop {
            let n = reader.read(&mut temp_buf).await;
            let n = match n {
                Ok(n) => n,
                Err(e) => return Err(format!("Failed to read error body: {}", e)),
            };
            if n == 0 {
                break;
            }
            err_body.extend_from_slice(&temp_buf[..n]);
            if err_body.len() > 65536 {
                break;
            }
        }
        return Err(format!(
            "Tailscale API error ({}): {}",
            status_code,
            String::from_utf8_lossy(&err_body)
        ));
    }

    let advertised = parse_content_length(&headers)?;

    // Body bytes that arrived in the same read as the header terminator are
    // the start of the body, not headers — write them first.
    let mut written: u64 = 0;
    let body_start = header_end + 4;
    if body_start < header_buf.len() {
        let prefix: &[u8] = &header_buf[body_start..];
        writer
            .write_all(prefix)
            .await
            .map_err(|e| format!("Failed to write to file: {}", e))?;
        written += prefix.len() as u64;
    }
    loop {
        let n = reader
            .read(&mut temp_buf)
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&temp_buf[..n])
            .await
            .map_err(|e| format!("Failed to write to file: {}", e))?;
        written += n as u64;
    }

    validate_received_length(advertised, written)
}

/// Synchronous counterpart of [`read_http_download_async`] used by the macOS
/// platform module (its accept path runs inside `spawn_blocking`). Same
/// framing and the same TD-02 Content-Length enforcement; response timeouts
/// come from the caller's stream-level read timeout.
///
/// Wired into the macOS socket download path; also compiled and unit-tested
/// on Linux so the framing logic is exercised on every Unix CI target.
#[cfg(unix)]
#[cfg_attr(all(unix, not(target_os = "macos")), allow(dead_code))]
fn read_http_download_sync<R: std::io::Read>(
    reader: &mut R,
    file: &mut std::fs::File,
) -> Result<(), String> {
    // `Read` is in the trait bound; only `Write` is needed by the body.
    use std::io::Write;

    let mut header_buf: Vec<u8> = Vec::with_capacity(8192);
    let mut temp_buf = [0u8; 8192];
    let header_end = loop {
        let n = reader
            .read(&mut temp_buf)
            .map_err(|e| format!("read headers: {}", e))?;
        if n == 0 {
            return Err("Connection closed before headers received".to_string());
        }
        header_buf.extend_from_slice(&temp_buf[..n]);
        if let Some(pos) = header_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if header_buf.len() > 65536 {
            return Err("Response headers too large".to_string());
        }
    };

    let headers = String::from_utf8_lossy(&header_buf[..header_end]).to_string();
    let status_line = headers.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status_code != 200 {
        // Drain the rest of the error body until connection close so the
        // diagnostic isn't truncated by the 8 KB header buffer, capped to
        // avoid unbounded memory growth on a misbehaving server.
        let mut err_body = header_buf[header_end + 4..].to_vec();
        loop {
            let n = reader
                .read(&mut temp_buf)
                .map_err(|e| format!("read error body: {}", e))?;
            if n == 0 {
                break;
            }
            err_body.extend_from_slice(&temp_buf[..n]);
            if err_body.len() > 65536 {
                break;
            }
        }
        return Err(format!(
            "Tailscale API error ({}): {}",
            status_code,
            String::from_utf8_lossy(&err_body)
        ));
    }

    let advertised = parse_content_length(&headers)?;

    let mut written: u64 = 0;
    let body_start = header_end + 4;
    if body_start < header_buf.len() {
        let prefix: &[u8] = &header_buf[body_start..];
        file.write_all(prefix)
            .map_err(|e| format!("write body to file: {}", e))?;
        written += prefix.len() as u64;
    }
    loop {
        let n = reader
            .read(&mut temp_buf)
            .map_err(|e| format!("read body: {}", e))?;
        if n == 0 {
            break;
        }
        file.write_all(&temp_buf[..n])
            .map_err(|e| format!("write body to file: {}", e))?;
        written += n as u64;
    }

    // Flush to disk before reporting success so the caller's path points at
    // durable content (and, with TD-02, at *complete* content).
    file.sync_all().map_err(|e| format!("sync file: {}", e))?;

    validate_received_length(advertised, written)
}

// ============================================================
// Linux implementation — hyperlocal (Unix socket)
// ============================================================

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{unique_save_path, url_encode};
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
            .header("Host", super::LOCALAPI_HOST)
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
            .header("Host", super::LOCALAPI_HOST)
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
    /// Streams file from disk in 8KB chunks to avoid loading entire file in memory.
    /// Uses HTTP/1.1 with Content-Length and Connection: close so the daemon
    /// knows the exact body size up front (Content-Length requires HTTP/1.1).
    /// If the daemon rejects the transfer (peer offline, not found, etc.) it may
    /// send an HTTP error response and close the connection while we are still
    /// streaming the body, causing a broken-pipe write error. We catch that and
    /// attempt to read the daemon's error response before reporting.
    async fn stream_file_to_socket(api_path: &str, file_path: &str) -> Result<Vec<u8>, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|e| format!("Failed to stat file '{}': {}", file_path, e))?;
        let file_size = metadata.len();

        let mut file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("Failed to open file '{}': {}", file_path, e))?;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

        // Timeout: 60s base + 60s per MB, capped at 600s. Saturating math so
        // the per-MB term can't overflow before the cap applies (the cap used
        // to bind only after the multiply).
        let timeout_secs = 60u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(540);
        let timeout = std::time::Duration::from_secs(timeout_secs);

        // Write HTTP/1.1 request with Content-Length
        let request = format!(
            "PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            api_path,
            super::LOCALAPI_HOST,
            file_size
        );
        tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
            .await
            .map_err(|_| "Timeout writing request headers to Tailscale daemon".to_string())?
            .map_err(|e| format!("Failed to write request: {}", e))?;

        // Stream file in 8KB chunks from disk to socket
        let mut buf = [0u8; 8192];
        loop {
            let n = tokio::time::timeout(timeout, file.read(&mut buf))
                .await
                .map_err(|_| "Timeout reading file data from disk".to_string())?
                .map_err(|e| format!("Failed to read file: {}", e))?;
            if n == 0 {
                break;
            }
            match tokio::time::timeout(timeout, stream.write_all(&buf[..n])).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // Write failed (likely broken pipe / connection reset). The
                    // Tailscale daemon may have rejected the transfer and sent
                    // an HTTP error response before closing the connection.
                    return Err(read_daemon_error(&mut stream, &e).await);
                }
                Err(_) => {
                    return Err("Timeout writing file data to Tailscale daemon".to_string());
                }
            }
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

        // Parse HTTP response. We sent Connection: close, so read_to_end reads
        // until the daemon closes the connection, yielding the full body. The
        // Tailscale localapi returns small JSON responses for PUT requests and
        // does not use chunked Transfer-Encoding, so direct body parsing works.
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

    /// Attempts to read and parse an HTTP error response from the Tailscale
    /// daemon after a write failure (broken pipe / connection reset) during
    /// file upload. The daemon often sends an HTTP error response (e.g. 400
    /// with a JSON body) before closing the connection; this surfaces that
    /// message instead of the opaque "Broken pipe (os error 32)".
    async fn read_daemon_error(
        stream: &mut tokio::net::UnixStream,
        write_err: &std::io::Error,
    ) -> String {
        use tokio::io::AsyncReadExt;

        let write_err_str = write_err.to_string();
        let is_broken_pipe = write_err_str.contains("Broken pipe")
            || write_err_str.contains("Connection reset")
            || write_err_str.contains("Connection reset by peer");

        // Try to read whatever the daemon sent before closing (short timeout —
        // the daemon has likely already closed the connection, so this returns
        // immediately in practice).
        let mut error_response = Vec::new();
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_end(&mut error_response),
        )
        .await;

        match read_result {
            Ok(Ok(_)) if !error_response.is_empty() => {
                parse_daemon_http_error(&error_response, &write_err_str, is_broken_pipe)
            }
            _ => {
                if is_broken_pipe {
                    "Tailscale daemon closed connection during file transfer \
                     (broken pipe). The peer may be offline or not accepting files."
                        .to_string()
                } else {
                    format!(
                        "Failed to write file data: {} \
                         (the peer may be offline or not accepting files)",
                        write_err_str
                    )
                }
            }
        }
    }

    /// Parses the daemon's raw HTTP error response into a human-readable error
    /// string. Extracts the status code and body when possible; falls back to
    /// including the raw response text if parsing fails.
    fn parse_daemon_http_error(response: &[u8], write_err: &str, is_broken_pipe: bool) -> String {
        let text = String::from_utf8_lossy(response);

        // Split headers from body at the first "\r\n\r\n".
        let (headers, body) = match text.find("\r\n\r\n") {
            Some(idx) => (&text[..idx], &text[idx + 4..]),
            None => (text.as_ref(), ""),
        };

        // Extract the HTTP status code from the status line, e.g.
        // "HTTP/1.1 400 Bad Request".
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: Option<u16> = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok());

        let body_trimmed = body.trim();

        // Linux socket-permission case: the localapi socket requires root or
        // operator privileges. The daemon returns 403 "file access denied" —
        // which misleadingly sounds like a receiver-side issue. Rewrite it to
        // the actionable guidance the Tailscale CLI itself prints.
        if status_code == Some(403) && body_trimmed.to_ascii_lowercase().contains("access denied") {
            return "Access denied to the Tailscale socket. \
                    Run this once: sudo tailscale set --operator=$USER"
                .to_string();
        }

        match status_code {
            Some(code) if code != 200 => {
                if !body_trimmed.is_empty() {
                    format!(
                        "Tailscale daemon rejected file transfer (HTTP {}): {}",
                        code, body_trimmed
                    )
                } else {
                    format!("Tailscale daemon rejected file transfer (HTTP {})", code)
                }
            }
            _ => {
                // Could not parse a useful status code; include the raw
                // daemon response so the user still gets a clue.
                if is_broken_pipe {
                    format!(
                        "Tailscale daemon closed connection during file transfer \
                         (broken pipe). Daemon response: {}",
                        text.trim()
                    )
                } else {
                    format!(
                        "Failed to write file data: {} | Daemon response: {}",
                        write_err,
                        text.trim()
                    )
                }
            }
        }
    }

    /// Async twin of the shared `reserve_unique_file`: reserves a unique
    /// output path with an exclusive create, retrying with the next unique
    /// suffix when the name is taken. Never truncates an existing file.
    async fn reserve_unique_file_async(
        dir: &std::path::Path,
        name: &str,
    ) -> Result<(tokio::fs::File, std::path::PathBuf), String> {
        let mut candidate = dir.join(name);
        loop {
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .await
            {
                Ok(file) => return Ok((file, candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    candidate = unique_save_path(dir, name);
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to create file '{}': {}",
                        candidate.display(),
                        e
                    ))
                }
            }
        }
    }

    /// Stream a GET response from the Tailscale localapi directly to a file on disk.
    /// Avoids buffering the entire response in memory (fixes OOM for large incoming files).
    ///
    /// Uses HTTP/1.0 with `Connection: close`. HTTP/1.0 prevents the daemon from
    /// using chunked Transfer-Encoding, so the body is delivered as raw bytes
    /// terminated by connection close — no chunked-framing decoder is needed. The
    /// response headers are read first (up to the `\r\n\r\n` boundary), then the
    /// body is streamed to disk in fixed-size chunks. This is consistent with the
    /// macOS `try_socket_get`/`try_socket_delete` helpers. (The Linux upload path
    /// `stream_file_to_socket` uses HTTP/1.1 instead, because PUT uploads require
    /// `Content-Length`, which needs HTTP/1.1.)
    ///
    /// `file` is the caller's exclusively-created destination (see
    /// `reserve_unique_file_async`); on failure the caller removes the partial.
    async fn stream_get_to_file(api_path: &str, file: &mut tokio::fs::File) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .await
            .map_err(|e| format!("Failed to connect to Tailscale daemon: {}", e))?;

        let request = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            api_path,
            super::LOCALAPI_HOST
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.write_all(request.as_bytes()),
        )
        .await
        .map_err(|_| "Timeout writing GET request to Tailscale daemon".to_string())?
        .map_err(|e| format!("Failed to write request: {}", e))?;

        // Shared framing enforces the advertised Content-Length (TD-02): a
        // connection that closes before the full body arrives is an error,
        // so the caller never deletes the inbox entry for a truncated file
        // nor reports it as successfully received.
        super::read_http_download_async(&mut stream, file).await?;

        let _ = stream.shutdown().await;
        Ok(())
    }

    /// Streams file from disk to socket instead of loading entire file into memory.
    /// Uses the peer's stable node ID (peer_id) for the localapi path.
    pub async fn send_file(
        peer_id: &str,
        _peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|e| format!("Failed to stat file '{}': {}", file_path, e))?;
        if !metadata.is_file() {
            return Err(format!("'{}' is not a regular file", file_path));
        }
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let api_path = format!(
            "/localapi/v0/file-put/{}/{}",
            url_encode(peer_id),
            url_encode(filename)
        );
        stream_file_to_socket(&api_path, file_path).await?;
        log::debug!("Sent {} to {}", filename, peer_id);
        Ok(format!("Sent {} to {}", filename, peer_id))
    }

    pub async fn get_incoming_files(_save_dir: &str) -> Result<Vec<u8>, String> {
        match get_request("/localapi/v0/files/").await {
            Ok(data) => {
                log::debug!("Linux: incoming files listing OK ({} bytes)", data.len());
                Ok(data)
            }
            Err(e) => {
                log::debug!("Linux: incoming files listing failed: {}", e);
                Err(e)
            }
        }
    }

    /// Accept an incoming file. Streams response directly to disk instead of
    /// buffering in memory (fixes OOM for large incoming files).
    /// Sanitizes filename to prevent path traversal.
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        // Single outer timeout bounds the whole operation (matches the upload
        // path's adaptive timeout and macOS/Windows' 120s wrapper).
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let safe_name = std::path::Path::new(&name)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| "Invalid filename".to_string())?;
            let api_path = format!("/localapi/v0/files/{}", url_encode(&name));
            let dir_path = std::path::Path::new(&save_dir);
            tokio::fs::create_dir_all(dir_path).await.map_err(|e| {
                format!(
                    "Cannot create save directory '{}': {}",
                    dir_path.display(),
                    e
                )
            })?;
            let (mut file, save_path) = reserve_unique_file_async(dir_path, safe_name).await?;
            if let Err(e) = stream_get_to_file(&api_path, &mut file).await {
                // Remove the partial download so a half-written file doesn't
                // linger under the reserved name.
                let _ = tokio::fs::remove_file(&save_path).await;
                // TD-05: failed accept must leave a durable receipt — the
                // socket path never drained the inbox (DELETE only happens
                // after success), so an inbox retry is valid recovery.
                crate::receipts::ReceiptStore::record_failed(
                    safe_name,
                    &e,
                    crate::receipts::ReceiptStore::classify_recovery(&e),
                );
                return Err(e);
            }

            // Delete from pending after successful download. Surface failures
            // so stale entries don't silently linger in the pending list.
            let delete_path = format!("/localapi/v0/files/{}", url_encode(&name));
            if let Err(e) = delete_request(&delete_path).await {
                log::warn!(
                    "Failed to delete pending file '{}' from Tailscale: {}",
                    name,
                    e
                );
            }

            log::debug!("Accepted file '{}' to '{}'", name, save_path.display());
            let size = tokio::fs::metadata(&save_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            // TD-05: durable receipt with the actual landing path.
            crate::receipts::ReceiptStore::record_saved(
                safe_name,
                &save_path.to_string_lossy(),
                size,
                None,
            );
            Ok(save_path.to_string_lossy().to_string())
        })
        .await
        .map_err(|_| "accept_file timed out".to_string())?
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

    /// Run the tailscale CLI by exec'ing the binary with an argument vector —
    /// no shell involved. `Command::args` passes each argument verbatim, so
    /// paths and peer names with spaces, quotes, or shell metacharacters need
    /// no escaping (arguments containing NUL bytes are rejected by the OS as
    /// `InvalidInput`). This matches the Windows implementation and removes
    /// the shell-injection surface the previous `/bin/sh -c` wrapper carried.
    /// (The child inherits the same environment either way — the shell
    /// intermediary added no launchd/XPC-relevant state.)
    fn tailscale_cmd_capped(
        args: &[&str],
        cap: std::time::Duration,
    ) -> Result<std::process::Output, String> {
        let binary = find_tailscale().unwrap_or("tailscale");
        log::debug!("macOS exec (capped {:?}): {} {:?}", cap, binary, args);
        let mut cmd = Command::new(binary);
        cmd.args(args);
        super::run_command_with_cap(cmd, cap)
    }

    const SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

    /// Try an HTTP/1.0 GET via the Tailscale Unix socket.
    /// Works when the socket is accessible (Homebrew/open-source installs).
    /// Fails gracefully for App Store installs with restricted permissions.
    /// Uses HTTP/1.0 which guarantees non-chunked responses and connection close,
    /// so read_to_end will read the complete response body without needing to
    /// parse chunked Transfer-Encoding.
    fn try_socket_get(path: &str) -> Result<Vec<u8>, super::SocketGetError> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("connect: {}", e)))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| super::SocketGetError::Other(format!("timeout: {}", e)))?;

        // Use HTTP/1.0 to guarantee non-chunked response and connection close
        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| super::SocketGetError::Other(format!("read: {}", e)))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| super::SocketGetError::Other("Invalid HTTP response".to_string()))?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            return Err(super::SocketGetError::Other(format!(
                "HTTP error: {}",
                status_line
            )));
        }

        Ok(response[header_end + 4..].to_vec())
    }

    /// CLI auto-receive fallback for when the Unix socket is unavailable (the
    /// macOS GUI install case). Delegates to the shared `cli_receive_files`
    /// helper with the macOS-specific `tailscale_cmd_capped` invocation —
    /// capped so a hung CLI child is terminated and reaped (TD-07) instead of
    /// surviving the outer `get_incoming_files` timeout as an orphaned
    /// process that keeps its staging dir alive indefinitely. Matches the
    /// Windows fallback's 110s cap.
    fn try_cli_receive_files(save_dir: &str) -> Result<Vec<u8>, String> {
        super::cli_receive_files(save_dir, "macOS", |args| {
            tailscale_cmd_capped(args, std::time::Duration::from_secs(110))
                .map_err(|e| format!("Failed to run tailscale file get: {}", e))
        })
    }

    // SocketGetError is defined at crate root (super::SocketGetError).
    // It distinguishes Connect (socket unavailable → CLI fallback safe)
    // from Other (HTTP error → must propagate).

    /// Stream a GET response from the Tailscale Unix socket directly to disk.
    ///
    /// Uses HTTP/1.0 (non-chunked, connection close at end of body). The
    /// response headers are read first in small chunks until the `\r\n\r\n`
    /// boundary is found; any body bytes that arrived in the same read buffer
    /// are flushed to the file before continuing. The remaining body is then
    /// streamed to disk in 8 KB chunks, so files much larger than memory can
    /// be downloaded without OOM risk — mirroring the Linux
    /// [`stream_get_to_file`](super::stream_get_to_file) design.
    ///
    /// Returns [`SocketGetError::Connect`] only when `UnixStream::connect`
    /// fails (the App Store install case). All other failures — including
    /// HTTP 4xx/5xx responses — are returned as [`SocketGetError::Other`] so
    /// callers can propagate them instead of silently falling back to the CLI.
    ///
    /// `file` is the caller's exclusively-reserved destination (see the
    /// shared `reserve_unique_file`); on failure the caller removes the
    /// partial file — including the empty reservation when falling back to
    /// the CLI, so the CLI path gets a clean shot at the original name.
    fn try_socket_get_to_file(
        path: &str,
        file: &mut std::fs::File,
    ) -> Result<(), super::SocketGetError> {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("connect: {}", e)))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|e| super::SocketGetError::Other(format!("timeout: {}", e)))?;

        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        // Shared framing enforces the advertised Content-Length (TD-02): a
        // connection that closes before the full body arrives is an error,
        // so the caller never deletes the inbox entry for a truncated file
        // nor reports it as successfully received. The sync variant also
        // fsyncs before reporting success.
        super::read_http_download_sync(&mut stream, file).map_err(super::SocketGetError::Other)
    }

    /// Best-effort DELETE of a pending file via the Tailscale Unix socket.
    ///
    /// Returns `Ok(())` only when the daemon responds with HTTP 200. Any other
    /// status (or a transport error) is surfaced as `Err` so the caller's
    /// `log::warn!` fires — matching the Linux [`delete_request`](super::delete_request)
    /// behaviour where HTTP errors are propagated, not silently discarded.
    fn try_socket_delete(path: &str) -> Result<(), String> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|e| format!("connect: {}", e))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| format!("timeout: {}", e))?;

        let req = format!(
            "DELETE {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {}", e))?;

        // DELETE responses are tiny (a short status line at most), so reading
        // the full response into memory is fine. HTTP/1.0 closes the
        // connection at end of body, so read_to_end captures everything
        // without needing to parse chunked encoding.
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| format!("read: {}", e))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "Invalid HTTP response from DELETE".to_string())?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            let body = String::from_utf8_lossy(&response[header_end + 4..]);
            return Err(format!(
                "Tailscale API error on DELETE ({}): {}",
                status_code, body
            ));
        }
        Ok(())
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(|| {
                let binary_path = find_tailscale().unwrap_or("tailscale");
                log::debug!("macOS fetch_status_json: binary={}", binary_path);

                // TD-07: capped — a hung CLI is terminated and reaped, not
                // orphaned when the outer timeout drops this future.
                let output = tailscale_cmd_capped(
                    &["status", "--json"],
                    std::time::Duration::from_secs(110),
                )
                .map_err(|e| {
                    format!(
                        "Could not run tailscale CLI [tried: {}]: {}",
                        binary_path, e
                    )
                })?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                log::debug!(
                    "macOS CLI result: exit={} stdout_len={} stderr_len={}",
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
            }),
        )
        .await
        .map_err(|_| "fetch_status_json timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Send file to peer using tailscale CLI. Non-blocking via spawn_blocking.
    pub async fn send_file(
        _peer_id: &str,
        peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s (saturating
        // math so the multiply can't overflow before the cap applies).
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = 120u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(480);
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || {
                // TD-07: cap sits just under the outer adaptive timeout so
                // the child is terminated here, not orphaned by the drop.
                let output = tailscale_cmd_capped(
                    &["file", "cp", &file_path, &format!("{}:", peer_name)],
                    std::time::Duration::from_secs(timeout_secs.saturating_sub(5).max(10)),
                )
                .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale file cp failed: {}", stderr));
                }
                Ok(format!("Sent file to {}", peer_name))
            }),
        )
        .await
        .map_err(|_| "send_file timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    pub async fn get_incoming_files(save_dir: &str) -> Result<Vec<u8>, String> {
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || match try_socket_get("/localapi/v0/files/") {
                Ok(data) => {
                    log::debug!("macOS: socket file listing OK ({} bytes)", data.len());
                    Ok(data)
                }
                Err(super::SocketGetError::Connect(e)) => {
                    log::debug!("macOS: socket file listing failed (connect): {}", e);
                    log::debug!("macOS: falling back to CLI auto-receive to '{}'", save_dir);
                    try_cli_receive_files(&save_dir)
                }
                Err(super::SocketGetError::Other(e)) => {
                    // TD-04: an HTTP/transport failure on the listing call is
                    // a receive failure, not an empty inbox. Propagate it so
                    // the receive loop's consecutive-failure counter (and the
                    // UI error banner) can trigger; swallowing it here made a
                    // broken daemon indistinguishable from an idle one.
                    log::debug!("macOS: socket file listing failed: {}", e);
                    Err(e)
                }
            }),
        )
        .await
        .map_err(|_| "get_incoming_files timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Accept an incoming file. Tries the Unix socket first (streams large
    /// files efficiently into an exclusively-reserved path), then falls back
    /// to the `tailscale file get` CLI — but **only** when the socket itself
    /// is unavailable (the App Store install case). HTTP-level errors from
    /// the daemon (404, 5xx, …) are propagated to the caller rather than
    /// masked by the CLI, which would otherwise download *every* pending
    /// file into `save_dir`. Uses the shared helpers with path traversal
    /// sanitization and staging-directory semantics for the CLI path.
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || {
                let safe_name = std::path::Path::new(&name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| "Invalid filename".to_string())?;
                let dir_path = std::path::Path::new(&save_dir);
                std::fs::create_dir_all(dir_path).map_err(|e| {
                    format!(
                        "Cannot create save directory '{}': {}",
                        dir_path.display(),
                        e
                    )
                })?;
                let api_path = format!("/localapi/v0/files/{}", super::url_encode(&name));
                let (mut file, save_path) = super::reserve_unique_file(dir_path, safe_name)?;

                // Try the socket first (streams large files without buffering).
                match try_socket_get_to_file(&api_path, &mut file) {
                    Ok(()) => {
                        // Best-effort delete of the pending file from the daemon.
                        let delete_path =
                            format!("/localapi/v0/files/{}", super::url_encode(&name));
                        if let Err(e) = try_socket_delete(&delete_path) {
                            log::warn!(
                                "Failed to delete pending file '{}' from Tailscale: {}",
                                name,
                                e
                            );
                        }
                        // TD-05: durable receipt with the actual landing path
                        // (the socket path is the common macOS case — this
                        // receipt was missing, so most macOS users never saw
                        // "Saved / Show in folder").
                        let size = std::fs::metadata(&save_path).map(|m| m.len()).unwrap_or(0);
                        crate::receipts::ReceiptStore::record_saved(
                            safe_name,
                            &save_path.to_string_lossy(),
                            size,
                            None,
                        );
                        Ok(save_path.to_string_lossy().to_string())
                    }
                    Err(super::SocketGetError::Connect(socket_err)) => {
                        // The Unix socket is missing/inaccessible (e.g. App
                        // Store install) — fall back to the `tailscale file
                        // get` CLI. Remove our empty reservation first so the
                        // CLI path can claim the original name.
                        let _ = std::fs::remove_file(&save_path);
                        log::debug!(
                            "macOS: socket unavailable ({}), falling back to CLI",
                            socket_err
                        );
                        // TD05-A: acknowledgement fast path — if this exact
                        // landing is already recorded (the auto-receive poll
                        // drained it), return it WITHOUT invoking the CLI:
                        // acknowledging must not re-drain the inbox.
                        if let Some(recorded) =
                            super::acknowledge_already_received(&name, &save_dir)
                        {
                            return Ok(recorded);
                        }
                        // accept_file_inner records exactly one receipt for
                        // the requested file AND every landed collateral file
                        // (single authoritative recording point — TD05-A).
                        super::accept_file_inner(
                            &name,
                            &save_dir,
                            |staging| {
                                // --wait=false: don't block if the inbox is empty
                                // (the file may have already been consumed by the
                                // auto-receive poll on macOS).
                                // TD-07: capped under the 120s outer wrapper.
                                let output = tailscale_cmd_capped(
                                    &["file", "get", "--wait=false", &staging.to_string_lossy()],
                                    std::time::Duration::from_secs(110),
                                )
                                .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
                                if !output.status.success() {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    return Err(format!("tailscale file get failed: {}", stderr));
                                }
                                Ok(())
                            },
                            false,
                        )
                        .inspect_err(|e| {
                            // Failure receipts stay here (one recording point
                            // per outcome; the helper records successes).
                            crate::receipts::ReceiptStore::record_failed(
                                safe_name,
                                e,
                                crate::receipts::ReceiptStore::classify_recovery(e),
                            );
                        })
                    }
                    Err(super::SocketGetError::Other(http_err)) => {
                        // The socket connected but the request failed (HTTP
                        // 4xx/5xx, transport failure mid-response, disk write
                        // error, …). Remove the partial download and propagate
                        // the real error instead of falling back to the CLI —
                        // otherwise a transient daemon error would cause
                        // `tailscale file get` to download every pending file
                        // into save_dir.
                        let _ = std::fs::remove_file(&save_path);
                        log::debug!(
                            "macOS: socket accept failed with HTTP/transport error ({}), \
                             not falling back to CLI",
                            http_err
                        );
                        // TD-05: failed accept leaves a durable receipt; the
                        // inbox entry survives (no DELETE ran), so retry from
                        // inbox is valid recovery.
                        crate::receipts::ReceiptStore::record_failed(
                            safe_name,
                            &http_err,
                            crate::receipts::ReceiptStore::classify_recovery(&http_err),
                        );
                        Err(http_err)
                    }
                }
            }),
        )
        .await
        .map_err(|_| "accept_file timed out".to_string())?
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

    /// TD-07: run the CLI with terminate-and-reap on cap expiry. CREATE_NO_WINDOW
    /// is preserved; stdout/stderr are piped by the runner.
    fn tailscale_cmd_capped(
        args: &[&str],
        cap: std::time::Duration,
    ) -> Result<std::process::Output, String> {
        let mut cmd = tailscale_cmd();
        cmd.args(args);
        super::run_command_with_cap(cmd, cap)
    }

    /// CLI auto-receive fallback for when the named pipe is unavailable.
    /// Delegates to the shared `cli_receive_files` helper with the
    /// Windows-specific `tailscale_cmd` invocation.
    fn try_cli_receive_files(save_dir: &str) -> Result<Vec<u8>, String> {
        super::cli_receive_files(save_dir, "Windows", |args| {
            // TD-07: capped under the 120s outer wrapper.
            tailscale_cmd_capped(args, std::time::Duration::from_secs(110))
        })
    }

    pub async fn fetch_status_json() -> Result<Vec<u8>, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(|| {
                // TD-07: capped — hung CLI is terminated and reaped.
                let output =
                    tailscale_cmd_capped(&["status", "--json"], std::time::Duration::from_secs(110))
                        .map_err(|e| {
                            format!(
                                "Could not run tailscale CLI. Make sure Tailscale is installed and in your PATH: {}",
                                e
                            )
                        })?;
                log::debug!(
                    "Windows CLI result: exit={} stdout_len={} stderr_len={}",
                    output.status,
                    output.stdout.len(),
                    output.stderr.len()
                );
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale status failed: {}", stderr));
                }
                Ok(output.stdout)
            }),
        )
        .await
        .map_err(|_| "fetch_status_json timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Send file to peer using tailscale CLI. Non-blocking via spawn_blocking.
    pub async fn send_file(
        _peer_id: &str,
        peer_name: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let peer_name = peer_name.to_string();
        let file_path = file_path.to_string();
        // Adaptive timeout: 120s base + 60s per MB, capped at 600s (saturating
        // math so the multiply can't overflow before the cap applies).
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let timeout_secs = 120u64 + (file_size / (1024 * 1024)).saturating_mul(60).min(480);
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || {
                // TD-07: cap sits just under the outer adaptive timeout.
                let output = tailscale_cmd_capped(
                    &["file", "cp", &file_path, &format!("{}:", peer_name)],
                    std::time::Duration::from_secs(timeout_secs.saturating_sub(5).max(10)),
                )
                .map_err(|e| format!("Failed to run tailscale file cp: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("tailscale file cp failed: {}", stderr));
                }
                let filename = std::path::Path::new(&file_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                log::debug!("Sent {} to {}", filename, peer_name);
                Ok(format!("Sent file to {}", peer_name))
            }),
        )
        .await
        .map_err(|_| "send_file timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Named pipe path for the Tailscale daemon's local API on Windows.
    const PIPE_PATH: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";

    /// Try an HTTP/1.0 GET via the Tailscale named pipe.
    /// Uses HTTP/1.0 which guarantees non-chunked responses and connection close,
    /// so read_to_end will read the complete response body without needing to
    /// parse chunked Transfer-Encoding.
    fn try_pipe_get(path: &str) -> Result<Vec<u8>, super::SocketGetError> {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};

        let mut pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_PATH)
            .map_err(|e| super::SocketGetError::Connect(format!("open pipe: {}", e)))?;

        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\n\r\n",
            path,
            super::LOCALAPI_HOST
        );
        pipe.write_all(req.as_bytes())
            .map_err(|e| super::SocketGetError::Other(format!("write: {}", e)))?;

        let mut response = Vec::new();
        pipe.read_to_end(&mut response)
            .map_err(|e| super::SocketGetError::Other(format!("read: {}", e)))?;

        let header_end = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| super::SocketGetError::Other("Invalid HTTP response".to_string()))?;

        let headers = String::from_utf8_lossy(&response[..header_end]);
        let status_line = headers.lines().next().unwrap_or("");
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status_code != 200 {
            // 401/403 means the pipe connected but the daemon rejected our
            // request due to permissions (non-admin process). The CLI bypasses
            // this because tailscale.exe has its own auth, so treat these as
            // Connect errors to trigger the CLI fallback.
            let err_type = if status_code == 401 || status_code == 403 {
                super::SocketGetError::Connect
            } else {
                super::SocketGetError::Other
            };
            return Err(err_type(format!("HTTP error: {}", status_line)));
        }

        Ok(response[header_end + 4..].to_vec())
    }

    pub async fn get_incoming_files(save_dir: &str) -> Result<Vec<u8>, String> {
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || match try_pipe_get("/localapi/v0/files/") {
                Ok(data) => {
                    log::debug!("Windows: pipe file listing OK ({} bytes)", data.len());
                    Ok(data)
                }
                Err(super::SocketGetError::Connect(e)) => {
                    log::debug!("Windows: pipe file listing failed (connect): {}", e);
                    log::debug!(
                        "Windows: falling back to CLI auto-receive to '{}'",
                        save_dir
                    );
                    try_cli_receive_files(&save_dir)
                }
                Err(super::SocketGetError::Other(e)) => {
                    // TD-04: same as macOS — an HTTP/transport failure on the
                    // listing call must propagate, not masquerade as an empty
                    // inbox, or the failure banner can never trigger.
                    log::debug!("Windows: pipe file listing failed: {}", e);
                    Err(e)
                }
            }),
        )
        .await
        .map_err(|_| "get_incoming_files timed out".to_string())?
        .map_err(|e| format!("Task panicked: {}", e))?
    }

    /// Accept an incoming file. Uses shared helper with path traversal
    /// sanitization and staging-directory semantics (downloads drain the
    /// whole inbox into a private staging dir, then move each file into the
    /// save dir without ever overwriting).
    pub async fn accept_file(name: &str, save_dir: &str) -> Result<String, String> {
        let name = name.to_string();
        let save_dir = save_dir.to_string();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || {
                let safe_name = std::path::Path::new(&name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| "Invalid filename".to_string())?;
                // TD05-A: acknowledgement fast path — a recorded landing is
                // returned without invoking the CLI (no inbox re-drain).
                if let Some(recorded) = super::acknowledge_already_received(&name, &save_dir) {
                    return Ok(recorded);
                }
                // Single authoritative recording point: the helper records
                // successes (requested + collateral); failures recorded here.
                super::accept_file_inner(
                    &name,
                    &save_dir,
                    |staging| {
                        // --wait=false: don't block if the inbox is empty.
                        // TD-07: capped under the 120s outer wrapper.
                        let output = tailscale_cmd_capped(
                            &["file", "get", "--wait=false", &staging.to_string_lossy()],
                            std::time::Duration::from_secs(110),
                        )
                        .map_err(|e| format!("Failed to run tailscale file get: {}", e))?;
                        if !output.status.success() {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            return Err(format!("tailscale file get failed: {}", stderr));
                        }
                        Ok(())
                    },
                    false,
                )
                .map_err(|e| {
                    crate::receipts::ReceiptStore::record_failed(
                        safe_name,
                        &e,
                        crate::receipts::ReceiptStore::classify_recovery(&e),
                    );
                    e
                })
            }),
        )
        .await
        .map_err(|_| "accept_file timed out".to_string())?
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
                "XL" | "XS" | "SE" | "TV" | "PC" | "NAS" | "VM" | "VPN" | "USB" | "NUC" | "AI"
                | "IO" | "UK" | "US" | "EU" => upper,
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
    log::debug!("fetch_status: got {} bytes of JSON", body.len());
    let status: TailscaleStatus = serde_json::from_slice(&body).map_err(|e| {
        log::error!("fetch_status: PARSE ERROR: {}", e);
        // Log first 200 chars of body for diagnosis
        let preview = String::from_utf8_lossy(&body[..body.len().min(200)]);
        log::debug!("JSON preview: {}", preview);
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

    peers.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    Ok(peers)
}

pub async fn send_file_to_peer(
    peer_id: &str,
    peer_name: &str,
    file_path: &str,
) -> Result<String, String> {
    platform::send_file(peer_id, peer_name, file_path).await
}

pub async fn fetch_incoming_files(save_dir: &str) -> Result<Vec<IncomingFile>, String> {
    let body = platform::get_incoming_files(save_dir).await?;
    // The Tailscale daemon returns `null` (not `[]`) when no files are pending.
    // Handle that gracefully instead of failing the parse.
    let body_str = String::from_utf8_lossy(&body);
    if body_str.trim() == "null" || body_str.trim().is_empty() {
        return Ok(Vec::new());
    }
    let files: Vec<IncomingFile> =
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse files: {}", e))?;
    Ok(files)
}

pub async fn accept_incoming_file(name: &str, save_dir: &str) -> Result<String, String> {
    platform::accept_file(name, save_dir).await
}

// ============================================================
// Tests — crate-root, runs on all platforms
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- TD-07: run_command_with_cap ---

    #[test]
    fn capped_command_returns_output_on_success() {
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello-td07");
        let out = run_command_with_cap(cmd, std::time::Duration::from_secs(10)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello-td07"));
    }

    /// Reopen-finding regression: output larger than the OS pipe capacity
    /// (64 KiB) must complete successfully, not deadlock-until-killed. The
    /// old runner read only after exit, so a full pipe blocked the child
    /// forever and it was killed as "timed out".
    #[test]
    fn capped_command_survives_pipe_capacity_output() {
        // ~1 MiB on stdout alone — 16x the macOS pipe capacity.
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "dd if=/dev/zero bs=1024 count=1024 2>/dev/null | tr '\\0' 'x'",
        ]);
        let start = std::time::Instant::now();
        let out = run_command_with_cap(cmd, std::time::Duration::from_secs(30)).unwrap();
        assert!(out.status.success(), "healthy command must not be killed");
        assert!(
            out.stdout.len() >= 1024 * 1024,
            "all output must be captured: got {}",
            out.stdout.len()
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "must not stall on a full pipe (took {:?})",
            start.elapsed()
        );
    }

    /// Same for stderr.
    #[test]
    fn capped_command_survives_pipe_capacity_stderr() {
        let mut cmd = std::process::Command::new("sh");
        // 1 MiB of 'y' written to stderr only.
        cmd.args([
            "-c",
            "{ dd if=/dev/zero bs=1024 count=1024 2>/dev/null; } | tr '\\0' 'y' 1>&2",
        ]);
        let out = run_command_with_cap(cmd, std::time::Duration::from_secs(30)).unwrap();
        assert!(out.status.success());
        assert!(
            out.stderr.len() >= 1024 * 1024,
            "stderr must drain concurrently: got {}",
            out.stderr.len()
        );
    }

    #[test]
    fn capped_command_terminates_and_errors_past_deadline() {
        // `sleep 30` would hold a thread for 30s if orphaned. The cap must
        // terminate the child AND return a timeout error promptly.
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        let start = std::time::Instant::now();
        let result = run_command_with_cap(cmd, std::time::Duration::from_millis(300));
        let elapsed = start.elapsed();
        assert!(result.is_err(), "cap expiry must be an error");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("timed out"),
            "error must name the timeout: {}",
            msg
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "must return promptly after the cap (took {:?})",
            elapsed
        );
    }

    #[test]
    fn capped_command_captures_nonzero_exit() {
        let cmd = std::process::Command::new("false");
        let out = run_command_with_cap(cmd, std::time::Duration::from_secs(10)).unwrap();
        assert!(!out.status.success(), "nonzero exit must be visible");
    }

    // --- url_encode ---

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
        assert_eq!(url_encode("-_.~"), "-_.~");
    }

    #[test]
    fn url_encode_unicode() {
        assert_eq!(url_encode("é"), "%C3%A9");
    }

    // --- prettify_name ---

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
    fn prettify_more_abbreviations() {
        assert_eq!(prettify_name("macbook-pro-se"), "Macbook Pro SE");
        assert_eq!(prettify_name("server-tv"), "Server TV");
        assert_eq!(prettify_name("office-pc"), "Office PC");
        assert_eq!(prettify_name("dev-vm"), "Dev VM");
        assert_eq!(prettify_name("work-vpn"), "Work VPN");
        assert_eq!(prettify_name("mini-nuc"), "Mini NUC");
    }

    #[test]
    fn prettify_underscores() {
        assert_eq!(prettify_name("my_device"), "My Device");
    }

    #[test]
    fn prettify_digit_only_word() {
        assert_eq!(prettify_name("node-100"), "Node 100");
    }

    #[test]
    fn prettify_empty() {
        assert_eq!(prettify_name(""), "");
    }

    // --- raw_machine_name ---

    #[test]
    fn raw_machine_name_basic() {
        assert_eq!(
            raw_machine_name("pixel.tail1234.ts.net."),
            Some("pixel".to_string())
        );
    }

    #[test]
    fn raw_machine_name_no_dot() {
        assert_eq!(raw_machine_name("hostname"), Some("hostname".to_string()));
    }

    #[test]
    fn raw_machine_name_empty() {
        assert_eq!(raw_machine_name(""), None);
    }

    #[test]
    fn raw_machine_name_trailing_dot_only() {
        assert_eq!(raw_machine_name("."), None);
    }

    // --- display_name_from_dns ---

    #[test]
    fn display_name_from_dns_basic() {
        assert_eq!(
            display_name_from_dns("my-laptop.tail9999.ts.net."),
            Some("My Laptop".to_string())
        );
    }

    #[test]
    fn display_name_from_dns_none() {
        assert_eq!(display_name_from_dns(""), None);
    }

    // --- unique_save_path ---

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

    // --- timestamp_tag ---

    #[test]
    fn timestamp_tag_is_monotonic() {
        let a = timestamp_tag();
        let b = timestamp_tag();
        assert!(
            b >= a,
            "timestamp_tag should be monotonically non-decreasing"
        );
    }

    #[test]
    fn timestamp_tag_counter_increments() {
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Rapid calls should produce different tags (counter increments).
        let a = timestamp_tag();
        let b = timestamp_tag();
        assert_ne!(a, b, "consecutive calls must produce different tags");
    }

    // --- accept_file_with_getter path traversal ---

    #[test]
    fn accept_file_rejects_path_traversal_dotdot() {
        let _iso = super::staging_guard::StagingRootGuard::new("traversal_dotdot");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // The function sanitizes name via Path::file_name(), so "../etc/passwd"
        // becomes just "passwd". We test that the function does NOT create a
        // file outside save_dir by checking it doesn't error on a safe name
        // but would error on a purely traversal-only name like "../".
        let dir = std::env::temp_dir().join("taildrop_test_traversal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // "../" has no file_name component → should error "Invalid filename"
        let result = accept_file_with_getter("../", dir.to_str().unwrap(), |_| Ok(()));
        assert!(result.is_err(), "path traversal should be rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_file_accepts_normal_filename() {
        let _iso = super::staging_guard::StagingRootGuard::new("normal_name");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join("taildrop_test_normal_name");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A normal filename with a no-op getter should find no file and error,
        // but the name itself should pass sanitization (not "Invalid filename").
        let result = accept_file_with_getter("photo.jpg", dir.to_str().unwrap(), |_| Ok(()));
        assert!(result.is_err(), "should fail because file doesn't appear");
        assert!(
            !result.unwrap_err().contains("Invalid filename"),
            "normal filename should pass sanitization"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- accept_file_with_getter P0 regression tests ---

    /// Helper: build a `run_get` fake that simulates `tailscale file get`
    /// delivering files into the (staging) directory it is handed.
    fn fake_cli_delivering(
        files: Vec<(&'static str, &'static str)>,
    ) -> impl FnOnce(&std::path::Path) -> Result<(), String> {
        move |staging: &std::path::Path| {
            for (name, content) in files {
                std::fs::write(staging.join(name), content).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }

    fn temp_test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("taildrop_test_{}", label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accept_returns_path_content_actually_landed_in() {
        let _iso = super::staging_guard::StagingRootGuard::new("actual_path");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("actual_path");
        // A same-named file already exists — the move must not clobber it and
        // must return the path the NEW content landed in.
        std::fs::write(dir.join("report.pdf"), "old").unwrap();

        let result = accept_file_with_getter(
            "report.pdf",
            dir.to_str().unwrap(),
            fake_cli_delivering(vec![("report.pdf", "new")]),
        )
        .unwrap();

        let returned = std::path::PathBuf::from(&result);
        assert_eq!(
            returned,
            dir.join("report (1).pdf"),
            "new content must land in a distinct file"
        );
        assert_eq!(
            std::fs::read_to_string(returned).unwrap(),
            "new",
            "returned path must contain the new content"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("report.pdf")).unwrap(),
            "old",
            "pre-existing file must never be overwritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_concurrent_double_accept_yields_two_distinct_files() {
        let _iso = super::staging_guard::StagingRootGuard::new("double_accept");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("double_accept");
        let dir_str = dir.to_str().unwrap().to_string();

        let (first, second) = std::thread::scope(|s| {
            let h1 = s.spawn(|| {
                accept_file_with_getter(
                    "report.pdf",
                    &dir_str,
                    fake_cli_delivering(vec![("report.pdf", "first")]),
                )
            });
            let h2 = s.spawn(|| {
                accept_file_with_getter(
                    "report.pdf",
                    &dir_str,
                    fake_cli_delivering(vec![("report.pdf", "second")]),
                )
            });
            (h1.join().unwrap(), h2.join().unwrap())
        });

        let p1 = std::path::PathBuf::from(first.expect("first accept should succeed"));
        let p2 = std::path::PathBuf::from(second.expect("second accept should succeed"));
        assert_ne!(p1, p2, "double accept must yield two distinct files");
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_moves_collateral_files_without_losing_them() {
        let _iso = super::staging_guard::StagingRootGuard::new("collateral");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("collateral");
        // The CLI drains the whole inbox: two pending files arrive even though
        // only one was accepted. Both must end up in the save dir (the old
        // single-new-entry heuristic lost track with 2+ pending files).
        let result = accept_file_with_getter(
            "wanted.txt",
            dir.to_str().unwrap(),
            fake_cli_delivering(vec![("wanted.txt", "wanted"), ("other.txt", "other")]),
        )
        .unwrap();

        assert_eq!(
            std::path::PathBuf::from(&result),
            dir.join("wanted.txt"),
            "must return the path of the requested file"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "other"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_falls_back_to_existing_file_when_inbox_empty() {
        let _iso = super::staging_guard::StagingRootGuard::new("already_received");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("already_received");
        // Auto-receive poll already saved the file; clicking Accept drains an
        // empty inbox. The existing exact-name file is returned.
        std::fs::write(dir.join("note.txt"), "already here").unwrap();
        let result =
            accept_file_with_getter("note.txt", dir.to_str().unwrap(), |_| Ok(())).unwrap();
        assert_eq!(std::path::PathBuf::from(&result), dir.join("note.txt"));
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "already here"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_reports_failure_when_file_never_appears() {
        let _iso = super::staging_guard::StagingRootGuard::new("never_appeared");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("never_appeared");
        let err =
            accept_file_with_getter("ghost.txt", dir.to_str().unwrap(), |_| Ok(())).unwrap_err();
        assert!(
            err.contains("ghost.txt"),
            "error should name the missing file: {}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_creates_missing_save_dir() {
        let _iso = super::staging_guard::StagingRootGuard::new("create_save_dir");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let root = temp_test_dir("create_save_dir");
        let save = root.join("nested/save");
        let result = accept_file_with_getter(
            "file.txt",
            save.to_str().unwrap(),
            fake_cli_delivering(vec![("file.txt", "hi")]),
        );
        assert!(
            result.is_ok(),
            "missing save dir should be created: {:?}",
            result
        );
        assert_eq!(
            std::fs::read_to_string(save.join("file.txt")).unwrap(),
            "hi"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- move_file_into_dir / reserve_unique_file ---

    #[test]
    fn move_file_into_dir_never_overwrites() {
        let dir = temp_test_dir("move_no_overwrite");
        std::fs::write(dir.join("a.txt"), "original").unwrap();
        let src = dir.join("src.txt");
        std::fs::write(&src, "incoming").unwrap();

        let dest = move_file_into_dir(&src, &dir, "a.txt").unwrap();
        assert_eq!(dest, dir.join("a (1).txt"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "original"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "incoming");
        assert!(!src.exists(), "source must be consumed by the move");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reserve_unique_file_does_not_truncate() {
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("reserve_no_truncate");
        std::fs::write(dir.join("data.bin"), "payload").unwrap();
        let (file, path) = reserve_unique_file(&dir, "data.bin").unwrap();
        assert_eq!(path, dir.join("data (1).bin"), "must take the next suffix");
        drop(file);
        assert_eq!(
            std::fs::read_to_string(dir.join("data.bin")).unwrap(),
            "payload",
            "existing file must be untouched"
        );
        assert!(path.exists(), "reserved file must exist");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- cli_receive_files (P0: no --conflict=overwrite) ---

    /// A successful exit status, built cross-platform via the OS-specific
    /// `ExitStatusExt` (there is no portable constructor).
    fn success_status() -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
    }

    /// Fake CLI output: stdout is what `tailscale file get --verbose` prints.
    fn fake_cli_output(moved_line: &str) -> std::process::Output {
        std::process::Output {
            status: success_status(),
            stdout: format!("{}\n", moved_line).into_bytes(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn cli_receive_files_uses_rename_conflict_policy() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_rename_policy");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("cli_rename_policy");
        let captured_args: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            *captured_args.lock().unwrap() = args.iter().map(|a| a.to_string()).collect();
            Ok(fake_cli_output("moved 0/0 files"))
        })
        .unwrap();
        assert_eq!(result, b"[]");

        let args = captured_args.into_inner().unwrap();
        assert!(
            !args.contains(&"--conflict=overwrite".to_string()),
            "poll must never overwrite: args = {:?}",
            args
        );
        assert!(
            args.contains(&"--conflict=rename".to_string()),
            "rename policy must be explicit: args = {:?}",
            args
        );
        // TD-05: the drain targets a private staging dir (identity is exact —
        // never the mtime heuristic over save_dir), under the temp dir.
        let last = args.last().expect("drain target must be the last arg");
        assert!(
            last.starts_with(staging_root().to_string_lossy().trim_end_matches('/'))
                && last.contains("taildrop-accept-"),
            "drain must target a private staging dir, got: {}",
            last
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_receive_files_propagates_save_dir_error() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_save_dir_error");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Make save_dir creation impossible: a regular file exists where the
        // directory should be created under it.
        let root = temp_test_dir("cli_save_dir_error");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, "not a dir").unwrap();
        let bad_dir = blocker.join("sub");

        let result = cli_receive_files(bad_dir.to_str().unwrap(), "test", |_| {
            Ok(fake_cli_output("moved 0/0 files"))
        });
        assert!(
            result.is_err(),
            "unusable save dir must surface an error, not an empty list"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cli_receive_files_reports_received_files() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_reports_files");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("cli_reports_files");
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            // The CLI drains into the staging dir (last arg); write the file
            // it "received" there — exact identity, not save_dir mtime.
            let staging = std::path::Path::new(args.last().unwrap());
            std::fs::write(staging.join("got.txt"), "x").unwrap();
            Ok(fake_cli_output("moved 1/1 files"))
        })
        .unwrap();
        // The landed list reports the file, which now verifiably sits in the
        // save dir under its original name.
        let text = String::from_utf8(result).unwrap();
        assert!(
            text.contains("got.txt"),
            "should list received file: {}",
            text
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("got.txt")).unwrap(),
            "x",
            "file must have landed in the save dir"
        );
        // TD-05: a durable receipt with the actual landing path exists.
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.saved_path.starts_with(dir.to_str().unwrap()))
            .collect();
        assert_eq!(receipts.len(), 1, "one receipt per landed file");
        assert_eq!(receipts[0].filename, "got.txt");
        assert_eq!(receipts[0].status, "saved");
        assert!(receipts[0].saved_path.ends_with("got.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TD-05 (repo-audititor acceptance): an unrelated NEWER file in the save
    /// dir must never be reported or receipted as a received file. The old
    /// mtime heuristic picked the N newest — this is its regression test.
    #[test]
    fn cli_receive_files_ignores_unrelated_newer_files() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_newer_unrelated");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("cli_newer_unrelated");
        // A pre-existing unrelated file, modified NOW (newer than anything
        // the CLI delivers in this test).
        std::fs::write(dir.join("browser-download.txt"), "unrelated").unwrap();
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            let staging = std::path::Path::new(args.last().unwrap());
            std::fs::write(staging.join("actual-receive.txt"), "received").unwrap();
            Ok(fake_cli_output("moved 1/1 files"))
        })
        .unwrap();
        let text = String::from_utf8(result).unwrap();
        assert!(
            !text.contains("browser-download.txt"),
            "unrelated file must not be reported as received: {}",
            text
        );
        assert!(text.contains("actual-receive.txt"));
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.saved_path.starts_with(dir.to_str().unwrap()))
            .collect();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].filename, "actual-receive.txt");
        assert_eq!(
            std::fs::read_to_string(dir.join("browser-download.txt")).unwrap(),
            "unrelated",
            "unrelated file untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TD-05: a collision-renamed landing gets a receipt pointing at the
    /// ACTUAL destination ("got (1).txt"), never the requested name.
    #[test]
    fn cli_receive_files_receipts_collision_resolved_path() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_collision_receipt");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("cli_collision_receipt");
        std::fs::write(dir.join("got.txt"), "original").unwrap();
        cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            let staging = std::path::Path::new(args.last().unwrap());
            std::fs::write(staging.join("got.txt"), "new").unwrap();
            Ok(fake_cli_output("moved 1/1 files"))
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("got.txt")).unwrap(),
            "original",
            "existing file must never be overwritten"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("got (1).txt")).unwrap(),
            "new",
            "new content lands collision-resolved"
        );
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.saved_path.starts_with(dir.to_str().unwrap()))
            .collect();
        assert_eq!(receipts.len(), 1);
        assert!(
            receipts[0].saved_path.ends_with("got (1).txt"),
            "receipt must point at the actual landing path: {}",
            receipts[0].saved_path
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TD-05 (repo-audititor acceptance): a partially-failed batch preserves
    /// successfully landed receipts while surfacing the failure; staged
    /// leftovers are NOT marked saved (unverified).
    #[test]
    fn cli_receive_files_partial_failure_preserves_landed_receipts() {
        let _iso = super::staging_guard::StagingRootGuard::new("cli_partial_failure");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("cli_partial_failure");
        let staging_seen = std::sync::Mutex::new(None::<std::path::PathBuf>);
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |args| {
            // First file completes into staging; then the CLI dies non-zero
            // before the second finishes.
            let staging = std::path::Path::new(args.last().unwrap());
            *staging_seen.lock().unwrap() = Some(staging.to_path_buf());
            std::fs::write(staging.join("done.txt"), "complete").unwrap();
            std::fs::write(staging.join("partial.bin"), "half").unwrap();
            Ok(std::process::Output {
                status: failure_status(),
                stdout: b"moved 1/2 files\n".to_vec(),
                stderr: b"second file failed\n".to_vec(),
            })
        });
        assert!(result.is_err(), "batch failure must surface");
        // Nothing verifiably completed (both files are unverified in staging
        // after a non-zero exit) — no saved receipts, but bytes preserved.
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.saved_path.starts_with(dir.to_str().unwrap()))
            .collect();
        assert!(
            receipts.is_empty(),
            "unverified staged files must not be receipted as saved: {:?}",
            receipts
        );
        assert!(
            !dir.join("done.txt").exists(),
            "unverified files must not land in the save dir"
        );
        // Both staged files survive in THIS call's staging dir (identified
        // from the closure, not by scanning the shared temp dir — leftovers
        // from other runs' crashed tests live there too).
        let staging = staging_seen
            .into_inner()
            .unwrap()
            .expect("closure must have run");
        let names: Vec<String> = std::fs::read_dir(&staging)
            .map(|rd| {
                rd.filter_map(|f| f.ok())
                    .map(|f| f.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            names.contains(&"done.txt".to_string()),
            "names: {:?}",
            names
        );
        assert!(
            names.contains(&"partial.bin".to_string()),
            "names: {:?}",
            names
        );
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- TD-01: staged files must survive CLI/move failures ---
    //
    // Each staging test now uses a `StagingRootGuard` (RA-01) that creates an
    // isolated per-test staging directory, so concurrent tests no longer
    // interfere.  The lock is retained as a safety net for any future tests
    // that might still scan shared state.
    static STAGING_TESTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn accept_preserves_staged_files_when_cli_fails_partway() {
        let _iso = super::staging_guard::StagingRootGuard::new("td01_cli_fail");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _guard = STAGING_TESTS_LOCK.lock().unwrap();
        let dir = temp_test_dir("td01_cli_fail");
        // The CLI drains a file out of the daemon inbox into staging, then
        // exits non-zero (e.g. a second file in the batch failed).
        let staging_seen = std::sync::Mutex::new(None::<std::path::PathBuf>);
        let err = accept_file_with_getter("photo.jpg", dir.to_str().unwrap(), |staging| {
            *staging_seen.lock().unwrap() = Some(staging.to_path_buf());
            std::fs::write(staging.join("photo.jpg"), "precious bytes").unwrap();
            Err("exit status 1: partial batch failure".to_string())
        })
        .unwrap_err();

        assert!(
            err.contains("preserved for recovery"),
            "error must point at the recovery location: {}",
            err
        );
        // The staged file must still exist — assert against the captured
        // staging path, not by scanning the shared temp dir (RA-01).
        let staging = staging_seen
            .into_inner()
            .unwrap()
            .expect("closure must have captured staging path");
        let staged_file = staging.join("photo.jpg");
        assert!(
            staged_file.exists(),
            "the drained file must survive in staging at '{}'",
            staged_file.display()
        );
        assert_eq!(
            std::fs::read(&staged_file).unwrap(),
            b"precious bytes",
            "staged content must be intact"
        );
        // Nothing may have leaked into the save dir under the requested name.
        assert!(!dir.join("photo.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_removes_staging_when_cli_fails_cleanly() {
        let _iso = super::staging_guard::StagingRootGuard::new("td01_cli_fail_clean");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // The CLI fails without draining anything — no recovery value, so the
        // staging directory must not survive the call. Assert on the exact
        // staging path (captured from the closure) rather than scanning the
        // shared temp dir, which races with other tests' in-flight staging.
        let dir = temp_test_dir("td01_cli_fail_clean");
        let staging_path = std::cell::RefCell::new(None::<std::path::PathBuf>);
        accept_file_with_getter("photo.jpg", dir.to_str().unwrap(), |staging| {
            staging_path.borrow_mut().replace(staging.to_path_buf());
            Err("tailscale not found".to_string())
        })
        .unwrap_err();

        let staging = staging_path.into_inner().expect("closure must have run");
        assert!(
            !staging.exists(),
            "empty staging dir '{}' must be cleaned up",
            staging.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// RA-01 regression verification: confirm that test fixtures left in an
    /// isolated staging root are invisible to `scan_staging_dirs()` (which
    /// scans the production temp dir) and that a fresh test run succeeds
    /// despite earlier residue on disk.
    #[test]
    fn ra01_isolated_fixtures_invisible_to_recovery_scan() {
        // --- (a) Simulate an interrupted test leaving fixtures behind ---
        let isolated_root = std::env::temp_dir().join(format!(
            "taildrop_test_staging_ra01_verify_{:016x}",
            timestamp_tag()
        ));
        std::fs::create_dir_all(&isolated_root).unwrap();
        let stale_staging = isolated_root.join("taildrop-accept-stale123");
        std::fs::create_dir_all(&stale_staging).unwrap();
        std::fs::write(stale_staging.join("done.txt"), "complete").unwrap();
        std::fs::write(stale_staging.join("partial.bin"), "half").unwrap();

        // With no override active, `scan_staging_dirs()` scans
        // `std::env::temp_dir()`.  The isolated root is NOT under it,
        // so the stale fixtures must be invisible.
        let dirs = crate::receipts::scan_staging_dirs();
        assert!(
            !dirs
                .iter()
                .any(|d| d.path.starts_with(isolated_root.to_string_lossy().as_ref())),
            "stale fixtures in isolated root must NOT appear in recovery scan: {:?}",
            dirs
        );

        // --- (b) A fresh staging-root guard succeeds despite the residue ---
        {
            let _iso = super::staging_guard::StagingRootGuard::new("ra01_after_residue");
            let dir = temp_test_dir("ra01_after_residue");
            let result = accept_file_with_getter(
                "photo.jpg",
                dir.to_str().unwrap(),
                fake_cli_delivering(vec![("photo.jpg", "fresh bytes")]),
            );
            assert!(
                result.is_ok(),
                "fresh test must succeed despite stale residue on disk: {:?}",
                result
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("photo.jpg")).unwrap(),
                "fresh bytes"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Clean up the synthetic residue.
        let _ = std::fs::remove_dir_all(&isolated_root);
    }

    // --- TD-02: Content-Length enforcement ---

    #[test]
    fn parse_content_length_reads_header_case_insensitively() {
        let headers =
            "HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\nCONTENT-LENGTH: 42\r\n";
        assert_eq!(parse_content_length(headers).unwrap(), 42);
    }

    #[test]
    fn parse_content_length_missing_is_error() {
        let headers = "HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\n";
        assert!(parse_content_length(headers).is_err());
    }

    #[test]
    fn parse_content_length_rejects_garbage() {
        let headers = "HTTP/1.0 200 OK\r\nContent-Length: abc\r\n";
        assert!(parse_content_length(headers).is_err());
    }

    /// Exercise the async download framing (the code the Linux socket path
    /// streams through) against a synthetic short-body HTTP/1.0 response:
    /// advertise 16 bytes, deliver 8, close cleanly.
    #[cfg(unix)]
    #[tokio::test]
    async fn download_framing_async_rejects_short_body() {
        use tokio::io::AsyncWriteExt;
        let response = b"HTTP/1.0 200 OK\r\nContent-Length: 16\r\n\r\n01234567".to_vec();
        let (mut server, client) = tokio::io::duplex(64);
        server.write_all(&response).await.unwrap();
        drop(server); // clean EOF after a short body

        let mut client = client;
        let mut writer = Vec::new();
        let err = read_http_download_async(&mut client, &mut writer)
            .await
            .expect_err("short body must be rejected");
        assert!(
            err.contains("Truncated download"),
            "unexpected error: {}",
            err
        );
        assert_eq!(writer.len(), 8, "only the delivered bytes reach the file");
    }

    /// The async framing accepts an exact-length body.
    #[cfg(unix)]
    #[tokio::test]
    async fn download_framing_async_accepts_exact_length() {
        use tokio::io::AsyncWriteExt;
        let mut response = b"HTTP/1.0 200 OK\r\nContent-Length: 16\r\n\r\n".to_vec();
        response.extend_from_slice(b"0123456789abcdef");
        let (mut server, client) = tokio::io::duplex(64);
        server.write_all(&response).await.unwrap();
        drop(server);

        let mut client = client;
        let mut writer = Vec::new();
        read_http_download_async(&mut client, &mut writer)
            .await
            .expect("exact length must pass");
        assert_eq!(writer, b"0123456789abcdef".to_vec());
    }

    /// Exercise the shared sync framing (the macOS socket path) against the
    /// same synthetic responses.
    #[cfg(unix)]
    #[test]
    fn download_framing_sync_rejects_short_body() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Length: 16\r\n\r\n01234567";
        let mut cursor = std::io::Cursor::new(response.to_vec());
        let mut tmp = tempfile_for_tests("td02_short");
        let err = read_http_download_sync(&mut cursor, &mut tmp.file)
            .expect_err("short body must be rejected");
        assert!(
            err.contains("Truncated download"),
            "unexpected error: {}",
            err
        );
        assert_eq!(tmp.file.metadata().unwrap().len(), 8);
        tmp.cleanup();
    }

    /// A complete body passes validation; over-long bodies are rejected too.
    #[cfg(unix)]
    #[test]
    fn download_framing_sync_accepts_exact_length() {
        let body = b"0123456789abcdef";
        let mut response = b"HTTP/1.0 200 OK\r\nContent-Length: 16\r\n\r\n".to_vec();
        response.extend_from_slice(body);
        let mut cursor = std::io::Cursor::new(response);
        let mut tmp = tempfile_for_tests("td02_exact");
        read_http_download_sync(&mut cursor, &mut tmp.file).expect("exact length must pass");
        assert_eq!(tmp.file.metadata().unwrap().len(), 16);
        tmp.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn download_framing_sync_rejects_overlong_body() {
        let body = b"0123456789abcdefEXTRA";
        let mut response = b"HTTP/1.0 200 OK\r\nContent-Length: 16\r\n\r\n".to_vec();
        response.extend_from_slice(body);
        let mut cursor = std::io::Cursor::new(response);
        let mut tmp = tempfile_for_tests("td02_overlong");
        let err = read_http_download_sync(&mut cursor, &mut tmp.file)
            .expect_err("over-long body must be rejected");
        assert!(
            err.contains("Truncated download"),
            "unexpected error: {}",
            err
        );
        tmp.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn download_framing_sync_rejects_missing_content_length() {
        let mut response =
            b"HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\n\r\nbody".to_vec();
        let mut cursor = std::io::Cursor::new(std::mem::take(&mut response));
        let mut tmp = tempfile_for_tests("td02_missing_cl");
        let err = read_http_download_sync(&mut cursor, &mut tmp.file)
            .expect_err("missing Content-Length must be rejected");
        assert!(
            err.contains("missing Content-Length"),
            "unexpected error: {}",
            err
        );
        tmp.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn download_framing_sync_surfaces_http_error_status() {
        let response = b"HTTP/1.0 500 Internal Server Error\r\n\r\nboom";
        let mut cursor = std::io::Cursor::new(response.to_vec());
        let mut tmp = tempfile_for_tests("td02_http_500");
        let err = read_http_download_sync(&mut cursor, &mut tmp.file)
            .expect_err("HTTP 500 must be rejected");
        assert!(
            err.contains("500") && err.contains("boom"),
            "error should carry status and body: {}",
            err
        );
        tmp.cleanup();
    }

    /// Minimal temp-file helper for the framing tests.
    struct TempFileForTest {
        path: std::path::PathBuf,
        file: std::fs::File,
    }

    impl TempFileForTest {
        fn cleanup(self) {
            drop(self.file);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn tempfile_for_tests(label: &str) -> TempFileForTest {
        let path =
            std::env::temp_dir().join(format!("taildrop_test_{}_{}", label, timestamp_tag()));
        let file = std::fs::File::create(&path).unwrap();
        TempFileForTest { path, file }
    }

    // --- TD05-A: idempotent acknowledgement and windowed suppression ---

    /// Acknowledging an already-recorded landing must not invoke the CLI at
    /// all (no inbox re-drain) — the fast path returns the recorded path.
    #[test]
    fn acknowledge_returns_recorded_path_without_draining() {
        let _iso = super::staging_guard::StagingRootGuard::new("td05a_ack_no_drain");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("td05a_ack_no_drain");
        std::fs::write(dir.join("photo.jpg"), "recorded").unwrap();
        crate::receipts::ReceiptStore::record_saved(
            "photo.jpg",
            &dir.join("photo.jpg").to_string_lossy(),
            8,
            None,
        );

        // The fast path alone finds it:
        let acked = acknowledge_already_received("photo.jpg", dir.to_str().unwrap())
            .expect("recorded landing must be acknowledged");
        assert_eq!(acked, dir.join("photo.jpg").to_string_lossy());

        // And via the production sequence (what the macOS/Windows callers
        // run): fast path first, accept_file_inner only on a miss. run_get
        // must NOT be invoked.
        let drained = std::sync::atomic::AtomicBool::new(false);
        let result = match acknowledge_already_received("photo.jpg", dir.to_str().unwrap()) {
            Some(recorded) => recorded,
            None => accept_file_inner(
                "photo.jpg",
                dir.to_str().unwrap(),
                |staging| {
                    drained.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = staging;
                    Ok(())
                },
                false,
            )
            .expect("accept must succeed"),
        };
        assert_eq!(result, dir.join("photo.jpg").to_string_lossy());
        assert!(
            !drained.load(std::sync::atomic::Ordering::SeqCst),
            "acknowledging a recorded landing must not drain the inbox"
        );
        // Still exactly one receipt for this file.
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.filename == "photo.jpg")
            .collect();
        assert_eq!(receipts.len(), 1, "no duplicate receipt on ack");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TD05-A boundary 1 (repo-auditor): a NEW transfer inside any window
    /// records its own receipt. Receive → delete → receive-again (same name,
    /// seconds apart) = two receipts, because they are two transfers.
    #[test]
    fn new_transfer_inside_window_records_new_receipt() {
        let _iso = super::staging_guard::StagingRootGuard::new("td05a_inside_window");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("td05a_inside_window");
        let path = dir.join("report.pdf").to_string_lossy().to_string();

        // First transfer: drains and lands.
        accept_file_with_getter("report.pdf", dir.to_str().unwrap(), |staging| {
            std::fs::write(staging.join("report.pdf"), "first").unwrap();
            Ok(())
        })
        .unwrap();

        // User deletes the file, then a second transfer arrives with the
        // same name — immediately (inside any conceivable window).
        std::fs::remove_file(&path).unwrap();
        accept_file_with_getter("report.pdf", dir.to_str().unwrap(), |staging| {
            std::fs::write(staging.join("report.pdf"), "second").unwrap();
            Ok(())
        })
        .unwrap();

        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.filename == "report.pdf")
            .collect();
        assert_eq!(
            receipts.len(),
            2,
            "receive-delete-receive inside the window must yield TWO receipts: {:?}",
            receipts
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TD05-A boundary 2 (repo-auditor): a DELAYED acknowledgement still
    /// resolves without draining, no matter how old the receipt is.
    #[test]
    fn delayed_acknowledgement_outside_any_window_still_skips_drain() {
        let _iso = super::staging_guard::StagingRootGuard::new("td05a_delayed_ack");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        crate::receipts::ReceiptStore::reset_for_tests();
        let dir = temp_test_dir("td05a_delayed_ack");
        let path = dir.join("notes.txt");
        std::fs::write(&path, "old landing").unwrap();
        // Record the landing with a timestamp far in the past.
        crate::receipts::ReceiptStore::record_saved_at(
            "notes.txt",
            &path.to_string_lossy(),
            now_ms_testhelper().saturating_sub(3_600_000),
        );

        let drained = std::sync::atomic::AtomicBool::new(false);
        let result = match acknowledge_already_received("notes.txt", dir.to_str().unwrap()) {
            Some(recorded) => recorded,
            None => accept_file_inner(
                "notes.txt",
                dir.to_str().unwrap(),
                |staging| {
                    let _ = staging;
                    drained.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
                false,
            )
            .unwrap(),
        };
        assert_eq!(result, path.to_string_lossy().to_string());
        assert!(
            !drained.load(std::sync::atomic::Ordering::SeqCst),
            "a delayed ack must still resolve from the recorded receipt, not re-drain"
        );
        // And no duplicate receipt was created by the fallback.
        let receipts: Vec<_> = crate::receipts::ReceiptStore::snapshot_for_tests()
            .into_iter()
            .filter(|r| r.filename == "notes.txt" && r.status == "saved")
            .collect();
        assert_eq!(receipts.len(), 1, "delayed ack must not duplicate receipts");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Test helper: current wall-clock ms (kept next to the tests that need
    /// to synthesize old timestamps).
    fn now_ms_testhelper() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    // --- TD-04: receive failures must surface, not look like "no files" ---

    #[test]
    fn cli_receive_files_propagates_nonzero_exit() {
        let _iso = super::staging_guard::StagingRootGuard::new("td04_nonzero_exit");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("td04_nonzero_exit");
        let result = cli_receive_files(dir.to_str().unwrap(), "test", |_| {
            Ok(std::process::Output {
                status: failure_status(),
                stdout: b"moved 1/2 files\n".to_vec(),
                stderr: b"failed to receive second file\n".to_vec(),
            })
        });
        assert!(
            result.is_err(),
            "non-zero CLI exit must surface an error, not an empty list"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_receive_files_propagates_unreadable_save_dir() {
        let _iso = super::staging_guard::StagingRootGuard::new("td04_unreadable");
        let _g = crate::receipts::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = temp_test_dir("td04_unreadable");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a dir").unwrap();
        let bad_dir = blocker.join("sub");
        // Create bad_dir as a real dir (so creation succeeds), then remove
        // read permission? Portable approach: make the *listing* fail by
        // replacing it with a file after creation is checked.
        // Simpler portable trigger: point save_dir at a path whose parent
        // is a file — creation fails, which is already covered above. For
        // the read_dir-failure branch, use a file where a dir is expected.
        let result = cli_receive_files(bad_dir.to_str().unwrap(), "test", |_| {
            Ok(fake_cli_output("moved 0/0 files"))
        });
        // bad_dir can't even be created → creation error path (still Err).
        assert!(result.is_err(), "unusable save dir must surface an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed exit status, built cross-platform.
    fn failure_status() -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(256)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(1)
        }
    }
}
