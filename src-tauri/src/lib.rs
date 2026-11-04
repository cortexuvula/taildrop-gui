mod debug_log;
mod receipts;
mod tailscale;

use std::sync::RwLock;

use tauri::{Emitter, Manager};

/// Shared receive settings: written by the `set_receive_settings` IPC
/// command, read by the background receive task on every iteration.
struct ReceiveSettings {
    save_dir: String,
    /// Plumbed through for completeness; auto-accept itself stays
    /// frontend-driven (see useIncomingFiles), so the receive loop does not
    /// read this field yet.
    #[allow(dead_code)]
    auto_accept: bool,
}

type SharedReceiveSettings = RwLock<ReceiveSettings>;

/// TD-03: the receive loop must not run a single destructive iteration until
/// the frontend has hydrated persisted settings and pushed them via
/// `set_receive_settings`. The initial `ReceiveSettings` holds the *default*
/// download directory; on CLI-fallback platforms `fetch_incoming_files` IS
/// the download, so an ungated first poll can drain pending inbox files into
/// Downloads before the user's persisted custom destination arrives over
/// IPC. Set once, never cleared.
static RECEIVE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Idle interval for the background receive loop when nothing changed.
const RECEIVE_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(8);
/// Interval used for a few iterations after the incoming list changed, so an
/// active transfer burst is picked up quickly (mirrors the old frontend poll).
const RECEIVE_ACTIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How many iterations stay fast after a change.
const RECEIVE_FAST_ITERATIONS: u32 = 5;
/// Consecutive fetch failures before `incoming-files-error` is emitted — a
/// dead daemon must not masquerade as "no incoming files".
const RECEIVE_FAILURE_THRESHOLD: u32 = 3;

#[tauri::command]
async fn get_tailscale_status() -> Result<Vec<tailscale::Peer>, String> {
    log::debug!("get_tailscale_status: invoking fetch_status...");
    match tailscale::fetch_status().await {
        Ok(peers) => {
            log::info!(
                "get_tailscale_status: OK — {} peers (self={}, online={})",
                peers.len(),
                peers.iter().filter(|p| p.is_self).count(),
                peers.iter().filter(|p| p.online && !p.is_self).count()
            );
            Ok(peers)
        }
        Err(e) => {
            log::error!("get_tailscale_status: ERROR — {}", e);
            Err(e)
        }
    }
}

#[tauri::command]
async fn send_file(
    app: tauri::AppHandle,
    peer_id: String,
    peer_name: String,
    file_path: String,
    transfer_id: String,
) -> Result<String, String> {
    // SIMULATED PROGRESS — documented contract: neither the Tailscale
    // localapi nor the CLI reports byte-level progress for Taildrop sends,
    // so these milestone events (10/30/60/90 on a timer, then 100 on
    // success) are cosmetic feedback, NOT real transferred bytes. Do not
    // build logic on them (e.g. ETA or speed estimates). Uses a cancellation
    // token so the task checks before each emit — preventing stale
    // milestones from arriving after the real result.
    let cancel = tokio_util::sync::CancellationToken::new();
    let progress_handle = {
        let app = app.clone();
        let tid = transfer_id.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            for pct in [10u8, 30, 60, 90] {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(
                        (pct as u64) * 20,
                    )) => {
                        let _ = app.emit(
                            "transfer-progress",
                            serde_json::json!({ "transferId": tid, "progress": pct }),
                        );
                    }
                    _ = cancel.cancelled() => break,
                }
            }
        })
    };

    let result = tailscale::send_file_to_peer(&peer_id, &peer_name, &file_path).await;
    cancel.cancel();
    // Wait for the task to observe cancellation so no stale events arrive.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), progress_handle).await;
    // Emit 100% on success so the UI shows completion before status flips
    if result.is_ok() {
        let _ = app.emit(
            "transfer-progress",
            serde_json::json!({ "transferId": transfer_id, "progress": 100 }),
        );
    }
    result
}

#[tauri::command]
async fn get_incoming_files(save_dir: String) -> Result<Vec<tailscale::IncomingFile>, String> {
    let dir = effective_save_dir(&save_dir);
    tailscale::fetch_incoming_files(&dir.to_string_lossy()).await
}

#[tauri::command]
async fn accept_file(name: String, save_dir: String) -> Result<String, String> {
    let dir = effective_save_dir(&save_dir);
    tailscale::accept_incoming_file(&name, &dir.to_string_lossy()).await
}

#[tauri::command]
fn get_default_download_dir() -> String {
    default_download_dir().to_string_lossy().to_string()
}

/// Default save directory: the OS download dir, falling back to the home
/// directory, then the working directory.
fn default_download_dir() -> std::path::PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
}

/// Resolve the effective save directory: the given directory, or the default
/// download dir when empty (same fallback the accept/poll commands use).
fn effective_save_dir(save_dir: &str) -> std::path::PathBuf {
    let save_dir = save_dir.trim();
    if save_dir.is_empty() {
        default_download_dir()
    } else {
        std::path::PathBuf::from(save_dir)
    }
}

/// Validate the save directory before it is used: must be absolute, must
/// already exist (accept creates missing dirs on demand — validation is the
/// UI's early warning, not a mutation), and must be writable. Returns the
/// canonical path on success so callers can normalize what they display.
#[tauri::command]
async fn validate_save_dir(save_dir: String) -> Result<String, String> {
    validate_save_dir_path(&save_dir).await
}

/// Body of [`validate_save_dir`], shared with `set_receive_settings`.
async fn validate_save_dir_path(save_dir: &str) -> Result<String, String> {
    let path = effective_save_dir(save_dir);
    if !path.is_absolute() {
        return Err(format!(
            "'{}' is not an absolute path — pick a folder via Browse",
            path.display()
        ));
    }
    if !tokio::fs::metadata(&path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(format!("Directory '{}' does not exist", path.display()));
    }
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .map_err(|e| format!("Cannot resolve '{}': {}", path.display(), e))?;
    // Writability probe: create and remove a uniquely-named temp file.
    let probe = canonical.join(format!(".taildrop-write-probe-{}", timestamp_probe_tag()));
    match tokio::fs::File::create(&probe).await {
        Ok(_) => {
            let _ = tokio::fs::remove_file(&probe).await;
        }
        Err(e) => {
            return Err(format!(
                "Directory '{}' is not writable: {}",
                canonical.display(),
                e
            ));
        }
    }
    Ok(canonical.to_string_lossy().to_string())
}

/// Short unique tag for the writability probe filename (ms clock + counter).
fn timestamp_probe_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (ms << 20) | (n & 0xFFFFF)
}

/// Monotonic submission counter for `set_receive_settings` (A2). Each IPC
/// request takes a value at SUBMISSION; only the newest submitted revision
/// may apply its values or emit its validation error, so an older request
/// whose filesystem validation finishes later can never clobber a newer
/// destination / auto-accept setting, nor surface an obsolete error.
/// Submission order is whatever order the invocations reach this process —
/// never inferred from validation completion.
///
/// ADMISSION is synchronized with the commit phase (see
/// [`admit_receive_settings_revision`]): the reservation is made under the
/// same settings write lock that the commit's freshness re-check and
/// error/readiness effects take, so admission and publication linearize
/// together and a commit never publishes effects while a newer request is
/// already admitted with its validation in flight.
static SETTINGS_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// R2: test-only scheduler seam. The
// terminal's exact-writer probe (and the maintained commit-phase tests
// below) pause the production settings writer right after the lock is
// released and before the commit re-check — exactly the interleaving window
// through which the old frozen-outcome code let a stale error or an early
// readiness flip escape. Production builds never read it.
#[cfg(test)]
thread_local! {
    static TERMINAL_AFTER_SETTINGS_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// R2 (TAILDROP-TERMINAL-REWORK-1518): the writer-phase check — was this
/// request still the LATEST submission when its values were stored inside
/// the critical section? A stale request stores nothing. Returns `true`
/// (applied) or `false` (superseded before the store).
///
/// The STILL_CURRENT decision (which guards error emission, readiness and
/// the return value) MOVED to the commit phase: after the settings lock is
/// released and the terminal-supplied test hook fires, the commit closure
/// re-acquires the lock, reloads the revision and atomically applies only
/// the effects belonging to the then-current submission. No frozen boolean
/// crosses the lock boundary.
fn settings_revision_outcome(
    current: &std::sync::atomic::AtomicU64,
    submitted_revision: u64,
) -> bool {
    current.load(std::sync::atomic::Ordering::SeqCst) == submitted_revision + 1
}

/// A2 (TAILDROP-OWNER-A2-ONE-EXTRA-20261104): REVISION ADMISSION shares the
/// settings write lock with the commit phase's freshness load and
/// error/readiness effects.
///
/// The terminal-1526 counterexample: with an unsynchronized `fetch_add`, a
/// NEWER request could be admitted (and reach its held async validation)
/// between an OLDER request's commit freshness load and that older commit's
/// effects — so the obsolete request opened initial readiness / emitted its
/// error while a newer admitted request's validation was still held.
///
/// Admission now reserves the monotonic revision under the SAME lock that
/// the commit re-check and effects take, i.e. admission and commit
/// linearize together: while a request's commit holds the lock, no newer
/// request can be admitted (nor enter validation), and a commit only ever
/// publishes effects while its request is still the newest admitted one at
/// that point. The admission critical section is synchronous (no await) —
/// filesystem validation remains entirely outside the lock.
///
/// Production passes the real static counter; tests pass an isolated
/// counter with the same lock so the test harness exercises the exact
/// production admission mechanism.
fn admit_receive_settings_revision(
    state: &SharedReceiveSettings,
    counter: &std::sync::atomic::AtomicU64,
) -> u64 {
    let _settings = state
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// A2 ordering core for `set_receive_settings` (kept injectable so the real
/// submission/validation/application ordering contract is testable without a
/// Tauri runtime; production calls it with the real validation future, state
/// writer, error emitter and readiness gate).
///
/// Contract:
/// - Await the validation BEFORE any state write (a destination the receive
///   loop drains into must never be installed unjudged).
/// - The APPLIED decision is atomic with the state write: the writer
///   closure must run the revision check inside its own critical section
///   (the shared settings lock in production) and return `true` when the
///   values were stored. A newer submission can interleave only BEFORE
///   the check — never between the check and the stored values.
/// - The COMMIT phase performs the error emission and readiness effects
///   under a LOCK (the same lock in production) after a LIVE re-check of
///   the submission revision. A request whose revision is stale at commit
///   time is a silent no-op — no error, no readiness flip.
/// - `commit` receives the validation result and may emit the validation
///   error, flip readiness, and decide the return value. Its body runs
///   under the commit lock so all effects are atomic with the re-check.
/// - A superseded request is a silent no-op, including its return value.
#[allow(clippy::too_many_arguments)]
async fn apply_receive_settings_ordered<Fut, F, W, C>(
    save_dir: String,
    auto_accept: bool,
    validate: F,
    write: W,
    commit: C,
) -> Result<(), String>
where
    Fut: std::future::Future<Output = Result<String, String>>,
    F: FnOnce() -> Fut,
    W: FnOnce(&str, bool) -> bool,
    C: FnOnce(Result<String, String>) -> Result<(), String>,
{
    let validation = validate().await;
    // R1: check + store happen inside the writer's critical section — the
    // stored values and the freshness decision cannot be torn apart by a
    // newer submission between them.
    let applied = write(&save_dir, auto_accept);
    // R2: test hook — the terminal inserts a thread-local scheduler pause
    // here to validate the commit-phase re-check semantics. MUST remain
    // immediately after `write()` returns, before the commit call.
    #[cfg(test)]
    if let Some(f) = crate::TERMINAL_AFTER_SETTINGS_WRITE.with(|hook| hook.borrow_mut().take()) {
        f();
    }
    if !applied {
        // Superseded before the store: silent no-op on every axis (values,
        // error, return value, readiness).
        return Ok(());
    }
    // R2: commit phase — re-check currency under the commit lock, then
    // apply error/readiness effects and decide the return value only if
    // the request is STILL the latest submission at this linearization
    // point. A newer submission that landed since the writer lock was
    // released (or the test hook paused) suppresses all effects.
    commit(validation)
}

/// Update the shared receive settings the background receive task reads.
/// Ordering (A2): admit (revision, under the settings lock) → validate →
/// (still latest?) → store → emit-on-failure → gate. Requests are ordered
/// by SUBMISSION, never by validation completion, so a slow older request
/// can never overwrite a newer destination. The save
/// dir is validated before any state write or gate flip so the receive loop
/// can never drain into a path validation has not yet judged. On failure the
/// validation error is emitted here directly via `APP_HANDLE` — the loop's
/// own emit only fires after [`RECEIVE_FAILURE_THRESHOLD`] consecutive fetch
/// failures. `RECEIVE_READY` is then set unconditionally: it is
/// set-once-never-cleared, and leaving it unset on a bad dir would deadlock
/// the loop and silently kill receives.
#[tauri::command]
async fn set_receive_settings(
    state: tauri::State<'_, SharedReceiveSettings>,
    save_dir: String,
    auto_accept: bool,
) -> Result<(), String> {
    // A2: ADMIT the request BEFORE the async validation — the revision is
    // reserved under the settings write lock shared with the commit phase,
    // so submission and commit effects linearize together (a newer request
    // cannot be admitted while an older request's commit publishes). An
    // older request finishing later is then recognized as stale by the
    // commit re-check under the same lock.
    let revision = admit_receive_settings_revision(&state, &SETTINGS_REVISION);
    let validate_dir = save_dir.clone();
    apply_receive_settings_ordered(
        save_dir,
        auto_accept,
        || validate_save_dir_path(&validate_dir),
        |dir, aa| {
            // R1: the ATOMIC writer section — the revision check and the
            // state write share ONE lock acquisition, so no other request's
            // check/store can interleave between them. Values are stored even
            // when validation fails so behavior matches the frontend-driven
            // poll this replaces: an unusable dir surfaces as a persistent
            // `incoming-files-error` event, not as a silent revert.
            let mut settings = state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let applied = settings_revision_outcome(&SETTINGS_REVISION, revision);
            if applied {
                settings.save_dir = dir.to_string();
                settings.auto_accept = aa;
            }
            applied
        },
        |validation| {
            // R2: the COMMIT phase — re-check the revision under the SAME
            // settings lock, then atomically apply the terminal effects
            // (validation-error emission, receive-gate flip, return value).
            // No frozen boolean crosses the lock boundary, so a newer
            // submission that landed between the writer section and this
            // commit linearizes here: the obsolete request is a silent
            // no-op on every axis and never opens the gate.
            let _settings = state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let still_current =
                SETTINGS_REVISION.load(std::sync::atomic::Ordering::SeqCst) == revision + 1;
            if !still_current {
                return Ok(());
            }
            if let Err(msg) = &validation {
                // The loop's threshold-gated emit may never fire if the
                // (created) dir polls as an empty inbox, so surface the
                // error immediately.
                let app = APP_HANDLE
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .expect("set_receive_settings called before app setup");
                if let Err(e) = app.emit("incoming-files-error", msg.to_string()) {
                    log::debug!(
                        "set_receive_settings: emit incoming-files-error failed: {}",
                        e
                    );
                }
            }
            // TD-03: the receive gate opens on the LATEST request's commit,
            // on BOTH validation paths — gating on success would deadlock
            // the receive loop for an unusable dir. Set-once; a superseded
            // request never flips it.
            RECEIVE_READY.store(true, std::sync::atomic::Ordering::Release);
            validation.map(|_| ())
        },
    )
    .await
}

/// TD-03: readiness probe for the frontend/tests — true once the receive
/// loop is permitted to poll. Purely informational; the loop enforces the
/// gate itself.
#[tauri::command]
fn receive_ready() -> bool {
    RECEIVE_READY.load(std::sync::atomic::Ordering::Acquire)
}

// ============================================================
// TD-05: transfer receipts
// ============================================================

/// Page through the receipt store (contract: get_recent_receipts).
#[tauri::command]
fn get_recent_receipts(since_seq: u64, limit: usize) -> receipts::ReceiptPage {
    receipts::page_public(since_seq, limit)
}

/// Recover preserved staging files into the current save dir (contract:
/// recover_staging_files). Moves reuse move_file_into_dir guarantees —
/// exclusive-create, never overwrites, collision-resolved names — and the
/// staging dir is removed only once verifiably empty. Each landed file
/// records a normal "saved" receipt with its actual landing path.
#[tauri::command]
async fn recover_staging_files(path: String) -> Result<Vec<String>, String> {
    let staging = receipts::validate_staging_path(&path)?;
    let save_dir = {
        let app = APP_HANDLE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .expect("recover_staging_files called before app setup");
        let state = app.state::<SharedReceiveSettings>();
        let settings = state.read().unwrap_or_else(|p| p.into_inner());
        effective_save_dir(&settings.save_dir)
    };
    let mut landed = Vec::new();
    let entries: Vec<_> = std::fs::read_dir(&staging)
        .map_err(|e| format!("Cannot read staging dir '{}': {}", staging.display(), e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    for file_path in entries {
        let name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "Invalid filename in staging".to_string())?
            .to_string();
        let dest = tailscale::move_file_for_recovery(&file_path, &save_dir, &name)?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        // TD05-B: salvage is NOT a verified download. These bytes were
        // preserved from a FAILED batch — moving them to safety proves
        // nothing about download completeness (a deliberately shortened file
        // moves just as well). Record an explicit "salvaged" status, never an
        // ordinary successful-download receipt.
        receipts::ReceiptStore::record_salvaged(&name, &dest.to_string_lossy(), size);
        landed.push(dest.to_string_lossy().to_string());
    }
    // A1: remove the staging dir only when provably empty — a successful
    // enumeration yielding zero entries, removed with the NON-RECURSIVE
    // primitive. Any file that landed meanwhile, any nested/non-regular
    // entry, or an unreadable directory survives; the preserved dir keeps
    // being offered by the recovery scan.
    match tailscale::staging_is_known_empty(&staging) {
        Ok(true) => {
            let _ = std::fs::remove_dir(&staging);
        }
        _ => log::warn!(
            "recover_staging_files: preserving non-empty/unknown staging '{}'",
            staging.display()
        ),
    }
    Ok(landed)
}

/// Explicitly discard a preserved staging directory (contract:
/// discard_staging_dir — user action only, never automatic).
#[tauri::command]
async fn discard_staging_dir(path: String) -> Result<(), String> {
    let staging = receipts::validate_staging_path(&path)?;
    std::fs::remove_dir_all(&staging).map_err(|e| {
        format!(
            "Failed to discard staging dir '{}': {}",
            staging.display(),
            e
        )
    })
}

/// Re-scan for preserved staging dirs on demand (contract:
/// staging_recovery_found event; the same scan runs at startup).
///
/// Payload shape (reopen-finding fix): returns `{ dirs: [...] }` — matching
/// the emitted event AND the frontend's `StagingRecoveryFoundEvent` type.
/// The previous bare-array return made `result.dirs.length` read undefined
/// and fall into the catch handler, so the recovery banner never populated
/// through the scan route.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StagingRecoveryFound {
    dirs: Vec<receipts::StagingDir>,
}

#[tauri::command]
fn staging_recovery_scan() -> StagingRecoveryFound {
    StagingRecoveryFound {
        dirs: receipts::scan_staging_dirs(),
    }
}

/// Forward receipt-store broadcasts to the webview as `transfer-receipt`
/// events (contract). Laggy/absent receivers are fine — the frontend
/// re-syncs via get_recent_receipts.
async fn receipt_emitter(app: tauri::AppHandle) {
    let mut rx = receipts::ReceiptStore::subscribe_global();
    loop {
        match rx.recv().await {
            Ok(receipt) => {
                if let Err(e) = app.emit("transfer-receipt", &receipt) {
                    log::debug!("receipt emitter: emit failed: {}", e);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("receipt emitter lagged, skipped {}", skipped);
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Global app handle for commands that need state outside their Tauri
/// injection (set once at setup; the receipt recovery path reads receive
/// settings).
static APP_HANDLE: std::sync::Mutex<Option<tauri::AppHandle>> = std::sync::Mutex::new(None);

/// Background receive loop. On macOS/Windows without a Tailscale socket,
/// `fetch_incoming_files` IS the download (it runs `tailscale file get` into
/// the save dir), so running it from Rust keeps incoming files flowing while
/// the webview — and with it the old JS poll — is suspended in a minimized
/// window. On Linux the call is list-only and downloading stays in
/// `accept_file`.
///
/// Emits `incoming-files-changed` (payload: the current incoming-file list)
/// when the list changed versus the previous iteration, and on recovery from
/// an error state (even when empty — the frontend uses that to clear the
/// error). Emits `incoming-files-error` (payload: message) once at least
/// [`RECEIVE_FAILURE_THRESHOLD`] consecutive iterations failed. Both emits
/// are best-effort: the task is cancelled at app shutdown and must not
/// panic if the webview is already gone.
async fn receive_loop(app: tauri::AppHandle) {
    let mut prev_files: Vec<tailscale::IncomingFile> = Vec::new();
    let mut consecutive_failures: u32 = 0;
    let mut error_emitted = false;
    let mut fast_iterations: u32 = 0;
    // A3: signature of the last-published staging recovery groups — a
    // one-off CLI failure (or failed move) that preserved bytes publishes
    // exactly once, even across subsequent successful empty polls.
    let mut last_recovery_sig: String = String::new();

    loop {
        // TD-03: block until the frontend has pushed hydrated settings.
        // Sleep in short intervals rather than awaiting a notify so a crash
        // before the first set_receive_settings can't spin the CPU.
        while !RECEIVE_READY.load(std::sync::atomic::Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let save_dir = {
            let settings = app.state::<SharedReceiveSettings>();
            let settings = settings
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            settings.save_dir.clone()
        };
        let dir = effective_save_dir(&save_dir);

        match tailscale::fetch_incoming_files(&dir.to_string_lossy()).await {
            Ok(files) => {
                let changed = incoming_lists_differ(&prev_files, &files);
                if changed || error_emitted {
                    if let Err(e) = app.emit("incoming-files-changed", &files) {
                        log::debug!("receive loop: emit incoming-files-changed failed: {}", e);
                    }
                }
                if changed {
                    log::debug!(
                        "receive loop: incoming list changed ({} file(s)), event emitted",
                        files.len()
                    );
                    fast_iterations = RECEIVE_FAST_ITERATIONS;
                }
                prev_files = files;
                consecutive_failures = 0;
                error_emitted = false;
            }
            Err(msg) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                // A3: a single failed poll may have preserved recovery bytes
                // (a CLI drain that staged files before failing, or a failed
                // move) — publish them to the recovery UI immediately, not
                // only after the three-strike error threshold or a restart.
                reconcile_staging_after_failure(
                    &mut last_recovery_sig,
                    &|| receipts::scan_staging_dirs(),
                    &|dirs| {
                        if let Err(e) = app.emit(
                            "staging-recovery-found",
                            &StagingRecoveryFound {
                                dirs: dirs.to_vec(),
                            },
                        ) {
                            log::debug!("receive loop: emit staging-recovery-found failed: {}", e);
                        }
                    },
                );
                log::debug!(
                    "receive loop: fetch failed ({}): {}",
                    consecutive_failures,
                    msg
                );
                if consecutive_failures >= RECEIVE_FAILURE_THRESHOLD {
                    error_emitted = true;
                    if let Err(e) = app.emit("incoming-files-error", msg) {
                        log::debug!("receive loop: emit incoming-files-error failed: {}", e);
                    }
                }
            }
        }

        let interval = if fast_iterations > 0 {
            fast_iterations -= 1;
            RECEIVE_ACTIVE_INTERVAL
        } else {
            RECEIVE_IDLE_INTERVAL
        };
        tokio::time::sleep(interval).await;
    }
}

/// Compare incoming-file lists by (name, size) — the same identity the
/// frontend's notification dedup uses. Order-insensitive so a daemon-side
/// reshuffle doesn't spam `incoming-files-changed` events.
fn incoming_lists_differ(a: &[tailscale::IncomingFile], b: &[tailscale::IncomingFile]) -> bool {
    fn sorted_keys(files: &[tailscale::IncomingFile]) -> Vec<(&str, u64)> {
        let mut keys: Vec<(&str, u64)> = files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        keys.sort_unstable();
        keys
    }
    sorted_keys(a) != sorted_keys(b)
}

/// A3: publish newly preserved staging groups to the recovery UI immediately
/// after a receive failure — including ONE-off failures (not only the
/// three-strike error threshold) and failed moves, and WITHOUT requiring an
/// app restart or refocus. The primary disclosure path is the backend's own
/// emit; the frontend refocus scan is the fallback.
///
/// De-duplication: `last` holds the signature of the last-published group
/// set, so repeated failures (or a successful empty poll in between) do not
/// re-emit the same groups; a genuinely new or changed set emits once. An
/// empty scan clears the signature so a later, distinct preservation group
/// re-publishes. (`scan` and `emit` are injectable seams for tests;
/// production scans the staging root and emits `staging-recovery-found`.)
fn reconcile_staging_after_failure(
    last: &mut String,
    scan: &dyn Fn() -> Vec<receipts::StagingDir>,
    emit: &dyn Fn(&[receipts::StagingDir]),
) {
    let dirs = scan();
    let sig = staging_dir_signature(&dirs);
    if sig.is_empty() {
        *last = String::new();
        return;
    }
    if sig == *last {
        return;
    }
    *last = sig;
    emit(&dirs);
}

/// Signature of a set of recovery groups (sorted staging paths) — used to
/// publish each distinct group set exactly once.
fn staging_dir_signature(dirs: &[receipts::StagingDir]) -> String {
    let mut paths: Vec<&str> = dirs.iter().map(|d| d.path.as_str()).collect();
    paths.sort_unstable();
    paths.join("\u{1f}")
}

#[tauri::command]
fn get_debug_logs() -> Vec<debug_log::LogEntry> {
    debug_log::snapshot()
}

#[tauri::command]
fn get_env_info() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

/// TD-05: Reveal a saved file in the system file manager (Finder/Explorer/Files).
/// Platform-specific: macOS uses `open -R`, Windows uses `explorer /select`,
/// Linux uses `xdg-open` on the parent directory (no universal "select" flag).
#[tauri::command]
async fn show_in_folder(path: String) -> Result<(), String> {
    use std::process::Command;

    let file_path = std::path::Path::new(&path);
    if !file_path.exists() {
        return Err(format!("File not found: {}", path));
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| format!("Failed to open Finder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| format!("Failed to open Explorer: {}", e))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux: no universal "select file" flag; open the parent directory.
        let parent = file_path.parent().unwrap_or(file_path);
        Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {}", e))?;
    }

    Ok(())
}

/// Self-heal the known WebKitGTK-on-Wayland crash in our AppImage builds
/// (observed: TailDrop 0.14.0 on Omarchy/Hyprland, Mesa 26.2.2 Intel —
/// WebKitWebProcess aborts during GPU init with
/// "Could not create default EGL display: EGL_BAD_PARAMETER").
///
/// The AppImage bundles the ubuntu-24.04 build runner's WebKit but not the
/// EGL/GL stack, so the bundled web process initializes against the host's
/// Mesa; that mismatch is a well-known Tauri/WebKitGTK AppImage failure
/// mode. Setting these variables before `tauri::Builder` (hence before any
/// WebKitWebProcess spawns) forces software rendering, which sidesteps EGL
/// display creation entirely.
///
/// Semantics: only ever SET — never overwrite — so a user who deliberately
/// configures compositing keeps their value, and logging both the pre-set
/// and applied state leaves a trace in debug logs. Linux-only by design:
/// WKWebView on macOS/Windows is a different engine and these variables are
/// meaningless there.
#[cfg(target_os = "linux")]
fn apply_webkit_linux_env_workarounds() {
    for key in [
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
    ] {
        match webkit_env_decision(std::env::var(key).ok().as_deref()) {
            Some(value) => {
                // SAFETY: single-threaded startup, before any threads spawn.
                unsafe { std::env::set_var(key, value) };
                log::info!("{key} was unset; set to {value:?} (WebKitGTK Wayland EGL workaround)");
            }
            None => {
                log::debug!("{key} already set by user; leaving untouched");
            }
        }
    }
}

/// Pure decision core for [`apply_webkit_linux_env_workarounds`], kept
/// `#[cfg]`-free so its never-overwrite contract is tested on every
/// platform's CI (same portability rule as the closure-injected CLI tests).
/// Off-Linux the only caller is the test suite, hence the targeted allow.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn webkit_env_decision(existing: Option<&str>) -> Option<&'static str> {
    match existing {
        Some(_) => None,
        None => Some("1"),
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_webkit_linux_env_workarounds() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Logger first: the workaround's log lines land only after a logger is
    // installed (log's global max level is Off until then) — review finding.
    // Env vars are still set well before tauri::Builder / any web process.
    debug_log::init();
    apply_webkit_linux_env_workarounds();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(RwLock::new(ReceiveSettings {
            save_dir: default_download_dir().to_string_lossy().to_string(),
            auto_accept: false,
        }))
        .setup(|app| {
            // The receive loop must live in Rust: WKWebView suspends DOM
            // timers in a minimized window, which used to stop the JS poll
            // that drives the CLI download on socket-less macOS/Windows.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(receive_loop(handle));
            // TD-05: forward receipt-store broadcasts to the webview.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(receipt_emitter(handle));
            // TD-05: global handle for commands reading shared state outside
            // Tauri injection (staging recovery needs the save dir).
            *APP_HANDLE.lock().unwrap_or_else(|p| p.into_inner()) = Some(app.handle().clone());
            // TD-05: discover preserved staging dirs from a previous crashed
            // run and notify (discover-and-notify — never auto-delete).
            // Wrapped as { dirs: [...] } to match the frontend event guard
            // (reopen-finding fix — the bare array was rejected).
            let dirs = receipts::scan_staging_dirs();
            if !dirs.is_empty() {
                log::warn!(
                    "startup: {} preserved staging dir(s) found — emitting staging-recovery-found",
                    dirs.len()
                );
                let _ = app.emit("staging-recovery-found", &StagingRecoveryFound { dirs });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
            validate_save_dir,
            set_receive_settings,
            receive_ready,
            get_recent_receipts,
            recover_staging_files,
            discard_staging_dir,
            staging_recovery_scan,
            show_in_folder,
            get_debug_logs,
            get_env_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the TailDrop 0.14.0-on-Omarchy crash
    /// ("Could not create default EGL display: EGL_BAD_PARAMETER" abort in
    /// WebKitWebProcess): the in-process workaround must apply when the
    /// variables are unset and must NEVER overwrite a user-set value —
    /// clobbering a deliberate `WEBKIT_DISABLE_DMABUF_RENDERER=0` (or any
    /// tuning like `=1`-vs-empty) would silently change a working setup.
    #[test]
    fn webkit_env_workaround_sets_when_unset_never_overwrites() {
        assert_eq!(webkit_env_decision(None), Some("1"));
        assert_eq!(webkit_env_decision(Some("0")), None);
        assert_eq!(webkit_env_decision(Some("1")), None);
        assert_eq!(webkit_env_decision(Some("")), None);
    }

    /// Reopen-finding regression: the staging-recovery payload (command AND
    /// event use this shape) must serialize as `{ dirs: [...] }` with
    /// camelCase fields — the exact shape the frontend's
    /// `isStagingRecoveryEvent` guard and `result.dirs.length` access expect.
    /// A bare array previously made both routes fail silently.
    #[test]
    fn staging_recovery_payload_matches_frontend_contract() {
        let payload = StagingRecoveryFound {
            dirs: vec![receipts::StagingDir {
                path: "/tmp/taildrop-accept-abc".to_string(),
                files: vec![receipts::StagingFile {
                    name: "photo.jpg".to_string(),
                    size: 42,
                }],
            }],
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(
            json.get("dirs").is_some_and(|d| d.is_array()),
            "payload must be an object with a dirs array, got: {}",
            json
        );
        let dir0 = &json["dirs"][0];
        assert_eq!(dir0["path"], "/tmp/taildrop-accept-abc");
        assert_eq!(dir0["files"][0]["name"], "photo.jpg");
        assert_eq!(dir0["files"][0]["size"], 42);
        // A bare array is exactly the regression — assert the object form.
        assert!(json.is_object(), "must not serialize as a bare array");
    }

    fn file(name: &str, size: u64) -> tailscale::IncomingFile {
        tailscale::IncomingFile {
            name: name.to_string(),
            size,
            peer_name: None,
        }
    }

    #[test]
    fn lists_differ_detects_new_and_removed_files() {
        let a = [file("a.txt", 10)];
        assert!(incoming_lists_differ(
            &a,
            &[file("a.txt", 10), file("b.txt", 5)]
        ));
        assert!(incoming_lists_differ(
            &[file("a.txt", 10), file("b.txt", 5)],
            &a
        ));
    }

    #[test]
    fn lists_differ_detects_size_change() {
        assert!(incoming_lists_differ(
            &[file("a.txt", 10)],
            &[file("a.txt", 11)]
        ));
    }

    #[test]
    fn lists_differ_ignores_order_and_peer_name() {
        let a = [file("a.txt", 10), file("b.txt", 5)];
        let mut b = [file("b.txt", 5), file("a.txt", 10)];
        b[1].peer_name = Some("peer".to_string());
        assert!(!incoming_lists_differ(&a, &b));
    }

    #[test]
    fn lists_differ_counts_duplicates() {
        assert!(incoming_lists_differ(
            &[file("a.txt", 10)],
            &[file("a.txt", 10), file("a.txt", 10)]
        ));
    }

    // ============================================================
    // A2 (TAILDROP-FIXES-20261104): latest receive-settings request wins
    // ============================================================

    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};

    type WriteLog = Arc<Mutex<Vec<(String, bool)>>>;
    type ErrorLog = Arc<Mutex<Vec<String>>>;

    fn recording_hooks() -> (WriteLog, ErrorLog, Arc<AtomicU64>) {
        (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AtomicU64::new(0)),
        )
    }

    /// R2 production-shaped writer: performs the revision check inside its
    /// (simulated) critical section and stores values only when the request
    /// is still the latest submission. Returns `true` if the values were
    /// stored (writer-phase applied); the separate commit closure decides
    /// whether error/readiness/return effects are applied.
    fn checked_write(
        counter: &Arc<AtomicU64>,
        submitted_revision: u64,
        log: &WriteLog,
    ) -> impl FnOnce(&str, bool) -> bool + 'static {
        let counter = counter.clone();
        let log = log.clone();
        move |dir, aa| {
            let applied = settings_revision_outcome(&counter, submitted_revision);
            if applied {
                log.lock().unwrap().push((dir.to_string(), aa));
            }
            applied
        }
    }

    /// Guard-disabled writer: every request looks latest and stores
    /// unconditionally, reproducing the BASELINE no-ordering behavior for
    /// the semantic guard-disable controls.
    fn unchecked_write(log: &WriteLog) -> impl FnOnce(&str, bool) -> bool + 'static {
        let log = log.clone();
        move |dir, aa| {
            log.lock().unwrap().push((dir.to_string(), aa));
            true // applied: always
        }
    }

    /// R2 commit shorthand for tests that always accept the validation
    /// result (no re-check — used in tests where the writer already
    /// controls the ordering and the commit just relays).
    fn current_commit(
        counter: &Arc<AtomicU64>,
        submitted_revision: u64,
        errors: &ErrorLog,
        readies: &Arc<AtomicU64>,
    ) -> impl FnOnce(Result<String, String>) -> Result<(), String> + 'static {
        let counter = counter.clone();
        let errors = errors.clone();
        let readies = readies.clone();
        move |validation| {
            let still_current = counter.load(AtomicOrdering::SeqCst) == submitted_revision + 1;
            if !still_current {
                return Ok(());
            }
            if let Err(msg) = &validation {
                errors.lock().unwrap().push(msg.to_string());
            }
            readies.fetch_add(1, AtomicOrdering::SeqCst);
            validation.map(|_| ())
        }
    }

    /// Guard-disabled commit: every request appears still_current at commit
    /// time, reproducing the old stale-error/early-readiness behaviour.
    fn unchecked_commit(
        errors: &ErrorLog,
        readies: &Arc<AtomicU64>,
    ) -> impl FnOnce(Result<String, String>) -> Result<(), String> + 'static {
        let errors = errors.clone();
        let readies = readies.clone();
        move |validation| {
            if let Err(msg) = &validation {
                errors.lock().unwrap().push(msg.to_string());
            }
            readies.fetch_add(1, AtomicOrdering::SeqCst);
            validation.map(|_| ())
        }
    }

    /// A2 core: hold A's validation open, submit and finish B, then release
    /// A — B must stay authoritative (an older request whose validation
    /// finishes later must not overwrite a newer destination, and must not
    /// emit an obsolete validation error).
    #[tokio::test]
    async fn a2_stale_request_never_overwrites_newer() {
        let counter = Arc::new(AtomicU64::new(0));
        let (writes, errors, readies) = recording_hooks();
        // Shared settings lock: submission uses the REAL synchronized
        // production admission helper (same mechanism as set_receive_settings).
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));

        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let rev_a = admit_receive_settings_revision(&state, &counter); // 0
        let fut_a = apply_receive_settings_ordered(
            "/dir/A".to_string(),
            false,
            move || async move {
                hold_rx.await.map_err(|e| e.to_string())?;
                Ok("canonical-A".to_string())
            },
            checked_write(&counter, rev_a, &writes),
            current_commit(&counter, rev_a, &errors, &readies),
        );

        // B submits while A's validation is still in flight, and completes.
        let rev_b = admit_receive_settings_revision(&state, &counter); // 1
        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            true,
            || async { Ok("canonical-B".to_string()) },
            checked_write(&counter, rev_b, &writes),
            current_commit(&counter, rev_b, &errors, &readies),
        )
        .await;
        assert_eq!(result_b, Ok(()));

        // Now A's validation completes (late). It must be a no-op.
        assert!(hold_tx.send(()).is_ok());
        assert_eq!(fut_a.await, Ok(()));

        // B stays authoritative on every axis: values, auto-accept, error.
        assert_eq!(
            *writes.lock().unwrap(),
            vec![("/dir/B".to_string(), true)],
            "the NEWER request's values must be the lasting ones"
        );
        assert!(
            errors.lock().unwrap().is_empty(),
            "an obsolete validation outcome must never emit an error"
        );
        assert_eq!(
            readies.load(AtomicOrdering::SeqCst),
            1,
            "the readiness gate opens exactly once, for the latest request"
        );
    }

    /// A2: older-invalid / newer-valid — the stale request's error message
    /// must never surface after the newer valid request applied.
    #[tokio::test]
    async fn a2_older_invalid_never_emits_after_newer_valid() {
        let counter = Arc::new(AtomicU64::new(0));
        let (writes, errors, readies) = recording_hooks();
        // Shared settings lock: submission uses the REAL synchronized
        // production admission helper (same mechanism as set_receive_settings).
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));

        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let rev_a = admit_receive_settings_revision(&state, &counter);
        let fut_a = apply_receive_settings_ordered(
            "/dir/A".to_string(),
            false,
            move || async move {
                hold_rx.await.map_err(|e| e.to_string())?;
                Err("Directory '/dir/A' does not exist".to_string())
            },
            checked_write(&counter, rev_a, &writes),
            current_commit(&counter, rev_a, &errors, &readies),
        );

        let rev_b = admit_receive_settings_revision(&state, &counter);
        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            true,
            || async { Ok("canonical-B".to_string()) },
            checked_write(&counter, rev_b, &writes),
            current_commit(&counter, rev_b, &errors, &readies),
        )
        .await;
        assert_eq!(result_b, Ok(()));

        assert!(hold_tx.send(()).is_ok());
        // The STALE request's own return value is suppressed too.
        assert_eq!(fut_a.await, Ok(()), "stale outcome must be a silent no-op");

        assert_eq!(*writes.lock().unwrap(), vec![("/dir/B".to_string(), true)]);
        assert!(
            errors.lock().unwrap().is_empty(),
            "no obsolete invalid-directory error may be emitted"
        );
        assert_eq!(readies.load(AtomicOrdering::SeqCst), 1);
    }

    /// A2: older-valid / newer-INVALID — the newer request still applies its
    /// values (invalid-directory semantics: values stored so the loop's
    /// persistent error can surface) and emits ITS error; the stale older
    /// request contributes nothing.
    #[tokio::test]
    async fn a2_newer_invalid_applies_values_and_emits_its_own_error() {
        let counter = Arc::new(AtomicU64::new(0));
        let (writes, errors, readies) = recording_hooks();
        // Shared settings lock: submission uses the REAL synchronized
        // production admission helper (same mechanism as set_receive_settings).
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));

        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let rev_a = admit_receive_settings_revision(&state, &counter);
        let fut_a = apply_receive_settings_ordered(
            "/dir/A".to_string(),
            false,
            move || async move {
                hold_rx.await.map_err(|e| e.to_string())?;
                Ok("canonical-A".to_string())
            },
            checked_write(&counter, rev_a, &writes),
            current_commit(&counter, rev_a, &errors, &readies),
        );

        let rev_b = admit_receive_settings_revision(&state, &counter);
        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            false,
            || async { Err("Directory '/dir/B' does not exist".to_string()) },
            checked_write(&counter, rev_b, &writes),
            current_commit(&counter, rev_b, &errors, &readies),
        )
        .await;
        assert_eq!(
            result_b,
            Err("Directory '/dir/B' does not exist".to_string())
        );

        assert!(hold_tx.send(()).is_ok());
        assert_eq!(fut_a.await, Ok(()));

        // The newer (invalid) request's values are the lasting ones — exactly
        // the established invalid-directory semantics (persistent error
        // surfacing), and its OWN error is the one emitted.
        assert_eq!(*writes.lock().unwrap(), vec![("/dir/B".to_string(), false)]);
        assert_eq!(
            *errors.lock().unwrap(),
            vec!["Directory '/dir/B' does not exist".to_string()]
        );
        assert_eq!(readies.load(AtomicOrdering::SeqCst), 1);
    }

    /// A2 targeted guard-disable control: the SAME production core with the
    /// staleness guard neutered (every request appears latest) reproduces the
    /// BASELINE pre-fix behavior — the older, slower-validating request
    /// overwrites the newer destination. This proves the ordering tests fail
    /// without the fix (the baseline race is real and the guard is what
    /// stops it).
    #[tokio::test]
    async fn a2_guard_disabled_reproduces_baseline_overwrite() {
        let (writes, errors, readies) = recording_hooks();
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let fut_a = apply_receive_settings_ordered(
            "/dir/A".to_string(),
            false,
            move || async move {
                hold_rx.await.map_err(|e| e.to_string())?;
                Ok("canonical-A".to_string())
            },
            unchecked_write(&writes), // guard disabled: A always looks latest
            unchecked_commit(&errors, &readies), // commit guard disabled too
        );

        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            true,
            || async { Ok("canonical-B".to_string()) },
            unchecked_write(&writes), // guard disabled: B always looks latest
            unchecked_commit(&errors, &readies),
        )
        .await;
        assert_eq!(result_b, Ok(()));

        assert!(hold_tx.send(()).is_ok());
        assert_eq!(fut_a.await, Ok(()));

        // Baseline semantics: A (slow validation) overwrites B — the bug the
        // ordering guard removes. The assertion documents that without the
        // guard, the OLDER request's values win.
        assert_eq!(
            *writes.lock().unwrap(),
            vec![("/dir/B".to_string(), true), ("/dir/A".to_string(), false)],
            "guard-disabled control must reproduce the baseline overwrite"
        );
        assert_eq!(readies.load(AtomicOrdering::SeqCst), 2);
    }

    /// R1 (terminal repro): the OLD code's freshness check ran OUTSIDE the
    /// writer — a scheduler interruption between the check and the state
    /// store let a NEWER submission be overwritten by a stale request that
    /// finished later, and its obsolete error emitted. The new contract runs
    /// the whole decision+store inside ONE critical section, so the closest
    /// remaining interleaving is: the decision passes (A is current), the
    /// section pauses, B submits and applies (blocked on the same lock), A
    /// stores transiently on resume, and the end-of-section re-check
    /// suppresses A's obsolete error. B's lock-serialized store is final.
    #[tokio::test]
    async fn a2_submission_during_critical_section_older_invalid_never_wins_or_emits() {
        let counter = Arc::new(AtomicU64::new(1)); // A (rev 0) already submitted
        let state = Arc::new(Mutex::new(String::new()));
        // Admission lock: the REAL synchronized production helper requires
        // the shared settings RwLock (this test models the writer/commit
        // critical section with a separate Mutex, but exercises the genuine
        // production submission mechanism through the helper).
        let admission_state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let readies = Arc::new(AtomicU64::new(0));
        let (a_paused_tx, a_paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_a_tx, resume_a_rx) = std::sync::mpsc::channel::<()>();

        let counter_a = counter.clone();
        let state_a = state.clone();
        let errors_a = errors.clone();
        let readies_a = readies.clone();
        // Separate clone for the writer closure (counter_a stays for commit)
        let counter_a_writer = counter_a.clone();
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            // Hook: pause after write() returns, before commit().
            // This is the exact interleaving window the terminal's exact-writer
            // probe targets: the writer lock is released, the frozen-outcome
            // code let an obsolete effect escape past here.
            TERMINAL_AFTER_SETTINGS_WRITE.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    a_paused_tx.send(()).unwrap();
                    resume_a_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                }));
            });
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Err("obsolete A error".to_string()) as Result<String, String> },
                move |dir, _aa| {
                    let mut settings = state_a.lock().unwrap();
                    let applied = counter_a_writer.load(AtomicOrdering::SeqCst) == 1;
                    if applied {
                        *settings = dir.to_string();
                    }
                    applied
                },
                move |validation| {
                    // R2: commit-phase re-check — A was superseded if B
                    // submitted during the pause above.
                    let still_current = counter_a.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    if let Err(msg) = &validation {
                        errors_a.lock().unwrap().push(msg.to_string());
                    }
                    readies_a.fetch_add(1, AtomicOrdering::SeqCst);
                    validation.map(|_| ())
                },
            ))
        });

        a_paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // B submits while A is paused at the hook — B's writer and commit
        // run immediately (no lock contention since A's writer released the
        // lock before the hook). B is the latest submission.
        let rev_b = admit_receive_settings_revision(&admission_state, &counter); // 1
        let counter_b = counter.clone();
        // Separate clone for the writer closure (counter_b stays for commit)
        let counter_b_writer = counter_b.clone();
        let state_b = state.clone();
        let readies_b = readies.clone();
        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            true,
            || async { Ok("/dir/B".to_string()) },
            move |dir, _aa| {
                let mut settings = state_b.lock().unwrap();
                let applied = settings_revision_outcome(&counter_b_writer, rev_b);
                if applied {
                    *settings = dir.to_string();
                }
                applied
            },
            move |validation| {
                let still_current = counter_b.load(AtomicOrdering::SeqCst) == rev_b + 1;
                if !still_current {
                    return Ok(());
                }
                readies_b.fetch_add(1, AtomicOrdering::SeqCst);
                validation.map(|_| ())
            },
        )
        .await;
        assert_eq!(result_b, Ok(()));

        // Resume A — its commit re-check sees counter != 1 → suppresses.
        resume_a_tx.send(()).unwrap();
        let result_a = a.join().expect("A panicked");

        assert_eq!(
            result_a,
            Ok(()),
            "superseded request must be a silent no-op"
        );
        assert_eq!(result_b, Ok(()));
        assert_eq!(
            *state.lock().unwrap(),
            "/dir/B",
            "a stale in-section store must never be the final destination"
        );
        assert!(
            errors.lock().unwrap().is_empty(),
            "an obsolete A validation error must never emit"
        );
        assert_eq!(
            readies.load(AtomicOrdering::SeqCst),
            1,
            "readiness opens once, for the NEWEST request's commit (A is superseded)"
        );
    }

    /// R2: the same commit-phase interleaving with the OTHER
    /// validity order — A VALID (decided, then paused inside its critical
    /// section), B INVALID submitted and applied meanwhile. B's stored
    /// values and B's OWN error stay authoritative; A's transient store is
    /// corrected by B's lock-serialized store, and A stays a silent no-op.
    #[tokio::test]
    async fn a2_submission_during_critical_section_newer_invalid_wins() {
        let counter = Arc::new(AtomicU64::new(1));
        let state = Arc::new(Mutex::new(String::new()));
        // Admission lock: the REAL synchronized production helper requires
        // the shared settings RwLock (this test models the writer/commit
        // critical section with a separate Mutex, but exercises the genuine
        // production submission mechanism through the helper).
        let admission_state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let (a_paused_tx, a_paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_a_tx, resume_a_rx) = std::sync::mpsc::channel::<()>();

        let counter_a = counter.clone();
        let state_a = state.clone();
        let errors_a = errors.clone();
        // Separate clone for the writer closure (counter_a stays for commit)
        let counter_a_writer = counter_a.clone();
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            // Hook: pause after write() returns, before commit().
            TERMINAL_AFTER_SETTINGS_WRITE.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    a_paused_tx.send(()).unwrap();
                    resume_a_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                }));
            });
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Ok("canonical-A".to_string()) },
                move |dir, _aa| {
                    let mut settings = state_a.lock().unwrap();
                    let applied = counter_a_writer.load(AtomicOrdering::SeqCst) == 1;
                    if applied {
                        *settings = dir.to_string();
                    }
                    applied
                },
                move |validation| {
                    let still_current = counter_a.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    if let Err(msg) = &validation {
                        errors_a.lock().unwrap().push(msg.to_string());
                    }
                    validation.map(|_| ())
                },
            ))
        });

        a_paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        let rev_b = admit_receive_settings_revision(&admission_state, &counter); // 1
        let counter_b = counter.clone();
        // Separate clone for the writer closure (counter_b stays for commit)
        let counter_b_writer = counter_b.clone();
        let state_b = state.clone();
        let errors_b = errors.clone();
        let result_b = apply_receive_settings_ordered(
            "/dir/B".to_string(),
            false,
            || async { Err("Directory '/dir/B' does not exist".to_string()) },
            move |dir, _aa| {
                let mut settings = state_b.lock().unwrap();
                let applied = settings_revision_outcome(&counter_b_writer, rev_b);
                if applied {
                    *settings = dir.to_string();
                }
                applied
            },
            move |validation| {
                let still_current = counter_b.load(AtomicOrdering::SeqCst) == rev_b + 1;
                if !still_current {
                    return Ok(());
                }
                if let Err(msg) = &validation {
                    errors_b.lock().unwrap().push(msg.to_string());
                }
                validation.map(|_| ())
            },
        )
        .await;
        assert_eq!(
            result_b,
            Err("Directory '/dir/B' does not exist".to_string()),
            "the invalid NEWER request returns its own error to its caller"
        );

        resume_a_tx.send(()).unwrap();
        let result_a = a.join().expect("A panicked");
        assert_eq!(
            result_a,
            Ok(()),
            "superseded request must be a silent no-op"
        );
        assert_eq!(
            *state.lock().unwrap(),
            "/dir/B",
            "the newer (invalid, per established stored-values semantics) destination is the lasting one"
        );
        assert_eq!(
            *errors.lock().unwrap(),
            vec!["Directory '/dir/B' does not exist".to_string()],
            "only the NEWER request's own error is emitted"
        );
    }

    /// R2 (terminal 1518 exact regression #1): the production writer +
    /// production commit closures, with the scheduler hook pausing between
    /// write and commit. Older INVALID A writes /dir/A and pauses; newer
    /// VALID B submits, validates, writes /dir/B and commits successfully.
    /// Resumed A must be a SILENT no-op: no obsolete error, Ok return,
    /// readiness untouched, final destination B.
    #[test]
    fn a2_terminal1518_obsolete_error_after_newer_valid_completed() {
        let counter = Arc::new(AtomicU64::new(1)); // A (rev 0) already submitted
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

        let ca = counter.clone();
        let sa = state.clone();
        let ea = errors.clone();
        let ra = ready.clone();
        // The writer closure consumes its own clones; the commit closure
        // keeps the originals (each closure gets exactly what it needs).
        let ca_writer = ca.clone();
        let sa_writer = sa.clone();
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            TERMINAL_AFTER_SETTINGS_WRITE.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    paused_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                }));
            });
            // Exact production writer + commit shapes (extracted from
            // set_receive_settings; only the statics are rebound).
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Err("obsolete A validation error".to_string()) },
                move |dir, aa| {
                    let mut settings = sa_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&ca_writer, 0);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sa.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = ca.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    if let Err(msg) = &validation {
                        ea.lock().unwrap().push(msg.to_string());
                    }
                    ra.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // B submits and FULLY completes while A is paused at the hook.
        // Admission goes through the REAL synchronized production helper
        // (A's writer released the lock before the hook, so B is admitted
        // immediately — same schedule as the terminal repro).
        let b_rev = admit_receive_settings_revision(&state, &counter); // 1
        let cb = counter.clone();
        let sb = state.clone();
        let rb = ready.clone();
        // Writer consumes clones; commit keeps originals.
        let cb_writer = cb.clone();
        let sb_writer = sb.clone();
        let b = {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/B".to_string(),
                true,
                || async { Ok("/dir/B".to_string()) },
                move |dir, aa| {
                    let mut settings = sb_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&cb_writer, b_rev);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sb.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = cb.load(AtomicOrdering::SeqCst) == b_rev + 1;
                    if !still_current {
                        return Ok(());
                    }
                    rb.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        };
        assert_eq!(b, Ok(()));
        {
            let s = state.read().unwrap();
            assert_eq!((s.save_dir.as_str(), s.auto_accept), ("/dir/B", true));
        }
        // Resume A — its commit phase re-checks the revision and suppresses.
        resume_tx.send(()).unwrap();
        let result_a = a.join().unwrap();
        assert!(
            errors.lock().unwrap().is_empty(),
            "obsolete A error must NOT be emitted AFTER newer valid B fully completed"
        );
        assert_eq!(
            result_a,
            Ok(()),
            "superseded A must return silent success, not obsolete validation failure"
        );
    }

    /// R2 (terminal 1518 exact regression #2): initial-first A writes and
    /// pauses at the hook with readiness still closed; B submits and its
    /// async validation is HELD. A's commit re-check sees the newer
    /// submission and must NOT open the receive gate; only B's post-
    /// validation commit opens it.
    #[test]
    fn a2_terminal1518_stale_initial_completion_opens_gate_while_newest_validation_pending() {
        let counter = Arc::new(AtomicU64::new(1)); // A (rev 0) already submitted
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

        let ca = counter.clone();
        let sa = state.clone();
        let ra = ready.clone();
        // Writer consumes clones; commit keeps originals.
        let ca_writer = ca.clone();
        let sa_writer = sa.clone();
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            TERMINAL_AFTER_SETTINGS_WRITE.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    paused_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                }));
            });
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Ok("/dir/A".to_string()) },
                move |dir, aa| {
                    let mut settings = sa_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&ca_writer, 0);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sa.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = ca.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    ra.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // B submits while A is paused; B's validation is HELD. Admission
        // uses the REAL synchronized production helper (the lock is free
        // while A waits at the hook, so B is admitted immediately).
        let b_rev = admit_receive_settings_revision(&state, &counter); // 1
        let (validated_tx, validated_rx) = tokio::sync::oneshot::channel::<()>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let cb = counter.clone();
        let sb = state.clone();
        let rb = ready.clone();
        // Writer consumes clones; commit keeps originals.
        let cb_writer = cb.clone();
        let sb_writer = sb.clone();
        let b = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/B".to_string(),
                true,
                || async move {
                    entered_tx.send(()).unwrap();
                    validated_rx.await.unwrap();
                    Ok("/dir/B".to_string())
                },
                move |dir, aa| {
                    let mut settings = sb_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&cb_writer, b_rev);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sb.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = cb.load(AtomicOrdering::SeqCst) == b_rev + 1;
                    if !still_current {
                        return Ok(());
                    }
                    rb.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        // Resume A while B's validation is still held.
        resume_tx.send(()).unwrap();
        let result_a = a.join().unwrap();
        assert_eq!(result_a, Ok(()), "superseded A returns silent success");
        let ready_before_latest_validation = ready.load(AtomicOrdering::Acquire);
        let destination_before_latest_validation = {
            let s = state.read().unwrap();
            (s.save_dir.clone(), s.auto_accept)
        };
        // Release B's validation, then B's commit opens the gate.
        validated_tx.send(()).unwrap();
        assert_eq!(b.join().unwrap(), Ok(()));
        assert!(
            !ready_before_latest_validation,
            "initial readiness must NOT open from obsolete A while newest B validation is held"
        );
        assert_eq!(
            destination_before_latest_validation,
            ("/dir/A".to_string(), false)
        );
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "B's post-validation commit opens the gate"
        );
    }

    /// A2 (TAILDROP-OWNER-A2-ONE-EXTRA-20261104): the terminal-1526
    /// post-freshness-load window, valid A. A's commit has passed its ACTUAL
    /// freshness load and holds the settings write lock; B attempts to
    /// SUBMIT. Synchronized admission blocks B at the lock — B acquires NO
    /// revision and enters NO validation while A's commit holds
    /// synchronization. A's commit then completes as the still-newest
    /// admitted request (opening readiness legitimately); only afterwards is
    /// B admitted, and B's validation/readiness/error outcomes are proven —
    /// B wins the final destination.
    #[test]
    fn a2_owner1526_postload_admission_blocked_until_commit_ends_valid_a() {
        let counter = Arc::new(AtomicU64::new(0));
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

        // A admits through the REAL synchronized production helper.
        assert_eq!(admit_receive_settings_revision(&state, &counter), 0);
        let (ca, sa, ea, ra) = (
            counter.clone(),
            state.clone(),
            errors.clone(),
            ready.clone(),
        );
        // Writer consumes clones; commit keeps originals.
        let (ca_writer, sa_writer) = (ca.clone(), sa.clone());
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Ok("/dir/A".to_string()) },
                move |dir, aa| {
                    let mut settings = sa_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&ca_writer, 0);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    // Production commit body: freshness load under the lock,
                    // then — AFTER the load, BEFORE the effects — the
                    // deterministic scheduler pause (the write guard stays
                    // held for the whole pause).
                    let _settings = sa.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = ca.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    paused_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                    if let Err(msg) = &validation {
                        ea.lock().unwrap().push(msg.to_string());
                    }
                    ra.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // B attempts admission while A's commit holds the lock. A barrier
        // guarantees B has ENTERED the admission call before we assert on
        // its (blocked) state — deterministic in both the fixed and the
        // admission-lock-disabled worlds.
        let (adm_enter_tx, adm_enter_rx) = std::sync::mpsc::channel::<()>();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel::<u64>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (validated_tx, validated_rx) = tokio::sync::oneshot::channel::<()>();
        let (cb, sb, rb) = (counter.clone(), state.clone(), ready.clone());
        let b = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            adm_enter_tx.send(()).unwrap();
            // REAL synchronized admission: blocks while A's commit holds the
            // settings write lock.
            let rev_b = admit_receive_settings_revision(&sb, &cb);
            admitted_tx.send(rev_b).unwrap();
            // Writer consumes clones; commit keeps originals.
            let (cb_writer, sb_writer) = (cb.clone(), sb.clone());
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/B".to_string(),
                true,
                || async move {
                    entered_tx.send(()).unwrap();
                    validated_rx.await.unwrap();
                    Ok("/dir/B".to_string())
                },
                move |dir, aa| {
                    let mut settings = sb_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&cb_writer, rev_b);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sb.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = cb.load(AtomicOrdering::SeqCst) == rev_b + 1;
                    if !still_current {
                        return Ok(());
                    }
                    rb.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        adm_enter_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // Terminal-1526 semantics: while A holds commit synchronization, B
        // has NOT acquired a revision and has NOT entered validation.
        assert!(
            admitted_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err(),
            "B's admission must be BLOCKED until A's commit ends"
        );
        assert!(
            entered_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "B must not have entered validation while A's commit holds the lock"
        );
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "B must NOT have acquired a revision while A holds commit synchronization"
        );

        // Resume A — still the newest ADMITTED request (B blocked at
        // admission), so its commit completes and opens readiness.
        resume_tx.send(()).unwrap();
        assert_eq!(a.join().unwrap(), Ok(()));
        assert!(errors.lock().unwrap().is_empty(), "valid A emits no error");
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "current A opens readiness at its commit"
        );

        // Only NOW is B admitted; prove its validation/readiness/error
        // outcomes and the final destination.
        assert_eq!(
            admitted_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            1,
            "B is admitted only after A's commit ends"
        );
        assert_eq!(counter.load(AtomicOrdering::SeqCst), 2);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "readiness open while newest B validates"
        );
        validated_tx.send(()).unwrap();
        assert_eq!(b.join().unwrap(), Ok(()));
        {
            let s = state.read().unwrap();
            assert_eq!(
                (s.save_dir.as_str(), s.auto_accept),
                ("/dir/B", true),
                "newest B wins the final destination"
            );
        }
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "readiness stays open (set-once)"
        );
    }

    /// A2 (TAILDROP-OWNER-A2-ONE-EXTRA-20261104): the terminal-1526
    /// post-freshness-load window, INVALID A. Same synchronized-admission
    /// schedule as the valid-A test: B is blocked at admission while A's
    /// commit holds the lock, so A is still the newest admitted request when
    /// its commit publishes — A's OWN validation error surfaces exactly once
    /// and readiness opens at A's commit. B is admitted only afterwards and
    /// its (valid) commit wins the final destination.
    #[test]
    fn a2_owner1526_postload_admission_blocked_until_commit_ends_invalid_a() {
        let counter = Arc::new(AtomicU64::new(0));
        let state = Arc::new(RwLock::new(ReceiveSettings {
            save_dir: String::new(),
            auto_accept: false,
        }));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

        // A admits through the REAL synchronized production helper.
        assert_eq!(admit_receive_settings_revision(&state, &counter), 0);
        let (ca, sa, ea, ra) = (
            counter.clone(),
            state.clone(),
            errors.clone(),
            ready.clone(),
        );
        // Writer consumes clones; commit keeps originals.
        let (ca_writer, sa_writer) = (ca.clone(), sa.clone());
        let a = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/A".to_string(),
                false,
                || async { Err("A validation error".to_string()) },
                move |dir, aa| {
                    let mut settings = sa_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&ca_writer, 0);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sa.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = ca.load(AtomicOrdering::SeqCst) == 1;
                    if !still_current {
                        return Ok(());
                    }
                    paused_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                    if let Err(msg) = &validation {
                        ea.lock().unwrap().push(msg.to_string());
                    }
                    ra.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        paused_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        let (adm_enter_tx, adm_enter_rx) = std::sync::mpsc::channel::<()>();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel::<u64>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (validated_tx, validated_rx) = tokio::sync::oneshot::channel::<()>();
        let (cb, sb, rb) = (counter.clone(), state.clone(), ready.clone());
        let b = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            adm_enter_tx.send(()).unwrap();
            // REAL synchronized admission: blocks while A's commit holds the
            // settings write lock.
            let rev_b = admit_receive_settings_revision(&sb, &cb);
            admitted_tx.send(rev_b).unwrap();
            // Writer consumes clones; commit keeps originals.
            let (cb_writer, sb_writer) = (cb.clone(), sb.clone());
            runtime.block_on(apply_receive_settings_ordered(
                "/dir/B".to_string(),
                true,
                || async move {
                    entered_tx.send(()).unwrap();
                    validated_rx.await.unwrap();
                    Ok("/dir/B".to_string())
                },
                move |dir, aa| {
                    let mut settings = sb_writer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let applied = settings_revision_outcome(&cb_writer, rev_b);
                    if applied {
                        settings.save_dir = dir.to_string();
                        settings.auto_accept = aa;
                    }
                    applied
                },
                move |validation| {
                    let _settings = sb.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let still_current = cb.load(AtomicOrdering::SeqCst) == rev_b + 1;
                    if !still_current {
                        return Ok(());
                    }
                    rb.store(true, AtomicOrdering::Release);
                    validation.map(|_| ())
                },
            ))
        });
        adm_enter_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        // While A holds commit synchronization, B has NOT acquired a
        // revision nor entered validation — the terminal-1526 interleaving
        // (obsolete error / early readiness while newest B validation held)
        // cannot occur.
        assert!(
            admitted_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err(),
            "B's admission must be BLOCKED until A's commit ends"
        );
        assert!(
            entered_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "B must not have entered validation while A's commit holds the lock"
        );
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "B must NOT have acquired a revision while A holds commit synchronization"
        );

        // Resume A — still the newest admitted request, so its OWN error is
        // the current request's error (surfaced exactly once) and readiness
        // opens at A's commit, before B is even admitted.
        resume_tx.send(()).unwrap();
        assert_eq!(
            a.join().unwrap(),
            Err("A validation error".to_string()),
            "current A returns its own validation error"
        );
        assert_eq!(
            *errors.lock().unwrap(),
            vec!["A validation error".to_string()],
            "A's error is emitted exactly once while A is still the newest admitted request"
        );
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "current A opens readiness at its commit"
        );

        // Only NOW is B admitted; its valid commit wins the destination.
        assert_eq!(
            admitted_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            1,
            "B is admitted only after A's commit ends"
        );
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        validated_tx.send(()).unwrap();
        assert_eq!(b.join().unwrap(), Ok(()));
        {
            let s = state.read().unwrap();
            assert_eq!(
                (s.save_dir.as_str(), s.auto_accept),
                ("/dir/B", true),
                "newest B wins the final destination"
            );
        }
        assert!(
            ready.load(AtomicOrdering::Acquire),
            "readiness stays open after B's commit"
        );
    }

    // ============================================================
    // A3 (TAILDROP-FIXES-20261104): immediate recovery disclosure
    // ============================================================

    fn staging_dir(path: &str) -> receipts::StagingDir {
        receipts::StagingDir {
            path: path.to_string(),
            files: vec![],
        }
    }

    #[test]
    fn a3_staging_signature_is_sorted_and_deterministic() {
        let dirs = vec![
            staging_dir("/tmp/taildrop-accept-2"),
            staging_dir("/tmp/taildrop-accept-1"),
            staging_dir("/tmp/taildrop-accept-1"),
        ];
        let sig = staging_dir_signature(&dirs);
        assert_eq!(
            sig, "/tmp/taildrop-accept-1\u{1f}/tmp/taildrop-accept-1\u{1f}/tmp/taildrop-accept-2",
            "signature must be order-insensitive and repeatable"
        );
        assert_eq!(staging_dir_signature(&[]), "");
    }

    #[test]
    fn a3_reconcile_publishes_distinct_group_set_once_and_rearms_after_empty() {
        let mut last = String::new();
        let emitted = Mutex::new(Vec::<usize>::new());
        let dirs_ab = || vec![staging_dir("/s/apple"), staging_dir("/s/zebra")];
        let dirs_a = || vec![staging_dir("/s/apple")];
        let empty = || Vec::new();

        // First failure: publishes the group set.
        reconcile_staging_after_failure(&mut last, &dirs_ab, &|d| {
            emitted.lock().unwrap().push(d.len())
        });
        // Same groups again (repeated failures / success then failure): no
        // duplicate publication.
        reconcile_staging_after_failure(&mut last, &dirs_ab, &|d| {
            emitted.lock().unwrap().push(d.len())
        });
        assert_eq!(
            *emitted.lock().unwrap(),
            vec![2],
            "same groups: emitted once"
        );

        // A successful empty poll between failures clears the signature…
        reconcile_staging_after_failure(&mut last, &empty, &|d| {
            emitted.lock().unwrap().push(d.len())
        });
        assert_eq!(*emitted.lock().unwrap(), vec![2], "empty scan never emits");

        // …so a later DISTINCT preservation group publishes again.
        reconcile_staging_after_failure(&mut last, &dirs_a, &|d| {
            emitted.lock().unwrap().push(d.len())
        });
        assert_eq!(
            *emitted.lock().unwrap(),
            vec![2, 1],
            "distinct group after empty poll must publish once"
        );
        // And its repeats stay suppressed.
        reconcile_staging_after_failure(&mut last, &dirs_a, &|d| {
            emitted.lock().unwrap().push(d.len())
        });
        assert_eq!(*emitted.lock().unwrap(), vec![2, 1]);
    }

    #[test]
    fn a3_reconcile_never_publishes_for_empty_scans() {
        let mut last = String::new();
        let emitted = Mutex::new(0usize);
        let empty = || Vec::new();
        reconcile_staging_after_failure(&mut last, &empty, &|_| {
            *emitted.lock().unwrap() += 1;
        });
        reconcile_staging_after_failure(&mut last, &empty, &|_| {
            *emitted.lock().unwrap() += 1;
        });
        assert_eq!(*emitted.lock().unwrap(), 0);
    }
}
