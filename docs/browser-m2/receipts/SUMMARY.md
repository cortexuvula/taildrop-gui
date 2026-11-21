# M2a gate receipts — taildrop-gui (task t_6197d588, agent ada)

All gates re-run and bound to **HEAD de472a4** on local master (nothing pushed).
Raw logs in this directory; each log's first line names the command, timestamp, and HEAD; the last line is `EXIT=<code>`.

| Gate | Receipt | Result |
|---|---|---|
| `cargo test --lib` (host) | cargo-test-lib.log | **249 passed**, 0 failed (baseline 229 → +20) · EXIT=0 |
| `cargo fmt -- --check` | cargo-fmt-check.log | clean · EXIT=0 |
| `cargo clippy --all-targets -- -D warnings` (host) | cargo-clippy-host.log | clean · EXIT=0 |
| `cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- -D warnings` | cargo-clippy-windows-gnu.log | clean · EXIT=0 (cached at de472a4; full compile run at 28dc9de — identical Rust sources) |
| `cargo check --target x86_64-pc-windows-gnu` | cargo-check-windows-gnu.log | clean · EXIT=0 (same caching note) |
| `npx tsc --noEmit` | tsc-noemit.log | clean · EXIT=0 |
| `npx vitest run` | vitest.log | **190 passed** (baseline 180 → +10) · EXIT=0 |
| `npx playwright test` | playwright.log | **62 passed** (baseline 55 → +7) · EXIT=0 |

Environment: macOS (Apple Silicon host), CARGO_INCREMENTAL=0, shared `src-tauri/target` (pre-existing from M1; kept, not created by this lane — nothing to clean up).

## What the new coverage pins

- **Rust (249)**: 7 two-fake-server loopback r2r tests (file copy w/ mtime+progress+bytes, folder fidelity incl. symlink/mode on unix, conflict→Exists→Replace, dst-side cancel, dst auth failure leaves nothing, dst-open close tripwire, dst write-failure stage cleanup); finish-notification shape + wiring tripwire; known-hosts wire shape + `all_pins`/`fingerprint` (externally verified ssh-keygen vector); W5 dev/ino guard incl. the REAL macOS firmlink alias case; config restart-memory round-trips.
- **vitest (190)**: `TwoPaneView.m2a.test.tsx` — r2r enqueues `browser_copy` with both remote targets; `=` mirrors active→other; g/G/h/l on real listings; `?`/F1/Esc help overlay; **help-table truthfulness pin** (reads the router `switch` cases from the component source via `?raw` and asserts both directions); known-hosts rows + Forget wire + empty state. `guards.test.ts` — `sanitizeKnownHosts` boundary; the old `remoteToRemoteProblem` describe was replaced (function removed per brief — r2r is now allowed; refusal coverage lives in the r2r-enqueue vitest + e2e).
- **Playwright (62)**: B5 refusal test REPLACED with the r2r success path (studio-mac→linux-box: queue done, wire args, bytes in dst mock FS, pane reload shows the file); same-folder refusal kept; `=` mirror; g/G/h/l; help overlay; known-hosts Forget + pin mutation; empty-pin hint; last-remote + saved-account restore (`?lastHost=`/`?savedUser=` config seeds).
- Mock bug fixed on the way: settled copy jobs never `clearInterval`'d, so a no-collision copy flipped done→failed one tick later (collided with the file it had just landed). Latent since M1; only the r2r no-collision path exposed it.

## Not verifiable locally

- MSVC compile/runtime — CI `windows-latest` is the authoritative gate (windows-gnu cross checks above are the local proxy).
- WebKit smoke — manual per brief.
- Real-fleet remote→remote copy (two live SFTP endpoints) — loopback fakes only.
