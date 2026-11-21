# M2c gate receipts — SUMMARY

Codie M2b review suggestions S-1/S-2/S-3 + nit N-1 (owner decision: implement
all). Gates re-run at code-final HEAD `7e20d70` (S-3 `0af8dc2` → S-2 `feb9dbf`
→ S-1 `af56a7b` → N-1 `7e20d70`), macOS (Apple Silicon), perl v5.34.1 in PATH.
M2a/M2b receipts in this directory are UNTOUCHED. Raw logs carry the exact
command, commit SHA and UTC date in their headers.

| Gate | Receipt | Result |
|---|---|---|
| `cargo test --lib` | m2c-cargo-test-lib.txt | **291 passed**, 0 failed (M2b host baseline 284 → +2 S-3, +2 S-2, +3 S-1) |
| `cargo test --lib` ×20 consecutive (F-1 regression watch) | m2c-cargo-stability-loop.txt | **20/20 clean** (291 passed each, 0 failed) |
| `cargo fmt -- --check` | m2c-cargo-fmt-check.txt | exit 0, no diff |
| `cargo clippy --all-targets -- -D warnings` (host) | m2c-cargo-clippy-host.txt | exit 0 |
| `cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- -D warnings` | m2c-cargo-clippy-windows-gnu.txt | exit 0 |
| `cargo check --target x86_64-pc-windows-gnu` | m2c-cargo-check-windows-gnu.txt | exit 0 |
| `npx tsc --noEmit` | m2c-tsc-noemit.txt | exit 0 |
| `npx vitest run` | m2c-vitest.txt | **193 passed** (19 files — M2c adds no frontend surface; `agentRc` documented on the wire type only) |
| `npx playwright test` | m2c-playwright.txt | **62 passed** (chromium, 10.4s) |

## Items

- **S-1 (`af56a7b`) — capability-cache self-heal on rc-9 "perl too old".**
  `BrowserError.agent_rc` (wire `agentRc`, optional) carries the remote
  helper's exit code as a TYPED marker (never text-sniffed);
  `agent::is_perl_too_old` matches exactly rc-9 + Unsupported;
  `AgentCapCache::downgrade_on_perl_too_old` drops THAT host+account's cached
  cap PerlExec → ExecNoPerl. Downgrade, not invalidation (a re-probe would
  re-classify PerlExec — the bootstrap exit 2 needs nothing modern — and
  re-arm the failing tier); monotonic (never upgrades, never touches
  SftpOnly/ExecNoPerl/absent); recovery stays the existing invalidation
  (forget-host / set-user / restart). Wired at BOTH agent lanes:
  `execute_copy` (after `run_with_sides`, destination account resolved from
  the owning transport incl. same-host borrow) and `browser_delete`; the
  tripwire pins the count at 2. Tests: `perl_too_old_marker_and_cache_self_heal`
  (marker + scoped/monotonic/idempotent downgrade), 
  `engine_rc9_selfheal_second_copy_takes_sftp_tier` (brief's flow: rc-9 fail →
  cache ExecNoPerl → second copy lands via RemoteSink with ZERO agent-tier
  exec opens, no stage debris), `downgraded_cap_short_circuits_get_or_probe`
  (no re-probe). RED: m2c-red-s1-demo.txt — pre-fix the cap stayed PerlExec
  and the second copy re-opened + re-failed the agent tier (demo removed with
  the fix).
- **S-2 (`feb9dbf`) — probe single-flight.** `AgentCapCache` entries are
  `Arc<tokio::sync::OnceCell<AgentCap>>`; new `get_or_probe(host, user, t)`
  claims the per-key cell under a short-lived lock and runs
  `probe_agent_cap` inside `get_or_init` — concurrent first opens of the same
  host+user coalesce onto exactly ONE bounded probe, waiters receive the
  winner's result; a cancelled/panicked probe leaves the cell unfilled for a
  bounded retry (never poisoned). `get()` reads None while in flight (the
  delete gate keeps the SFTP path — unchanged semantics). `open_side`
  rewired; `put` is now a `#[cfg(test)]` seam (production fills the cache
  only via get_or_probe + the S-1 downgrade). Tripwire re-pinned to the new
  wiring (`.get_or_probe(target, transport.user(), &transport)` in
  commands.rs + `get_or_init(|| probe_agent_cap(t))` in agent.rs). Tests:
  `cap_cache_single_flights_concurrent_probes` (gated/blocking probe double —
  NO sleeps; asserts 1 exec open, both callers PerlExec, in-flight get()
  None), `loopback_get_or_probe_single_flight_one_exec` (real perl, join!
  concurrency + cache-hit no-reprobe). RED: m2c-red-s2-demo.txt — the old
  get/miss/probe/put pattern double-probed (exec_opens == 2; demo removed
  with the fix).
- **S-3 (`0af8dc2`) — delete xargs-missing → SFTP fallback.** `agent_delete`'s
  rc mapping extracted into the pure `classify_delete_failure` +
  `delete_helper_missing`: rc 127 AND a command-not-found spelling for the
  helper command (`xargs`, derived from `posix_delete_cmd`) → Unsupported →
  browser_delete's documented SFTP-walk fallback. Observed shell spellings
  (all pinned): bash-as-sh macOS "xargs: No such file or directory", dash
  "xargs: not found", zsh "command not found: xargs" — the brief's
  "not found" alone would have missed macOS remotes, so the ENOENT spelling
  is included. Refusals ("Refusing to delete" / unresolvable home) → Denied
  at ANY rc, checked FIRST (Codie's no-double-delete pin; rc 5 never falls
  back). rc 127 with unrelated stderr stays a typed error. When the shape
  matches, the helper never ran → the fallback cannot double-delete.
  Pre-exec relative/NUL refusal unchanged. Tests:
  `delete_failure_classification_shapes` (full matrix incl. refusal-wins-over-
  127) + `loopback_delete_xargs_missing_is_unsupported_fallback` (real
  /bin/sh, empty PATH). RED: m2c-red-s3-loopback.txt (pre-fix: Notfound).
- **N-1 (`7e20d70`) — docs-only.** m2b-SUMMARY.md vitest row reworded:
  counts moved 190/18 → 193/19; no NEW production view/badge — the +3 tests
  pin the owner-decided notify-sync effect + the instant-refusal mirror.

## Constraints held

- Vendored agent bytes + `AGENT_CMD` UNCHANGED (`vendored_*` sha256 pins +
  `agent_cmd_is_plugin_exact` pass at every run).
- No test weakened/deleted: the two removed `red_demo_*` tests were
  uncommitted scratch demos whose runs are preserved as RED receipts; the
  M2b tripwire assertions were RE-PINNED to the intentionally-changed S-2
  wiring at equal strength (get/put → get_or_probe + get_or_init), and the
  invalidate-count (2) + agent_delete pins are unchanged. 0 assertion
  removals elsewhere (diff review).
- No silent downgrade: SFTP tiers untouched; `engine_sftp_tier_unchanged_without_perl`
  and `engine_windows_dst_never_uses_agent_tier` still assert ZERO exec
  opens; Windows local engine still fails closed (`local_unsupported()`
  intact); Windows flavors still never probe (`is_posix` gate untouched).
- Loopback/fakes only — no fleet host, no accuro-server, no real SSH anywhere
  in the new tests; new loopback children reuse the existing
  `LocalExecChannel`/`loopback_perl()` resolver (CI Homebrew-perl safe).
- Conventional commits, explicit paths only, no `git add -A`; nothing
  pushed/tagged/merged/version-bumped. codie-*.md and M2A/M2B-STATUS.md left
  untracked.

## Not verifiable locally (unchanged ceilings)

- Real-fleet exec under Tailscale SSH policy (a live too-old-perl host for
  S-1 end-to-end; a live xargs-less host for S-3 — loopback/doubles only).
- `SshExecChannel` against a live SSH server; MSVC + WebKit runtimes
  (windows-gnu clippy/check only, as in M2b).
- The macOS bash-as-sh rc-127 spelling was verified on THIS machine's
  /bin/sh; other remote shells' spellings beyond the three pinned are
  possible (matcher is rc-gated + helper-gated, so unknown spellings stay
  typed errors — fail-safe direction).
