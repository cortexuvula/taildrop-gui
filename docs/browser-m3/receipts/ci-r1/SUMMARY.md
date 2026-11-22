# M3 CI repair checkpoint — CI-A fixed, CI-B fixture corrected, CI-C BLOCKED

**M3 is NOT complete. No Windows runtime GREEN, release acceptance, review waiver or smoke acceptance is claimed.**

Base / failed hosted candidate: `baa019b26ee9a11d259cfc511302e9a1ffd88423`.
Source-final / freshly tested partial-fix candidate: `106e464ba073a77dd6b29711b1cf15736bfc58ed`.
Local branch: master. The subsequent receipts-only commit does not alter source/config/tests. No push/tag/merge/version bump, fleet contact, new card, parallel writer or parallel reviewer.

## Per-failure disposition

### CI-A — FIXED with observed isolated RED→GREEN

- `6dc8ed38781ba44d8a38e553d44b343778c05458`, `test(browser): make Windows engine fixture auth explicit (CI-A)`.
- Unchanged base test failed locally with exactly the hosted `Auth: no working credential for fakeuser@127.0.0.1` panic: 0 passed / 1 failed, exit **101**.
- Executed the actual compiled Rust test binary with an empty owned HOME and USERPROFILE plus nonexistent SSH_AUTH_SOCK. Cargo/rustup were not run inside the empty home; the binary was compiled separately into the existing target. `ci-a-red.json` records a clean tracked tree at baa019b.
- Reused `fake::write_temp_key` / `connect_as(..., "fakeuser", &[key])` and retained the key guard through the session. Production auth acceptance is unchanged. No key material is in these receipts.
- Same isolated exact test then passed: 1 passed / 0 failed, exit **0**. All original byte-count, skipped-count, C#-sink exec count, zero destination SFTP mutation and actual wire payload assertions remain.
- Full source-final lib test binary also passed **318/318** under the isolated home/agent environment.

### CI-B — FIXTURE CORRECTED; native GREEN still unobserved

- `106e464ba073a77dd6b29711b1cf15736bfc58ed`, `test(browser): repair native junction fixture arguments (CI-B)`.
- Hosted RED: `windows.rs:633`, `junction fixture creation: The filename, directory name, or volume label syntax is incorrect.` No local native repro or pass is possible on this Mac.
- The old `arg(format!("mklink /J \"{}\" \"{}\"", ...))` wraps a complete already-quoted command as one normal Rust argument. Rust's Windows `append_arg` escapes the embedded quotes using CRT backslashes; cmd /c does not decode them as C argv would. See [Rust CommandExt raw_arg documentation](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.raw_arg) and [pinned std encoding source](https://raw.githubusercontent.com/rust-lang/rust/b940084d7/library/std/src/sys/args/windows.rs). `provenance.json` records the before/after **static encoding model**, explicitly not a Windows execution.
- Pass `cmd.exe` the separate arguments `/d`, `/c`, `mklink`, `/J`, link and target instead. This avoids embedded-quote escaping without introducing a raw-shell interpolation path. Disable cmd AutoRun for deterministic fixture startup. Construct the nested junction with `join("nested").join("junction")` rather than embedded `/` in a component.
- Give this native fixture a path containing a space, and verify both actual reparse attributes and sentinel contents through each link before deletion. Direct + nested junction deletion, ordinary nested child deletion, read-only deletion, and surviving out-of-tree sentinel assertions remain.
- Windows-GNU check/tests and clippy pass on the final source; these prove only compilation/lint, **not mklink/native deletion success**. Exact-head hosted Windows runtime must confirm this repair.

### CI-C — UNRESOLVED SCOPE BLOCKER; STOPPED without vendor/test weakening

See **[CI-C-BLOCKER.md](CI-C-BLOCKER.md)** for full trace, primary references, uncertainty and options.

- Replace consent is correctly sent when the C# recv session opens; file renaming occurs during F handling, before Z/place. Changing the later place argument does not fix this failure.
- The vendor uses legacy `NtSetInformationFile(..., 10)` / FileRenameInformation. Microsoft's documented contract refuses replacement of an open target; the original test intentionally reads that target concurrently. The vendor collapses *all* negative Replace rename statuses to `C(report.txt)` and removes the temp. The observed conflict therefore does not establish missing consent or a name race.
- This is a strong **source/API attribution**, not a measured failed NTSTATUS. The exact native status remains unknown because the C reply discards it. No C# execution occurred here, and the current test can nondeterministically pass when rename lands between reader opens.
- Original-case spelling, no-replace race safety, bounded concurrent reader, complete-old AND complete-new assertions are byte-equal to baa019b. C# and production sink are unchanged. No pauses, retry-until-green, sleeps, changed expectations, fallback writes or vendor divergence were added.
- **Owner decision needed:** authorize a separately bound native diagnostic with closed/held-open target controls and exact NTSTATUS capture. If confirmed, preserving concurrent-reader success requires a separately approved vendor successor/upstream fix (e.g. FileRenameInformationEx/POSIX semantics) or an explicit contract change. Both exceed current authority. Do not publish this partial checkpoint as a CI-C repair.

## Actual execution on source-final 106e464 (macOS)

Environment: CARGO_TARGET_DIR=`/Users/cortexuvula/Development/taildrop-gui/src-tauri/target`; CARGO_INCREMENTAL=0; CI=1. Logs/records provide exact UTC times and argv; no target/executable content digest is recorded.

| Actual command | Exit / observed result | Record |
|---|---|---|
| `cargo test --manifest-path src-tauri/Cargo.toml` | 0; **318** lib passed, main/doc **0**, none failed/ignored | cargo-full.json |
| Actual compiled lib test executable with empty HOME/USERPROFILE and absent SSH_AUTH_SOCK | 0; **318** passed | cargo-lib-isolated.json |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib browser::win_agent::tests -- --nocapture` | 0; **25** passed | targeted-win-agent.json |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 0 | fmt.json |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | 0 | clippy-host.json |
| `cargo check --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --tests` | 0; compilation only | check-windows.json |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --all-targets -- -D warnings` | 0; compilation/lint only | clippy-windows.json |
| `npm test` | 0; **211** tests / **20** files | vitest.json |
| `./node_modules/.bin/tsc --noEmit` | 0 | tsc.json |
| `npm run build` | 0; tsc + Vite frontend production build | frontend-build.json |
| `./node_modules/.bin/playwright test --workers=4 --retries=0` | 0; **66** passed, zero retries | playwright.json |
| `git diff --check baa019b HEAD` | 0 for both source commits | diff-hygiene.json |
| `python3 docs/browser-m3/receipts/ci-r1/diagnose.py` | 0; preservation/vendor/static checks, NOT native Windows | source-diagnosis.json |

## Hosted evidence is separate and remains RED

Run: https://github.com/cortexuvula/taildrop-gui/actions/runs/45031529401
Bound SHA: `baa019b26ee9a11d259cfc511302e9a1ffd88423` (not the new local source-final).

- macOS: **317 passed / 1 failed** (CI-A).
- Ubuntu: **316 passed / 1 failed** (CI-A).
- Windows: **258 passed / 3 failed** (CI-A/B/C).

No new hosted run was triggered or inspected as GREEN by Ada. These historical results do not prove the repaired candidate. No fleet smoke or real SSH installer/default-shell test was performed. Mandatory source review, exact-head hosted Windows runtime and both bounded peer smokes remain Frikkie/owner gates.

## Receipt integrity / preservation

- `results.json`: **19** actual local command records, expected exits checked, counts parsed programmatically; `raw-logs.tar.gz`: **20** byte-exact raw logs including hosted RED (archive round-trip equality checked). Logical `.log` names in the JSON are archive members, not loose files. Use `tar -xOf raw-logs.tar.gz <name>` to inspect without rewriting this directory.
- Raw archive SHA256: `6e3361e29410fc6e4a92b3f29bf3d9ddf9c8bb21c1676790b23104dfcf599294`.
- Hosted raw-log SHA256: `c2e3396d9391ee1498ca40093507d77debf90b1a1634c8327d29db2724227853`. Original external parent copy untouched; only the new local copy was archived.
- All **109** bound pre-existing status/review/vendor/receipt files remain byte-equal (`preserved-before.json`, `provenance.json`). Original R1/R2 verdicts and scanner-regex receipt were not edited.
- Vendor independently AST-extracted at upstream `c95f1b7bf446304b6c8826b149f6d94ca719ac69`: **35,158 bytes**, SHA256 `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133`, byte-exact. No deviations added.
- Protected `tailscale.rs`, `browser/agent.rs`, `agents/agent.pl`, `agents/posix_delete.sh` equal d111578. Only two test files differ in the new source commits; no product auth or production behavior changed.
- Initial compiler JSON artifact selector assumed `kind=lib`; this package exposes staticlib/cdylib/rlib, so the setup selector failed. Corrected to executable+profile.test and then captured the actual isolated RED. This was a capture setup correction, not fabricated failure output.
- Startup disk: **99 GiB** free; final source-diagnosis free bytes **106292817920**, above the 60 GB stop threshold. Only existing `src-tauri/target` reused/retained, final `du -sk` **9567008 KiB** (9.1 GiB display). Target trees created/deleted this run: **none**. No target-content digest.

## Same-card handoff requested

Codie: inspect the two fixture commits and CI-C source/API attribution. Write any new verdict separately as **`codie-m3-ci-review.md`**; preserve R1/R2 verdicts. A source-only review of this partial checkpoint is not M3 completion, a passing native Replace test, authorization to diverge vendor bytes or release acceptance. If same-card request-review is rejected circularly, retain this checkpoint and block once for parent routing as instructed; do not make another card or repeat the transition.
