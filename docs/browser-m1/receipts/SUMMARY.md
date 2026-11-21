# M1 gate receipts

## Fix round (continuation 4) — CURRENT

All gates run fresh against HEAD `6562689` (branch `feature/browser-m1`,
worktree `/Users/cortexuvula/Development/taildrop-gui-browser-m1`) plus the
docs-only commit that carries these receipts, on 2026-11-20 (UTC), macOS
26.6.2, single cargo target dir `src-tauri/target`, `CARGO_INCREMENTAL=0`
for verification runs. No fleet or network operations: the Rust fake-server
suite and the Playwright mock run entirely in-process / on loopback.

| Gate | Command (cwd) | Exit | Result | Raw log |
|---|---|---|---|---|
| Rust tests | `CARGO_INCREMENTAL=0 cargo test` (src-tauri) | 0 | **220 passed, 0 failed** | cargo-test.txt |
| Rust lint | `CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings` (src-tauri) | 0 | 0 warnings | cargo-clippy.txt |
| Rust format | `cargo fmt --check` (src-tauri) | 0 | clean | cargo-fmt.txt |
| TS types | `npx tsc --noEmit` (root) | 0 | clean | tsc.txt |
| Unit tests | `npx vitest run` (root) | 0 | **17 files / 180 tests passed** | vitest.txt |
| E2E | `npx playwright test` (root, chromium) | 0 | **54 passed** | playwright.txt |
| Prod build | `npm run build` (root) | 0 | dist built | npm-build.txt |

Rust test-count breakdown (220): 181 skeleton unit tests, 25 fake-SFTP
loopback tests (transport, extension negotiation, trust incl. the B4
full-line cross-check cases, placement tiers, cleanup ordering, B3 lossy-name
skips, B6 junction probe, W3 end-to-end pulls), 10 local-sink filesystem
attack tests, 4 fix-round additions elsewhere (W2 trust-tmp sweep, B5
preflight case inside preflight_refusals, B1 tripwire + ampersand-name).

Known flake (W6, PRE-EXISTING ON MASTER, out of M1 scope per brief):
`tailscale::tests::acknowledge_returns_recorded_path_without_draining`
raced once across this round's full-suite runs and passed on immediate
rerun; Codie measured the same flake on master 89aeda3.

## Not locally provable (fix-round additions)

| Gate | Status |
|---|---|
| Windows compile (`cargo check --target x86_64-pc-windows-msvc`) | NOT provable on this Mac (no MSVC SDK; ring's C build fails first — same as Codie's attempt). B2 fixed statically; CI windows-latest is the gate. |
| Windows runtime (B1 ShellExecuteW open, B6 junction probe semantics) | Not executable here; on-device Windows smoke required before release. |
| `cargo audit` (B7) | rsa/RUSTSEC-2023-0071 decision was PENDING OWNER at the time of this docs commit — russh features deliberately unchanged here. RESOLVED in the follow-up `fix(browser): drop russh rsa feature` commit (continuation 5, decisions.md #6); audit output captured in cargo-audit.txt. |

## Initial M1 completion (continuation 3, superseded by the table above)

Ran at HEAD `2c33425` on 2026-11-20: cargo test 209/209, clippy clean,
fmt clean, tsc clean, vitest 178/178 (17 files), playwright 53/53,
npm build OK — all exits 0. Raw logs from that run were overwritten by
the fix-round reruns; the counts are preserved here and in the
continuation-3 commit message (6c0e6d3).
