//! Tauri command surface (report §7.2) + the Rust-side serial job registry.
//!
//! Jobs live in Rust so copies survive webview suspension — the same lesson
//! as the Taildrop receive loop (lib.rs:846-848). The queue is pumped
//! serially by a single task (plugin: exactly one copy at a time), and every
//! mutation command re-runs its guards server-side in addition to the UI's
//! dialog scope lock (invariant §4.7 "server re-checks too").
//!
//! Account pinning (invariant §4.6): every remote command takes `as_user` —
//! the pane's loadedUser — and the account is resolved ONCE per call from
//! that pin (never re-read from config mid-call), so changing the saved user
//! while a dialog is open cannot retarget the action.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::browser::listing;
use crate::browser::probe::{build_probe_info, ProbeCache};
use crate::browser::session::{RusshTransport, SessionCtx};
use crate::browser::transfer::{self, Side};
use crate::browser::transport::SftpTransport as _;
use crate::browser::{
    valid_host, valid_user, BrowserConfig, BrowserError, BrowserPeer, CopyEvent, CopyJobSnapshot,
    CopyRequest, DeleteOutcome, ErrorKind, JobState, Listing, ProbeInfo, LOCAL_TARGET,
};

/// Event name for copy progress/done (frontend: useBrowserTransfers).
pub const COPY_EVENT: &str = "browser-copy-event";

// ---------------------------------------------------------------------------
// Peer metadata (tailscale status) — OS hint + sshHostKeys for trust
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PeerMeta {
    pub os: String,
    pub ssh_host_keys: Vec<String>,
}

#[derive(Deserialize)]
struct StatusNode {
    #[serde(rename = "HostName")]
    host_name: Option<String>,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    #[serde(rename = "OS")]
    os: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    ips: Option<Vec<String>>,
    #[serde(rename = "Online")]
    online: Option<bool>,
    #[serde(rename = "CapMap")]
    cap_map: Option<HashMap<String, serde_json::Value>>,
    /// NOTE: camelCase on the wire (verified against `tailscale status
    /// --json` on the live daemon) — not PascalCase like its neighbors.
    #[serde(rename = "sshHostKeys")]
    ssh_host_keys: Option<Vec<String>>,
    #[serde(rename = "ExitNodeOption")]
    exit_node_option: Option<bool>,
}

#[derive(Deserialize)]
struct CurrentTailnet {
    #[serde(rename = "MagicDNSEnabled")]
    magic_dns_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct StatusJson {
    #[serde(rename = "Peer")]
    peer: Option<HashMap<String, StatusNode>>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<CurrentTailnet>,
}

/// Hosts with no SSH server to talk to — visible for Taildrop, marked
/// non-browsable (bin/tailfiles:2707-2708).
fn os_browsable(os: &str) -> bool {
    !matches!(os, "iOS" | "android" | "tvOS")
}

/// Pick the connect target: MagicDNS short name when MagicDNS is on, else
/// the first IPv4 (falling back to the first Tailscale IP). Port of
/// bin/tailfiles:2696-2703 with an IPv4 preference for the IP fallback
/// (russh dials the literal; IPv4 is the proven M0 path).
fn pick_host(dns: &str, ips: &[String], magic: bool) -> String {
    let short = dns.trim_end_matches('.').split('.').next().unwrap_or("");
    if magic && !short.is_empty() && valid_host(short) {
        return short.to_string();
    }
    if let Some(v4) = ips.iter().find(|ip| !ip.contains(':')) {
        return v4.clone();
    }
    ips.first().cloned().unwrap_or_default()
}

fn cap_ssh_hint(node: &StatusNode) -> bool {
    // Field-presence test, NOT truthiness — the CapMap value can be null
    // (M0 capability warning).
    let cap = node
        .cap_map
        .as_ref()
        .map(|m| m.contains_key("https://tailscale.com/cap/ssh"))
        .unwrap_or(false);
    cap || node
        .ssh_host_keys
        .as_ref()
        .map(|k| !k.is_empty())
        .unwrap_or(false)
}

/// Short-lived cache of `tailscale status` so a burst of pane commands
/// doesn't re-shell the daemon, and connects can cross-check Tailscale SSH
/// keys (M0) without a fresh status call each time.
pub struct PeerMetaCache {
    ttl: Duration,
    inner: Mutex<(Option<Instant>, HashMap<String, PeerMeta>)>,
}

impl PeerMetaCache {
    pub fn new() -> Self {
        Self {
            ttl: Duration::from_secs(10),
            inner: Mutex::new((None, HashMap::new())),
        }
    }

    async fn refresh_if_stale(&self) -> Result<(), BrowserError> {
        {
            let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(at) = guard.0 {
                if at.elapsed() <= self.ttl {
                    return Ok(());
                }
            }
        }
        let body = crate::tailscale::fetch_status_json_raw()
            .await
            .map_err(|e| {
                BrowserError::new(
                    ErrorKind::Unreachable,
                    format!("tailscale is not available: {e}"),
                )
            })?;
        let status: StatusJson = serde_json::from_slice(&body)
            .map_err(|e| BrowserError::other(format!("Could not read tailscale status: {e}")))?;
        let magic = status
            .current_tailnet
            .as_ref()
            .and_then(|t| t.magic_dns_enabled)
            .unwrap_or(true);
        let mut map = HashMap::new();
        for (_key, node) in status.peer.unwrap_or_default() {
            // Exit nodes (Mullvad etc.) are not SSH targets and would flood
            // the picker — excluded (the Send view still lists them).
            if node.exit_node_option.unwrap_or(false) {
                continue;
            }
            let dns = node.dns_name.clone().unwrap_or_default();
            let ips = node.ips.clone().unwrap_or_default();
            let host = pick_host(&dns, &ips, magic);
            if host.is_empty() || !valid_host(&host) {
                continue;
            }
            map.insert(
                host,
                PeerMeta {
                    os: node.os.clone().unwrap_or_default(),
                    ssh_host_keys: node.ssh_host_keys.clone().unwrap_or_default(),
                },
            );
        }
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        *guard = (Some(Instant::now()), map);
        Ok(())
    }

    pub async fn meta(&self, host: &str) -> Option<PeerMeta> {
        let _ = self.refresh_if_stale().await;
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.1.get(host).cloned()
    }

    /// Full peer list for browser_peers (sorted online-first, then name).
    pub async fn peers(
        &self,
        saved_user: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<BrowserPeer>, BrowserError> {
        let body = crate::tailscale::fetch_status_json_raw()
            .await
            .map_err(|e| {
                BrowserError::new(
                    ErrorKind::Unreachable,
                    format!("tailscale is not available: {e}"),
                )
            })?;
        let status: StatusJson = serde_json::from_slice(&body)
            .map_err(|e| BrowserError::other(format!("Could not read tailscale status: {e}")))?;
        let magic = status
            .current_tailnet
            .as_ref()
            .and_then(|t| t.magic_dns_enabled)
            .unwrap_or(true);
        let mut out = Vec::new();
        for (_key, node) in status.peer.unwrap_or_default() {
            if node.exit_node_option.unwrap_or(false) {
                continue;
            }
            let dns = node.dns_name.clone().unwrap_or_default();
            let short = dns
                .trim_end_matches('.')
                .split('.')
                .next()
                .unwrap_or("")
                .to_string();
            let ips = node.ips.clone().unwrap_or_default();
            let host = pick_host(&dns, &ips, magic);
            if host.is_empty() || !valid_host(&host) {
                continue;
            }
            let os = node.os.clone().unwrap_or_default();
            out.push(BrowserPeer {
                name: node.host_name.clone().unwrap_or_else(|| {
                    if short.is_empty() {
                        host.clone()
                    } else {
                        short
                    }
                }),
                host: host.clone(),
                dns: dns.trim_end_matches('.').to_string(),
                ip: ips.first().cloned().unwrap_or_default(),
                online: node.online.unwrap_or(false),
                user: saved_user(&host).unwrap_or_default(),
                tailscale_ssh: cap_ssh_hint(&node),
                browsable: os_browsable(&os),
                os,
                is_self: false,
            });
        }
        out.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }
}

impl Default for PeerMetaCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Job registry (serial queue, survives webview suspension)
// ---------------------------------------------------------------------------

struct JobSlot {
    snap: CopyJobSnapshot,
    cancel: CancellationToken,
}

pub struct JobRegistry {
    inner: Mutex<HashMap<u64, JobSlot>>,
    next: AtomicU64,
}

/// Finished jobs kept for re-sync before pruning (bounded memory).
const MAX_FINISHED_JOBS: usize = 50;

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    pub fn create(&self, req: CopyRequest) -> (u64, CancellationToken) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let label = job_label(&req);
        let cancel = CancellationToken::new();
        let snap = CopyJobSnapshot {
            id,
            state: JobState::Queued,
            label,
            pct: 0,
            rate: String::new(),
            file: String::new(),
            skipped: 0,
            error: None,
            kind: None,
            names: None,
            request: req,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(
            id,
            JobSlot {
                snap,
                cancel: cancel.clone(),
            },
        );
        // Prune oldest finished jobs beyond the cap.
        if map.len() > MAX_FINISHED_JOBS * 2 {
            let mut finished: Vec<(u64, u64)> = map
                .iter()
                .filter(|(_, s)| {
                    matches!(
                        s.snap.state,
                        JobState::Done | JobState::Failed | JobState::Cancelled
                    )
                })
                .map(|(id, s)| (*id, s.snap.created))
                .collect();
            finished.sort_by_key(|(_, created)| *created);
            let excess = finished.len().saturating_sub(MAX_FINISHED_JOBS);
            for (id, _) in finished.into_iter().take(excess) {
                map.remove(&id);
            }
        }
        (id, cancel)
    }

    /// Queued → Running. Returns None when the job was cancelled while
    /// queued (then it never starts — plugin: queued jobs are cancelled
    /// without starting).
    pub fn begin(&self, id: u64) -> Option<(CopyRequest, CancellationToken)> {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let slot = map.get_mut(&id)?;
        if slot.cancel.is_cancelled() {
            slot.snap.state = JobState::Cancelled;
            return None;
        }
        slot.snap.state = JobState::Running;
        Some((slot.snap.request.clone(), slot.cancel.clone()))
    }

    /// Cancel a queued or running job. Queued jobs flip straight to
    /// Cancelled; running jobs see the token at the next chunk boundary.
    pub fn cancel(&self, id: u64) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match map.get_mut(&id) {
            Some(slot) => {
                slot.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub fn update_progress(&self, id: u64, pct: u8, rate: String, file: String) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = map.get_mut(&id) {
            slot.snap.pct = pct;
            slot.snap.rate = rate;
            slot.snap.file = file;
        }
    }

    pub fn finish_done(&self, id: u64, skipped: u32) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = map.get_mut(&id) {
            slot.snap.state = JobState::Done;
            slot.snap.pct = 100;
            slot.snap.skipped = skipped;
            slot.snap.file.clear();
        }
    }

    pub fn finish_failed(&self, id: u64, err: &BrowserError, cancelled: bool) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = map.get_mut(&id) {
            slot.snap.state = if cancelled {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            slot.snap.error = Some(err.message.clone());
            slot.snap.kind = Some(err.kind);
            slot.snap.names = err.names.clone();
            slot.snap.file.clear();
        }
    }

    pub fn snapshots(&self) -> Vec<CopyJobSnapshot> {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<CopyJobSnapshot> = map.values().map(|s| s.snap.clone()).collect();
        v.sort_by_key(|s| s.id);
        v
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Queue-row label: "report.pdf → /Downloads @ studio-mac" (plugin row shape).
pub fn job_label(req: &CopyRequest) -> String {
    let what = if req.names.len() == 1 {
        req.names[0].clone()
    } else {
        format!("{} items", req.names.len())
    };
    let dst = if req.dst_target == LOCAL_TARGET {
        req.dst_dir.clone()
    } else {
        format!("{} @ {}", req.dst_dir, req.dst_target)
    };
    format!("{what} → {dst}")
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

pub struct BrowserState {
    pub ctx: Arc<SessionCtx>,
    pub probes: ProbeCache,
    pub peers: Arc<PeerMetaCache>,
    pub jobs: Arc<JobRegistry>,
    pub config_dir: PathBuf,
    queue_tx: mpsc::UnboundedSender<u64>,
    queue_rx: Mutex<Option<mpsc::UnboundedReceiver<u64>>>,
}

impl BrowserState {
    pub fn new(config_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            ctx: Arc::new(SessionCtx::new(&config_dir)),
            probes: ProbeCache::new(),
            peers: Arc::new(PeerMetaCache::new()),
            jobs: Arc::new(JobRegistry::new()),
            config_dir,
            queue_tx: tx,
            queue_rx: Mutex::new(Some(rx)),
        }
    }

    /// Spawn the serial queue pump. Called once from tauri setup; jobs then
    /// run in Rust regardless of webview lifecycle.
    pub fn start_pump(self: &Arc<Self>, app: AppHandle) {
        let mut rx_guard = self.queue_rx.lock().unwrap_or_else(|p| p.into_inner());
        let Some(mut rx) = rx_guard.take() else {
            log::warn!("browser pump already started");
            return;
        };
        drop(rx_guard);
        let me = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            // Serial: exactly one copy at a time (plugin invariant §4.18).
            while let Some(id) = rx.recv().await {
                run_job(&app, &me, id).await;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Copy job execution
// ---------------------------------------------------------------------------

enum OwnedSide {
    Local,
    /// Boxed: RusshTransport is ~300 bytes; Local must not pay for it
    /// (clippy::large_enum_variant).
    Remote(Box<OwnedRemote>),
}

struct OwnedRemote {
    transport: RusshTransport,
    windows: bool,
}

async fn open_side(
    state: &BrowserState,
    target: &str,
    as_user: Option<&str>,
) -> Result<OwnedSide, BrowserError> {
    if target == LOCAL_TARGET {
        return Ok(OwnedSide::Local);
    }
    let user = state.ctx.resolve_user(target, as_user)?;
    let meta = state.peers.meta(target).await;
    let ts_keys = meta
        .as_ref()
        .map(|m| m.ssh_host_keys.clone())
        .unwrap_or_default();
    let os_hint = meta.map(|m| m.os).unwrap_or_default();
    let transport = state.ctx.open(target, &user, ts_keys).await?;
    // Refresh the probe cache from this very connection (free home).
    if state.probes.get(target, transport.user()).is_none() {
        if let Ok(home) = transport.realpath(".").await {
            let info = build_probe_info(
                home,
                os_hint.clone(),
                transport.flavor(),
                transport.user().to_string(),
                transport.extensions().clone(),
            );
            state.probes.put(target, transport.user(), info);
        }
    }
    let windows = transport.flavor().is_windows();
    Ok(OwnedSide::Remote(Box::new(OwnedRemote {
        transport,
        windows,
    })))
}

async fn run_job(app: &AppHandle, state: &Arc<BrowserState>, id: u64) {
    let Some((req, cancel)) = state.jobs.begin(id) else {
        // Cancelled while queued — surface it so the row updates.
        let _ = app.emit(COPY_EVENT, CopyEvent::done_cancelled(id));
        return;
    };
    let _ = app.emit(COPY_EVENT, CopyEvent::start(id, req.names.len() as u32));

    let result = execute_copy(app, state, id, &req, &cancel).await;
    match result {
        Ok(skipped) => {
            state.jobs.finish_done(id, skipped);
            let _ = app.emit(COPY_EVENT, CopyEvent::done_ok(id, skipped));
        }
        Err(e) => {
            let cancelled = transfer::is_cancelled(&e) || cancel.is_cancelled();
            state.jobs.finish_failed(id, &e, cancelled);
            let event = if cancelled {
                CopyEvent::done_cancelled(id)
            } else {
                CopyEvent::done_err(id, &e)
            };
            let _ = app.emit(COPY_EVENT, event);
        }
    }
}

async fn execute_copy(
    app: &AppHandle,
    state: &Arc<BrowserState>,
    id: u64,
    req: &CopyRequest,
    cancel: &CancellationToken,
) -> Result<u32, BrowserError> {
    // Resolve both sides (accounts pinned from the request — never re-read).
    let src_side = open_side(state, &req.src_target, req.src_user.as_deref()).await?;
    // Same host + same account: one connection serves both sides (the
    // plugin's same-host copy rule; two accounts are refused in preflight).
    let dst_same_as_src = req.dst_target == req.src_target
        && req.dst_user == req.src_user
        && req.dst_target != LOCAL_TARGET;
    let dst_side = if dst_same_as_src {
        None
    } else {
        Some(open_side(state, &req.dst_target, req.dst_user.as_deref()).await?)
    };

    let result = run_with_sides(app, state, id, req, cancel, &src_side, dst_side.as_ref()).await;

    // Close transports on every path (cooperative cleanup ordering: staging
    // was already removed by run_copy before we get here).
    if let OwnedSide::Remote(remote) = src_side {
        let _ = Box::new(remote.transport).close().await;
    }
    if let Some(OwnedSide::Remote(remote)) = dst_side {
        let _ = Box::new(remote.transport).close().await;
    }
    result
}

fn side_ref(s: &OwnedSide) -> Side<'_> {
    match s {
        OwnedSide::Local => Side::Local,
        OwnedSide::Remote(remote) => Side::Remote {
            transport: &remote.transport,
            windows: remote.windows,
        },
    }
}

async fn run_with_sides(
    app: &AppHandle,
    state: &Arc<BrowserState>,
    id: u64,
    req: &CopyRequest,
    cancel: &CancellationToken,
    src_side: &OwnedSide,
    dst_side: Option<&OwnedSide>,
) -> Result<u32, BrowserError> {
    let src_ref = side_ref(src_side);
    // Same host+account borrows the src transport (no second connection).
    let dst_ref = side_ref(dst_side.unwrap_or(src_side));

    let dst_windows = matches!(dst_ref, Side::Remote { windows: true, .. });
    // Server-side preflight (in addition to the UI's own checks).
    transfer::preflight(req, dst_windows)?;

    // Existence pre-check unless replace consent — ask the filesystem, not
    // the listing (case-insensitive volumes answer correctly).
    if !req.replace {
        let clash = transfer::existing_names(&dst_ref, &req.dst_dir, &req.names).await?;
        if !clash.is_empty() {
            return Err(BrowserError::exists(clash, &req.dst_dir, false));
        }
    }

    let jobs = Arc::clone(&state.jobs);
    let app2 = app.clone();
    let emit: Arc<dyn Fn(u8, String, String) + Send + Sync> = Arc::new(move |pct, file, rate| {
        jobs.update_progress(id, pct, rate.clone(), file.clone());
        let _ = app2.emit(COPY_EVENT, CopyEvent::progress(id, pct, rate, file));
    });

    let outcome = transfer::run_copy(req, &src_ref, &dst_ref, cancel.clone(), emit).await?;
    Ok(outcome.skipped)
}

// ---------------------------------------------------------------------------
// Tauri commands (§7.2 surface)
// ---------------------------------------------------------------------------

/// Discovery: browsable peers + saved users + capability hints. Extends (not
/// replaces) get_tailscale_status; phones stay visible for Taildrop but are
/// marked non-browsable.
#[tauri::command]
pub async fn browser_peers(
    state: State<'_, Arc<BrowserState>>,
) -> Result<Vec<BrowserPeer>, String> {
    let ctx = Arc::clone(&state.ctx);
    state
        .peers
        .peers(move |host| ctx.config.user_for(host))
        .await
        .map_err(|e| e.message)
}

/// Connect + capability probe: {system, posix, home, flavor, user,
/// extensions}. Cached 600 s per host+account.
#[tauri::command]
pub async fn browser_probe(
    state: State<'_, Arc<BrowserState>>,
    host: String,
    as_user: Option<String>,
) -> Result<ProbeInfo, BrowserError> {
    probe_impl(&state, &host, as_user.as_deref(), true).await
}

async fn probe_impl(
    state: &BrowserState,
    host: &str,
    as_user: Option<&str>,
    use_cache: bool,
) -> Result<ProbeInfo, BrowserError> {
    if !valid_host(host) {
        return Err(BrowserError::new(
            ErrorKind::Other,
            format!("{host:?} is not a usable host name"),
        ));
    }
    let user = state.ctx.resolve_user(host, as_user)?;
    if use_cache {
        if let Some(info) = state.probes.get(host, &user) {
            return Ok(info);
        }
    } else {
        // Fresh probe (delete guards need THIS machine's current home).
        state.probes.invalidate_host(host);
    }
    let meta = state.peers.meta(host).await;
    let ts_keys = meta
        .as_ref()
        .map(|m| m.ssh_host_keys.clone())
        .unwrap_or_default();
    let os_hint = meta.map(|m| m.os).unwrap_or_default();
    let transport = state.ctx.open(host, &user, ts_keys).await?;
    let home = transport.realpath(".").await?;
    let info = build_probe_info(
        home,
        os_hint,
        transport.flavor(),
        transport.user().to_string(),
        transport.extensions().clone(),
    );
    let _ = Box::new(transport).close().await;
    state.probes.put(host, &user, info.clone());
    Ok(info)
}

/// Listing. `as_user` is the pane's loadedUser pin; the returned `user` is
/// what the pane pins later actions to.
#[tauri::command]
pub async fn browser_ls(
    state: State<'_, Arc<BrowserState>>,
    target: String,
    path: String,
    as_user: Option<String>,
) -> Result<Listing, BrowserError> {
    if target == LOCAL_TARGET {
        return listing::list_local(&path);
    }
    if !valid_host(&target) {
        return Err(BrowserError::new(
            ErrorKind::Other,
            format!("{target:?} is not a usable host name"),
        ));
    }
    let user = state.ctx.resolve_user(&target, as_user.as_deref())?;
    let home_hint = state
        .probes
        .get(&target, &user)
        .map(|p| p.home)
        .unwrap_or_default();
    let meta = state.peers.meta(&target).await;
    let ts_keys = meta
        .as_ref()
        .map(|m| m.ssh_host_keys.clone())
        .unwrap_or_default();
    let os_hint = meta.map(|m| m.os).unwrap_or_default();
    let transport = state.ctx.open(&target, &user, ts_keys).await?;
    let home = if home_hint.is_empty() {
        transport.realpath(".").await.unwrap_or_default()
    } else {
        home_hint
    };
    if state.probes.get(&target, transport.user()).is_none() {
        let info = build_probe_info(
            home.clone(),
            os_hint,
            transport.flavor(),
            transport.user().to_string(),
            transport.extensions().clone(),
        );
        state.probes.put(&target, transport.user(), info);
    }
    let result = listing::list_remote(&transport, &path, &home, transport.user()).await;
    let _ = Box::new(transport).close().await;
    result
}

/// Enqueue a copy job; returns the job id immediately. Progress/done arrive
/// as `browser-copy-event` emissions; re-sync via browser_copy_jobs.
#[tauri::command]
pub async fn browser_copy(
    state: State<'_, Arc<BrowserState>>,
    job: CopyRequest,
) -> Result<u64, BrowserError> {
    // Cheap structural validation up front so obviously-bad requests never
    // enter the queue (full preflight re-runs in the pump, server-side).
    if job.names.is_empty() {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "Nothing selected to copy",
        ));
    }
    if job.src_target != LOCAL_TARGET && !valid_host(&job.src_target) {
        return Err(BrowserError::new(ErrorKind::Other, "unusable source host"));
    }
    if job.dst_target != LOCAL_TARGET && !valid_host(&job.dst_target) {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "unusable destination host",
        ));
    }
    let (id, _cancel) = state.jobs.create(job);
    state
        .queue_tx
        .send(id)
        .map_err(|_| BrowserError::other("the copy queue is not running"))?;
    Ok(id)
}

/// Cancel a queued or running job. Cleanup is awaited by the pump (the done
/// event arrives after staging is removed).
#[tauri::command]
pub async fn browser_cancel_copy(
    state: State<'_, Arc<BrowserState>>,
    job_id: u64,
) -> Result<(), String> {
    if state.jobs.cancel(job_id) {
        Ok(())
    } else {
        Err(format!("no such job: {job_id}"))
    }
}

/// Re-sync the queue (mount/focus) — jobs live in Rust and survive webview
/// suspension.
#[tauri::command]
pub fn browser_copy_jobs(state: State<'_, Arc<BrowserState>>) -> Vec<CopyJobSnapshot> {
    state.jobs.snapshots()
}

/// Create a folder. Name validated server-side; Windows targets refused (M1).
#[tauri::command]
pub async fn browser_mkdir(
    state: State<'_, Arc<BrowserState>>,
    target: String,
    path: String,
    as_user: Option<String>,
) -> Result<(), BrowserError> {
    let name = listing_basename(&path)?;
    if let Some(why) = crate::browser::guards::new_name_problem(&name) {
        return Err(BrowserError::new(ErrorKind::Other, why));
    }
    if target == LOCAL_TARGET {
        local_write_name_guard(&name)?;
        std::fs::create_dir(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                BrowserError::new(ErrorKind::Exists, format!("{name} already exists"))
            } else {
                BrowserError::from(e)
            }
        })?;
        return Ok(());
    }
    let transport = open_remote_mut(&state, &target, as_user.as_deref()).await?;
    let r = mkdir_remote(&transport, &target, &path, &name).await;
    let _ = Box::new(transport).close().await;
    r
}

async fn mkdir_remote(
    t: &RusshTransport,
    target: &str,
    path: &str,
    name: &str,
) -> Result<(), BrowserError> {
    if t.flavor().is_windows() {
        return Err(windows_write_refusal("New folder", target));
    }
    // Note: for non-Windows targets the plugin validates names only on the
    // Windows destination itself; this branch intentionally does not refuse
    // winnames-problematic names when the target is not Windows (parity).
    t.mkdir(path, 0o755).await.map_err(|mut e| {
        if e.kind == ErrorKind::Exists {
            e.message = format!("{name} already exists");
        }
        e
    })
}

/// Rename — NEVER replaces an existing target (invariant §4.13). Local:
/// atomic no-replace renameat. Remote: existence check + rename (TOCTOU
/// ceiling documented, §6.4); Windows targets refused (M1).
#[tauri::command]
pub async fn browser_rename(
    state: State<'_, Arc<BrowserState>>,
    target: String,
    old: String,
    new: String,
    as_user: Option<String>,
) -> Result<(), BrowserError> {
    let old_name = listing_basename(&old)?;
    let new_name = listing_basename(&new)?;
    if let Some(why) = crate::browser::guards::new_name_problem(&new_name) {
        return Err(BrowserError::new(ErrorKind::Other, why));
    }
    if old_name == new_name {
        return Ok(()); // nothing to do (plugin: value === ctx.name → return)
    }
    // Same folder only (the UI never offers cross-folder rename).
    if parent_of(&old) != parent_of(&new) {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "Rename stays within one folder",
        ));
    }
    if target == LOCAL_TARGET {
        local_write_name_guard(&new_name)?;
        return local_rename(&old, &new_name);
    }
    let transport = open_remote_mut(&state, &target, as_user.as_deref()).await?;
    let r = async {
        if transport.flavor().is_windows() {
            return Err(windows_write_refusal("Rename", &target));
        }
        // Never replace: ask the filesystem (case-insensitive volumes answer
        // for case-variant clashes too).
        if transport.lstat(&new).await?.is_some() {
            return Err(BrowserError::new(
                ErrorKind::Exists,
                format!("{new_name} already exists"),
            ));
        }
        if transport.lstat(&old).await?.is_none() {
            return Err(BrowserError::new(
                ErrorKind::Notfound,
                format!("{old_name} no longer exists"),
            ));
        }
        transport.rename(&old, &new).await
    }
    .await;
    let _ = Box::new(transport).close().await;
    r
}

/// Local atomic no-replace rename through dir fds (Unix). Non-Unix local
/// hosts fail closed rather than using a replacing rename.
#[cfg(unix)]
fn local_rename(old: &str, new_name: &str) -> Result<(), BrowserError> {
    use crate::browser::transfer::local_side::{open_top, RealRename, RenameNoReplace as _};
    use std::ffi::CString;
    let parent = parent_of(old);
    let dfd = open_top(&parent)?;
    let oldc = CString::new(listing_basename(old)?).map_err(|_| BrowserError::other("bad name"))?;
    let newc = CString::new(new_name).map_err(|_| BrowserError::other("bad name"))?;
    // Existence check for a good message; the atomic call still enforces.
    let outcome =
        RealRename.rename_noreplace(dfd.as_raw_fd_ref(), &oldc, dfd.as_raw_fd_ref(), &newc);
    match outcome {
        Ok(()) => Ok(()),
        Err(errno) => {
            let mapped = crate::browser::transfer::place::rename_errno_outcome(errno);
            use crate::browser::transfer::place::RenameOutcome;
            Err(match mapped {
                RenameOutcome::Exists => {
                    BrowserError::new(ErrorKind::Exists, format!("{new_name} already exists"))
                }
                RenameOutcome::NonAtomic => BrowserError::new(
                    ErrorKind::Other,
                    "this disk can't rename atomically without replacing — rename refused",
                ),
                _ => BrowserError::new(
                    ErrorKind::Other,
                    format!(
                        "rename failed: {}",
                        std::io::Error::from_raw_os_error(errno)
                    ),
                ),
            })
        }
    }
}

#[cfg(not(unix))]
fn local_rename(_old: &str, _new_name: &str) -> Result<(), BrowserError> {
    // std::fs::rename REPLACES on Windows — never acceptable for the
    // no-replace rename invariant. Fail closed until a Windows-safe
    // no-replace path exists (M3 gate territory).
    Err(transfer::local_unsupported())
}

/// Delete. Local = Trash only, never permanent (invariant §4.11); partial
/// progress reported. Remote = permanent, with fresh-home fail-closed guards
/// checked on the path as written AND as the server resolves it.
#[tauri::command]
pub async fn browser_delete(
    state: State<'_, Arc<BrowserState>>,
    target: String,
    paths: Vec<String>,
    as_user: Option<String>,
) -> Result<DeleteOutcome, BrowserError> {
    // No trailing slash: `rm -rf link/` would act on the link's target.
    let paths: Vec<String> = paths
        .iter()
        .map(|p| {
            let t = p.trim_end_matches('/');
            if t.is_empty() { "/" } else { t }.to_string()
        })
        .collect();
    if paths.is_empty() {
        return Err(BrowserError::new(ErrorKind::Other, "Nothing selected"));
    }
    if target == LOCAL_TARGET {
        return local_delete(&paths);
    }
    // Fresh probe: the guard needs THIS machine's current home (never a
    // cached one — invariant §4.10 "home probed fresh per delete").
    let probe = probe_impl(&state, &target, as_user.as_deref(), false).await?;
    if !probe.posix && probe.flavor.is_windows() {
        return Err(windows_write_refusal("Delete", &target));
    }
    let homes: Vec<String> = if probe.home.is_empty() {
        Vec::new() // fail closed: guard_delete refuses without a home
    } else {
        vec![probe.home.clone()]
    };
    let transport = open_remote_mut(&state, &target, as_user.as_deref()).await?;
    let windows = transport.flavor().is_windows();
    let r = async {
        // Guard as-written, then resolve parents and guard again (a
        // symlinked parent can't hide a home folder).
        let mut resolved = Vec::with_capacity(paths.len());
        for p in &paths {
            if let Some(why) =
                crate::browser::guards::guard_delete_problem(p, &homes_strs(&homes), windows)
            {
                return Err(BrowserError::new(ErrorKind::Denied, why));
            }
            let real = resolve_parent(&transport, p).await?;
            if let Some(why) =
                crate::browser::guards::guard_delete_problem(&real, &homes_strs(&homes), windows)
            {
                return Err(BrowserError::new(ErrorKind::Denied, why));
            }
            resolved.push(real);
        }
        let mut done = 0usize;
        for p in &resolved {
            remote_delete_one(&transport, p, 0).await.map_err(|mut e| {
                e.message = format!(
                    "{}{}",
                    e.message,
                    if done == 0 {
                        String::new()
                    } else {
                        format!(" The {done} before it were deleted.")
                    }
                );
                e
            })?;
            done += 1;
        }
        Ok(DeleteOutcome {
            trashed: false,
            done,
        })
    }
    .await;
    let _ = Box::new(transport).close().await;
    r
}

fn homes_strs(homes: &[String]) -> Vec<&str> {
    homes.iter().map(|s| s.as_str()).collect()
}

/// Resolve the PARENT via realpath and re-append the basename — the item
/// itself is never resolved (a symlink item is deleted as a link, never its
/// target). Port of the plugin's local `realpath(parent)+name` rule.
async fn resolve_parent(t: &RusshTransport, path: &str) -> Result<String, BrowserError> {
    let parent = parent_of(path);
    let name = listing_basename(path)?;
    let real_parent = t.realpath(&parent).await?;
    Ok(transfer::join_remote(&real_parent, &name))
}

/// Recursive SFTP delete. Links are removed AS LINKS (never descended);
/// depth-bounded; unknown/special nodes are attempted as files.
async fn remote_delete_one(
    t: &RusshTransport,
    path: &str,
    depth: usize,
) -> Result<(), BrowserError> {
    if depth > crate::browser::MAX_DEPTH {
        return Err(BrowserError::other("nested too deeply to delete safely"));
    }
    let Some(st) = t.lstat(path).await? else {
        return Ok(()); // already gone
    };
    if st.is_symlink || !st.is_dir {
        return t.remove_file(path).await;
    }
    for child in t.read_dir(path).await? {
        // B3: remote rule — a lossy-decoded (U+FFFD) name is a guess; refuse
        // the whole delete rather than act on or around it.
        if !listing::safe_remote_name(&child.name) {
            return Err(BrowserError::new(
                ErrorKind::Denied,
                format!(
                    "{path} contains a name that can't be removed safely, so nothing was deleted"
                ),
            ));
        }
        Box::pin(remote_delete_one(
            t,
            &transfer::join_remote(path, &child.name),
            depth + 1,
        ))
        .await?;
    }
    t.remove_dir(path).await
}

/// Local delete: Trash ONLY. If trashing fails, stop — never fall back to
/// permanent deletion (invariant §4.11). Guards run on the path as written
/// and on the parent-resolved path.
#[cfg(unix)]
fn local_delete(paths: &[String]) -> Result<DeleteOutcome, BrowserError> {
    let home = listing::local_home();
    let real_home = std::fs::canonicalize(&home)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| home.clone());
    let homes = [home.as_str(), real_home.as_str()];
    let mut resolved = Vec::with_capacity(paths.len());
    for p in paths {
        if let Some(why) = crate::browser::guards::guard_delete_problem(p, &homes, false) {
            return Err(BrowserError::new(ErrorKind::Denied, why));
        }
        // Resolve symlinks in the PARENT folders, not the item itself: the
        // trash moves a link, never its target.
        let parent = parent_of(p);
        let name = listing_basename(p)?;
        let real = match std::fs::canonicalize(&parent) {
            Ok(rp) => rp.join(name).to_string_lossy().to_string(),
            Err(_) => p.clone(),
        };
        if let Some(why) = crate::browser::guards::guard_delete_problem(&real, &homes, false) {
            return Err(BrowserError::new(ErrorKind::Denied, why));
        }
        resolved.push(real);
    }
    let mut done = 0usize;
    for p in &resolved {
        if let Err(e) = trash::delete(p) {
            let suffix = if done == 0 {
                String::new()
            } else {
                format!(" The {done} before it went to the Trash.")
            };
            return Err(BrowserError::new(
                ErrorKind::Other,
                format!(
                    "Couldn't move {} to the Trash ({e}). It was not deleted.{suffix}",
                    Path::new(p)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
            ));
        }
        done += 1;
    }
    Ok(DeleteOutcome {
        trashed: true,
        done,
    })
}

#[cfg(not(unix))]
fn local_delete(paths: &[String]) -> Result<DeleteOutcome, BrowserError> {
    // The guard set (home/ancestor rules with symlinked-parent resolution)
    // is Unix-shaped; on non-Unix local hosts fail closed for M1 rather
    // than deleting with weaker guards. Trash-only would use the trash crate
    // once the guard port lands with the Windows local-sink work.
    let _ = paths;
    Err(transfer::local_unsupported())
}

/// Open a file in the default app. Remote files download to the app cache
/// first (fresh copy, no exec bits — invariant §4.14). Returns the local
/// path opened.
#[tauri::command]
pub async fn browser_open(
    state: State<'_, Arc<BrowserState>>,
    target: String,
    path: String,
    as_user: Option<String>,
) -> Result<String, BrowserError> {
    if target == LOCAL_TARGET {
        if !Path::new(&path).exists() {
            return Err(BrowserError::new(
                ErrorKind::Notfound,
                format!("{path} no longer exists"),
            ));
        }
        open_with_default_app(&path)?;
        return Ok(path);
    }
    let name = listing_basename(&path)?;
    let transport = open_remote_mut(&state, &target, as_user.as_deref()).await?;
    let local = async {
        let st = transport.lstat(&path).await?.ok_or_else(|| {
            BrowserError::new(ErrorKind::Notfound, format!("{name} no longer exists"))
        })?;
        if st.is_dir {
            return Err(BrowserError::new(
                ErrorKind::Other,
                "That's a folder — open it instead of downloading it",
            ));
        }
        // Fresh copy into the app cache dir per open (never reuse a stale
        // download), created 0600 — no exec bits ever (invariant §4.14).
        let cache = open_cache_dir(&state.config_dir, &target)?;
        let unique = format!(
            "{}-{name}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dest = cache.join(unique);
        // Defense in depth: the composed download destination must stay
        // inside the cache dir (a hostile remote basename like `..` fails
        // here even though listing_basename already reduced it).
        if !listing::path_within(&cache, &dest) {
            return Err(BrowserError::other("download escaped the cache directory"));
        }
        download_to(&transport, &path, &dest, st.size).await?;
        Ok(dest.to_string_lossy().to_string())
    }
    .await;
    let _ = Box::new(transport).close().await;
    let local_path = local?;
    open_with_default_app(&local_path)?;
    Ok(local_path)
}

fn open_cache_dir(config_dir: &Path, host: &str) -> Result<PathBuf, BrowserError> {
    // Slug the host so it's a safe single path component.
    let slug: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = config_dir.join("browser-open").join(slug);
    std::fs::create_dir_all(&dir)
        .map_err(|e| BrowserError::other(format!("can't prepare the open cache: {e}")))?;
    Ok(dir)
}

async fn download_to(
    t: &RusshTransport,
    remote: &str,
    dest: &Path,
    size: u64,
) -> Result<(), BrowserError> {
    let mut reader = t.open_read(remote).await?;
    // Exclusive create, 0600: a fresh copy, never overwriting anything and
    // never executable.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = opts
        .open(dest)
        .map_err(|e| BrowserError::other(format!("can't create the download: {e}")))?;
    let mut buf = vec![0u8; transfer::CHUNK];
    let mut done = 0u64;
    let r = async {
        loop {
            let want = std::cmp::min(buf.len() as u64, size.saturating_sub(done)) as usize;
            if want == 0 {
                break;
            }
            let n = reader.read(&mut buf[..want]).await?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])
                .map_err(|e| BrowserError::other(format!("download write: {e}")))?;
            done += n as u64;
        }
        Ok::<(), BrowserError>(())
    }
    .await;
    let close = reader.close().await;
    if r.is_err() {
        let _ = std::fs::remove_file(dest); // never leave a partial download
    }
    r?;
    close?;
    Ok(())
}

fn open_with_default_app(path: &str) -> Result<(), BrowserError> {
    // Same platform split as the app's existing show_in_folder command.
    #[cfg(target_os = "macos")]
    let (prog, args) = ("open", vec![path.to_string()]);
    #[cfg(target_os = "linux")]
    let (prog, args) = ("xdg-open", vec![path.to_string()]);
    #[cfg(target_os = "windows")]
    let (prog, args) = (
        "cmd",
        vec![
            "/C".to_string(),
            "start".to_string(),
            "".to_string(),
            path.to_string(),
        ],
    );
    std::process::Command::new(prog)
        .args(&args)
        .spawn()
        .map_err(|e| BrowserError::other(format!("can't open {path}: {e}")))?;
    Ok(())
}

/// Per-host account pin (Change user). Persisted; probe cache dropped so a
/// stale home/OS can't leak into later guards.
#[tauri::command]
pub async fn browser_set_user(
    state: State<'_, Arc<BrowserState>>,
    host: String,
    user: Option<String>,
) -> Result<(), String> {
    if !valid_host(&host) {
        return Err(format!("{host:?} is not a usable host name"));
    }
    if let Some(u) = &user {
        if !valid_user(u) {
            return Err("That doesn't look like a usable account name".into());
        }
    }
    state
        .ctx
        .config
        .set_user(&host, user)
        .map_err(|e| e.message)?;
    state.probes.invalidate_host(&host);
    Ok(())
}

/// Current browser config (per-host users, last remote).
#[tauri::command]
pub fn browser_config(state: State<'_, Arc<BrowserState>>) -> BrowserConfig {
    state.ctx.config.load()
}

/// Remember the last remote machine (reopen there if online — plugin row 5).
#[tauri::command]
pub async fn browser_set_last(
    state: State<'_, Arc<BrowserState>>,
    host: String,
) -> Result<(), String> {
    if !host.is_empty() && !valid_host(&host) {
        return Err(format!("{host:?} is not a usable host name"));
    }
    state.ctx.config.set_last(&host).map_err(|e| e.message)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn open_remote_mut(
    state: &BrowserState,
    target: &str,
    as_user: Option<&str>,
) -> Result<RusshTransport, BrowserError> {
    if !valid_host(target) {
        return Err(BrowserError::new(
            ErrorKind::Other,
            format!("{target:?} is not a usable host name"),
        ));
    }
    let user = state.ctx.resolve_user(target, as_user)?;
    let meta = state.peers.meta(target).await;
    let ts_keys = meta.map(|m| m.ssh_host_keys).unwrap_or_default();
    state.ctx.open(target, &user, ts_keys).await
}

fn windows_write_refusal(op: &str, target: &str) -> BrowserError {
    BrowserError::unsupported(format!(
        "{op} on Windows peers ({target}) is disabled until the M3 safety gate passes — \
         browse and pull only for now"
    ))
}

/// Basename of an absolute path, refusing empties and traversal.
fn listing_basename(path: &str) -> Result<String, BrowserError> {
    let trimmed = path.trim_end_matches('/');
    let name = match trimmed.rsplit_once('/') {
        Some((_, n)) => n,
        None => trimmed,
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "That path has no usable name",
        ));
    }
    if crate::browser::guards::control_char_problem(name).is_some() {
        return Err(BrowserError::new(
            ErrorKind::Other,
            "That name contains a control character",
        ));
    }
    Ok(name.to_string())
}

fn parent_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((p, _)) => {
            if p.is_empty() {
                "/".to_string()
            } else {
                p.to_string()
            }
        }
        None => "/".to_string(),
    }
}

/// On Windows app hosts, local writes additionally pass the Windows name
/// rules (the local FS is the Windows FS). No-op elsewhere.
fn local_write_name_guard(name: &str) -> Result<(), BrowserError> {
    #[cfg(windows)]
    {
        if let Some(why) = crate::browser::winnames::windows_name_problem(name) {
            return Err(BrowserError::new(ErrorKind::Other, why.to_string()));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = name;
    }
    Ok(())
}

#[cfg(unix)]
trait AsRawFdRef {
    fn as_raw_fd_ref(&self) -> std::os::fd::RawFd;
}
#[cfg(unix)]
impl AsRawFdRef for std::os::fd::OwnedFd {
    fn as_raw_fd_ref(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(src: &str, dst: &str, names: &[&str]) -> CopyRequest {
        CopyRequest {
            src_target: src.into(),
            src_user: if src == LOCAL_TARGET {
                None
            } else {
                Some("u".into())
            },
            src_dir: "/a".into(),
            dst_target: dst.into(),
            dst_user: if dst == LOCAL_TARGET {
                None
            } else {
                Some("u".into())
            },
            dst_dir: "/b".into(),
            names: names.iter().map(|s| s.to_string()).collect(),
            replace: false,
        }
    }

    #[test]
    fn label_shapes() {
        assert_eq!(job_label(&req("local", "h", &["f.txt"])), "f.txt → /b @ h");
        assert_eq!(job_label(&req("h", "local", &["a", "b"])), "2 items → /b");
    }

    #[test]
    fn registry_lifecycle() {
        let r = JobRegistry::new();
        let (id, cancel) = r.create(req("local", "h", &["f"]));
        assert_eq!(r.snapshots().len(), 1);
        assert_eq!(r.snapshots()[0].state, JobState::Queued);
        // begin() hands out the request + token.
        let (got, tok) = r.begin(id).unwrap();
        assert_eq!(got.names, vec!["f".to_string()]);
        assert!(!tok.is_cancelled());
        assert_eq!(r.snapshots()[0].state, JobState::Running);
        r.update_progress(id, 42, "1.0MB/s".into(), "f".into());
        assert_eq!(r.snapshots()[0].pct, 42);
        r.finish_done(id, 1);
        let s = &r.snapshots()[0];
        assert_eq!(s.state, JobState::Done);
        assert_eq!(s.pct, 100);
        assert_eq!(s.skipped, 1);
        let _ = cancel;
    }

    #[test]
    fn cancel_queued_job_never_starts() {
        let r = JobRegistry::new();
        let (id, _) = r.create(req("local", "h", &["f"]));
        assert!(r.cancel(id));
        assert!(r.begin(id).is_none(), "cancelled job must not start");
        assert_eq!(r.snapshots()[0].state, JobState::Cancelled);
    }

    #[test]
    fn cancel_running_sets_token() {
        let r = JobRegistry::new();
        let (id, _) = r.create(req("local", "h", &["f"]));
        let (_, tok) = r.begin(id).unwrap();
        assert!(r.cancel(id));
        assert!(tok.is_cancelled());
    }

    #[test]
    fn failed_snapshot_carries_kind_and_names() {
        let r = JobRegistry::new();
        let (id, _) = r.create(req("local", "h", &["f"]));
        r.begin(id).unwrap();
        let e = BrowserError::exists(vec!["f".into()], "/b", false);
        r.finish_failed(id, &e, false);
        let s = &r.snapshots()[0];
        assert_eq!(s.state, JobState::Failed);
        assert_eq!(s.kind, Some(ErrorKind::Exists));
        assert_eq!(s.names.as_ref().unwrap(), &vec!["f".to_string()]);
    }

    #[test]
    fn host_picking() {
        let ips = vec!["100.1.2.3".to_string(), "fd7a::1".to_string()];
        assert_eq!(pick_host("studio.tail.net.", &ips, true), "studio");
        assert_eq!(pick_host("", &ips, true), "100.1.2.3");
        assert_eq!(pick_host("studio.tail.net.", &ips, false), "100.1.2.3");
        let v6 = vec!["fd7a::1".to_string()];
        assert_eq!(pick_host("", &v6, true), "fd7a::1");
    }

    #[test]
    fn browsable_and_hint_rules() {
        assert!(!os_browsable("iOS"));
        assert!(!os_browsable("android"));
        assert!(!os_browsable("tvOS"));
        assert!(os_browsable("linux"));
        assert!(os_browsable("macOS"));
        assert!(os_browsable("windows"));
        // CapMap presence (null value) counts; truthiness must not be used.
        let node = StatusNode {
            host_name: None,
            dns_name: None,
            os: None,
            ips: None,
            online: None,
            cap_map: Some(HashMap::from([(
                "https://tailscale.com/cap/ssh".to_string(),
                serde_json::Value::Null,
            )])),
            ssh_host_keys: None,
            exit_node_option: None,
        };
        assert!(cap_ssh_hint(&node));
        let node2 = StatusNode {
            cap_map: None,
            ssh_host_keys: Some(vec!["AAA".into()]),
            host_name: None,
            dns_name: None,
            os: None,
            ips: None,
            online: None,
            exit_node_option: None,
        };
        assert!(cap_ssh_hint(&node2));
    }

    #[test]
    fn basename_and_parent() {
        assert_eq!(listing_basename("/a/b/c.txt").unwrap(), "c.txt");
        assert_eq!(listing_basename("/a/b/").unwrap(), "b");
        assert!(listing_basename("/a/..").is_err());
        assert!(listing_basename("/").is_err());
        assert_eq!(parent_of("/a/b/c"), "/a/b");
        assert_eq!(parent_of("/a"), "/");
        assert_eq!(parent_of("/"), "/");
    }

    #[test]
    fn cache_dir_slug_is_path_safe() {
        let dir = open_cache_dir(Path::new("/tmp/does-not-matter-x"), "a/b:c");
        // Only checks the slug shape; creation under /tmp is harmless.
        let s = dir.unwrap().to_string_lossy().to_string();
        assert!(s.ends_with("a_b_c"));
        let _ = std::fs::remove_dir_all(Path::new("/tmp/does-not-matter-x/browser-open"));
    }
}
