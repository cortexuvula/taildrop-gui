# M2b gate receipts — SUMMARY

All gates run at HEAD `3b8787b` (code-final `4530b85`; docs-only `3b8787b`),
macOS (Apple Silicon), perl v5.34.1 (darwin-thread-multi-2level) in PATH.
M2a receipts in this directory are UNTOUCHED (kept intact per the brief);
M2b receipts are the `m2b-*.txt` siblings. Raw logs include the exact
command, commit SHA and UTC date in their headers.

| Gate | Receipt | Result |
|---|---|---|
| `cargo test --lib` | m2b-cargo-test-lib.txt | **283 passed**, 0 failed (M2a baseline 249 → +2 carry-forwards at checkpoint → +32 agent lane: 27 module/loopback + 4 engine + 1 tripwire) |
| `cargo fmt -- --check` | m2b-cargo-fmt-check.txt | exit 0, no diff |
| `cargo clippy --all-targets -- -D warnings` (host) | m2b-cargo-clippy-host.txt | exit 0 |
| `cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- -D warnings` | m2b-cargo-clippy-windows-gnu.txt | exit 0 |
| `cargo check --target x86_64-pc-windows-gnu` | m2b-cargo-check-windows-gnu.txt | exit 0 |
| `npx tsc --noEmit` | m2b-tsc-noemit.txt | exit 0 |
| `npx vitest run` | m2b-vitest.txt | **193 passed** (19 files; unchanged — M2b adds no frontend surface, no capability badge per design) |
| `npx playwright test` | m2b-playwright.txt | **62 passed** (chromium, 9.0s) |

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
