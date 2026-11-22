# M3 Windows parity — original R1 handoff (historical)

> **Superseded by [R2 correction handoff](r2/SUMMARY.md).** Codie requested
> changes at `8d555e9`; W1/W2/W3 are corrected and freshly gated at `3d4651c`.
> The results below are preserved as historical R1 evidence, not the current
> candidate's verification authority. R2 review, Windows CI and fleet smoke
> remain separate outstanding gates.

## Status

**Implementation and local verification complete; release gate HOLD.**
Codie review, actual Windows CI execution and Frikkie's two-peer smoke remain
unobserved. Nothing was pushed, tagged, merged or version-bumped; no fleet peer
was contacted. `tailscale.rs`, the Perl agent, POSIX delete script and
`browser/agent.rs` are byte-unchanged against `d111578`.

Final independently tested commit:
`23a00f583458927d339a6c22a2ed2fb12153c383`.
The source-code-final change is `b5184d5`; `23a00f5` adds historical-receipt
clarifications only. Subsequent handoff commits are docs/receipts only.
All ten fresh parent checks bind the tested SHA and exact commands in
`ada-observed-results.json`. `BACKEND-SUMMARY.md` is the earlier isolated-lane
receipt, not the final authority for repaired recovery behavior.

## Local master commits

| SHA | Change |
|---|---|
| `aa6f9c0` | Strict optional `winAgent` IPC sanitizer |
| `641dbce` | Host/account/generation-scoped frontend write gate |
| `87bcc13` | Byte-exact C# vendor, installer/cache and Windows write routing |
| `9317f5a` | Production-App Windows capability Playwright tests |
| `81c1283` | Protocol, proof/refusal tests and Windows child-process harness |
| `1e5782d` | Historical backend receipts |
| `49c66ef` | Cancel test reaches C#'s actual 256 KiB disk-write barrier |
| `b5184d5` | Retry-safe installation, EOF-error proof revocation, bounded native harness and engine/shell coverage |
| `23a00f5` | Distinguish historical receipts from final gates |

The implementation lanes worked on an isolated branch; their patches were
reviewed and cherry-picked onto local master. The isolated worktree was removed.
Its branch is retained so the historical lane receipt SHAs remain reachable.

## Observed checks

Source: parent-owned execution in this checkout, all at the tested SHA above.
All commands use the repository's existing toolchains/dependencies. Rust uses
`CARGO_TARGET_DIR=/Users/cortexuvula/Development/taildrop-gui/src-tauri/target`
and `CARGO_INCREMENTAL=0`; Playwright uses `CI=1`, a fresh owned Vite server,
four workers and zero retries.

| Exact command | Result | Scope / raw receipt |
|---|---|---|
| `cargo test --manifest-path src-tauri/Cargo.toml --lib` | **313 passed**, zero failed/ignored | Host lib tests, including 21 new all-platform M3 tests; `ada-cargo-test.log` |
| Same full Rust command, ten consecutive repetitions | **10/10, 313 passed each** | Stability, not Windows execution; `ada-stability.json` / `.log` |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | exit 0 | Rust formatting; `ada-fmt.log` |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | exit 0 | Host production + tests; `ada-clippy-host.log` |
| `cargo check --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --tests` | exit 0 | Windows compilation, including eight native tests; `ada-check-windows.log` |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-gnu --all-targets -- -D warnings` | exit 0 | Windows compilation/lints, no execution; `ada-clippy-windows.log` |
| `./node_modules/.bin/tsc --noEmit` | exit 0 | TypeScript; `ada-tsc.log` |
| `npm test` | **211 passed**, 20 files | Existing 193 plus 18; `ada-vitest.log` |
| `./node_modules/.bin/playwright test --workers=4 --retries=0` | **66 passed** | Existing 62 plus four, IPC fakes through production UI; `ada-playwright.log` |
| `npm run build` | exit 0 | TypeScript + Vite production frontend; `ada-build.log` |
| `git diff --check d111578 HEAD` | exit 0 | Full committed delta at tested SHA; `ada-diff-hygiene.log` |
| Added-source-line static scan with positive controls | No matches | 22 source files; secret literals, shell=True/os.system, unsafe deserialization, unlinked TODO/FIXME/stubs; `ada-static-scan.json`. Not a full security audit. |

Fresh baseline execution is retained under `baseline-d111578/`: 292 Rust,
193 Vitest, 62 Playwright, TypeScript clean. Initial parent verification at
`49c66ef` is retained under `initial-49c66ef/`; its one failed gate was a new
blank line at the end of a raw receipt (introduced, not a product failure).
The captured output was preserved and given an explicit end delimiter; final
full diff hygiene passed. Raw logs with end delimiters retain original output
bytes as a prefix.

## Functional requirement coverage

IDs below are assigned by this handoff in authoritative-brief order; every row
maps to implementation task `t_3de70319` (inferred from the full brief, not IDs
pre-existing in it). A named test is evidence within its stated platform only.

| Requirement | Implementation | Evidence |
|---|---|---|
| FR-001: MIT C# vendor, Pipe, attribution and SHA/LF pins | `agents/agent.cs`, THIRD_PARTY.md, .gitattributes | Independent AST extraction equals all 35158 upstream bytes; `vendored_csharp_is_plugin_exact` |
| FR-002: Account-local SFTP install, encoded PowerShell compile, cache/invalidation | `win_agent.rs`, BrowserState, RusshTransport | Exact-answer spoof refusal, install-only SFTP, single-flight, account/host separation, in-flight invalidation, bounded capture, retry and dead-helper tests |
| FR-003: Windows D/F/Z sink, atomic placement/RealName semantics | `WinAgentSink`; Perl AgentSink unchanged | Wire/payload/reply tests and `proven_windows_engine_streams_remote_source_only_through_csharp_sink`; native atomic/RealName behavior **pending CI** |
| FR-004: Proven-only Windows destination/UI; no silent SFTP fallback; local Windows unchanged | preflight + engine gates, scoped pane proof | Unproven/reforged capability tests, fake Windows engine refusal, production-UI proved/unproven/stale-account tests |
| FR-005: Windows mkdir/rename/delete with existing guard semantics | Native helper verbs; mkdir uses recv D/Z | Native-verb all-platform tests; eight native tests include read-only/junction/all-target guard **pending CI**; existing POSIX tests unchanged |
| FR-006: Existing Windows-name rules at every Windows-bound component | Absolute-path validation (drive exempt), each stream record, rename/mkdir/delete helpers | Nested reserved/ADS/traversal/trailing-dot/space rejection, CON before header, case-variant rejection before second header |
| SC-001: All required local gates stay green | Parent checks above | Ten fresh checks and 10/10 full Rust repeats |
| SC-002: Actual C# behavior on windows-latest | Existing CI already runs cargo test there | Harness compiles locally; Windows job **not observed**, not claimed passed |
| SC-003: Review and two-peer operator smoke before release | Same-card Codie handoff and `../SMOKE.md` | **Pending**, neither preparation nor dispatch is approval |

No formal generated-artifact map is declared for these touched paths. The C#
file is an exact vendored source body, not a reconstructed implementation.

## Reconciliation fixes and regression witnesses

- Independent inspection found the initial cancel test sent 64 KiB while C#
  `Exact` waits for a complete 256 KiB buffer before `WriteFile`. Fixed to send
  one full buffer and wait for its actual on-disk length. Windows execution of
  this test remains pending; no Mac runtime RED/GREEN claim.
- A deterministic installer source could poison all later retries after an
  interrupted upload/failed compile. New all-platform test observed a real
  `installer source already exists` failure before the repair.
- A write failure followed by EOF failure bypassed missing-helper diagnostic
  collection/revocation. The mock reproduces diagnostics arriving only during
  exit collection and observed the capability wrongly remaining valid.
- `ada-recovery-red.log` records both behavioral failures on pre-fix production
  behavior with test seams; `ada-recovery-green.log` records both passing after
  repair. The latter also includes the proved remote-source → C# sink engine test.
- The native Replace reader now stops on unwind and has readiness/reader
  deadlines so an earlier helper failure cannot hang Tokio runtime shutdown.
- The harness now really launches `cmd /c`, rather than merely asserting that
  string and bypassing it. An additional native test compiles and probes through
  both cmd and PowerShell login-command shapes, including a path with spaces.
- Earlier case-guard removal RED and strict IPC sanitizer/UI RED outputs are
  retained in backend and `frontend-lane/` receipts. These are regression
  witnesses, not claims that the browse/pull-only baseline already had writes.

## Vendoring, deliberate deviations and limitations

C# body: **zero deviations**, including overlapped `Pipe`.
Upstream commit `c95f1b7bf446304b6c8826b149f6d94ca719ac69`;
SHA-256 `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133`.
Final installed executable remains `agent-cfa1c1af6b161718.exe`.

Rust wrapper changes relative to plugin:

- Entire installer/probe I/O bounded; output capped while reading; source/exe
  presence or an `installed` marker is never proof. Exact `exists` output `1`
  with rc0 is required for both existing and freshly compiled binaries.
- Exclusive source names include an attempt nonce:
  `agent-<hash>.<random-16hex>.cs`, rather than the plugin's deterministic
  `agent-<hash>.cs`. This is an intentional installer deviation to avoid
  retry poisoning or overwriting old source. Compile `finally` removes its own
  source/PID-temp. A disconnect before compile can leave a harmless orphan
  source but cannot block a new attempt. No garbage collection of old versions.
- Installed shell paths additionally reject `&;!()`; no path from a copied item
  enters shell text. Windows paths are absolute drive paths, not UNC/device paths.
- Client case tracking also refuses clashes in Replace/merge, where the upstream
  server alone could replace successive A/a items. Unicode uppercasing is
  conservative and can refuse more pairs than Windows ordinal-insensitive rules.
- Cache is host+account single-flight, TTL 600 s and bounded to 128 entries;
  account/forget-host invalidation revokes outstanding proofs. Negative answers
  are cached until expiry/invalidation; runtime missing-helper diagnostics revoke
  a positive proof. No automatic replay of a failed mutation.
- Upstream receive accepts D/F/Z, **not L**; symlinks are counted as skipped.
  Mkdir is recv D/Z because no mkdir verb exists. The receiver inherits ACLs and
  ignores POSIX modes; mtime is upstream best effort. No invented mode fidelity.
- Per-item temps are dot-prefixed, not Windows Hidden attributes. Completed
  siblings commit incrementally; cancellation cleans unfinished owned items, not
  the whole job. Explicit destination Top follows its path; descendants are
  no-follow. Client never deletes destination temp names itself.
- `base64 0.22` becomes an explicit dependency (already transitively locked) for
  UTF-16LE EncodedCommand. No dependency versions were upgraded.

## Windows execution: what is and is not verified

**CI-verified C# behaviors: none in this run.** Mac cross-compilation is not
execution. Existing `ci.yml` already executes these eight tests on windows-latest:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib browser::win_agent::tests::windows -- --nocapture
```

Pending native checks: tree/mtime and ignored-mode semantics, TOCTOU Keep,
atomic Replace/RealName, partial-EOF cleanup, planted exact temp/lookalike,
case-clash and client CON refusal, read-only and junction-safe delete,
all-target home guard, and real cmd/PowerShell loader/probe behavior. Missing
PowerShell/.NET csc is a hard test failure, never a silent skip.

Real SSH session behavior, current peer DefaultShell, account ACLs and per-peer
native execution still require Frikkie's `../SMOKE.md`. Neither room-6 nor
accuro-server was contacted by any accepted implementation/test lane.
Frontend screenshots are retained; parent vision inspection was unavailable
(model-not-found), so the parent claims DOM/E2E checks, not independent visual QA.

## Resource and workspace closeout

- Existing `src-tauri/target`: reused, **8.7 GiB observed at closeout**, kept;
  not task-created. All accepted Rust builds used this one target, incremental off.
- Rejected initial CLI input lane created `/private/tmp/taildrop-m3-target`:
  **2.0 GiB observed**, deleted and absence rechecked. This violated the instructed
  target/scratch location; it was stopped and none of its incomplete code accepted.
  The rejected CLI used its configured glm model, not the parent model.
- No other target directories created. Startup free space remained above 60 GB.
- Isolated worktree, node_modules symlink and task temp prompt/scripts/logs removed.
  All wanted evidence copied outside target directories before cleanup.
- Existing untracked `codie-*.md`, `M2A-STATUS.md`, `M2B-STATUS.md` remain untracked
  and unchanged. Final product changes are local master commits only.

## Required next owner actions

1. **Codie:** inspect `d111578..HEAD`, especially proof invalidation, no Windows
   SFTP fallback, installer nonce deviation, and native test failure paths.
2. **Frikkie:** after review, own push/exact-head Windows CI; inspect actual eight
   native test results rather than inferring from cross-checks.
3. **Frikkie:** run and record `../SMOKE.md` separately on room-6 and accuro-server.
   No release approval, push, tag or version bump is delegated by this receipt.
