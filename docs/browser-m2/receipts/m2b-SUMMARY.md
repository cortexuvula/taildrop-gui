# M2b gate receipts — SUMMARY

All gates RE-RUN and refreshed at HEAD `6dee72b` (review round-1 fixes:
`e93578f` ra01 test receipt-store lock + `6dee72b` F-1 loopback stderr-drain
join — both test-only; original M2b capture was at `3b8787b`, code-final
`4530b85`), macOS (Apple Silicon), perl v5.34.1 (darwin-thread-multi-2level)
in PATH.
M2a receipts in this directory are UNTOUCHED (kept intact per the brief);
M2b receipts are the `m2b-*.txt` siblings. Raw logs include the exact
command, commit SHA and UTC date in their headers.

| Gate | Receipt | Result |
|---|---|---|
| `cargo test --lib` | m2b-cargo-test-lib.txt | **283 passed**, 0 failed (M2a baseline 249 → +2 carry-forwards at checkpoint → +32 agent lane: 27 module/loopback + 4 engine + 1 tripwire) |
| `cargo test --lib` ×50 consecutive (F-1 stability) | m2b-cargo-stability-loop.txt | **50/50 clean** (283 passed each run, 0 failures, 100s wall; pre-fix RED: F-1 flake 1-in-2 runs at `e93578f`, ra01 flake 1-in-3 at `20a384b`) |
| `cargo fmt -- --check` | m2b-cargo-fmt-check.txt | exit 0, no diff |
| `cargo clippy --all-targets -- -D warnings` (host) | m2b-cargo-clippy-host.txt | exit 0 |
| `cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- -D warnings` | m2b-cargo-clippy-windows-gnu.txt | exit 0 |
| `cargo check --target x86_64-pc-windows-gnu` | m2b-cargo-check-windows-gnu.txt | exit 0 |
| `npx tsc --noEmit` | m2b-tsc-noemit.txt | exit 0 |
| `npx vitest run` | m2b-vitest.txt | **193 passed** (19 files; unchanged — M2b adds no frontend surface, no capability badge per design) |
| `npx playwright test` | m2b-playwright.txt | **62 passed** (chromium, 11.5s) |

Review round-1 fixes (both test-only, no production code, no test weakened):

- `e93578f` — `ra01_isolated_fixtures_invisible_to_recovery_scan` part (b)
  recorded a receipt into the shared store WITHOUT `TEST_STORE_LOCK`, so it
  could inject a second `photo.jpg` receipt inside the TD05-A receipt-count
  assertion window (`acknowledge_returns_recorded_path_without_draining`,
  'left: 2, right: 1'). Pre-existing M1-era test-isolation bug, surfaced by
  the F-1 repro loop (caught 1-in-3 full-suite runs).
- `6dee72b` — **F-1**: `LocalExecChannel::wait_exit` now joins the spawned
  stderr-drain `JoinHandle` on every path (bounded by pipe EOF; 2s safety
  bound + abort), removing the race between the drain and post-wait
  `stderr_tail()` reads (reproduced 1-in-2 runs pre-fix:
  `loopback_delete_script_raw_exit_codes`, agent.rs:1598, rc fine but
  stderr empty). Post-fix: **50/50** consecutive clean full-suite runs
  (m2b-cargo-stability-loop.txt carries both RED logs and the 50-run log).
  Production `SshExecChannel` is order-pumped and was never affected.

RED-first spot check (recorded in m2b-handoff.md): disabling the run_copy
tier gate (`if false && …PerlExec`) fails
`engine_agent_tier_lands_bytes_via_loopback_perl` and
`engine_agent_tier_cancel_midcopy_leaves_nothing`; restoring it passes all
five engine tests. The loopback module tests exercise code that did not
exist before M2b (cannot compile without it).

Loopback harness = the REAL vendored Perl agent as a local `sh -c` child
with the exact NUL-framed protocol; no SSH server, no fleet host, no
accuro-server touch at any point.

Not re-runnable locally (documented in m2b-handoff.md "Ceilings / not
verifiable locally"): real-fleet exec permission per peer, MSVC runtime,
WebKit/WKWebView runtime.
