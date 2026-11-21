# M1 gate receipts

## C1 fix + Codie S1–S4 (continuation 8) — CURRENT

Fixes the ONE blocker in Codie's review of continuation 7
(codie-winfix-review.md, REQUEST CHANGES): Windows CI run 44865268846
failed `browser::fake::tests::listing_skips_control_char_name_exactly_once`
— the fixture setup writes a real `"bad\nname"` file and Win32 forbids
0x01–0x1F in filenames (Os code 123 InvalidFilename). Fixed per Codie's
recommended option (a):

- C1: real-file test gated `#[cfg(unix)]` (unchanged otherwise); new
  `FakeConfig::control_char_names` knob injects the name synthetically into
  READDIR (mirrors `non_utf8_names`); new ALL-PLATFORM twin
  `listing_skips_synthetic_control_char_name_exactly_once` keeps the
  skip-and-count rule covered on Windows (skipped exactly once, never
  returned, never guessed).
- S1: `#[cfg_attr(not(unix), allow(dead_code))]` + one-line reasons on the
  Windows-dead items Codie listed (listing.rs `local_home`; place.rs
  `ItemKind`/`RenameOutcome`/`PlaceAction`/`place_action`; tailscale.rs
  `url_encode`/`parse_content_length`/`validate_received_length` +
  `TempFileForTest` helpers — attribute/lint touches only in that pre-M1
  file), plus the behavior-identical `map_err`→`inspect_err` fix at
  tailscale.rs:2508. Goal MET: windows-gnu clippy `-D warnings` exits 0
  (was 15 errors in Codie's run).
- S2/S4: m1-handoff.md — Windows agent auth = OpenSSH service pipe only
  (Pageant not supported in M1, honest typed error instead); full list of
  the 17 Unix-only fake.rs tests with reasons and what still runs on
  Windows.
- S3: session.rs comment — russh's ERROR_PIPE_BUSY retry is bounded by the
  25 s AUTH_TIMEOUT (busy pipe → typed timeout error, never a hang).

| Gate | Command (cwd src-tauri) | Exit | Result | Raw log |
|---|---|---|---|---|
| Rust tests (host) | `CARGO_INCREMENTAL=0 cargo test --lib` | 0 | **229 passed, 0 failed** (228 prior + 1 new knob twin; 224 original all still green) | cargo-test.txt |
| Rust format | `cargo fmt -- --check` | 0 | clean | cargo-fmt.txt |
| Rust lint (host) | `CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings` | 0 | 0 warnings | cargo-clippy.txt |
| Windows lint (local, GNU) | `CARGO_INCREMENTAL=0 cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- -D warnings` | 0 | **PASS — S1 goal met** (Codie's 15 dead-code/lint errors gone) | win-gnu-crosscheck.txt |
| Windows type-check (local, GNU) | `CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu --lib --tests` | 0 | PASS — lib + tests incl. new knob code | win-gnu-crosscheck.txt |
| Windows compile+test (CI, msvc) | GitHub Actions windows-latest "Rust tests" | pending | **Remains the authoritative runtime gate** — no Windows host available locally; the C1 failure class (runtime fixture behavior, not typing) is only provable there. Frikkie pushes; do NOT tag v0.16.x until it is green (Codie's instruction). | — |

MSVC cross-check from macOS remains impossible (ring C build needs the MSVC
SDK — win-crosscheck-attempt.txt, continuation 7). TS/frontend untouched in
this round → tsc/vitest/playwright skipped per brief.

## Windows compile fix (continuation 7)

CI windows-latest `cargo test` failed to COMPILE at master 07969c1 (run
44859283804, job 136709470314): 27 errors — Unix-only APIs without platform
gates in `fake.rs` (test fixture), `session.rs` (ssh-agent `connect_env`,
PRODUCTION), and `trust.rs` (test helper). Fixed forward on master with ZERO
Unix behavior change and no test deleted or weakened. Full ported-vs-gated
inventory and Windows semantic differences: m1-handoff.md § "Windows compile
fix (continuation 7)". Highlights:

- Production: Windows ssh-agent auth now uses russh's
  `AgentClient::connect_named_pipe(\\.\pipe\openssh-ssh-agent)` (the
  Windows-OpenSSH agent service pipe) — real agent auth on Windows, not a
  skip. Absent service → honest fall-through to the EXISTING typed
  no-credential error (text unchanged; no test asserted on it).
- Fixture: positional IO, mtime (FileTimes), mode synthesis with S_IF* type
  bits, symlink op, and the READLINK "exists, not a link" detection ported;
  permission application stays Unix-only (documented difference).
- Gated: only tests that drive `run_copy` through `Side::Local` — the local
  engine is Unix-only BY M1 DESIGN (local_side.rs is whole-module
  `#[cfg(unix)]`, callers fail closed), so those tests cannot exercise real
  behavior on Windows. Each gate carries a one-line reason.
- Added: 4 platform-neutral tests in transfer/remote_side.rs (plan-level
  B6-R probe semantics, POSIX never-probes, RemoteSink exclusive
  staging/placement/cleanup, build_plan→stream_plan roundtrip) so Windows CI
  KEEPS coverage of copy engine, probe semantics, and staging.

| Gate | Command (cwd) | Exit | Result | Raw log |
|---|---|---|---|---|
| Rust tests (host) | `CARGO_INCREMENTAL=0 cargo test` (src-tauri) | 0 | **228 passed, 0 failed** (224 existing — all green, unchanged — plus 4 new) | cargo-test.txt |
| Rust lint (host) | `CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings` (src-tauri) | 0 | 0 warnings | cargo-clippy.txt |
| Rust format | `cargo fmt -- --check` (src-tauri) | 0 | clean | cargo-fmt.txt |
| Windows type-check (local, GNU) | `CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu --lib --tests` (src-tauri) | 0 | **PASS** — lib AND lib-test type-check for Windows incl. every new cfg(windows)/cfg(not(unix)) branch (rustup windows-gnu target + brew mingw-w64 14.0.0; ring builds via mingw) | win-gnu-crosscheck.txt |
| Windows type-check (local, MSVC) | `CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-msvc --lib --tests` (src-tauri) | 101 | **NOT provable on this Mac** — ring's C build fails first (no MSVC SDK: `assert.h` not found); our crate never reached. Same wall as prior rounds; recorded honestly, NOT a pass. | win-crosscheck-attempt.txt |
| Windows compile+test (CI, msvc) | GitHub Actions windows-latest "Rust tests" | pending | Authoritative msvc gate — runs after Frikkie pushes (no push from this round per brief) | — |

Known W6 flake note: one intermediate host run hit
`tailscale::tests::acknowledge_returns_recorded_path_without_draining` once
(pre-existing master flake, tailscale.rs untouched here); passed on immediate
rerun and in the receipted full run (details in cargo-test.txt).

TS/frontend files were NOT touched → tsc/vitest/playwright/npm-build skipped
per brief (gate 3 condition).

Remaining Windows-build warnings (13, GNU check) are pre-existing dead_code
in UNTOUCHED files (listing.rs `local_home`; place.rs `place_action` family;
tailscale.rs `url_encode`/`parse_content_length`/`validate_received_length` +
test-only tempfile helpers) — items consumed only by cfg(unix) code paths.
Warnings, not errors; the CI windows job runs `cargo test` without
`-D warnings` (clippy is ubuntu-only per ci.yml).

## Fix round (continuation 4)

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
| `cargo audit` (B7) | RESOLVED in continuation 5 (decisions.md #6: drop the russh `rsa` feature). `cargo audit --no-fetch --stale` after the drop: RUSTSEC-2023-0071 GONE, no `rsa` crate in the tree (cargo-audit.txt). Remaining advisories (quick-xml, rustls, zip, rand, glib, unic-*, zbus-*) are the pre-existing master set — out of M1 scope, unchanged. |

## B7 / continuation 5 (current HEAD)

The `fix(browser): drop russh rsa feature` commit changes only Cargo.toml
(russh features `["ring"]`), Cargo.lock (pure removals: rsa, pkcs1,
crypto-primes — no version bumps; pins russh =0.65.2 / russh-sftp =3.0.1
intact) and session.rs (typed Unsupported error for RSA key files; agent
RSA identities still attempted). All seven gates were re-run against that
tree and the receipts in this directory refreshed:

| Gate | Exit | Result |
|---|---|---|
| `CARGO_INCREMENTAL=0 cargo test` | 0 | **220 passed, 0 failed** (2nd attempt; 1st hit the known W6 master flake — noted in cargo-test.txt) |
| `cargo clippy --all-targets -- -D warnings` | 0 | clean |
| `cargo fmt --check` | 0 | clean |
| `npx tsc --noEmit` | 0 | clean |
| `npx vitest run` | 0 | 17 files / 180 tests |
| `npx playwright test` | 0 | 54 passed |
| `npm run build` | 0 | dist built |
| `cargo audit --no-fetch --stale` | 1 (advisories present) | **RUSTSEC-2023-0071 GONE**; only the pre-existing master advisory set remains |

## B6-R (continuation 6)

The `fix(browser): B6-R` commit corrects the Windows junction probe to the
REAL Win32-OpenSSH READLINK semantics (R3 blocker: the first probe treated
only NO_SUCH_FILE as "plain dir", but Win32-OpenSSH answers generic
SSH_FX_FAILURE for plain dirs and SSH_FX_BAD_MESSAGE for junctions — every
ordinary Windows folder was refused). Raw SSH_FXP_STATUS now rides through
`BrowserError::sftp_status`; the fake server answers per real-server flavor
(Windows: plain→FAILURE, junction→BAD_MESSAGE; POSIX: plain→BAD_MESSAGE) so
tests encode measured semantics, not desired ones. Changes: mod.rs,
session.rs, transfer/remote_side.rs, fake.rs, types/browser.ts (additive
optional field). New tests: plain-dir descent (the R3 regression), nested
ancestor-junction no-loop, vanished-dir race tolerance, POSIX never-probes
(counter), N1 RSA-key fail-safe pair.

All seven gates re-run against this tree (receipts refreshed in this
directory):

| Gate | Exit | Result |
|---|---|---|
| `CARGO_INCREMENTAL=0 cargo test` | 0 | **224 passed, 0 failed** (first attempt, no W6 flake) |
| `cargo clippy --all-targets -- -D warnings` | 0 | clean |
| `cargo fmt --check` | 0 | clean |
| `npx tsc --noEmit` | 0 | clean |
| `npx vitest run` | 0 | 180/180 |
| `npx playwright test` | 0 | 54 passed |
| `npm run build` | 0 | dist built |

cargo audit is unchanged from the B7 receipt (no dependency changes in
B6-R). Windows runtime confirmation (room-6 folder pull with a junction)
remains the pre-release smoke — handoff gap 13.

## Initial M1 completion (continuation 3, superseded by the table above)

Ran at HEAD `2c33425` on 2026-11-20: cargo test 209/209, clippy clean,
fmt clean, tsc clean, vitest 178/178 (17 files), playwright 53/53,
npm build OK — all exits 0. Raw logs from that run were overwritten by
the fix-round reruns; the counts are preserved here and in the
continuation-3 commit message (6c0e6d3).
