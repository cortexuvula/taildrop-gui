# M3 backend verification and handoff

> Historical isolated-lane receipt at `84c1f10`. Parent `SUMMARY.md` supersedes
> this receipt for final SHAs, gate results and recovery/harness repairs.
> In particular, the deterministic-source retry limitation below was repaired
> with attempt-unique source names; do not treat it as a current limitation.

## Outcome and scope

Backend implementation complete; **Windows runtime gate remains HOLD / not observed**.
No C# behavior was executed on this macOS host. No fleet connection or fleet
write was performed. No push, tag, merge, version bump, board transition, or
parent-master change was performed by this lane. Frontend belongs to the sibling.

- Backend implementation commit: `e3edc65`.
- Backend code-final + test commit: `84c1f10f98d7d66cb7ef33dbedbd01d17e576825`.
- Final observed commands and UTC times: `backend-final-results.json` and
  the six `backend-final-*.txt` raw logs alongside this file.
- All build commands used only
  `CARGO_TARGET_DIR=/Users/cortexuvula/Development/taildrop-gui/src-tauri/target`
  and `CARGO_INCREMENTAL=0`. `df -g .` ran before each; 99 GiB was available,
  above the mandatory 60 GB stop threshold. No other target directory created.
- This worktree also contains independent frontend commits; the two backend
  commit IDs above identify this lane's changes, not all branch work.

## Final observed checks (code-final SHA above)

All commands ran in `.m3-codex-lane`, all exit statuses **0**:

| Exact command | Observation / scope |
|---|---|
| `cargo test --manifest-path src-tauri/Cargo.toml --lib` | **310 passed**, 0 failed/ignored. Includes 18 new all-platform Windows tests and unchanged POSIX agent tests. |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | Pass, no formatting output. |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | Pass, host production and test code. |
| `cargo check --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --tests` | Pass, including all seven cfg(windows) C# runtime tests. **Compilation only**. |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --all-targets -- -D warnings` | Pass, Windows production and test code. **No execution**. |
| `git diff --check` | Pass, code-final tree clean before receipt-only changes. |

Frontend TypeScript/Vitest/Playwright gates were not run by this backend lane;
the sibling/parent owns those fresh results. GitHub CI has not been dispatched
or observed by this lane (push forbidden). Existing `ci.yml` already runs
`cargo test` on `windows-latest`; no workflow change was needed.

## Requirement-to-evidence map

1. **Vendor exact C# + MIT + LF.** `src-tauri/agents/agent.cs` is the exact
   35158-byte WIN_AGENT_CS body at upstream
   `c95f1b7bf446304b6c8826b149f6d94ca719ac69`, including overlapped `Pipe`.
   SHA-256: `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133`.
   No source-header deviation; full MIT attribution is in `agents/THIRD_PARTY.md`.
   `.gitattributes` explicitly pins `*.cs` LF. `vendored_csharp_is_plugin_exact`
   checks hash, filename-prefix binding, Pipe, and no CR bytes.
2. **Install/exec/prove/cache.** `win_agent.rs` implements UTF-16LE
   EncodedCommand/InputFormat None, account-local SFTP source placement,
   PowerShell Add-Type ConsoleApplication, PID-temp + Move-Item publication,
   strict executable path validation, and exact known-answer
   `exists\0<actual TailFiles dir>\0 1\0<exe basename>\0` (without the display
   space before 1): output exactly `1`, rc0. Both existing and newly compiled
   executables are tested; an installed marker or Test-Path alone never grants
   capability. Entire I/O is timed; stdout is capped while reading. Timeout
   cleanup retains the channel, closes stdin first, then waits bounded.
   Host/account cache is single-flight, TTL600s, bounded128; account/forget
   invalidation revokes shared handles and in-flight results. Missing executable
   / `not recognized` revokes proof and requires a new known-answer probe.
   Mock tests cover all of these; **real remote install still needs operator smoke**.
3. **C# sink.** Production `WinAgentSink` drives exact NUL D/F/Z and C/E/Z,
   tracks exact payload lengths, validates every relative component before its
   header, detects sibling case clashes even during Replace/merge, and skips
   symlinks without sending L. Cleanup is EOF + bounded wait only, never a
   client-name temp deletion. Real Windows tests are present and compile-checked,
   not yet executed.
4. **Proven-only write gates.** `ProbeInfo.winAgent` serde default is false.
   `browser_probe` proves the exact authenticated host/account even when a listing
   already cached home/OS. Source-only open/pull never requires an agent.
   Same-host copies borrow the source transport but prove its destination role.
   Backend preflight and engine both refuse unproven Windows destinations;
   Windows never reaches RemoteSink. Existing fake Windows engine test now
   asserts typed Unsupported and no destination file, replacing the old internal
   test expectation of SFTP fallback. Local Windows remains unchanged/fail-closed.
5. **Mkdir/rename/delete.** Mkdir is a single D/Z receive; rename/exists/delete
   are native helper verbs. Mock transports panic on destination SFTP operations.
   Delete still runs fresh home and parent-resolved guards before C#'s own
   all-target held-handle checks; there is no Windows SFTP delete fallback.
   All-platform verb tests pass; actual read-only/junction behavior awaits Windows.
6. **Names.** Existing winnames implementation is reused, not rebuilt. All
   Windows-bound absolute components (drive prefix exempt) and every streamed
   component are validated, including nested CON/AUX/COM1, traversal, trailing
   dots/spaces, separators/ADS. Tests assert rejection before the record write.

## Windows execution gate, not yet observed

On `windows-latest` (already wired through the existing Rust tests step), run:

```
cargo test --manifest-path src-tauri/Cargo.toml --lib browser::win_agent::tests::windows -- --nocapture
```

The harness fails loudly if Windows PowerShell **5.1** or the .NET Framework 4
`Framework64/v4.0.30319/csc.exe` is missing; it never silently skips. Each
fixture compiles the exact vendored source with production compile-script
shape, proves the binary, and drives the **production sink** through a local
child ExecChannel with exact NUL bytes. No SSH server or fleet peer involved.
The seven tests cover:

- Tree placement/file+directory mtimes; mode=0 explicitly ignored (not made
  read-only), symlink skip and client-side CON rejection.
- TOCTOU Keep refusal, atomic Replace reader seeing only complete old/new data,
  and RealName preserving `Report.TXT` while receiving `report.txt`.
- EOF/short-payload cleanup after a real partial-file barrier, preserving an
  already committed sibling and unrelated lookalike.
- Planted exact next temporary basename and lookalike both untouched.
- Client-side case clash in Replace/merge before the second header.
- Native mkdir/rename/exists, read-only delete, junction removal with an external
  sentinel untouched.
- All-target home guard refusing before the first selected target is deleted.

**Do not label any of these seven behaviors CI-verified until an actual Windows
job returns green.** GNU cross-checking does not execute C# or prove NT behavior.

## RED and issues encountered

- `backend-red-vendor.log`: real assertion failure with the intentionally empty
  source seam before vendoring (expected empty-source hash != upstream hash).
- `backend-red-case.log`: real behavioral regression witness with only the
  client case guard temporarily suppressed, i.e.:
  `if self.paths.get(&folded).is_some_and(|p| p != &path && false) { ... }`.
  `replace_merge_rejects_case_variants_before_second_header` failed because the
  second header incorrectly returned Ok. Guard restored immediately; final full
  suite is green. This is a guard-removal seam witness, not a pre-existing bug
  claim about the original browse/pull-only product.
- First broad run (`backend-lib.log`) exposed an **introduced** overbroad
  engine preflight: five existing local tests use synthetic `local2` targets and
  got Account rather than exercising the POSIX sink. Corrected by keeping the
  new engine-level preflight Windows-only; command-layer general preflight is
  unchanged. Final full pass: 310/310.
- Host Clippy initially caught test-only constant chunks/MutexGuard lifetime;
  corrected using `as_chunks` and a lexical lock scope. Final host + Windows
  Clippy both pass without allows.

## Deliberate limits and deviations

- Existing Perl source and AgentSink, and `tailscale.rs`, are untouched.
- Source bodies are exact; source SHA hashes the **actual uploaded bytes**.
  Installer shell paths additionally reject `&;!()` beyond upstream WIN_PATH_RE.
- No old helper-version garbage collection. Account-private source/exe placement
  is the sole Windows SFTP write exception. A failed/interrupted exclusive source
  upload can leave the bound `.cs` and require operator cleanup before retry;
  it never enables destination SFTP writes or unproven capabilities.
- Temps are dot-prefixed, **not Windows Hidden attributes**. Completed siblings
  are committed incrementally and are not rolled back on later cancellation.
  EOF cleans only unfinished helper-owned items through handles.
- Explicit destination Top follows its path; descendant opens are no-follow.
- Receive ignores POSIX modes, inherits destination ACLs; mtime is best effort
  upstream (SetFileTime return unchecked). No general mode-fidelity claim.
- Client Unicode uppercase collision detection is conservative; it can refuse
  more names than the Windows ordinal-insensitive comparison. A/a and nested
  Replace/merge collisions are explicitly covered.

## Operator-owned on-machine smoke before release approval

Only Frikkie should run this, on **each** Windows peer (`room-6` and
`accuro-server`), using a newly created, empty, dedicated scratch folder beneath
his authenticated account. This lane did not connect to either host.

1. Open the peer as the intended account; browse and pull a small fixture first.
   Confirm probe user matches loaded account and `winAgent === true` only after
   installer/known-answer proof. Inspect account-local `TailFiles` exe basename
   `agent-cfa1c1af6b161718.exe` (first-run and existing-exe paths).
2. Copy a scratch tree with empty directory, binary file and controlled mtimes;
   compare bytes and timestamps. POSIX mode is intentionally not preserved.
3. Retry a same-name copy: Keep refuses unchanged; explicit Replace succeeds.
   Precreate `Report.TXT`, receive `report.txt`, confirm spelling remains
   `Report.TXT`. Check no half-written replacement becomes visible.
4. On a case-sensitive source select A/a in one folder; verify the second
   Windows-bound record is refused (also under Replace/merge). Try nested CON:
   client refuses before remote creation. Symlinks are counted as skipped.
5. Cancel a large scratch transfer mid-file: unfinished temp disappears after
   bounded cleanup; prior committed siblings and unrelated dot-prefixed files
   remain. No promise of whole-copy rollback.
6. In scratch only, create/rename a folder; delete a read-only fixture; remove a
   scratch junction whose target contains a sentinel outside the junction.
   Confirm the sentinel survives and home/ancestor deletion is refused.
7. Switch the saved account while retaining a loaded pane/dialog: proof must
   not transfer to another account. Forget the host key then reconnect only
   after independently checking the fingerprint. For a deliberately unavailable
   helper/exec fixture, confirm `winAgent === false`, browse/pull still work,
   writes return typed Unsupported, and no SFTP destination writes occur.
8. Remove only the dedicated smoke fixtures. Record the actual release-candidate
   SHA, account, Windows job URL and per-peer smoke results before v0.18.0.
