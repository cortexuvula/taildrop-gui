//! Host-key trust store — invariant §4.15: accept-new on first contact,
//! refuse on change. Port of the M0 spike's proven pinning flow.
//!
//! Per-flavor pin files (M0: capability hints cannot classify the serving
//! endpoint — this Mac advertises cap/ssh but serves OpenSSH; and the
//! Tailscale custom known_hosts convention must not be blindly merged with
//! OpenSSH trust):
//!
//!   <app config dir>/browser-trust/pins.tailscale-ssh.json
//!   <app config dir>/browser-trust/pins.openssh.json
//!   <app config dir>/browser-trust/pins.openssh-windows.json
//!
//! Directory 0700, files 0600, atomic tmp+rename writes. Keys are stored as
//! their OpenSSH public-key text ("ssh-ed25519 AAAA…"), which includes the
//! algorithm — a changed algorithm for the same host is a changed pin.
//! Certificate host keys are refused (spike behavior).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::browser::{BrowserError, ErrorKind, ServerFlavor};

/// Serializes check-and-record across concurrent connects (the spike's
/// TRUST_LOCK), so two panes hitting one host can't both "accept-new".
static TRUST_LOCK: Mutex<()> = Mutex::new(());

/// The accept-new / pinned-match / refuse decision, pure so it is testable
/// without a filesystem. `Ok(true)` = new pin (record it), `Ok(false)` =
/// pinned key matched, `Err` = changed key, refuse the connection.
pub fn pin_decision(previous: Option<&str>, offered: &str) -> Result<bool, BrowserError> {
    match previous {
        None => Ok(true),
        Some(k) if k == offered => Ok(false),
        Some(_) => Err(BrowserError::new(
            ErrorKind::Hostkey,
            "hostkey: changed key refused — the server's host key no longer matches the pinned key. \
             If this change is legitimate (reinstall, Tailscale-managed rotation), remove the pin \
             from the browser-trust store and reconnect.",
        )),
    }
}

pub struct TrustStore {
    dir: PathBuf,
}

impl TrustStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn pin_file(&self, flavor: ServerFlavor) -> PathBuf {
        self.dir.join(format!("pins.{}.json", flavor.as_str()))
    }

    /// Check the offered key against the pin for (flavor, host); record it on
    /// first contact. Returns true when a NEW pin was recorded (accept-new),
    /// false on a pinned match. Err(hostkey) on a changed key — the caller
    /// must refuse the connection before authenticating.
    pub fn check_and_record(
        &self,
        flavor: ServerFlavor,
        host: &str,
        offered_openssh: &str,
    ) -> Result<bool, BrowserError> {
        let _guard = TRUST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let file = self.pin_file(flavor);
        let mut pins = load_pins(&file)?;
        let is_new = pin_decision(pins.get(host).map(String::as_str), offered_openssh)?;
        if is_new {
            pins.insert(host.to_string(), offered_openssh.to_string());
            save_pins(&self.dir, &file, &pins)?;
        }
        Ok(is_new)
    }

    /// Remove a single pin (user-authorized rotation recovery).
    /// Drop a pin (M2 trust-management surface; not an M1 command).
    #[allow(dead_code)]
    pub fn forget(&self, flavor: ServerFlavor, host: &str) -> Result<(), BrowserError> {
        let _guard = TRUST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let file = self.pin_file(flavor);
        let mut pins = load_pins(&file)?;
        pins.remove(host);
        save_pins(&self.dir, &file, &pins)
    }
}

fn load_pins(file: &Path) -> Result<BTreeMap<String, String>, BrowserError> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            BrowserError::new(
                ErrorKind::Hostkey,
                format!(
                    "hostkey: trust store {} is corrupt ({e}); refusing to guess",
                    file.display()
                ),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(BrowserError::new(
            ErrorKind::Hostkey,
            format!("hostkey: cannot read trust store: {e}"),
        )),
    }
}

fn save_pins(dir: &Path, file: &Path, pins: &BTreeMap<String, String>) -> Result<(), BrowserError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("trust dir: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("trust dir: {e}")))?;
    }
    // Crash-leftover recovery (W2): sweep stale tmp files for THIS flavor
    // before writing. The mtime guard (older than 1 h) means a concurrent
    // live writer's tmp is never touched; callers hold TRUST_LOCK so
    // in-process writers are serialized. Best effort — a sweep failure must
    // not block a legitimate pin write.
    if let Ok(rd) = std::fs::read_dir(dir) {
        let stem = file.file_stem().and_then(|t| t.to_str()).unwrap_or("pins");
        let prefix = format!("{stem}.tmp-");
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with(&prefix)
                && e.metadata()
                    .and_then(|m| m.modified())
                    .map(|m| m < cutoff)
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // Atomic write: exclusive-create tmp (0600), write, sync, rename. The
    // tmp name carries pid + random suffix: a crash leftover with the same
    // recycled pid can never collide with create_new (W2).
    let tmp = {
        use rand::Rng as _;
        let nonce: u32 = rand::rng().random();
        file.with_extension(format!("tmp-{}-{:08x}", std::process::id(), nonce))
    };
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts
            .open(&tmp)
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("trust write: {e}")))?;
        let payload = serde_json::to_vec_pretty(pins)
            .map_err(|e| BrowserError::new(ErrorKind::Other, format!("trust encode: {e}")))?;
        f.write_all(&payload)
            .and_then(|_| f.sync_all())
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                BrowserError::new(ErrorKind::Other, format!("trust write: {e}"))
            })?;
    }
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        BrowserError::new(ErrorKind::Other, format!("trust commit: {e}"))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_first_key() {
        assert!(pin_decision(None, "ssh-ed25519 A").unwrap());
    }

    #[test]
    fn accepts_pinned_key() {
        assert!(!pin_decision(Some("ssh-ed25519 A"), "ssh-ed25519 A").unwrap());
    }

    #[test]
    fn refuses_changed_key() {
        let e = pin_decision(Some("ssh-ed25519 A"), "ssh-ed25519 B").unwrap_err();
        assert_eq!(e.kind, ErrorKind::Hostkey);
    }

    #[test]
    fn refuses_changed_algorithm_same_blob_shape() {
        // The stored string includes the algorithm, so a swap is a change.
        assert!(pin_decision(Some("ssh-ed25519 AAAA"), "ssh-rsa AAAA").is_err());
    }

    #[test]
    fn store_roundtrip_isolated_dirs() {
        for flavor in [
            ServerFlavor::TailscaleSsh,
            ServerFlavor::Openssh,
            ServerFlavor::OpensshWindows,
        ] {
            let dir = tempfile_like(format!("trust-{}", flavor.as_str()));
            let store = TrustStore::new(&dir);
            // First contact: accept-new.
            assert!(store
                .check_and_record(flavor, "host-a", "ssh-ed25519 AAA")
                .unwrap());
            // Same key: pinned match.
            assert!(!store
                .check_and_record(flavor, "host-a", "ssh-ed25519 AAA")
                .unwrap());
            // Changed key: refuse.
            assert!(store
                .check_and_record(flavor, "host-a", "ssh-ed25519 BBB")
                .is_err());
            // A different host pins independently.
            assert!(store
                .check_and_record(flavor, "host-b", "ssh-ed25519 CCC")
                .unwrap());
            // Pins are per flavor: the same host under another flavor is a
            // fresh accept-new, not a collision.
            let other = match flavor {
                ServerFlavor::TailscaleSsh => ServerFlavor::Openssh,
                _ => ServerFlavor::TailscaleSsh,
            };
            assert!(store
                .check_and_record(other, "host-a", "ssh-ed25519 DDD")
                .unwrap());
            // forget() allows legitimate rotation recovery.
            store.forget(flavor, "host-a").unwrap();
            assert!(store
                .check_and_record(flavor, "host-a", "ssh-ed25519 BBB")
                .unwrap());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn pin_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile_like("trust-perms".to_string());
        let store = TrustStore::new(&dir);
        store
            .check_and_record(ServerFlavor::Openssh, "h", "ssh-ed25519 A")
            .unwrap();
        let meta = std::fs::metadata(store.pin_file(ServerFlavor::Openssh)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let dmeta = std::fs::metadata(&dir).unwrap();
        assert_eq!(dmeta.permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_store_fails_closed() {
        let dir = tempfile_like("trust-corrupt".to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pins.openssh.json"), b"{not json").unwrap();
        let store = TrustStore::new(&dir);
        let e = store
            .check_and_record(ServerFlavor::Openssh, "h", "ssh-ed25519 A")
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Hostkey);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// W2: crash leftovers must never block a legitimate pin write (random
    /// suffix can't collide even with a recycled pid), and STALE leftovers
    /// (mtime > 1 h) are swept while fresh ones — possibly a concurrent live
    /// writer's — are left alone.
    #[test]
    fn tmp_leftovers_never_block_writes_and_stale_ones_are_swept() {
        let dir = tempfile_like("trust-tmp".to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pins.openssh.json");

        // Crash leftover with OUR pid in the OLD naming scheme: the pre-W2
        // create_new would have failed with "File exists" on pid reuse.
        let recycled = dir.join(format!("pins.openssh.tmp-{}", std::process::id()));
        std::fs::write(&recycled, b"crash").unwrap();
        // Stale leftover (mtime 2 h ago): must be swept.
        let stale = dir.join("pins.openssh.tmp-999999-deadbeef");
        std::fs::write(&stale, b"stale").unwrap();
        set_mtime_for_test(
            &stale,
            std::time::SystemTime::now() - std::time::Duration::from_secs(7200),
        );
        // Fresh leftover: must survive (could be a concurrent live writer).
        let fresh = dir.join("pins.openssh.tmp-999999-0000beef");
        std::fs::write(&fresh, b"fresh").unwrap();

        let mut pins = BTreeMap::new();
        pins.insert("host".to_string(), "ssh-ed25519 AAA".to_string());
        save_pins(&dir, &file, &pins).expect("write succeeds despite leftovers");
        assert!(file.exists());
        assert!(!stale.exists(), "stale leftover swept");
        assert!(fresh.exists(), "fresh leftover untouched");
        assert!(recycled.exists(), "crash leftover (fresh mtime) untouched");
        // No tmp file from THIS write survived (renamed onto the pin file).
        let tmps: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert_eq!(
            tmps.len(),
            2,
            "recycled-pid + fresh leftovers remain (stale one swept): {tmps:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Portable mtime setter (std `FileTimes` — same semantics on every
    /// platform, so the W2 sweep test also runs on Windows; the previous
    /// libc `utimensat` version was Unix-only). Sets atime+mtime, matching
    /// the old `timespec [ts, ts]` behavior.
    fn set_mtime_for_test(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open file for mtime change");
        let times = std::fs::FileTimes::new()
            .set_accessed(when)
            .set_modified(when);
        f.set_times(times).expect("set mtime");
    }

    /// Unique temp dir under the system temp root (isolated per test).
    fn tempfile_like(tag: String) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "taildrop-m1-{tag}-{}-{nonce:x}",
            std::process::id()
        ));
        p
    }
}
