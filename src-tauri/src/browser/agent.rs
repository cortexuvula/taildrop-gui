//! M2b: the POSIX exec-agent lane (report §7.4 "M2 exec agents" column).
//!
//! On POSIX remotes that can exec `perl` (probed ONCE per host/account and
//! cached in `AgentCapCache`), copies place through the vendored MIT Perl
//! agent (`src-tauri/agents/agent.pl`, provenance + sha256 pins in
//! `src-tauri/agents/THIRD_PARTY.md`) over an SSH exec channel instead of
//! the SFTP placement tiers. What this buys (the v1 ceilings it removes):
//!
//!   - ATOMIC no-replace placement independent of server SFTP extensions:
//!     the agent renames through two held dir-fds with renameat2
//!     RENAME_NOREPLACE (Linux) / renameatx_np RENAME_EXCL (macOS), failing
//!     CLOSED on filesystems without an atomic primitive. The
//!     hardlink@openssh.com tier only exists where advertised, and
//!     check-then-rename has the documented TOCTOU window — with the agent,
//!     a name that appears AFTER the listing is still refused (the rename
//!     itself is the check).
//!   - HANDLE-BOUND stage cleanup on every exit path: the agent holds its
//!     stage dir-fd from creation to removal and deletes through that handle
//!     only (`drop_stage` = empty via held handles + dev/ino-matched rmdir),
//!     triggered by finish, failure, stdin EOF, SIGPIPE, HUP or TERM. Our
//!     side NEVER removes anything remote by name — cleanup ordering mirrors
//!     the plugin (bin/tailfiles:2318-2349): close the agent's stdin FIRST,
//!     wait bounded, the agent self-cleans on EOF.
//!   - PHYSICAL delete re-check (`agent_delete`): the vendored POSIX delete
//!     script re-resolves every target's parent with `cd -P` at delete time
//!     and re-runs the home guard against the PHYSICAL spelling, closing the
//!     "path changed between confirm and delete" class.
//!
//! Hosts where the probe says no-perl / no-exec keep the CURRENT SFTP tiers
//! UNCHANGED (`RemoteSink`); tier selection lives in `transfer::run_copy`
//! and never silently downgrades — the SFTP tiers ARE the documented
//! fallback, with their ceilings documented in remote_side.rs.
//!
//! Wire protocol (report §1.4, plugin-exact):
//!   remote command = the FIXED `AGENT_CMD` string below; stdin =
//!   "<byte length of AGENT_PL>\n" + AGENT_PL + NUL-terminated fields, the
//!   first field being the verb. For `recv`: dst_dir, stage_name,
//!   "replace"|"keep"; then stream records `D rel\0 mode\0 mtime\0`,
//!   `F rel\0 mode\0 mtime\0 size\0` + exactly `size` raw bytes,
//!   `L rel\0 target\0`, terminated by a single `Z` byte. After `Z` the
//!   agent places, drops its stage and replies `C rel\0` (conflict),
//!   `E rel\0` (error) records and a final `Z`. Bail paths print to stderr
//!   and exit non-zero AFTER removing the stage through held handles.
//!   Paths are DATA on stdin — nothing user-controlled ever reaches the
//!   remote shell (plugin invariant §4.9).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::OnceCell;

use crate::browser::transfer::{stage_dir_name, CopySink, FileMeta};
use crate::browser::transport::{classify_message, SftpTransport};
use crate::browser::{BrowserError, ErrorKind, MAX_FIELD_BYTES};

/// The vendored Perl agent, sent verbatim over the length-prefix framing.
/// Byte-faithful to the plugin's AGENT_PL below the attribution header
/// (the single documented deviation; sha256-pinned in THIRD_PARTY.md and
/// re-pinned by the `vendored_*` tests below). The header is Perl comments
/// — semantically inert — and the length prefix measures THIS file as sent.
pub const AGENT_PL: &str = include_str!("../../agents/agent.pl");

/// The fixed remote bootstrap command — byte-identical to the plugin's
/// AGENT_CMD (bin/tailfiles:1264-1265), pinned by `agent_cmd_is_plugin_exact`.
/// perl reads the length line, reads exactly that many bytes, evals them.
/// Exit 2 on a short/absent read is the probe's positive signal: perl RAN
/// and honored the framing contract.
pub const AGENT_CMD: &str = "perl -e 'my $n = <STDIN>; defined $n or exit 2; read(STDIN, my $c, $n) == $n or exit 2; eval $c; die $@ if $@; exit 0'";

/// The vendored POSIX delete script (runtime value of the plugin's
/// POSIX_DELETE_SCRIPT, byte-identical; see THIRD_PARTY.md). A trailing
/// newline is appended for file hygiene and trimmed at embed time.
pub const POSIX_DELETE_SCRIPT: &str = include_str!("../../agents/posix_delete.sh");

/// The fixed delete command: plugin `POSIX_DELETE_CMD = "xargs -0 sh -c
/// '%s' sh"`. Paths travel NUL-separated on stdin — never through a shell
/// word. The script contains no single quotes (pinned by test), so the
/// `sh -c '…'` wrap is exact.
pub fn posix_delete_cmd() -> String {
    format!(
        "xargs -0 sh -c '{}' sh",
        POSIX_DELETE_SCRIPT.trim_end_matches('\n')
    )
}

/// stderr capture cap for one exec channel (diagnostics only).
pub const STDERR_CAP: usize = 64 * 1024;
/// Hard cap on the total reply stream after `Z` (conflicts/errors lists).
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// Capability probe budget (open exec → EOF → exit code).
pub const PROBE_WAIT: Duration = Duration::from_secs(15);
/// Plugin cleanup wait: stdin closed, bounded wait for the agent to
/// self-clean and exit before we let the session end (bin/tailfiles:2334).
pub const CLEANUP_WAIT: Duration = Duration::from_secs(20);
/// Placement (post-Z) and delete budgets — the plugin's copy/delete timeouts.
pub const PLACE_WAIT: Duration = Duration::from_secs(300);
pub const DELETE_WAIT: Duration = Duration::from_secs(300);

/// What a remote can do for us beyond SFTP. Probed once per host/account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCap {
    /// No usable exec channel (refused, or the probe timed out): SFTP tiers
    /// unchanged — the documented fallback, never a silent downgrade.
    SftpOnly,
    /// Exec works but `perl` didn't run (e.g. exit 127): SFTP tiers
    /// unchanged, same documented note.
    ExecNoPerl,
    /// The vendored Perl agent runs (bootstrap honored the framing contract
    /// and exited 2 on immediate EOF): atomic agent placement + delete
    /// re-check available.
    PerlExec,
}

/// A remote command execution channel: stdin is ours to write, stdout is
/// the agent's reply stream, stderr is captured (capped) for diagnostics.
/// Implemented by the real russh exec channel (`session::SshExecChannel`)
/// and, for tests, by `LocalExecChannel` (the loopback harness: the exact
/// same NUL-framed protocol against a local `sh -c` child — the protocol
/// is the contract, not the transport).
#[async_trait]
#[allow(clippy::double_must_use)] // async_trait + Result output (known interaction)
pub trait ExecChannel: Send {
    /// Write to the remote process's stdin. Fails once the channel is dead
    /// (a bailed agent surfaces here at the next write, like EPIPE).
    async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError>;
    /// Read the reply stream (stdout). `Ok(0)` = EOF.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError>;
    /// End the remote process's stdin (SSH channel EOF / closing the pipe).
    /// This is the cleanup trigger: the agent self-cleans its stage on EOF.
    async fn close_stdin(&mut self) -> Result<(), BrowserError>;
    /// Wait (bounded) for the exit code. `None` = timeout or no code
    /// (signalled/killed) — callers treat it as "capability unknown".
    async fn wait_exit(&mut self, timeout: Duration) -> Option<u32>;
    /// Captured stderr (lossy, capped at `STDERR_CAP`) for error messages.
    fn stderr_tail(&self) -> String;
}

// ---------------------------------------------------------------------------
// Framing (plugin agent_input / record shapes — pinned by tests)
// ---------------------------------------------------------------------------

fn check_field(f: &str) -> Result<(), BrowserError> {
    if f.as_bytes().contains(&0) {
        return Err(BrowserError::other(
            "a path for the remote helper contains an interior NUL byte",
        ));
    }
    if f.len() > MAX_FIELD_BYTES {
        return Err(BrowserError::other(
            "a path for the remote helper is longer than the protocol cap",
        ));
    }
    Ok(())
}

/// The stdin prefix for one agent run: `"<len>\n" + AGENT_PL + fields…\0`,
/// the first field being the verb. Port of the plugin's `agent_input`
/// (bin/tailfiles:1274-1279) — measures the code as SENT (our attribution
/// header rides along as inert Perl comments).
pub fn agent_stdin_prefix(verb: &str, fields: &[&str]) -> Result<Vec<u8>, BrowserError> {
    let code = AGENT_PL.as_bytes();
    let mut out = Vec::with_capacity(24 + code.len());
    out.extend_from_slice(code.len().to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(code);
    check_field(verb)?;
    out.extend_from_slice(verb.as_bytes());
    out.push(0);
    for f in fields {
        check_field(f)?;
        out.extend_from_slice(f.as_bytes());
        out.push(0);
    }
    Ok(out)
}

fn record(tag: u8, fields: &[&str]) -> Result<Vec<u8>, BrowserError> {
    let mut out = Vec::with_capacity(64);
    out.push(tag);
    for f in fields {
        check_field(f)?;
        out.extend_from_slice(f.as_bytes());
        out.push(0);
    }
    Ok(out)
}

/// `D rel\0 mode\0 mtime\0` — a directory in the stream (agent applies
/// `(mode & 0777) | 0700` + mtime through its held fd when the dir closes).
pub fn d_record(rel: &str, mode: u32, mtime: i64) -> Result<Vec<u8>, BrowserError> {
    record(b'D', &[rel, &mode.to_string(), &mtime.to_string()])
}

/// `F rel\0 mode\0 mtime\0 size\0` — a file header; exactly `size` raw
/// bytes must follow. Mode is masked 0o777 here (W1: setuid/setgid/sticky
/// never travel; the agent masks again on its side).
pub fn f_record(rel: &str, mode: u32, mtime: i64, size: u64) -> Result<Vec<u8>, BrowserError> {
    record(
        b'F',
        &[
            rel,
            &(mode & 0o777).to_string(),
            &mtime.to_string(),
            &size.to_string(),
        ],
    )
}

/// `L rel\0 target\0` — a symlink (created inside the stage; never followed).
pub fn l_record(rel: &str, target: &str) -> Result<Vec<u8>, BrowserError> {
    record(b'L', &[rel, target])
}

/// One reply record from the agent after `Z`.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentReply {
    /// `C rel\0` — the name exists and replace consent was not given (or a
    /// folder/file kind mismatch under replace).
    Conflict(String),
    /// `E rel\0` — placement failed for this item (non-atomic FS, etc).
    Error(String),
    /// Final `Z` — the agent finished placing and dropped its stage.
    Done,
}

/// Incremental reader for the post-`Z` reply stream, with the protocol caps
/// (per-field ≤ MAX_FIELD_BYTES, total ≤ MAX_REPLY_BYTES).
#[derive(Default)]
pub struct ReplyReader {
    total: usize,
}

impl ReplyReader {
    pub fn new() -> Self {
        Self { total: 0 }
    }

    /// Next reply record; `Ok(None)` on a clean EOF before `Z` (the agent
    /// died mid-place — the caller maps the exit code + stderr).
    pub async fn next(
        &mut self,
        ch: &mut dyn ExecChannel,
    ) -> Result<Option<AgentReply>, BrowserError> {
        let tag = match self.read_byte(ch).await? {
            Some(b) => b,
            None => return Ok(None),
        };
        match tag {
            b'Z' => Ok(Some(AgentReply::Done)),
            b'C' | b'E' => {
                let rel = self.read_field(ch).await?;
                Ok(Some(if tag == b'C' {
                    AgentReply::Conflict(rel)
                } else {
                    AgentReply::Error(rel)
                }))
            }
            other => Err(BrowserError::other(format!(
                "the remote helper replied with an unknown record ({other:#04x})"
            ))),
        }
    }

    async fn read_byte(&mut self, ch: &mut dyn ExecChannel) -> Result<Option<u8>, BrowserError> {
        self.bump(1)?;
        let mut b = [0u8; 1];
        let n = ch.read(&mut b).await?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(b[0]))
    }

    async fn read_field(&mut self, ch: &mut dyn ExecChannel) -> Result<String, BrowserError> {
        let mut v: Vec<u8> = Vec::new();
        loop {
            match self.read_byte(ch).await? {
                None => {
                    return Err(BrowserError::other(
                        "the remote helper's reply ended mid-record",
                    ))
                }
                Some(0) => break,
                Some(b) => {
                    v.push(b);
                    if v.len() > MAX_FIELD_BYTES {
                        return Err(BrowserError::other(
                            "the remote helper sent a name longer than the protocol cap",
                        ));
                    }
                }
            }
        }
        Ok(String::from_utf8_lossy(&v).into_owned())
    }

    fn bump(&mut self, n: usize) -> Result<(), BrowserError> {
        self.total += n;
        if self.total > MAX_REPLY_BYTES {
            return Err(BrowserError::other(
                "the remote helper's reply stream is longer than any real one",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AgentSink — the CopySink that drives the vendored Perl agent
// ---------------------------------------------------------------------------

/// Destination sink over the exec channel: streams the copy into the
/// agent's private stage and lets the AGENT do placement + cleanup through
/// its own held handles. Our side never creates, renames, or removes
/// anything on this host by name — not the stage, not the destination.
pub struct AgentSink {
    ch: Box<dyn ExecChannel>,
    /// Kept for diagnostics only — cleanup is the agent's (handle-bound).
    #[allow(dead_code)]
    stage_name: String,
    placed: bool,
    stdin_closed: bool,
}

impl AgentSink {
    /// Open the agent on `transport`'s exec channel and start the `recv`
    /// verb. `replace` is baked into the prefix ("replace"/"keep") exactly
    /// like the plugin's recv framing; `run_copy` passes the same
    /// `req.replace` here and to `place`.
    pub async fn open(
        t: &dyn SftpTransport,
        dst_dir: &str,
        tag: &str,
        replace: bool,
    ) -> Result<Self, BrowserError> {
        let mut ch = t.open_exec(AGENT_CMD).await?;
        let stage_name = stage_dir_name(tag);
        let mode = if replace { "replace" } else { "keep" };
        let prefix = agent_stdin_prefix("recv", &[dst_dir, &stage_name, mode])?;
        // A dead-on-arrival agent (can't open dst, stage name taken) bails
        // within milliseconds; the bail surfaces at the next write (channel
        // closed, EPIPE-class) or at `place` — the plugin's own ordering.
        ch.write_all(&prefix).await?;
        Ok(Self {
            ch,
            stage_name,
            placed: false,
            stdin_closed: false,
        })
    }

    async fn close_stdin(&mut self) {
        if self.stdin_closed {
            return;
        }
        self.stdin_closed = true;
        if let Err(e) = self.ch.close_stdin().await {
            log::debug!("browser agent: stdin close reported: {e}");
        }
    }

    /// Map a non-zero (or absent) agent exit into the typed taxonomy —
    /// thin wrapper over the pure `classify_agent_failure` (tested there).
    fn agent_failure(&self, rc: Option<u32>) -> BrowserError {
        classify_agent_failure(rc, &self.ch.stderr_tail())
    }
}

/// Pure classification of an agent bail: classify the stderr tail like the
/// plugin's `agent_fail` (bin/tailfiles:1282-1292 — `classify(err)`), keep
/// the exit code visible, and treat 9 ("perl here is too old") as
/// Unsupported. Exit-code contract of the vendored agent: 1 create/write
/// failed, 2 protocol/short input, 3 can't open the destination, 6 identity
/// changed mid-copy, 9 perl too old, 130 HUP/TERM, 141 SIGPIPE.
pub fn classify_agent_failure(rc: Option<u32>, stderr: &str) -> BrowserError {
    let tail: String = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect();
    let rc_text = match rc {
        Some(n) => format!("exit {n}"),
        None => "no exit code".to_string(),
    };
    let message = if tail.is_empty() {
        format!("the copy helper on the other machine stopped early ({rc_text})")
    } else {
        format!("{tail} ({rc_text})")
    };
    let kind = match rc {
        Some(9) => ErrorKind::Unsupported, // "perl here is too old"
        _ => classify_message(stderr),
    };
    BrowserError::new(kind, message)
}

#[async_trait]
impl CopySink for AgentSink {
    async fn dir_begin(&mut self, rel: &str, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        let rec = d_record(rel, mode & 0o7777, mtime)?;
        self.ch.write_all(&rec).await
    }

    async fn dir_end(&mut self) -> Result<(), BrowserError> {
        // The agent applies mode/mtime through its held dir-fd when its own
        // stack closes the directory (recv's $close) — nothing to send.
        Ok(())
    }

    async fn file_begin(&mut self, rel: &str, meta: &FileMeta) -> Result<(), BrowserError> {
        let rec = f_record(rel, meta.mode, meta.mtime, meta.size)?;
        self.ch.write_all(&rec).await
    }

    async fn file_chunk(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        self.ch.write_all(data).await
    }

    async fn file_end(&mut self, _mode: u32, _mtime: i64) -> Result<(), BrowserError> {
        // mode/mtime traveled in the F header; the agent chmod/utimes
        // through its held file handle before closing it.
        Ok(())
    }

    async fn symlink(&mut self, rel: &str, target: &str) -> Result<bool, BrowserError> {
        let rec = l_record(rel, target)?;
        self.ch.write_all(&rec).await?;
        // A POSIX agent can always hold links (perl symlink(2)); a failure
        // bails the agent and surfaces at the next write or at place.
        Ok(true)
    }

    async fn place(
        &mut self,
        _top_names: &[String],
        _replace: bool,
    ) -> Result<(Vec<String>, Vec<(String, String)>), BrowserError> {
        // The replace/keep decision was framed into the recv prefix at open
        // (the plugin's shape); run_copy passes the same req.replace to both.
        self.ch.write_all(b"Z").await?;
        // Close stdin before reading replies: the agent never reads after Z,
        // and an EOF-armed agent self-cleans if placing fails halfway.
        self.close_stdin().await;
        let mut conflicts = Vec::new();
        let mut errors = Vec::new();
        let mut reader = ReplyReader::new();
        let mut done = false;
        while let Some(reply) = reader.next(self.ch.as_mut()).await? {
            match reply {
                AgentReply::Conflict(rel) => conflicts.push(rel),
                AgentReply::Error(rel) => errors.push((
                    rel,
                    "the other machine couldn't move it into place".to_string(),
                )),
                AgentReply::Done => {
                    done = true;
                    break;
                }
            }
        }
        let rc = self.ch.wait_exit(PLACE_WAIT).await;
        if !done {
            // EOF (or a cap error) before the final Z: the agent died
            // mid-place — its $UNDO handlers already dropped the stage.
            return Err(self.agent_failure(rc));
        }
        match rc {
            Some(0) => {
                self.placed = true;
                Ok((conflicts, errors))
            }
            other => Err(self.agent_failure(other)),
        }
    }

    async fn remove(&mut self) -> Result<(), BrowserError> {
        // Cleanup = end the agent's input and wait, bounded. The agent
        // removes what it made through its OWN handles on EOF/failure
        // (drop_stage); nothing is ever removed by name from this side
        // (plugin cleanup ordering, bin/tailfiles:2318-2349).
        self.close_stdin().await;
        if !self.placed {
            match self.ch.wait_exit(CLEANUP_WAIT).await {
                Some(_) => {}
                None => log::warn!(
                    "browser agent: helper did not exit within {}s of its input ending; \
                     its stage self-clean is armed on EOF and the session end kills it",
                    CLEANUP_WAIT.as_secs()
                ),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Capability probe (once per host/account)
// ---------------------------------------------------------------------------

/// Pure classification of the probe's exit code (pinned by tests):
///   2    → perl ran and honored the framing contract (immediate EOF makes
///          the bootstrap's `defined $n or exit 2` fire) → PerlExec
///   any other code (127 = perl not found, 126 = not executable, …)
///        → ExecNoPerl: exec works, the agent doesn't
///   None (timeout / no exit status) → SftpOnly
pub fn classify_probe_exit(rc: Option<u32>) -> AgentCap {
    match rc {
        Some(2) => AgentCap::PerlExec,
        Some(_) => AgentCap::ExecNoPerl,
        None => AgentCap::SftpOnly,
    }
}

/// Run the probe: open the fixed AGENT_CMD, close stdin immediately, wait
/// bounded for the exit code. NEVER errors — a refused/failed probe means
/// the host stays on the unchanged SFTP tiers (documented fallback).
pub async fn probe_agent_cap(t: &dyn SftpTransport) -> AgentCap {
    let mut ch = match t.open_exec(AGENT_CMD).await {
        Ok(ch) => ch,
        Err(e) => {
            log::info!("browser agent probe: exec refused ({e}) — SFTP tiers unchanged");
            return AgentCap::SftpOnly;
        }
    };
    if let Err(e) = ch.close_stdin().await {
        log::info!("browser agent probe: stdin close failed ({e}) — SFTP tiers unchanged");
        return AgentCap::SftpOnly;
    }
    let rc = ch.wait_exit(PROBE_WAIT).await;
    let cap = classify_probe_exit(rc);
    log::info!("browser agent probe: rc={rc:?} → {cap:?}");
    cap
}

/// Probe results per host+account. NO TTL by design ("probe once per
/// host/account"): a capability change is a server-admin event; the cache
/// is invalidated on account change and on forget-host, and a runtime
/// exec refusal falls back with a documented note (never a hard failure).
///
/// S-2 (Codie M2b review): entries are tokio `OnceCell`s, so "probe once
/// per host/account" holds LITERALLY under concurrency — two concurrent
/// first opens of the same host+user coalesce into exactly ONE bounded
/// probe (`get_or_probe`'s `get_or_init` single-flight: the first caller
/// runs it, late callers wait for and receive its result, never a second
/// probe). A cancelled or panicked probe leaves the cell unfilled and the
/// next caller retries — bounded, never poisoned.
pub struct AgentCapCache {
    inner: Mutex<HashMap<String, Arc<OnceCell<AgentCap>>>>,
}

impl AgentCapCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn key(host: &str, user: &str) -> String {
        format!("{host}\u{0}{user}")
    }

    /// The cached capability. `None` when unprobed OR while a probe is
    /// still in flight — callers that must not wait (the delete-path gate)
    /// read "no agent tier yet" and keep the SFTP path.
    pub fn get(&self, host: &str, user: &str) -> Option<AgentCap> {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&Self::key(host, user))
            .and_then(|cell| cell.get())
            .copied()
    }

    /// Test seam: force a cache entry. Production fills the cache ONLY
    /// through `get_or_probe` (and the S-1 self-heal downgrade) — a forced
    /// `put` must never stand in for a real probe result in shipped code.
    #[cfg(test)]
    pub fn put(&self, host: &str, user: &str, cap: AgentCap) {
        let cell = Arc::new(OnceCell::new());
        let _ = cell.set(cap); // fresh, exclusively held: cannot fail
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(Self::key(host, user), cell);
    }

    /// S-2 single-flight: the cached capability, probing EXACTLY ONCE. The
    /// per-key cell is claimed under the short-lived map lock; the probe
    /// itself runs with NO lock held. Concurrent callers for the same
    /// host+user park on the cell and receive the winner's result — the
    /// answer is identical whichever caller filled it. The probe is bounded
    /// (`PROBE_WAIT`) and never errors.
    pub async fn get_or_probe(&self, host: &str, user: &str, t: &dyn SftpTransport) -> AgentCap {
        let cell = {
            let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(
                map.entry(Self::key(host, user))
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };
        *cell.get_or_init(|| probe_agent_cap(t)).await
    }

    /// Drop every cached capability for a host (account change / forget).
    pub fn invalidate_host(&self, host: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|k, _| !k.starts_with(&format!("{host}\u{0}")));
    }
}

impl Default for AgentCapCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Physical delete re-check
// ---------------------------------------------------------------------------

/// S-3: does `stderr` carry the shell's COMMAND-NOT-FOUND shape for the
/// delete helper's command (`xargs`, the first word of `posix_delete_cmd`)?
/// The spelling varies by the remote's shell — all observed shapes, all
/// with rc 127:
///   bash (macOS /bin/sh): "xargs: No such file or directory"
///   dash (Linux /bin/sh): "xargs: not found"
///   zsh:                  "command not found: xargs"
/// Callers gate this on `rc == 127` (the shells' not-found exit code) AND
/// the helper mention below, so an rc 127 with unrelated stderr stays a
/// typed error — never a fallback. When this shape matches, the helper
/// NEVER RAN (exec failed before any path was touched), so taking the
/// documented SFTP-walk fallback cannot double-delete.
pub fn delete_helper_missing(stderr: &str) -> bool {
    let helper = posix_delete_cmd()
        .split_whitespace()
        .next()
        .unwrap_or("xargs")
        .to_lowercase();
    let m = stderr.to_lowercase();
    m.contains(&helper) && (m.contains("not found") || m.contains("no such file or directory"))
}

/// Pure mapping of the delete helper's failed exit (extracted from
/// `agent_delete` so every shape is unit-testable without a transport):
///   - refusal text ("Refusing to delete" / unresolvable home) → Denied,
///     NEVER a fallback (Codie's multi-path probe pinned no-double-delete:
///     a refusal means paths may already be deleted — the SFTP walk must
///     not re-walk them);
///   - S-3: rc 127 + the helper's command-not-found shape → Unsupported,
///     the documented SFTP-walk fallback arm (M2a behavior on hosts whose
///     perl exec lane works but that lack `xargs`);
///   - no exit code → Unreachable (bounded wait expired; nothing deleted);
///   - anything else → classified from the stderr text (typed error).
///
/// The message keeps the last non-empty stderr line (capped 300 chars) or
/// an honest "Delete failed (exit n)" when stderr is empty.
pub fn classify_delete_failure(rc: Option<u32>, stderr: &str) -> BrowserError {
    let Some(n) = rc else {
        return BrowserError::new(
            ErrorKind::Unreachable,
            "the delete check on the other machine timed out — nothing was deleted",
        );
    };
    let tail: String = stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect();
    // The physical re-check REFUSED (home-equivalent target at delete time)
    // or the home is unresolvable — fail closed, Denied, never something to
    // fall back around.
    let refused =
        stderr.contains("Refusing to delete") || stderr.contains("cannot resolve the home folder");
    let kind = if refused {
        ErrorKind::Denied
    } else if n == 127 && delete_helper_missing(stderr) {
        // S-3: the helper command itself is missing → the exec delete lane
        // is unavailable on this host → the documented SFTP-walk fallback.
        ErrorKind::Unsupported
    } else {
        classify_message(stderr)
    };
    let message = if tail.is_empty() {
        format!("Delete failed (exit {n})")
    } else {
        tail
    };
    BrowserError::new(kind, message)
}

/// Delete `paths` (absolute, parent-resolved by the caller's guards) through
/// the exec agent: the vendored POSIX script re-resolves each parent with
/// `cd -P` AT DELETE TIME and re-runs the home guard against the physical
/// spelling before `rm -rf ./name` — the identity re-check the SFTP walk
/// can't do. Script exit codes: 0 done; 1 an rm failed; 3 home unresolvable;
/// 4 a path wasn't absolute; 5 the physical target contains the home folder
/// → REFUSED. NOTE: `xargs` flattens every non-zero child status (BSD → 1,
/// GNU → 123), so — exactly like the plugin (`fail(error_text(err),
/// classify(err))`, bin/tailfiles:2641-2644) — the MEANING is read from
/// stderr via `classify_delete_failure`: a "Refusing to delete" /
/// unresolvable-home line is a Denied decision (never fall back around it);
/// an rc-127 command-not-found for the helper is Unsupported (S-3: the
/// documented SFTP-walk fallback); anything else is classified from the
/// stderr text.
pub async fn agent_delete(t: &dyn SftpTransport, paths: &[String]) -> Result<(), BrowserError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut payload = Vec::new();
    for p in paths {
        if !p.starts_with('/') {
            return Err(BrowserError::other(format!(
                "the delete re-check needs absolute paths (got {p:?})"
            )));
        }
        check_field(p)?;
        payload.extend_from_slice(p.as_bytes());
        payload.push(0);
    }
    let mut ch = t.open_exec(&posix_delete_cmd()).await?;
    ch.write_all(&payload).await?;
    ch.close_stdin().await?;
    let rc = ch.wait_exit(DELETE_WAIT).await;
    let stderr = ch.stderr_tail();
    match rc {
        Some(0) => Ok(()),
        other => Err(classify_delete_failure(other, &stderr)),
    }
}

// ---------------------------------------------------------------------------
// Loopback harness (test-only): the vendored agent as a LOCAL child process
// with the exact same NUL-framed stdin/stdout protocol used over SSH. The
// protocol is the contract — this proves framing, placement, cleanup and
// the delete re-check against the real perl (dev-Mac perl v5.34.1) with no
// SSH server and no fleet host in sight.
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
pub mod loopback {
    use super::*;
    use std::process::Stdio;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::{Child, ChildStdin, ChildStdout, Command};

    /// Safety bound on joining the stderr-drain task inside `wait_exit`.
    /// The child's exit EOFs the stderr pipe, so the drain always ends on
    /// its own; this only guards a pathological case (never observed).
    const STDERR_DRAIN_BOUND: Duration = Duration::from_secs(2);

    /// Loopback only: pick a perl whose build HAS `syscall` (Config
    /// `d_syscall=define`). Homebrew perl 5.44 (default on GitHub macos
    /// runners) is built without it; the system perl has it. Override with
    /// TAILDROP_LOOPBACK_PERL. Falls back to `perl` so a genuinely
    /// incapable machine surfaces the interpreter's real error.
    pub fn loopback_perl() -> String {
        if let Ok(p) = std::env::var("TAILDROP_LOOPBACK_PERL") {
            if !p.is_empty() {
                return p;
            }
        }
        for cand in ["/usr/bin/perl", "perl"] {
            let capable = std::process::Command::new(cand)
                .args([
                    "-MConfig",
                    "-e",
                    "exit(($Config{d_syscall}//q{}) eq 'define' ? 0 : 1)",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if capable {
                return cand.to_string();
            }
        }
        "perl".to_string()
    }

    /// An `ExecChannel` backed by a local `sh -c <cmd>` child. Same framing,
    /// same lifecycle: stdin writes, stdout replies, capped stderr drain,
    /// EOF-by-drop, bounded wait.
    pub struct LocalExecChannel {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
        stderr: Arc<Mutex<Vec<u8>>>,
        /// JoinHandle of the spawned stderr drain. `wait_exit` joins it
        /// before returning, so a `stderr_tail()` read after the wait can
        /// never race a still-running drain (F-1: flaky delete tests).
        stderr_drain: Option<tokio::task::JoinHandle<()>>,
        exit: Option<u32>,
    }

    impl LocalExecChannel {
        pub async fn spawn(cmd: &str) -> Result<Box<dyn ExecChannel>, BrowserError> {
            Self::spawn_opts(cmd, None, None).await
        }

        /// `path`: override the child's PATH (`Some("")` = perl-missing
        /// host simulation). `home`: override $HOME (the delete script
        /// guards against the PHYSICAL home).
        pub async fn spawn_opts(
            cmd: &str,
            path: Option<&str>,
            home: Option<&str>,
        ) -> Result<Box<dyn ExecChannel>, BrowserError> {
            // Loopback-only interpreter resolution. GitHub macos runners put
            // Homebrew perl 5.44 first in PATH, and that build has NO syscall
            // ("The syscall function is unimplemented") — the vendored
            // agent's renameat(2) syscall needs it. The system perl
            // (/usr/bin/perl) has it. Production AGENT_CMD is untouched;
            // this only picks the interpreter for the local test child.
            // `path: Some(_)` simulates a foreign PATH (perl-missing probe)
            // and must keep the literal command.
            let cmd_owned;
            let cmd = if path.is_none() && cmd.starts_with("perl ") {
                cmd_owned = format!("{}{}", loopback_perl(), &cmd["perl".len()..]);
                &cmd_owned[..]
            } else {
                cmd
            };
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(cmd);
            c.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(p) = path {
                c.env("PATH", p);
            }
            if let Some(h) = home {
                c.env("HOME", h);
            }
            let mut child = c
                .spawn()
                .map_err(|e| BrowserError::unsupported(format!("exec spawn refused: {e}")))?;
            let stderr = Arc::new(Mutex::new(Vec::new()));
            let stderr_drain = {
                // Drain stderr concurrently (capped) so a chatty child can
                // never deadlock on a full pipe. `wait_exit` JOINS this task
                // before returning, so `stderr_tail()` read after the wait
                // always sees the complete tail (F-1).
                let mut es = child.stderr.take().expect("stderr piped");
                let sink = Arc::clone(&stderr);
                Some(tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    loop {
                        match es.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut g = sink.lock().unwrap_or_else(|p| p.into_inner());
                                let room = STDERR_CAP.saturating_sub(g.len());
                                g.extend_from_slice(&buf[..n.min(room)]);
                                if room == 0 {
                                    break;
                                }
                            }
                        }
                    }
                }))
            };
            Ok(Box::new(Self {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                child,
                stderr,
                stderr_drain,
                exit: None,
            }))
        }
    }

    #[async_trait]
    impl ExecChannel for LocalExecChannel {
        async fn write_all(&mut self, data: &[u8]) -> Result<(), BrowserError> {
            let si = self.stdin.as_mut().ok_or_else(|| {
                BrowserError::other("the remote helper's input is already closed")
            })?;
            si.write_all(data)
                .await
                .map_err(|e| BrowserError::other(format!("agent stream write: {e}")))?;
            si.flush()
                .await
                .map_err(|e| BrowserError::other(format!("agent stream flush: {e}")))
        }
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
            match self.stdout.as_mut() {
                None => Ok(0),
                Some(so) => so
                    .read(buf)
                    .await
                    .map_err(|e| BrowserError::other(format!("agent reply read: {e}"))),
            }
        }
        async fn close_stdin(&mut self) -> Result<(), BrowserError> {
            self.stdin = None; // drop closes the pipe → the child sees EOF
            Ok(())
        }
        async fn wait_exit(&mut self, timeout: Duration) -> Option<u32> {
            if let Some(rc) = self.exit {
                return Some(rc);
            }
            let status = match tokio::time::timeout(timeout, self.child.wait()).await {
                Ok(Ok(st)) => {
                    // A signalled exit has no code; map it to 255 so callers
                    // still see "exited, not the success contract".
                    let rc = st.code().map(|c| c as u32).unwrap_or(255);
                    self.exit = Some(rc);
                    Some(rc)
                }
                Ok(Err(_)) => None,
                Err(_) => {
                    let _ = self.child.start_kill();
                    None
                }
            };
            // The child has exited (or was just killed): the stderr pipe
            // EOFs, so the drain task necessarily finishes. Join it BEFORE
            // returning so every `stderr_tail()` read after `wait_exit` is
            // deterministic (F-1: the spawned drain used to race the
            // assertions under parallel suite load). The bound is pure
            // safety; abort so a wedged drain can never hang the harness.
            if let Some(mut h) = self.stderr_drain.take() {
                if tokio::time::timeout(STDERR_DRAIN_BOUND, &mut h)
                    .await
                    .is_err()
                {
                    h.abort();
                }
            }
            status
        }
        fn stderr_tail(&self) -> String {
            self.stderr
                .lock()
                .map(|g| String::from_utf8_lossy(&g).into_owned())
                .unwrap_or_default()
        }
    }

    /// The transport double for loopback tests: only `open_exec` (and the
    /// cap) are real; every SFTP verb refuses — the agent lane must never
    /// touch SFTP for staging, placement or cleanup.
    pub struct LoopTransport {
        exts: std::collections::BTreeMap<String, String>,
        pub exec_opens: Arc<std::sync::atomic::AtomicU64>,
        pub refuse_exec: bool,
        pub path_override: Option<String>,
        pub home_override: Option<String>,
    }

    impl LoopTransport {
        pub fn new() -> Self {
            Self {
                exts: Default::default(),
                exec_opens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                refuse_exec: false,
                path_override: None,
                home_override: None,
            }
        }
        pub fn exec_opens(&self) -> u64 {
            self.exec_opens.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn no_sftp<T>() -> Result<T, BrowserError> {
            Err(BrowserError::unsupported(
                "the loopback agent lane never uses SFTP",
            ))
        }
    }

    impl Default for LoopTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl SftpTransport for LoopTransport {
        async fn realpath(&self, _path: &str) -> Result<String, BrowserError> {
            Self::no_sftp()
        }
        async fn read_dir(
            &self,
            _path: &str,
        ) -> Result<Vec<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn stat(
            &self,
            _path: &str,
        ) -> Result<Option<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn lstat(
            &self,
            _path: &str,
        ) -> Result<Option<crate::browser::transport::RemoteNode>, BrowserError> {
            Self::no_sftp()
        }
        async fn mkdir(&self, _path: &str, _perm: u32) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn remove_file(&self, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn remove_dir(&self, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn rename(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn posix_rename(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn hardlink(&self, _from: &str, _to: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn symlink(&self, _target: &str, _path: &str) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        async fn read_link(&self, _path: &str) -> Result<String, BrowserError> {
            Self::no_sftp()
        }
        async fn open_read(
            &self,
            _path: &str,
        ) -> Result<Box<dyn crate::browser::transport::RemoteReader>, BrowserError> {
            Self::no_sftp()
        }
        async fn create_exclusive(
            &self,
            _path: &str,
            _perm: u32,
        ) -> Result<Box<dyn crate::browser::transport::RemoteWriter>, BrowserError> {
            Self::no_sftp()
        }
        async fn set_attrs(
            &self,
            _path: &str,
            _mode: Option<u32>,
            _mtime: Option<i64>,
        ) -> Result<(), BrowserError> {
            Self::no_sftp()
        }
        fn has_hardlink(&self) -> bool {
            false
        }
        fn has_posix_rename(&self) -> bool {
            false
        }
        fn extensions(&self) -> &std::collections::BTreeMap<String, String> {
            &self.exts
        }
        async fn close(self: Box<Self>) -> Result<(), BrowserError> {
            Ok(())
        }
        fn agent_cap(&self) -> AgentCap {
            AgentCap::PerlExec
        }
        async fn open_exec(&self, cmd: &str) -> Result<Box<dyn ExecChannel>, BrowserError> {
            if self.refuse_exec {
                return Err(BrowserError::unsupported("exec refused (loopback)"));
            }
            self.exec_opens
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            LocalExecChannel::spawn_opts(
                cmd,
                self.path_override.as_deref(),
                self.home_override.as_deref(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure protocol / framing -----------------------------------------

    #[test]
    fn agent_cmd_is_plugin_exact() {
        // Byte-identical to bin/tailfiles:1264-1265 (AGENT_CMD).
        assert_eq!(
            AGENT_CMD,
            "perl -e 'my $n = <STDIN>; defined $n or exit 2; read(STDIN, my $c, $n) == $n or exit 2; eval $c; die $@ if $@; exit 0'"
        );
    }

    #[test]
    fn stdin_prefix_matches_plugin_agent_input() {
        // Plugin agent_input (POSIX branch): str(len(code)) + "\n" + code +
        // each field NUL-terminated — measured on the code AS SENT.
        let fields = [
            "recv",
            "/dst dir",
            ".tailfiles-incoming-0123456789abcdef",
            "keep",
        ];
        let prefix = agent_stdin_prefix(fields[0], &fields[1..]).unwrap();
        let code = AGENT_PL.as_bytes();
        let header = format!("{}\n", code.len());
        assert!(prefix.starts_with(header.as_bytes()));
        let rest = &prefix[header.len()..];
        assert!(rest.starts_with(code));
        let after = &rest[code.len()..];
        let mut expected: Vec<u8> = Vec::new();
        for f in fields {
            expected.extend_from_slice(f.as_bytes());
            expected.push(0);
        }
        assert_eq!(after, expected.as_slice());
    }

    #[test]
    fn framing_refuses_nul_and_oversize_fields() {
        assert!(agent_stdin_prefix("recv", &["/ok", "bad\u{0}field"]).is_err());
        assert!(agent_stdin_prefix("recv", &["x".repeat(MAX_FIELD_BYTES + 1).as_str()]).is_err());
        assert!(d_record("a\u{0}b", 0o755, 0).is_err());
        assert!(f_record("ok", 0o644, 0, 1).is_ok());
        assert!(l_record("ok", "tar\u{0}get").is_err());
    }

    #[test]
    fn record_shapes_are_exact() {
        // Wire fields are DECIMAL ASCII (perl stringifies numbers as
        // decimal; the recv side re-masks with `& 0777`).
        assert_eq!(
            d_record("d", 0o755, 17).unwrap(),
            [b"Dd\0".as_slice(), b"493\0".as_slice(), b"17\0".as_slice()].concat(),
            "0o755 travels as decimal 493"
        );
        assert_eq!(
            f_record("f.txt", 0o1644, 18, 99).unwrap(),
            [
                b"Ff.txt\0".as_slice(),
                b"420\0".as_slice(),
                b"18\0".as_slice(),
                b"99\0".as_slice()
            ]
            .concat(),
            "F masks the mode to 0777 (W1: setuid/setgid/sticky never travel); 0o644 = 420"
        );
        assert_eq!(l_record("l", "t").unwrap(), b"Ll\0t\0".to_vec());
    }

    #[test]
    fn posix_delete_cmd_is_the_plugin_wrap() {
        let cmd = posix_delete_cmd();
        let script = POSIX_DELETE_SCRIPT.trim_end_matches('\n');
        assert!(cmd.starts_with("xargs -0 sh -c '"));
        assert!(cmd.ends_with("' sh"));
        assert!(!script.contains('\''), "the sh -c wrap needs no escaping");
        assert_eq!(
            cmd,
            format!("xargs -0 sh -c '{script}' sh"),
            "plugin POSIX_DELETE_CMD shape (bin/tailfiles:2573)"
        );
    }

    #[test]
    fn probe_exit_classification() {
        assert_eq!(classify_probe_exit(Some(2)), AgentCap::PerlExec);
        assert_eq!(classify_probe_exit(Some(127)), AgentCap::ExecNoPerl);
        assert_eq!(classify_probe_exit(Some(0)), AgentCap::ExecNoPerl);
        assert_eq!(classify_probe_exit(Some(126)), AgentCap::ExecNoPerl);
        assert_eq!(classify_probe_exit(None), AgentCap::SftpOnly);
    }

    #[test]
    fn agent_failure_classification() {
        // The plugin's agent_fail contract: classify the stderr text.
        let e = classify_agent_failure(Some(3), "Can't write to /dst (File exists)\n");
        assert_eq!(e.kind, ErrorKind::Exists, "{e:?}");
        assert!(e.message.contains("exit 3"), "{e:?}");
        let e = classify_agent_failure(Some(1), "Can't create x (Permission denied)\n");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        let e = classify_agent_failure(Some(9), "perl here is too old (needs 5.22 or newer)\n");
        assert_eq!(e.kind, ErrorKind::Unsupported, "{e:?}");
        let e = classify_agent_failure(Some(6), "docs/x changed while copying\n");
        assert_eq!(e.kind, ErrorKind::Other, "{e:?}");
        // No stderr, no exit code: honest generic message, never a guess.
        let e = classify_agent_failure(None, "");
        assert_eq!(e.kind, ErrorKind::Other);
        assert!(e.message.contains("stopped early"), "{e:?}");
        assert!(e.message.contains("no exit code"), "{e:?}");
        // Only the LAST non-empty stderr line rides along (capped 300 chars).
        let e = classify_agent_failure(Some(2), "noise\nshort input\n");
        assert!(e.message.starts_with("short input"), "{e:?}");
    }

    /// S-3: every delete-failure shape the brief pins — the command-not-found
    /// arm (all three real shell spellings), the rc-127-with-unrelated-stderr
    /// arm (typed error, NEVER a fallback), and the refusal arm (Denied,
    /// never a fallback — Codie's multi-path probe pinned no-double-delete).
    #[test]
    fn delete_failure_classification_shapes() {
        // rc 127 + the helper's not-found shape → Unsupported (SFTP-walk
        // fallback arm). Observed spellings per shell:
        for stderr in [
            "/bin/sh: xargs: No such file or directory\n", // bash-as-sh (macOS)
            "sh: 1: xargs: not found\n",                   // dash (Linux /bin/sh)
            "zsh:1: command not found: xargs\n",           // zsh
        ] {
            let e = classify_delete_failure(Some(127), stderr);
            assert_eq!(
                e.kind,
                ErrorKind::Unsupported,
                "{stderr:?} must map to the fallback arm: {e:?}"
            );
        }
        // rc 127 with UNRELATED stderr → typed error, never the fallback.
        let e = classify_delete_failure(Some(127), "the helper exploded weirdly\n");
        assert_eq!(e.kind, ErrorKind::Other, "{e:?}");
        // A not-found spelling WITHOUT the helper mention → not the shape.
        let e = classify_delete_failure(Some(127), "config.json: not found\n");
        assert_eq!(e.kind, ErrorKind::Notfound, "{e:?}");
        assert!(!delete_helper_missing("config.json: not found"));
        // The not-found shape WITHOUT rc 127 → stays a typed error.
        let e = classify_delete_failure(Some(1), "/bin/sh: xargs: No such file or directory\n");
        assert_eq!(e.kind, ErrorKind::Notfound, "{e:?}");
        // REFUSALS are never a fallback, whatever the rc (Codie probe pin):
        let e = classify_delete_failure(Some(5), "Refusing to delete /home/u/x (home inside)\n");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        let e = classify_delete_failure(Some(1), "Refusing to delete /private/tmp/home\n");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        // A refusal wins even in a bizarre rc-127 + not-found combination.
        let e = classify_delete_failure(
            Some(127),
            "Refusing to delete /home/u\nsh: xargs: not found\n",
        );
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        // Unresolvable home → Denied (fail closed).
        let e = classify_delete_failure(Some(1), "cannot resolve the home folder\n");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        // No exit code → Unreachable timeout (nothing deleted).
        let e = classify_delete_failure(None, "");
        assert_eq!(e.kind, ErrorKind::Unreachable, "{e:?}");
        assert!(e.message.contains("nothing was deleted"), "{e:?}");
        // Empty stderr keeps the exit code visible; a tail rides along.
        let e = classify_delete_failure(Some(127), "");
        assert_eq!(e.message, "Delete failed (exit 127)");
        let e = classify_delete_failure(Some(1), "noise\nrm: /x/y: Permission denied\n");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        assert_eq!(e.message, "rm: /x/y: Permission denied");
    }

    #[test]
    fn cap_cache_is_per_host_account_and_invalidates() {
        let c = AgentCapCache::new();
        assert_eq!(c.get("h", "alice"), None);
        c.put("h", "alice", AgentCap::PerlExec);
        c.put("h", "bob", AgentCap::ExecNoPerl);
        c.put("h2", "alice", AgentCap::SftpOnly);
        assert_eq!(c.get("h", "alice"), Some(AgentCap::PerlExec));
        assert_eq!(c.get("h", "bob"), Some(AgentCap::ExecNoPerl));
        c.invalidate_host("h");
        assert_eq!(c.get("h", "alice"), None);
        assert_eq!(c.get("h", "bob"), None);
        assert_eq!(
            c.get("h2", "alice"),
            Some(AgentCap::SftpOnly),
            "invalidate is host-scoped only"
        );
        // A host name can't prefix-collide into another host's entries.
        c.put("host", "u", AgentCap::PerlExec);
        c.invalidate_host("hos");
        assert_eq!(c.get("host", "u"), Some(AgentCap::PerlExec));
    }

    // ---- vendoring integrity (sha256 pins from THIRD_PARTY.md) -----------

    #[test]
    fn vendored_agent_body_matches_pin() {
        use sha2::{Digest, Sha256};
        // The body = every line from "use strict; use warnings;" to EOF. The
        // attribution header QUOTES that marker inside a comment, so anchor
        // on the line-start occurrence (the last one). The plugin's AGENT_PL
        // raw string begins with a newline; the vendored file's header ends
        // at that boundary, so "\n" + body reconstitutes the plugin bytes
        // exactly — that reconstituted form is what the pin covers.
        let marker = "\nuse strict; use warnings;\n";
        let idx = AGENT_PL.rfind(marker).expect("body marker present") + 1;
        let body = &AGENT_PL[idx..];
        let plugin_form = format!("\n{body}");
        let hash = Sha256::digest(plugin_form.as_bytes());
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "10f732185203fca259a75b06fcaadf00f6a00ecea786cbc45a03f1840f1e1bbd",
            "the vendored body must stay byte-identical to the plugin's AGENT_PL"
        );
        assert_eq!(plugin_form.len(), 9820);
        assert_eq!(
            body.lines().next().unwrap(),
            "use strict; use warnings;",
            "the body starts at the marker line"
        );
    }

    #[test]
    fn vendored_posix_delete_matches_pin() {
        use sha2::{Digest, Sha256};
        let script = POSIX_DELETE_SCRIPT.trim_end_matches('\n');
        let hash = Sha256::digest(script.as_bytes());
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b00c22f6f55502cac13983a040bfb6bd82b5851e63cf500b397ab9b5f50798ff"
        );
        assert_eq!(script.len(), 633);
        assert_eq!(script.lines().count(), 1, "single line, as vendored");
    }

    // ---- reply reader (protocol caps) -------------------------------------

    /// An ExecChannel over a canned byte script (EOF after).
    struct CannedChannel {
        data: std::collections::VecDeque<u8>,
    }

    #[async_trait]
    impl ExecChannel for CannedChannel {
        async fn write_all(&mut self, _data: &[u8]) -> Result<(), BrowserError> {
            Ok(())
        }
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, BrowserError> {
            let n = buf.len().min(self.data.len());
            for (i, b) in self.data.drain(..n).enumerate() {
                buf[i] = b;
            }
            Ok(n)
        }
        async fn close_stdin(&mut self) -> Result<(), BrowserError> {
            Ok(())
        }
        async fn wait_exit(&mut self, _timeout: Duration) -> Option<u32> {
            Some(0)
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    #[tokio::test]
    async fn reply_reader_parses_c_e_z() {
        let mut ch = CannedChannel {
            data: b"Cx.txt\0Edir/bad\0Z".iter().copied().collect(),
        };
        let mut r = ReplyReader::new();
        assert_eq!(
            r.next(&mut ch).await.unwrap(),
            Some(AgentReply::Conflict("x.txt".into()))
        );
        assert_eq!(
            r.next(&mut ch).await.unwrap(),
            Some(AgentReply::Error("dir/bad".into()))
        );
        assert_eq!(r.next(&mut ch).await.unwrap(), Some(AgentReply::Done));
        // EOF after Z stays None (never a spurious extra record).
        assert_eq!(r.next(&mut ch).await.unwrap(), None);
    }

    #[tokio::test]
    async fn reply_reader_caps_and_refusals() {
        // Unknown record type.
        let mut ch = CannedChannel {
            data: b"Qx\0".iter().copied().collect(),
        };
        assert!(ReplyReader::new().next(&mut ch).await.is_err());
        // Field longer than the cap.
        let mut big = b"C".to_vec();
        big.extend_from_slice(&vec![b'x'; MAX_FIELD_BYTES + 1]);
        big.push(0);
        let mut ch = CannedChannel { data: big.into() };
        assert!(ReplyReader::new().next(&mut ch).await.is_err());
        // EOF mid-field.
        let mut ch = CannedChannel {
            data: b"Cpartial".iter().copied().collect(),
        };
        assert!(ReplyReader::new().next(&mut ch).await.is_err());
    }
}

// ---------------------------------------------------------------------------
// Loopback integration: the REAL vendored perl agent as a local child
// (unix-only; perl v5.34.1 on the dev Mac — recorded per the brief).
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod loopback_tests {
    use super::loopback::{LocalExecChannel, LoopTransport};
    use super::*;
    use crate::browser::fake::temp_root;
    use crate::browser::transfer::new_stage_tag;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

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

    fn meta(size: u64) -> FileMeta {
        FileMeta {
            size,
            mode: 0o644,
            mtime: 1_700_000_123,
        }
    }

    fn mtime_secs(path: &Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn ino(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().ino()
    }

    /// The loopback interpreter resolution must land on a perl whose build
    /// has `syscall` (CI macos runners default to Homebrew perl 5.44, which
    /// lacks it; the system perl has it). Pins the CI fix itself.
    #[test]
    fn loopback_perl_resolution_supports_syscall() {
        let p = super::loopback::loopback_perl();
        let status = std::process::Command::new(&p)
            .args([
                "-MConfig",
                "-e",
                "exit(($Config{d_syscall}//q{}) eq 'define' ? 0 : 1)",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("resolved perl runs");
        assert!(
            status.success(),
            "resolved loopback perl {p} must support syscall"
        );
    }

    /// The probe end-to-end over the loopback channel: real perl honors the
    /// bootstrap contract (immediate EOF → exit 2) → PerlExec.
    #[tokio::test]
    async fn loopback_probe_real_perl_is_perl_exec() {
        let t = LoopTransport::new();
        assert_eq!(probe_agent_cap(&t).await, AgentCap::PerlExec);
        assert_eq!(t.exec_opens(), 1, "exactly one probe exec");
    }

    /// perl-missing host: exec works, perl doesn't (PATH stripped → 127) →
    /// ExecNoPerl → the caller keeps the SFTP tiers (documented fallback).
    #[tokio::test]
    async fn loopback_probe_perl_missing_is_exec_no_perl() {
        let mut t = LoopTransport::new();
        t.path_override = Some(String::new());
        assert_eq!(probe_agent_cap(&t).await, AgentCap::ExecNoPerl);
    }

    /// exec refused by the server → SftpOnly, never an error.
    #[tokio::test]
    async fn loopback_probe_exec_refused_is_sftp_only() {
        let mut t = LoopTransport::new();
        t.refuse_exec = true;
        assert_eq!(probe_agent_cap(&t).await, AgentCap::SftpOnly);
        assert_eq!(t.exec_opens(), 0);
    }

    /// S-2 over the loopback harness: two concurrent first opens of the same
    /// host+user share ONE real perl probe (exactly one exec open; the
    /// waiter receives the winner's PerlExec), and a later call is a pure
    /// cache hit — never a second probe.
    #[tokio::test]
    async fn loopback_get_or_probe_single_flight_one_exec() {
        let cache = AgentCapCache::new();
        let t = LoopTransport::new();
        let (a, b) = tokio::join!(
            cache.get_or_probe("h", "u", &t),
            cache.get_or_probe("h", "u", &t),
        );
        assert_eq!(a, AgentCap::PerlExec);
        assert_eq!(b, AgentCap::PerlExec);
        assert_eq!(t.exec_opens(), 1, "concurrent first opens share one probe");
        assert_eq!(cache.get_or_probe("h", "u", &t).await, AgentCap::PerlExec);
        assert_eq!(t.exec_opens(), 1, "a cache hit never re-probes");
        assert_eq!(cache.get("h", "u"), Some(AgentCap::PerlExec));
    }

    /// Full recv roundtrip through the production AgentSink: a folder with
    /// mode/mtime, a file spanning the record reader, and a symlink —
    /// placed atomically, stage gone, no SFTP ever touched.
    #[tokio::test]
    async fn loopback_agent_sink_places_tree_and_cleans_stage() {
        let dst = temp_root("agent-place");
        let t = LoopTransport::new();
        let tag = new_stage_tag();
        let mut sink = AgentSink::open(&t, dst.to_str().unwrap(), &tag, false)
            .await
            .expect("agent opens");
        sink.dir_begin("docs", 0o750, 1_700_000_001).await.unwrap();
        let payload = b"inner bytes";
        sink.file_begin("docs/in.txt", &meta(payload.len() as u64))
            .await
            .unwrap();
        sink.file_chunk(payload).await.unwrap();
        sink.file_end(0o644, 1_700_000_123).await.unwrap();
        sink.symlink("docs/link", "in.txt").await.unwrap();
        sink.dir_end().await.unwrap();
        let top = b"top file";
        sink.file_begin("top.txt", &meta(top.len() as u64))
            .await
            .unwrap();
        sink.file_chunk(top).await.unwrap();
        sink.file_end(0o644, 1_700_000_123).await.unwrap();
        let (conflicts, errors) = sink.place(&[], false).await.expect("place ok");
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert!(errors.is_empty(), "{errors:?}");
        sink.remove().await.unwrap();

        assert_eq!(std::fs::read(dst.join("docs/in.txt")).unwrap(), payload);
        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), top);
        let link = std::fs::read_link(dst.join("docs/link")).unwrap();
        assert_eq!(link, PathBuf::from("in.txt"));
        assert_eq!(mtime_secs(&dst.join("docs/in.txt")), 1_700_000_123);
        let dmeta = std::fs::metadata(dst.join("docs")).unwrap();
        assert_eq!(dmeta.mode() & 0o777, 0o750, "dir mode travels");
        assert_eq!(
            mtime_secs(&dst.join("docs")),
            1_700_000_001,
            "dir mtime travels"
        );
        assert!(stage_leftovers(&dst).is_empty(), "agent dropped its stage");
        assert_eq!(t.exec_opens(), 1);
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// TOCTOU class CLOSED: the destination name appears AFTER the listing
    /// (here: after streaming started) — the agent's atomic no-replace
    /// rename still refuses it, the planted bytes are untouched, and the
    /// item is reported as a conflict (never overwritten, never half-placed).
    #[tokio::test]
    async fn loopback_toctou_no_replace_refuses_late_collision() {
        let dst = temp_root("agent-toctou");
        let t = LoopTransport::new();
        let mut sink = AgentSink::open(&t, dst.to_str().unwrap(), &new_stage_tag(), false)
            .await
            .unwrap();
        let payload = b"our new bytes";
        sink.file_begin("x.txt", &meta(payload.len() as u64))
            .await
            .unwrap();
        // The collision appears AFTER any listing could have seen it.
        std::fs::write(dst.join("x.txt"), b"PLANTED").unwrap();
        sink.file_chunk(payload).await.unwrap();
        sink.file_end(0o644, 1_700_000_123).await.unwrap();
        let (conflicts, errors) = sink.place(&[], false).await.expect("place reports");
        assert_eq!(conflicts, vec!["x.txt".to_string()]);
        assert!(errors.is_empty(), "{errors:?}");
        sink.remove().await.unwrap();
        assert_eq!(
            std::fs::read(dst.join("x.txt")).unwrap(),
            b"PLANTED",
            "the late-planted file is untouched"
        );
        assert!(stage_leftovers(&dst).is_empty());
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// Consented Replace is ATOMIC: the old inode is swapped for the staged
    /// one by a single replacing rename (inode changes, bytes exact).
    #[tokio::test]
    async fn loopback_replace_is_atomic() {
        let dst = temp_root("agent-replace");
        std::fs::write(dst.join("x.txt"), b"OLD").unwrap();
        let old_ino = ino(&dst.join("x.txt"));
        let t = LoopTransport::new();
        let mut sink = AgentSink::open(&t, dst.to_str().unwrap(), &new_stage_tag(), true)
            .await
            .unwrap();
        let payload = b"NEW BYTES";
        sink.file_begin(
            "x.txt",
            &FileMeta {
                size: payload.len() as u64,
                mode: 0o644,
                mtime: 1_700_000_456,
            },
        )
        .await
        .unwrap();
        sink.file_chunk(payload).await.unwrap();
        sink.file_end(0o644, 1_700_000_456).await.unwrap();
        let (conflicts, errors) = sink.place(&[], true).await.unwrap();
        assert!(conflicts.is_empty() && errors.is_empty());
        sink.remove().await.unwrap();
        assert_eq!(std::fs::read(dst.join("x.txt")).unwrap(), payload);
        assert_ne!(ino(&dst.join("x.txt")), old_ino, "replaced by rename");
        assert_eq!(mtime_secs(&dst.join("x.txt")), 1_700_000_456);
        assert!(stage_leftovers(&dst).is_empty());
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// Mid-stream EOF (cancel/timeout/failure class): stdin closes without
    /// `Z`, the agent bails "short input" and removes ITS stage through its
    /// held handles — nothing lands, nothing is left.
    #[tokio::test]
    async fn loopback_eof_midstream_cleans_stage() {
        let dst = temp_root("agent-eof");
        let t = LoopTransport::new();
        let mut sink = AgentSink::open(&t, dst.to_str().unwrap(), &new_stage_tag(), false)
            .await
            .unwrap();
        sink.file_begin("big.bin", &meta(100)).await.unwrap();
        sink.file_chunk(&[7u8; 50]).await.unwrap(); // half the declared size
        sink.remove().await.unwrap(); // EOF without Z — the cancel path
        let entries: Vec<String> = std::fs::read_dir(&dst)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            entries.is_empty(),
            "the agent removed its stage and placed nothing: {entries:?}"
        );
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// A planted lookalike stage (same prefix, different tag, with content)
    /// is NEVER touched by the agent's cleanup — it removes only what it
    /// created, through its own handles.
    #[tokio::test]
    async fn loopback_planted_lookalike_stage_untouched() {
        let dst = temp_root("agent-lookalike");
        let planted_name = format!("{}deadbeefcafebabe", crate::browser::STAGE_PREFIX);
        let planted = dst.join(&planted_name);
        std::fs::create_dir_all(planted.join("inner")).unwrap();
        std::fs::write(planted.join("inner/keep.txt"), b"KEEP ME").unwrap();
        let t = LoopTransport::new();
        let mut sink = AgentSink::open(&t, dst.to_str().unwrap(), &new_stage_tag(), false)
            .await
            .unwrap();
        sink.file_begin("f.bin", &meta(40)).await.unwrap();
        sink.file_chunk(&[1u8; 10]).await.unwrap();
        sink.remove().await.unwrap(); // EOF cleanup with a lookalike present
        assert_eq!(
            std::fs::read(planted.join("inner/keep.txt")).unwrap(),
            b"KEEP ME",
            "the planted stage is byte-identical"
        );
        let left = stage_leftovers(&dst);
        assert_eq!(left, vec![planted_name], "only the planted stage remains");
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// A stage-name collision at open (planted at OUR exact random tag)
    /// makes the agent bail (its exclusive mkdir fails, exit 3 with "File
    /// exists") — the copy errors and the planted folder is untouched.
    #[tokio::test]
    async fn loopback_stage_collision_at_open_refuses() {
        let dst = temp_root("agent-collide");
        let tag = new_stage_tag();
        let planted = dst.join(stage_dir_name(&tag));
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::write(planted.join("mine.txt"), b"MINE").unwrap();
        let t = LoopTransport::new();
        // The agent bails right after reading the recv fields; depending on
        // scheduling the refusal surfaces at open (dead pipe), at a stream
        // write, or at place (EOF without the final Z + a non-zero exit).
        // Every path is an error — none of them places or removes anything.
        match AgentSink::open(&t, dst.to_str().unwrap(), &tag, false).await {
            Err(_) => {}
            Ok(mut sink) => {
                let mut result = sink.file_begin("f.txt", &meta(3)).await;
                if result.is_ok() {
                    result = sink.file_chunk(b"abc").await;
                }
                if result.is_ok() {
                    result = sink.file_end(0o644, 0).await;
                }
                if result.is_ok() {
                    result = sink.place(&[], false).await.map(|_| ());
                }
                assert!(
                    result.is_err(),
                    "a taken stage name must fail the copy: {result:?}"
                );
                sink.remove().await.unwrap();
            }
        }
        assert!(
            !dst.join("f.txt").exists(),
            "nothing was placed next to the planted stage"
        );
        assert_eq!(
            std::fs::read(planted.join("mine.txt")).unwrap(),
            b"MINE",
            "the planted folder at our tag is untouched"
        );
        let _ = std::fs::remove_dir_all(&dst);
    }

    // ---- physical delete re-check -----------------------------------------

    fn delete_home_setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        // base/home (HOME, with a secret), base/work (the confirmed victim)
        let base = temp_root(tag);
        let home = base.join("home");
        let work = base.join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("secret.txt"), b"HOME SECRET").unwrap();
        std::fs::create_dir_all(work.join("nested")).unwrap();
        std::fs::write(work.join("item.txt"), b"victim item").unwrap();
        std::fs::write(work.join("nested/deep.txt"), b"deep").unwrap();
        (base, home, work)
    }

    /// Happy path: the agent deletes the confirmed tree (rm -rf via held
    /// physical resolution), exit 0, home untouched.
    #[tokio::test]
    async fn loopback_delete_removes_confirmed_tree() {
        let (base, home, work) = delete_home_setup("agent-del-ok");
        let mut t = LoopTransport::new();
        t.home_override = Some(home.to_str().unwrap().to_string());
        agent_delete(&t, &[work.to_str().unwrap().to_string()])
            .await
            .expect("delete succeeds");
        assert!(!work.exists(), "the confirmed tree is gone");
        assert_eq!(
            std::fs::read(home.join("secret.txt")).unwrap(),
            b"HOME SECRET"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The identity-change class CLOSED: between confirm and delete, the
    /// home folder is MOVED UNDER the confirmed path — the physical re-check
    /// sees that deleting the target would take the home with it and
    /// REFUSES (exit 5 → Denied). Victim and home both intact.
    #[tokio::test]
    async fn loopback_delete_recheck_refuses_identity_change() {
        let (base, home, work) = delete_home_setup("agent-del-swap");
        // "Confirm" happened here: `work` was a plain dir with item.txt.
        // Attacker (or admin move): home now lives UNDER work.
        std::fs::rename(&home, work.join("home")).unwrap();
        let moved_home = work.join("home");
        let mut t = LoopTransport::new();
        t.home_override = Some(moved_home.to_str().unwrap().to_string());
        let e = agent_delete(&t, &[work.to_str().unwrap().to_string()])
            .await
            .expect_err("the physical re-check must refuse");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        assert!(work.exists(), "the target was NOT deleted");
        assert_eq!(
            std::fs::read(moved_home.join("secret.txt")).unwrap(),
            b"HOME SECRET",
            "home intact"
        );
        assert_eq!(
            std::fs::read(work.join("item.txt")).unwrap(),
            b"victim item"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Deleting the home folder itself is refused by the physical guard.
    #[tokio::test]
    async fn loopback_delete_refuses_home_itself() {
        let (base, home, work) = delete_home_setup("agent-del-home");
        let mut t = LoopTransport::new();
        t.home_override = Some(home.to_str().unwrap().to_string());
        let e = agent_delete(&t, &[home.to_str().unwrap().to_string()])
            .await
            .expect_err("home is never deletable");
        assert_eq!(e.kind, ErrorKind::Denied);
        assert_eq!(
            std::fs::read(home.join("secret.txt")).unwrap(),
            b"HOME SECRET"
        );
        assert!(work.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Relative paths never reach the shell — the Rust gate refuses first.
    #[tokio::test]
    async fn delete_refuses_relative_paths_before_exec() {
        let t = LoopTransport::new();
        let e = agent_delete(&t, &["relative/path".to_string()])
            .await
            .expect_err("relative refused");
        assert_eq!(e.kind, ErrorKind::Other);
        assert_eq!(t.exec_opens(), 0, "no exec for a malformed path");
        // NUL inside a path is refused too (framing integrity).
        let e = agent_delete(&t, &["/ok\u{0}/bad".to_string()])
            .await
            .expect_err("NUL refused");
        assert_eq!(e.kind, ErrorKind::Other);
        assert_eq!(t.exec_opens(), 0);
        // Empty input is a no-op, not an exec.
        agent_delete(&t, &[]).await.unwrap();
        assert_eq!(t.exec_opens(), 0);
    }

    /// The raw script's own exit-code contract (0/3/4/5), invoked WITHOUT
    /// the xargs wrapper so the codes arrive unmangled — the production
    /// path goes through xargs, which flattens non-zero codes (BSD → 1,
    /// GNU → 123) and leaves stderr as the meaning carrier (agent_delete
    /// reads it that way, exactly like the plugin).
    #[tokio::test]
    async fn loopback_delete_script_raw_exit_codes() {
        let (base, home, work) = delete_home_setup("agent-del-rc");
        let script = POSIX_DELETE_SCRIPT.trim_end_matches('\n');
        let home_s = home.to_str().unwrap();

        // rc 3: HOME can't be resolved → fail closed before any rm.
        let cmd = format!("sh -c '{script}' sh {}", work.display());
        let mut ch = LocalExecChannel::spawn_opts(&cmd, None, Some("/nonexistent-home-xyz/home"))
            .await
            .unwrap();
        ch.close_stdin().await.unwrap();
        assert_eq!(ch.wait_exit(DELETE_WAIT).await, Some(3));
        assert!(ch.stderr_tail().contains("cannot resolve the home folder"));

        // rc 4: a relative path never reaches rm.
        let cmd = format!("sh -c '{script}' sh relative/thing");
        let mut ch = LocalExecChannel::spawn_opts(&cmd, None, Some(home_s))
            .await
            .unwrap();
        ch.close_stdin().await.unwrap();
        assert_eq!(ch.wait_exit(DELETE_WAIT).await, Some(4));
        assert!(ch.stderr_tail().contains("not absolute"));

        // rc 5: the target IS the home folder → refused, nothing removed.
        let cmd = format!("sh -c '{script}' sh {home_s}");
        let mut ch = LocalExecChannel::spawn_opts(&cmd, None, Some(home_s))
            .await
            .unwrap();
        ch.close_stdin().await.unwrap();
        assert_eq!(ch.wait_exit(DELETE_WAIT).await, Some(5));
        assert!(ch.stderr_tail().contains("Refusing to delete"));
        assert!(home.join("secret.txt").exists());

        // rc 0: a plain confirmed victim deletes cleanly.
        let cmd = format!("sh -c '{script}' sh {}", work.display());
        let mut ch = LocalExecChannel::spawn_opts(&cmd, None, Some(home_s))
            .await
            .unwrap();
        ch.close_stdin().await.unwrap();
        assert_eq!(ch.wait_exit(DELETE_WAIT).await, Some(0));
        assert!(!work.exists());
        assert_eq!(
            std::fs::read(home.join("secret.txt")).unwrap(),
            b"HOME SECRET"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S-3 (Codie M2b review): a perl host WITHOUT `xargs` — the delete
    /// helper's command-not-found shape (rc 127 + "not found" stderr) must
    /// map to Unsupported so browser_delete takes the documented SFTP-walk
    /// fallback (M2a behavior preserved). The failed helper deletes nothing.
    #[tokio::test]
    async fn loopback_delete_xargs_missing_is_unsupported_fallback() {
        let (base, home, work) = delete_home_setup("agent-del-noxargs");
        let mut t = LoopTransport::new();
        t.home_override = Some(home.to_str().unwrap().to_string());
        // PATH without xargs: the shell itself still runs (spawned by
        // absolute path), but it can't FIND `xargs` → rc 127 with the
        // shell's own not-found spelling (bash-as-sh on macOS: "xargs: No
        // such file or directory"; dash: "xargs: not found"; zsh: "command
        // not found: xargs" — delete_helper_missing covers all three).
        t.path_override = Some(String::new());
        let e = agent_delete(&t, &[work.to_str().unwrap().to_string()])
            .await
            .expect_err("a missing xargs must fail the delete helper");
        assert_eq!(
            e.kind,
            ErrorKind::Unsupported,
            "command-not-found must map to the SFTP-fallback arm: {e:?}"
        );
        assert!(
            work.exists(),
            "the failed helper deleted nothing (fail closed before fallback)"
        );
        assert_eq!(t.exec_opens(), 1, "one helper exec, then the typed error");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The production (xargs-wrapped) shape: a refusal arrives as a
    /// NON-ZERO rc with the meaning on stderr — platform-independent, since
    /// BSD xargs maps child failures to 1 and GNU xargs to 123.
    #[tokio::test]
    async fn loopback_delete_wrapped_refusal_shape() {
        let (base, home, _work) = delete_home_setup("agent-del-wrap");
        let mut t = LoopTransport::new();
        t.home_override = Some(home.to_str().unwrap().to_string());
        let e = agent_delete(&t, &[home.to_str().unwrap().to_string()])
            .await
            .expect_err("home refusal through the production wrapper");
        assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
        assert!(e.message.contains("Refusing to delete"), "{e:?}");
        assert_eq!(
            std::fs::read(home.join("secret.txt")).unwrap(),
            b"HOME SECRET"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
