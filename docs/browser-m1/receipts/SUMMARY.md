# M1 gate receipts

All gates run fresh against HEAD `2c33425` (branch `feature/browser-m1`,
worktree `/Users/cortexuvula/Development/taildrop-gui-browser-m1`) on
2026-11-20 (UTC), macOS 26.6.2, single cargo target dir `src-tauri/target`,
`CARGO_INCREMENTAL=0` for verification runs. No fleet or network operations:
the Rust fake-server suite and the Playwright mock run entirely in-process /
on loopback.

| Gate | Command (cwd) | Exit | Result | Raw log |
|---|---|---|---|---|
| Rust tests | `CARGO_INCREMENTAL=0 cargo test` (src-tauri) | 0 | **209 passed, 0 failed** | cargo-test.txt |
| Rust lint | `CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings` (src-tauri) | 0 | 0 warnings | cargo-clippy.txt |
| Rust format | `cargo fmt --check` (src-tauri) | 0 | clean | cargo-fmt.txt |
| TS types | `npx tsc --noEmit` (root) | 0 | clean | tsc.txt |
| Unit tests | `npx vitest run` (root) | 0 | **17 files / 178 tests passed** (117 pre-existing + 61 new) | vitest.txt |
| E2E | `npx playwright test` (root, chromium) | 0 | **53 passed** (43 pre-existing + 10 new browser) | playwright.txt |
| Prod build | `npm run build` (root) | 0 | dist built | npm-build.txt |

Test-count breakdown of the 209 Rust tests: 181 skeleton unit tests
(guards/winnames/trust/probe/plan/place/progress/session mapping),
18 fake-SFTP loopback tests (browser/fake.rs — transport, extension
negotiation, trust refusal, placement tiers, cleanup ordering),
10 local-sink filesystem attack tests (planted stage, swap-before-unlink,
EXDEV/non-atomic injection, case-clash, local→local fidelity).

Note on `cargo test` output: three `test result` lines appear (lib suite
plus two empty suites for bin targets); the lib line carries the 209.
