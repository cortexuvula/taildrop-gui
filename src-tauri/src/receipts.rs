//! TD-05: durable transfer receipts.
//!
//! A receipt is a fact, not a view: once a receive completes (or terminally
//! fails) the record survives every subsequent empty inbox poll. The store is
//! process-global (single-app process; tests reset it) so platform code deep
//! in `tailscale.rs` can record without plumbing an app handle through every
//! call. A broadcast channel forwards each new receipt to the lib.rs emitter
//! task, which emits `transfer-receipt` to the webview.
//!
//! Contract: docs/td05-receipt-contract.md (seq cursor, hasMore paging,
//! reset-on-retention, inbox/staging/none recovery split).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use tokio::sync::broadcast;

/// Receipts older than this are pruned (contract: 24h, session-scoped v1).
const RETENTION_MS: u64 = 24 * 60 * 60 * 1000;

/// Recovery discriminator for failed receipts (contract: RecoveryInfo).
/// `None` is part of the wire contract but not yet constructed by the
/// backend (every current failure classifies as Inbox or Staging); it must
/// stay in the enum so the frontend can deserialize it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Recovery {
    /// Daemon inbox still lists the file; retry = existing accept_file.
    Inbox,
    /// Bytes preserved in a staging dir; recover via recover_staging_files.
    Staging { staging_path: String },
    /// No recovery action available; show error detail only.
    #[allow(dead_code)]
    None,
}

/// One completed or terminally-failed receive. Field names serialize
/// camelCase to match the frontend contract types exactly.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransferReceipt {
    pub seq: u64,
    pub id: String,
    pub filename: String,
    pub saved_name: String,
    pub saved_path: String,
    pub size: u64,
    pub peer_name: Option<String>,
    pub direction: &'static str, // always "received" in v1
    pub status: &'static str,    // "saved" | "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
}

/// Response shape for `get_recent_receipts` (contract: ReceiptPage).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptPage {
    pub receipts: Vec<TransferReceipt>,
    pub next_since_seq: u64,
    pub has_more: bool,
    pub reset: bool,
}

/// A preserved staging directory discovered at startup.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagingDir {
    pub path: String,
    pub files: Vec<StagingFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagingFile {
    pub name: String,
    pub size: u64,
}

struct Inner {
    receipts: VecDeque<TransferReceipt>,
    next_seq: u64,
    id_counter: u64,
    /// Highest seq ever assigned, surviving pruning — used to detect that a
    /// client's cursor predates pruned history even when the deque is empty.
    highest_seq_ever: u64,
}

pub struct ReceiptStore {
    inner: Mutex<Inner>,
    /// New-receipt notifications; the lib.rs emitter task is the receiver.
    /// Sending to zero receivers is a no-op (e.g. before app setup).
    events: broadcast::Sender<TransferReceipt>,
}

static STORE: LazyLock<ReceiptStore> = LazyLock::new(|| ReceiptStore {
    inner: Mutex::new(Inner {
        receipts: VecDeque::new(),
        next_seq: 1,
        id_counter: 0,
        highest_seq_ever: 0,
    }),
    events: broadcast::channel(128).0,
});

/// Tests that (directly or via accept/drain code paths) touch the global
/// store must hold this lock: parallel resets or concurrent recordings break
/// count-based assertions. Shared across modules (tailscale tests exercise
/// receipt-recording paths too).
#[cfg(test)]
pub(crate) static TEST_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl ReceiptStore {
    /// Subscribe to new-receipt notifications from the global store (for
    /// the lib.rs emitter task).
    pub fn subscribe_global() -> broadcast::Receiver<TransferReceipt> {
        STORE.subscribe()
    }

    /// Subscribe to new-receipt notifications (for the emitter task).
    pub fn subscribe(&self) -> broadcast::Receiver<TransferReceipt> {
        self.events.subscribe()
    }

    fn prune_locked(inner: &mut Inner) {
        let cutoff = now_ms().saturating_sub(RETENTION_MS);
        while let Some(front) = inner.receipts.front() {
            if front.timestamp < cutoff {
                inner.receipts.pop_front();
            } else {
                break;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_locked(
        inner: &mut Inner,
        filename: String,
        saved_name: String,
        saved_path: String,
        size: u64,
        peer_name: Option<String>,
        status: &'static str,
        error: Option<String>,
        recovery: Option<Recovery>,
        timestamp: u64,
    ) -> TransferReceipt {
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.id_counter += 1;
        if seq > inner.highest_seq_ever {
            inner.highest_seq_ever = seq;
        }
        let id = format!("recv-{}-{:04x}", timestamp, inner.id_counter);
        let receipt = TransferReceipt {
            seq,
            id,
            filename,
            saved_name,
            saved_path,
            size,
            peer_name,
            direction: "received",
            status,
            error,
            timestamp,
            recovery,
        };
        inner.receipts.push_back(receipt.clone());
        Self::prune_locked(inner);
        receipt
    }

    /// Record a successful receive. `saved_path` must be the actual
    /// collision-resolved landing path (never reconstructed from the name).
    pub fn record_saved(
        filename: &str,
        saved_path: &str,
        size: u64,
        peer_name: Option<String>,
    ) -> TransferReceipt {
        let receipt = Self::record_with_timestamp(
            filename,
            saved_path,
            size,
            peer_name,
            "saved",
            None,
            None,
            now_ms(),
        );
        let _ = STORE.events.send(receipt.clone());
        receipt
    }

    /// Test/internal variant with an explicit completion timestamp (retention
    /// tests need receipts that are already old).
    #[allow(clippy::too_many_arguments)]
    fn record_with_timestamp(
        filename: &str,
        saved_path: &str,
        size: u64,
        peer_name: Option<String>,
        status: &'static str,
        error: Option<String>,
        recovery: Option<Recovery>,
        timestamp: u64,
    ) -> TransferReceipt {
        let saved_name = std::path::Path::new(saved_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(filename)
            .to_string();
        let mut inner = STORE.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::record_locked(
            &mut inner,
            filename.to_string(),
            saved_name,
            saved_path.to_string(),
            size,
            peer_name,
            status,
            error,
            recovery,
            timestamp,
        )
    }

    /// Record a terminally-failed receive with its recovery discriminator.
    pub fn record_failed(filename: &str, error: &str, recovery: Recovery) -> TransferReceipt {
        let receipt = Self::record_with_timestamp(
            filename,
            "",
            0,
            None,
            "failed",
            Some(error.to_string()),
            Some(recovery),
            now_ms(),
        );
        let _ = STORE.events.send(receipt.clone());
        receipt
    }

    /// Fetch receipts with `seq > since_seq`, ascending, up to `limit`.
    /// `has_more` is true when records remain beyond the page; the client
    /// must page to completion. `reset` is true when `since_seq` predates
    /// retained history (records were pruned the client hasn't seen).
    pub fn page(&self, since_seq: u64, limit: usize) -> ReceiptPage {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::prune_locked(&mut inner);

        let oldest = inner.receipts.front().map(|r| r.seq);
        let reset = since_seq > 0
            && match oldest {
                // Non-empty: records between since_seq and oldest were pruned.
                Some(oldest) => oldest > since_seq + 1,
                // Empty: anything ever assigned past since_seq was pruned.
                None => inner.highest_seq_ever > since_seq,
            };

        let matching: Vec<TransferReceipt> = inner
            .receipts
            .iter()
            .filter(|r| r.seq > since_seq)
            .take(limit.max(1))
            .cloned()
            .collect();
        let next_since_seq = matching.last().map(|r| r.seq).unwrap_or(since_seq);
        let has_more = inner
            .receipts
            .back()
            .map(|last| last.seq > next_since_seq)
            .unwrap_or(false);
        ReceiptPage {
            receipts: matching,
            next_since_seq,
            has_more,
            reset,
        }
    }

    /// Record a salvaged receive: bytes recovered from a preserved staging
    /// directory (TD05-B). Salvage moves the data to safety but CANNOT prove
    /// the download completed before the CLI failed — a deliberately
    /// shortened file salvages just as successfully as a complete one. It
    /// must never be reported as an ordinary successful download.
    pub fn record_salvaged(filename: &str, saved_path: &str, size: u64) -> TransferReceipt {
        let receipt = Self::record_with_timestamp(
            filename,
            saved_path,
            size,
            None,
            "salvaged",
            None,
            None,
            now_ms(),
        );
        let _ = STORE.events.send(receipt.clone());
        receipt
    }

    /// Whether a saved receipt exists for this exact landing (filename AND
    /// path), regardless of age. This is the TD05-A correction: identity is
    /// per-completed-landing, not elapsed time.
    ///
    /// Two distinct roles, and the distinction is the fix:
    /// - **Acknowledgement** (no drain): time-unconditional. A delayed ack
    ///   must still find the recorded landing and return it — a window here
    ///   made old acks fall through to a redundant CLI drain.
    /// - **Recording suppression**: REMOVED from the drain/accept paths.
    ///   Every newly completed landing records its own receipt — receiving,
    ///   deleting, and receiving the same name again (inside any window)
    ///   produces two receipts, because they are two transfers. The only
    ///   remaining suppression is the existing-file FALLBACK in
    ///   `accept_file_inner`, which fires when the inbox delivered nothing
    ///   and must not re-record a landing that was already recorded.
    pub fn saved_landing_recorded(filename: &str, saved_path: &str) -> bool {
        let inner = STORE.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .receipts
            .iter()
            .any(|r| r.status == "saved" && r.filename == filename && r.saved_path == saved_path)
    }

    /// Classify a failed accept into its recovery kind. The TD-01/TD-04 fix
    /// messages embed the preserved staging path in quotes; when present the
    /// bytes may live ONLY there (retrying against the inbox could even
    /// "succeed" via the existing-file fallback while the new bytes stay
    /// marooned). Otherwise the inbox entry was not deleted (delete happens
    /// only after success), so an inbox retry is valid.
    pub fn classify_recovery(error: &str) -> Recovery {
        const MARKER: &str = "preserved for recovery in '";
        if let Some(start) = error.find(MARKER) {
            let path_start = start + MARKER.len();
            if let Some(end) = error[path_start..].find('\'') {
                let path = &error[path_start..path_start + end];
                if !path.is_empty() {
                    return Recovery::Staging {
                        staging_path: path.to_string(),
                    };
                }
            }
        }
        Recovery::Inbox
    }

    #[cfg(test)]
    pub fn reset_for_tests() {
        let mut inner = STORE.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.receipts.clear();
        inner.next_seq = 1;
        inner.id_counter = 0;
        inner.highest_seq_ever = 0;
    }

    /// Test-only snapshot of recorded receipts in seq order.
    #[cfg(test)]
    pub fn snapshot_for_tests() -> Vec<TransferReceipt> {
        let inner = STORE.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.receipts.iter().cloned().collect()
    }

    #[cfg(test)]
    pub fn record_saved_at(filename: &str, saved_path: &str, timestamp: u64) -> TransferReceipt {
        Self::record_with_timestamp(
            filename, saved_path, 1, None, "saved", None, None, timestamp,
        )
    }
}

/// Page through the global store (what the `get_recent_receipts` command
/// calls). Free function so deep platform code and tests need no handle.
pub fn page_public(since_seq: u64, limit: usize) -> ReceiptPage {
    STORE.page(since_seq, limit)
}

/// Age past which an empty `taildrop-accept-*` dir is treated as the
/// leavings of an interrupted run and swept. Every accept/drain path is
/// now bounded (capped CLI child ≤110s under a 120s outer wrapper), so
/// 15 minutes vastly exceeds any live operation — but the age gate stays
/// a heuristic, NOT a liveness proof (clock skew, killed-but-unreaped
/// processes), and the sweep's safety comes from the removal primitive
/// (`remove_dir` fails if a file lands in the race), not from the gate;
/// the gate only keeps the scan from churning genuinely fresh dirs.
const EMPTY_STAGING_SWEEP_AGE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Decide whether an empty staging dir is stale (its owning accept/drain
/// process is dead) and safe to sweep. Pure and explicit about `now` so
/// the age gate is unit-testable everywhere without filesystem mtime
/// tricks. A missing/future-dated mtime reads as fresh — never sweep.
fn empty_dir_is_stale(modified: Option<std::time::SystemTime>, now: std::time::SystemTime) -> bool {
    modified
        .and_then(|t| now.duration_since(t).ok())
        .map(|age| age > EMPTY_STAGING_SWEEP_AGE)
        .unwrap_or(false)
}

/// Scan the staging root for preserved `taildrop-accept-*` staging
/// directories (TD-01 recovery data). Returns only directories containing
/// at least one regular file — an empty dir holds nothing recoverable.
///
/// Empty leftovers get swept when they are STALE (older than
/// [`EMPTY_STAGING_SWEEP_AGE`]): the accept paths clean up their own dirs
/// on success/failure, but a process killed between `create_dir_all` and
/// cleanup (SIGKILL, power loss, forced quit during a receive poll)
/// otherwise leaks the empty dir into the staging root forever — skipped
/// by the recovery UI and never removed by anything. The removal is
/// `remove_dir` (fails if anything landed in the race window), so the
/// sweep is non-destructive even when the age gate misjudges a live
/// drain. A FRESH empty dir is left untouched so a concurrent in-flight
/// drain that has not landed its first file yet is never disturbed.
/// Unstat-able dirs are treated as fresh.
pub fn scan_staging_dirs() -> Vec<StagingDir> {
    let mut dirs = Vec::new();
    let entries = match std::fs::read_dir(crate::tailscale::staging_root()) {
        Ok(e) => e,
        Err(_) => return dirs,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("taildrop-accept-") {
            continue;
        }
        let path = entry.path();
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&path) {
            for f in rd.filter_map(|e| e.ok()) {
                if f.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    files.push(StagingFile {
                        name: f.file_name().to_string_lossy().to_string(),
                        size: f.metadata().map(|m| m.len()).unwrap_or(0),
                    });
                }
            }
        }
        if files.is_empty() {
            if empty_dir_is_stale(
                entry.metadata().ok().and_then(|m| m.modified().ok()),
                std::time::SystemTime::now(),
            ) {
                match std::fs::remove_dir(&path) {
                    Ok(()) => log::debug!(
                        "scan_staging_dirs: swept empty staging dir left by interrupted run: '{}'",
                        path.display()
                    ),
                    Err(e) => log::warn!(
                        "scan_staging_dirs: failed to sweep empty staging dir '{}': {}",
                        path.display(),
                        e
                    ),
                }
            }
            continue;
        }
        dirs.push(StagingDir {
            path: path.to_string_lossy().to_string(),
            files,
        });
    }
    dirs
}

/// Validate that `path` is a `taildrop-accept-*` directory directly under the
/// staging root — recovery/discard commands must never touch arbitrary paths.
pub fn validate_staging_path(path: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::Path::new(path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid staging path".to_string())?;
    if !name.starts_with("taildrop-accept-") {
        return Err(format!("'{}' is not a preserved staging directory", path));
    }
    let root = crate::tailscale::staging_root();
    if p.parent() != Some(root.as_path()) && p.parent() != root.canonicalize().ok().as_deref() {
        return Err(format!(
            "'{}' is not directly under the staging root directory",
            path
        ));
    }
    Ok(p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Receipts-module tests use the SHARED TEST_STORE_LOCK: tailscale-module
    // tests also record into the global store, and a separate lock here let
    // the two groups reset/record over each other (real flakes caught by
    // repeated full-suite runs).

    fn fresh() {
        ReceiptStore::reset_for_tests();
    }

    #[test]
    fn seq_is_strictly_increasing_and_ids_unique_same_ms() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        fresh();
        let a = ReceiptStore::record_saved("a.txt", "/tmp/a.txt", 1, None);
        let b = ReceiptStore::record_saved("b.txt", "/tmp/b.txt", 1, None);
        assert_eq!(a.seq + 1, b.seq, "seq must be strictly increasing");
        assert_ne!(a.id, b.id, "same-millisecond receipts need distinct ids");
        assert_eq!(a.direction, "received");
        assert_eq!(a.status, "saved");
    }

    #[test]
    fn pagination_never_skips_backlog() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        fresh();
        for i in 0..125 {
            ReceiptStore::record_saved(&format!("f{}.txt", i), "/tmp/f.txt", 1, None);
        }
        let mut seen = 0usize;
        let mut since = 0;
        let mut pages = 0;
        let mut has_more = true;
        while has_more {
            let page = page_public(since, 50);
            seen += page.receipts.len();
            since = page.next_since_seq;
            pages += 1;
            has_more = page.has_more;
        }
        assert_eq!(seen, 125, "all records delivered across pages");
        assert_eq!(pages, 3);
        assert!(!has_more);
    }

    #[test]
    fn reset_flag_when_history_pruned() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        fresh();
        let now = now_ms();
        // Two "old" receipts (beyond retention) then fresh ones.
        ReceiptStore::record_saved_at("old1.txt", "/tmp/old1.txt", now - RETENTION_MS - 5000);
        ReceiptStore::record_saved_at("old2.txt", "/tmp/old2.txt", now - RETENTION_MS - 4000);
        let kept = ReceiptStore::record_saved("new.txt", "/tmp/new.txt", 1, None);

        // Cursor from before the pruned pair → reset must be true.
        let page = page_public(0, 10);
        // since_seq 0 means "from the beginning": pruning still applies, but
        // reset is defined as "cursor predates retained history" — seq 0 is
        // the session start, not a stale cursor; reset stays false.
        assert!(!page.reset);
        assert!(page.receipts.iter().all(|r| r.seq >= kept.seq));

        // A cursor pointing at the first pruned receipt → reset true.
        let page = page_public(1, 10);
        assert!(page.reset, "cursor predating retained history must reset");
        assert_eq!(page.receipts.len(), 1);

        // Cursor exactly at the last seen receipt → no reset, no skips.
        let page = page_public(kept.seq - 1, 10);
        assert!(!page.reset);
        assert_eq!(page.receipts.first().map(|r| r.seq), Some(kept.seq));
    }

    #[test]
    fn salvaged_receipts_are_distinct_from_saved() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        ReceiptStore::reset_for_tests();
        // TD05-B: a deliberately SHORTENED staged file, "recovered" — the
        // move succeeds, but it must never emerge as a verified download.
        ReceiptStore::record_salvaged("half-file.bin", "/tmp/save/half-file.bin", 512);
        let page = page_public(0, 10);
        assert_eq!(page.receipts.len(), 1);
        assert_eq!(page.receipts[0].status, "salvaged");
        // Salvaged is not "saved": verified-history checks skip it.
        assert!(!ReceiptStore::saved_landing_recorded(
            "half-file.bin",
            "/tmp/save/half-file.bin"
        ));
    }

    #[test]
    fn classify_recovery_parses_staging_path() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let r = ReceiptStore::classify_recovery(
            "tailscale file get failed: x. 1 file(s) it had already downloaded are preserved for recovery in '/var/folders/taildrop-accept-1a0e'",
        );
        assert_eq!(
            r,
            Recovery::Staging {
                staging_path: "/var/folders/taildrop-accept-1a0e".to_string()
            }
        );
        assert_eq!(
            ReceiptStore::classify_recovery("Tailscale API error (500): boom"),
            Recovery::Inbox
        );
    }

    #[test]
    fn validate_staging_path_rejects_escaping_paths() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        assert!(validate_staging_path("/etc").is_err());
        assert!(validate_staging_path("/tmp/whatever").is_err());
        let inside = std::env::temp_dir().join("taildrop-accept-deadbeef");
        let s = inside.to_string_lossy().to_string();
        // Path construction is valid even if the dir doesn't exist.
        assert!(validate_staging_path(&s).is_ok());
    }

    #[test]
    fn scan_finds_only_staging_dirs_with_files() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // RA-01: use an isolated staging root (RAII guard) so test fixtures
        // never pollute the application's recovery namespace, and the
        // override is cleared even if an assertion panics mid-test.
        let _iso = crate::tailscale::staging_guard::StagingRootGuard::new("scan_only_with_files");
        let base = crate::tailscale::staging_root();

        let with_files = base.join("taildrop-accept-scan1");
        let empty = base.join("taildrop-accept-scan2");
        let _ = std::fs::remove_dir_all(&with_files);
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&with_files).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(with_files.join("data.bin"), b"payload").unwrap();

        let dirs = scan_staging_dirs();
        let found = dirs
            .iter()
            .find(|d| d.path == with_files.to_string_lossy())
            .expect("dir with files must be discovered");
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.files[0].name, "data.bin");
        assert!(
            !dirs.iter().any(|d| d.path == empty.to_string_lossy()),
            "empty staging dirs are not recovery data"
        );
    }

    #[test]
    fn empty_dir_sweep_decision_is_age_gated() {
        // Pure gate: an empty dir older than the sweep age is stale
        // (its owning process cannot be alive); fresh or future-dated
        // dirs are never swept — a concurrent in-flight drain may not
        // have landed its first file yet.
        let now = std::time::SystemTime::now();
        let old = now
            .checked_sub(std::time::Duration::from_secs(16 * 60))
            .unwrap();
        let fresh = now.checked_sub(std::time::Duration::from_secs(60)).unwrap();
        let future = now
            .checked_add(std::time::Duration::from_secs(120))
            .unwrap();
        assert!(empty_dir_is_stale(Some(old), now), "> 15 min → stale");
        assert!(!empty_dir_is_stale(Some(fresh), now), "fresh → untouched");
        assert!(
            !empty_dir_is_stale(Some(future), now),
            "future-dated (clock skew) → never sweep"
        );
        assert!(!empty_dir_is_stale(None, now), "unstat-able → never sweep");
    }

    /// Real-filesystem sweep: needs to backdate a dir's mtime, which
    /// `File::open` on a directory only supports on Unix (Windows would
    /// need FILE_FLAG_BACKUP_SEMANTICS). The decision gate itself is
    /// covered cross-platform by `empty_dir_sweep_decision_is_age_gated`.
    #[cfg(unix)]
    #[test]
    fn scan_sweeps_stale_empty_dir_keeps_fresh() {
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _iso = crate::tailscale::staging_guard::StagingRootGuard::new("sweep_stale");
        let base = crate::tailscale::staging_root();

        let stale = base.join("taildrop-accept-stale");
        let fresh_empty = base.join("taildrop-accept-fresh");
        let with_files = base.join("taildrop-accept-withfiles");
        for d in [&stale, &fresh_empty, &with_files] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(with_files.join("data.bin"), b"payload").unwrap();
        // Backdate the stale dir one hour — simulating a crashed accept
        // run that created the dir and never returned to clean it up.
        let f = std::fs::File::open(&stale).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();

        let dirs = scan_staging_dirs();
        assert!(dirs.iter().any(|d| d.path == with_files.to_string_lossy()));
        assert!(
            !dirs.iter().any(|d| d.path == stale.to_string_lossy()),
            "stale empty dir swept before return"
        );
        assert!(
            !stale.exists(),
            "stale empty dir physically removed (leak closed)"
        );
        assert!(
            fresh_empty.exists(),
            "fresh empty dir untouched (concurrent in-flight protection)"
        );
    }

    #[test]
    fn sweep_removal_fails_non_destructively_on_landed_file() {
        // The property that makes the sweep safe against an orphaned
        // drain landing a file in the check-then-remove race: `remove_dir`
        // (unlike `remove_dir_all`) refuses to delete a dir that gained a
        // file — the dir survives with its file as recovery data instead
        // of silently deleting user bytes.
        let _g = TEST_STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _iso = crate::tailscale::staging_guard::StagingRootGuard::new("sweep_race");
        let base = crate::tailscale::staging_root();
        let d = base.join("taildrop-accept-race");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // The dir is empty at "check" time; the file lands before removal.
        std::fs::write(d.join("late.bin"), b"payload").unwrap();

        assert!(
            std::fs::remove_dir(&d).is_err(),
            "refuses to remove non-empty dir"
        );
        assert!(d.join("late.bin").exists(), "landed file survives");
        assert!(d.exists(), "dir survives as recovery data");
    }
}
