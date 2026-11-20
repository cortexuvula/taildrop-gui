//! OS/home detection + a 600 s probe cache (report §7.1, port of the
//! plugin's probe/probe_cache_path, bin/tailfiles:445-478 / 157-159).
//!
//! Exec-free in v1: home comes from SFTP `realpath(".")` (the plugin's
//! Windows-safe fallback, which is also the right model for Tailscale SSH
//! where there is no sshd_config to rely on — report §6.7 Case B). The OS
//! string comes from `tailscale status` (passed in by the caller), never from
//! a remote shell. The pure parts live here; the connect+realpath
//! orchestration lives in `commands.rs` where the transport is opened.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::browser::{ProbeInfo, ServerFlavor};

/// Default probe cache lifetime (plugin: 600 s).
pub const PROBE_TTL: Duration = Duration::from_secs(600);

/// POSIX-ness rule: Windows is the only non-POSIX flavor we classify; the OS
/// string from tailscale status is authoritative when present. Port of the
/// plugin's `windows = not probe.posix`.
pub fn is_posix(system: &str, flavor: ServerFlavor) -> bool {
    if flavor.is_windows() {
        return false;
    }
    !system.eq_ignore_ascii_case("windows")
}

/// Assemble a ProbeInfo from the pieces gathered during connect. Pure so the
/// posix/flavor rules are testable without a connection.
pub fn build_probe_info(
    home: String,
    system: String,
    flavor: ServerFlavor,
    user: String,
    extensions: BTreeMap<String, String>,
) -> ProbeInfo {
    let posix = is_posix(&system, flavor);
    ProbeInfo {
        system,
        posix,
        home,
        flavor,
        user,
        extensions,
    }
}

/// Cache key: host + account, exactly like the plugin's probe_cache_path
/// (which hashed the account so two accounts on one host don't collide).
fn cache_key(host: &str, user: &str) -> String {
    format!("{host}\u{0}{user}")
}

/// In-memory probe cache with an injectable TTL for tests.
pub struct ProbeCache {
    ttl: Duration,
    inner: Mutex<HashMap<String, (Instant, ProbeInfo)>>,
}

impl ProbeCache {
    pub fn new() -> Self {
        Self {
            ttl: PROBE_TTL,
            inner: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, host: &str, user: &str) -> Option<ProbeInfo> {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let (at, info) = map.get(&cache_key(host, user))?;
        if at.elapsed() > self.ttl {
            None
        } else {
            Some(info.clone())
        }
    }

    pub fn put(&self, host: &str, user: &str, info: ProbeInfo) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(cache_key(host, user), (Instant::now(), info));
    }

    /// Drop cached probes for a host (after "Change user" or a failed probe)
    /// so a stale home/OS can't leak into a guard decision.
    pub fn invalidate_host(&self, host: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|k, _| !k.starts_with(&format!("{host}\u{0}")));
    }
}

impl Default for ProbeCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(home: &str) -> ProbeInfo {
        build_probe_info(
            home.into(),
            "Linux".into(),
            ServerFlavor::Openssh,
            "alice".into(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn posix_rules() {
        assert!(is_posix("Linux", ServerFlavor::Openssh));
        assert!(is_posix("macOS", ServerFlavor::TailscaleSsh));
        assert!(!is_posix("Windows", ServerFlavor::OpensshWindows));
        assert!(!is_posix("Linux", ServerFlavor::OpensshWindows)); // flavor wins
        assert!(!is_posix("windows", ServerFlavor::Openssh));
    }

    #[test]
    fn build_probe_sets_posix_and_fields() {
        let p = build_probe_info(
            "/C:/Users/corte".into(),
            "Windows".into(),
            ServerFlavor::OpensshWindows,
            "corte".into(),
            BTreeMap::new(),
        );
        assert!(!p.posix);
        assert_eq!(p.home, "/C:/Users/corte");
        assert!(p.flavor.is_windows());
    }

    #[test]
    fn cache_roundtrip_and_expiry() {
        let c = ProbeCache::with_ttl(Duration::from_millis(50));
        assert!(c.get("h", "alice").is_none());
        c.put("h", "alice", info("/home/alice"));
        assert_eq!(c.get("h", "alice").unwrap().home, "/home/alice");
        // A different account on the same host is a distinct entry.
        assert!(c.get("h", "bob").is_none());
        std::thread::sleep(Duration::from_millis(70));
        assert!(c.get("h", "alice").is_none(), "entry must expire");
    }

    #[test]
    fn invalidate_host_only_drops_that_host() {
        let c = ProbeCache::new();
        c.put("h1", "alice", info("/a"));
        c.put("h2", "alice", info("/b"));
        c.invalidate_host("h1");
        assert!(c.get("h1", "alice").is_none());
        assert!(c.get("h2", "alice").is_some());
    }
}
