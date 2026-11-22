# M3 R2 — Codie corrections and fresh local gates

**W1, W2 and W3 corrected; returning for Codie re-review. Release HOLD.**
No push, tag, merge, version bump or fleet access. No C# compilation/execution
on this Mac. Frikkie retains exact-head Windows CI and both peer smokes.

Reviewed predecessor: `8d555e91bdba78c29a72f08bdd2eabb854b26af3`.
Code-final and freshly tested SHA: `3d4651cb38644c13c48388d224c11b33515c113c`.
Subsequent changes are documentation and receipts only. `results.json` is the
current verification authority; parent-directory R1 results remain historical.

## Local commits

| SHA | Change |
|---|---|
| `e417b4f2baeca4ba75d75e77f8e48b4a4fe935db` | W1: drive-root parents preserved by Windows mkdir/rename command adapters |
| `b250bb51dcc78c4aab0877882121d79d26c6fb7b` | W2: current Unix time for mkdir; wire and native timestamp-window assertions |
| `a7e03650bf2af9b8eddaa761c74803f83f15d866` | W3: shared 255 UTF-16-unit component bound; refusal before offending records/verbs |
| `3d4651cb38644c13c48388d224c11b33515c113c` | Native race, old/new reader, unfinished-tree cancellation and nested-junction assertions |

## Correction coverage and regression witnesses

All rows map to implementation task `t_3de70319`; W1/W2/W3 reuse Codie's
finding IDs. Their mapping to brief FR-005/006 uses the IDs assigned in the
original handoff, not IDs present in the authoritative brief.

| Requirement | Implementation and behavioral evidence | Observed result |
|---|---|---|
| W1 / FR-005: mkdir and rename at `/C:/` and `C:/` | Windows-only parent helper; production adapters extracted behind the existing transport trait. `w1_mkdir_command_adapter_keeps_drive_root_absolute`, `w1_rename_command_adapter_keeps_drive_root_absolute` inspect actual recv/rename bytes for both roots and both nested controls. Original POSIX `parent_of` unchanged; `/C:` and `C:` remain invalid absolute paths. | `w1-red.log`: both fail with absolute-drive-path refusal before fix. `w1-green.log`: both pass after fix. |
| W2 / FR-005: new folder has current mtime | Mkdir emits current Unix time rather than zero. Existing native-verb test parses the actual D-record timestamp and requires it between operation start/end. Existing native mkdir test now checks actual on-disk mtime in its operation window. Copy-source timestamp serialization is unchanged. | `w2-red.log`: emitted mtime 0 outside current window. `w2-green.log`: entire selected M3 host suite passes. Native timestamp assertion compile-checked only. |
| W3 / FR-006: supported Windows component length at every boundary | Shared `windows_name_problem` rejects >255 UTF-16 code units, covering absolute paths, records, mkdir/rename/exists/delete. ASCII 255 accepted/256 refused; 127 non-BMP scalars + ASCII accepted/128 non-BMP refused. 32,769-ASCII hostile remote child refused as F and D, with identical wire bytes before/after admission. Symlink refusal also checked. Overlong verb/absolute inputs open no exec and mutate no SFTP. | `w3-red.log`: three behavioral failures for admitted long names. `w3-green.log`: three tests pass. This proves client refusal, NOT native truncation execution. |
| Review suggestion: non-vacuous native checks | TOCTOU waits for the helper-owned temp before planting winner. Atomic reader must observe complete old AND complete new data, with readiness/notification deadlines and unwind stop. New unfinished-directory EOF test checks a partial child and completed child are removed with the staged tree. Recursive delete now includes a nested junction and checks external sentinel survival. | Windows-GNU check/clippy pass; all native runtime results pending Windows CI. No Mac RED/GREEN claim for native behavior. |

The RED runs used pre-fix behavior in this checkout after adding the tests.
W1 extracted a transport-trait seam without changing its parent behavior before
observing RED. Early compile/setup errors while writing seams were corrected;
they are not counted as regression witnesses. Green logs with end delimiters
preserve the original raw output as a prefix and avoid receipt EOF-whitespace
failures. No existing tests were suppressed or weakened.

## Fresh local verification

Source: direct parent-owned execution in this checkout, at the code-final SHA.
Exact argv, timestamps, exit statuses, output summaries, scopes and log SHA-256s
are in `results.json`; repeatable runner is `run-gates.py`. Required local gates
come from the authoritative M3 brief, project commands, and `.github/workflows/ci.yml`.
All Rust builds use the existing `src-tauri/target`, `CARGO_INCREMENTAL=0`.

| Command | Result | Receipt |
|---|---|---|
| `cargo test --manifest-path src-tauri/Cargo.toml --lib` | **318 passed**, 0 failed/ignored | `cargo-lib.log` |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | exit 0 | `fmt.log` |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | exit 0 | `clippy-host.log` |
| `cargo check --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --tests` | exit 0, Rust compilation only | `check-windows.log` |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --all-targets -- -D warnings` | exit 0, Rust compilation/lint only | `clippy-windows.log` |
| `./node_modules/.bin/tsc --noEmit` | exit 0 | `tsc.log` |
| `npm test` | **211 passed**, 20 files | `vitest.log` |
| `./node_modules/.bin/playwright test --workers=4 --retries=0` | **66 passed**, fresh Vite, zero retries, IPC fakes | `playwright.log` |
| `npm run build` | exit 0 | `frontend-build.log` |
| `git diff --check d111578 HEAD` | exit 0 at tested SHA | `diff-hygiene.log` |
| `python3 docs/browser-m3/receipts/r2/check-provenance.py` | exact vendor/protected-file equality and narrow added-line scan pass | `provenance.json` |

No formal generated-artifact map is declared (`map_not_declared`). A narrow
added-line hygiene scan of the changed Rust source includes positive controls;
no matches were found. It is not an external security audit. An independent
read-only second reader found no substantive correction/harness defect; this
is not Codie approval. Its receipt-whitespace observation was repaired before
the full gate. R1's ten-repeat stability result is historical, not claimed as
an R2 repetition run.

## Vendoring and wrapper deviations

- C# remains byte-exact: **35,158 bytes**, SHA-256
  `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133`.
  Independently AST-extracted from upstream
  `c95f1b7bf446304b6c8826b149f6d94ca719ac69` and compared again in R2.
- No C# source deviations. The inherited 16-bit native conversion is not edited;
  the Rust boundary refuses impossible components before it can be reached.
- The new 255-UTF-16-unit limit is a deliberate additional Rust validation rule.
  Existing stricter launcher validation, bounded I/O, client case-clash guard,
  capability proof/invalidation, and attempt-unique source upload remain as
  documented in the original handoff. Final exe is still hash-bound; upload
  source uses `agent-<hash>.<nonce>.cs` to avoid interrupted-upload retry poisoning.
- `tailscale.rs`, `browser/agent.rs`, `agent.pl` and `posix_delete.sh` remain
  byte-equal to `d111578`. POSIX agent suites pass unchanged.

## Unobserved Windows behavior and required next gates

**CI-verified C# behaviors in this run: none.** There are now nine Windows-native
harness tests; `provenance.json` enumerates their exact names. Cross-compilation
checks their Rust code, not their C# compilation or filesystem assertions.

After Codie approval, Frikkie should own publication/CI and inspect the exact-head
`windows-latest` Rust-tests job. The native selector is:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib browser::win_agent::tests::windows -- --nocapture
```

Pending native behaviors: PowerShell 5.1/.NET csc, cmd/PowerShell command loaders,
tree and source mtime preservation, current mkdir mtime, TOCTOU Keep, atomic
Replace/RealName, file and directory EOF cleanup, planted-temp/lookalike safety,
case-clash/CON refusal, read-only deletion, top-level AND nested junction-safe
delete, and all-target home guard. Missing compiler/PowerShell is a hard test
failure, not a skip. Real SSH overlapped pipes, default shells and account ACLs
still require operator smoke even if loopback CI is green.

## Exact on-machine smoke for Frikkie

Run the full updated [`../../SMOKE.md`](../../SMOKE.md) separately on **room-6**
and **accuro-server**, only after Codie and exact-head CI. It includes literal
PowerShell fixture creation and assertions, pinned account, unique owned
LocalApplicationData scratch root and cleanup; it is prepared, not executed.

In addition to install/cache/copy/consent/RealName/case/CON/cancel and scope-lock
checks, it now explicitly checks mkdir's on-disk mtime before changing the new
folder, a 256-ASCII New folder refusal, and recursive deletion containing a
nested junction whose external sentinel must survive. No drive-root fixtures
are authorized by this smoke. Keep accuro-server fixtures within the previously
approved 2 MiB bound; an already-finished cancellation is inconclusive, not pass.
Record candidate SHA, CI URL, account and every pass/fail/not-run/inconclusive
result separately for both peers. Do not alter services, registry, clinical
paths or shared helper installations to simulate failures.

## Resource/workspace closeout

- Startup disk checked above 60 GB before builds; runner records actual free bytes.
- **New target directories this R2 run: none. Deleted target directories: none.**
  Existing `src-tauri/target`: **8.7 GiB observed**, reused and retained. No receipt
  binds target contents. Original R1's extra-target cleanup remains historical.
- Existing `codie-*.md`, `codie-m3-evidence/`, `M2A-STATUS.md`, `M2B-STATUS.md`
  remain untracked; no edits by this run. No source worktree or CLI lane created.
- All implementation changes remain local conventional commits using explicit
  paths. No `git add -A`, push, tag, merge, version change or fleet contact.
