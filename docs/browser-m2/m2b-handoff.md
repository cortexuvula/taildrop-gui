# M2b handoff — POSIX exec-agent parity + owner-decision carry-forwards

Work by Ada (task t_d7720305) on top of M2a (39c7f77). This file documents
the M2b delta: owner-decision S-2, the Codie M2a carry-forwards (W-1/S-3/S-1),
and the vendored Perl exec agent (attribution, protocol, deviations, test
harness, ceilings).

## S-2 (OWNER DECISION, supersedes the brief's "do not change")

Copy-finish notifications now HONOR the app's "Desktop notifications"
Settings toggle — fire only when it is ON, consistent with incoming-file
notifications (useIncomingFiles.ts gate).

Mechanics (the toggle lives in the webview's localStorage; the notifications
fire from the Rust pump, which must survive a suspended webview):

- `BrowserConfig.notify: bool` (persisted browser.json, serde default false —
  a missing/old/corrupt config NEVER enables notifications).
- `ConfigStore::notify_enabled()` / `set_notify(bool)` (browser/mod.rs).
- `notify_finish` (browser/commands.rs) returns early when the flag is off.
  Done / failed-not-cancelled semantics unchanged; cancels never notify.
- New IPC command `browser_set_finish_notifications(enabled)` (registered in
  lib.rs) stores the flag.
- Frontend: ONE effect in `useTailscale.ts` (the settings owner, which calls
  useSettings) mirrors `settings.notifications` into the browser config —
  after hydration (TD-03: persisted value first, never pre-hydration
  defaults) and on every change. `browserSetFinishNotifications` in
  lib/browser/api.ts.
- useIncomingFiles and the Settings UI are untouched (per owner instruction).

Tests: Rust `notify_flag_defaults_off_and_persists` (default-off, corrupt/old
config off, set-on fires → persistence across ConfigStore instances, set-off
suppresses, no clobber of per-host accounts); tripwire extension in
`run_job_notifies_on_settle_tripwire` (gate inside notify_finish + setter
writes the flag the gate reads); vitest
`src/hooks/__tests__/useTailscale.notify-sync.test.tsx` (startup push carries
the PERSISTED value; default-false startup; re-push on every change).

## Codie M2a carry-forwards

- **W-1 (done):** Codie's throwaway probe promoted permanently into fake.rs —
  `remote_to_remote_same_host_one_transport`: `/a` → `/b` on ONE FakeServer,
  both sides ONE transport (the commands.rs `dst_same_as_src` shape), file of
  3×CHUNK+9 bytes + nested folder; asserts byte fidelity, mtime travel,
  source untouched, stage cleanup on both sides. All platforms.
- **S-3 (done):** pre-enqueue mirror of the same-host-two-account refusal in
  TwoPaneView `copyFrom` — instant status-line feedback; engine Account
  refusal + queue row stay authoritative. Vitest pins status text +
  `browser_copy` never called (verified RED-first without the mirror).
- **S-1 (done):** third-spelling ancestor-alias residual documented next to
  gap 12 in m1-handoff.md. No guard change.

## Vendored exec agent (MIT, omarchy-tailnet-files)

`src-tauri/agents/agent.pl` (plugin AGENT_PL, body byte-identical below the
attribution header — the only deviation) and `src-tauri/agents/posix_delete.sh`
(plugin POSIX_DELETE_SCRIPT, byte-identical) are vendored under MIT with full
provenance, sha256 pins and the license text in `src-tauri/agents/THIRD_PARTY.md`.
Upstream: https://github.com/dmajerus6/omarchy-tailnet-files @ c95f1b7.

## Rust integration (landed — `browser/agent.rs` + seams)

Architecture (all four pieces source-verified against russh 0.65.2 vendored
source, not memory: `Handle::channel_open_session` client/mod.rs:844,
`Channel::exec` channels/mod.rs:542, `Channel::eof` :408,
`Channel::wait -> Option<ChannelMsg>` :657, `make_writer() -> impl AsyncWrite
+ 'static` :429):

- **`agent::ExecChannel` seam** — write/read/close_stdin/wait_exit(bounded)/
  stderr_tail(capped 64 KiB). Real impl: `session::SshExecChannel` (one
  dedicated session channel per exec, never multiplexed with SFTP; `wait()`
  demux: Data→stdout, ExtendedData ext1→stderr cap, ExitStatus→slot,
  want_reply Failure→typed "exec refused"). Transport trait gained TWO
  default methods (`agent_cap()` → SftpOnly, `open_exec()` → typed
  Unsupported) — zero churn to existing impls.
- **`AgentCap` probe + `AgentCapCache`** — probe runs the fixed AGENT_CMD,
  closes stdin immediately, waits bounded (15 s): exit 2 = perl honored the
  framing contract → `PerlExec`; any other exit (e.g. 127) → `ExecNoPerl`;
  refused/timeout → `SftpOnly`. Probed ONCE per host+account in `open_side`
  (POSIX flavors only; Windows flavors never probe — the M3 C# agent is a
  separate lane), cached with NO TTL, invalidated on `browser_set_user` and
  `browser_forget_host_key` alongside the probe cache.
- **`AgentSink` (CopySink over the `recv` verb)** — open frames
  `len\n + AGENT_PL + "recv\0" + dst\0 + stage\0 + replace|keep\0`
  (plugin `agent_input` exact); stream = `D rel\0 mode\0 mtime\0`,
  `F rel\0 mode\0 mtime\0 size\0` + size raw bytes, `L rel\0 target\0`,
  then `Z`; replies `C rel\0` / `E rel\0` / final `Z` map to
  conflicts/errors exactly like RemoteSink's. Wire numbers are DECIMAL
  ASCII (perl stringification); file modes masked 0o777 in the F header
  (W1: setuid/setgid/sticky never travel — the agent masks again).
  `CopySink::file_begin` widened to carry `FileMeta{size,mode,mtime}`
  because the F record frames the size up front (SFTP/local sinks ignore
  it — attrs still applied at file_end through their own handles).
- **Cleanup ordering (plugin bin/tailfiles:2318-2349 mirror)** — every
  non-placed exit path: close the agent's stdin FIRST (SSH channel EOF),
  wait bounded (20 s); the agent self-cleans its stage through its HELD
  handles on EOF/SIGPIPE/HUP/TERM (`drop_stage` = empty via handles +
  dev/ino-matched rmdir). Our side NEVER removes anything remote by name.
- **Tier selection (`transfer::run_copy` step 2)** — `!windows &&
  agent_cap()==PerlExec` → AgentSink; EVERY other host keeps the M2a SFTP
  tiers UNCHANGED (`RemoteSink`): documented fallback, no silent downgrade.
- **`agent_delete` (physical delete re-check)** — `browser_delete` keeps
  all M1 guards and parent resolution, then on perl-exec hosts runs the
  vendored script via the fixed `xargs -0 sh -c '…' sh` command with
  NUL-separated absolute paths on stdin (never shell words). The script
  re-resolves each parent `cd -P` AT DELETE TIME and re-runs the home
  guard against the PHYSICAL spelling. Runtime exec refusal → documented
  fallback to the unchanged SFTP walk; a refusal ("Refusing to delete",
  unresolvable home) is Denied and NEVER falls back. `xargs` flattens
  non-zero script codes (BSD→1, GNU→123), so — exactly like the plugin
  (`fail(error_text(err), classify(err))`) — the meaning is read from
  stderr; raw script codes (0/1/3/4/5) are pinned by direct tests.

### Deviations from the plugin's AGENT_PL (complete list)

1. The attribution header in `agents/agent.pl` (Perl comments, semantically
   inert; the length prefix measures the file as sent). The body below it
   is byte-identical — sha256-pinned in THIRD_PARTY.md AND re-pinned at
   every test run (`vendored_agent_body_matches_pin`,
   `vendored_posix_delete_matches_pin`; note the pin covers the plugin raw
   string's leading newline, so the test hashes `"\n" + body`).
   No functional deviation. `send`/`rename` verbs are vendored unmodified
   but never invoked (source walks stay on SFTP; rename stays on its M1
   tier).

### Loopback harness (design constraint from the brief)

The REAL vendored perl (dev-Mac **perl v5.34.1**, darwin-thread-multi-2level
— recorded per the brief) runs as a LOCAL `sh -c` child with the exact
NUL-framed stdin/stdout protocol (`agent::loopback::LocalExecChannel`,
unix-only, test-only): the protocol is the contract, not the transport.
Coverage (27 module tests + 5 engine tests): TOCTOU late-collision refusal
(name planted AFTER streaming began → C record, planted bytes untouched);
atomic Replace (inode swaps, mtime lands); mid-stream EOF cleanup (no Z →
agent bails "short input" → stage gone); planted lookalike stage untouched
byte-identical; stage-name collision at open refuses; delete identity-change
refusal (home moved UNDER the confirmed target → refused, both intact),
home-itself refusal, raw script exit codes 0/3/4/5, xargs-wrapped refusal
shape; probe fallbacks (PATH-stripped perl-missing → ExecNoPerl, exec
refused → SftpOnly, real perl → PerlExec); engine-level `run_copy` through
the PerlExec tier (3×CHUNK+9 file + folder land via real perl, mtime
travels, agent cleans its own stage), cancel-mid-copy (the brief's hard
requirement: agent removed its stage, no partial file), ExecNoPerl and
windows-dst tiers stay on SFTP with ZERO exec opens; wiring tripwire.
RED-first spot-checked: disabling the run_copy tier gate fails the two
agent-tier engine tests; restoring it passes all five.

### Ceilings / not verifiable locally

- Real-fleet exec was NOT exercised (no fleet mutations allowed): whether
  Tailscale SSH's tailnet policy grants `exec` on each production peer, and
  real-network perl latency/timeout behavior, can only be proven live. The
  loopback harness proves protocol, placement, cleanup and re-check
  semantics; the SSH channel adapter is source-verified against vendored
  russh. A peer that refuses exec at runtime degrades to the unchanged
  SFTP tiers (probe → SftpOnly; delete → documented fallback).
- The probe has NO TTL by design ("probe once per host/account"): a server
  gaining/losing perl mid-session keeps the cached verdict until account
  change, forget-host, or app restart. Runtime exec refusal still falls
  back safely (delete) or errors the copy (placement — visible, never
  silent).
- Agent placement errors mid-place surface as the copy failing with the
  agent's stderr tail (typed via the plugin's classify rules); partial
  placement of OTHER top-level items follows the plugin's own place_in
  semantics (each item placed or C/E-recorded independently).
- Windows desktop (MSVC) and WebKit/WKWebView runtime behavior remain
  CI/manual-gated as in M1/M2a; windows-gnu clippy+check pass with the
  agent lane compiled in (loopback harness cfg-unix-gated out).
- accuro-server and all patient-data machines: never touched (loopback +
  fake-server only, per the brief).
