# M3 busy-file replacement — vendor successor build receipt

Branch: `fix/m3-busy-file-replace` (from local master `991fce07d0431c29b8b233c349746149bb32f97b`)
Code-final commit: `92c564a9628c77ff69c21465983acf933b51e37b`
Date: 2026-11-22 (MST). Builder: Turing. Owner ruling implemented:
"Preserve busy-file success — authorize a narrow vendor change, Codie review
and CI" (clarify submission, 2026-11-22).

## What changed

Product/vendor (the entire semantic delta is TWO code lines in `RenameTo`):

| File | Change |
|---|---|
| `src-tauri/agents/agent.cs` | Replacing renames now call `NtSetInformationFile` with class 65 (`FileRenameInformationEx`), DWORD `Flags=0x3` (`FILE_RENAME_FLAG_REPLACE_IF_EXISTS \| FILE_RENAME_FLAG_POSIX_SEMANTICS`, MS-FSCC) at offset 0 of the unchanged native layout; non-replacing renames keep class 10, flags 0 (byte-equivalent behavior). 35927 bytes, sha256 `adefa12c41424c983b78cf1ba5956ba23d1dd0e1a03be11784167cdf9e563f10`. Diff vs upstream: +10 comment lines, 2 changed code lines. |
| `src-tauri/agents/agent.upstream-c95f1b7.cs` | NEW immutable reference: the original upstream bytes, byte-identical, 35158 bytes, sha256 `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133` (upstream `c95f1b7bf446304b6c8826b149f6d94ca719ac69`, `bin/tailfiles:1495–2216`). Never uploaded/compiled in production; `.gitattributes` `src-tauri/agents/*.cs text eol=lf` covers it (verified with `git check-attr`). |
| `src-tauri/src/browser/win_agent.rs` | `SOURCE_HASH` → `adefa12c41424c98` (exe name `agent-adefa12c41424c98.exe`, hash-bound install/proof unchanged). `compile_script` refactored to delegate to `compile_script_for(folder, source, hash)` so the native test fixture can compile a reference payload under its own hash; production path passes `SOURCE_HASH` (format string otherwise byte-identical). |
| `src-tauri/src/browser/win_agent/tests.rs` | Old `vendored_csharp_is_plugin_exact` replaced by `vendored_csharp_upstream_reference_is_immutable` (pins the reference file + legacy class-10 source lines) and `vendored_csharp_active_successor_is_pinned_divergence` (pins active sha256/SOURCE_HASH + the successor source lines + `assert_ne` vs upstream — divergence is never disguised as byte equivalence). |
| `src-tauri/src/browser/win_agent/tests/windows.rs` | Native-only additions: `compile_reference` (test-seam compile of an arbitrary payload under its own hash), `raw_recv_replace` (exact production wire encoding via `fields`/`agent::f_record`/`ReplyReader`, bypassing the proof layer), `incoming_temps`, and three tests — see below. |
| `src-tauri/agents/THIRD_PARTY.md` | Provenance rewritten: two-payload table (roles, bytes, sha256), reviewed-delta description, behavior/support limits (share-delete requirement, fail-closed mapping to the existing conflict reply, OS floor qualification, pending-CI statement). |

Untouched: POSIX agents, original Taildrop surface, `agent.pl`,
`posix_delete.sh`, `.gitattributes` (existing glob already covers the new
file), CI workflows, all historical receipts. Untracked verdict/status files
preserved byte-exact (15 untracked at branch creation, still untracked).

## Regression design (RED→GREEN)

- `csharp_busy_destination_replacement_succeeds_under_held_reader` — GREEN
  witness. One `std::fs::File` handle (Rust default share
  READ|WRITE|DELETE) stays open across a production `WinAgentSink`
  replace: place succeeds; the held handle re-reads complete OLD bytes;
  new opens read complete NEW bytes; `Report.TXT` spelling retained; no
  `.tailfiles-incoming-*` leftovers; sentinel intact; zero SFTP mutations.
  Deterministic — the destination is continuously held, not a reopen loop.
- `legacy_upstream_busy_destination_replacement_fails_closed_red_witness` —
  RED witness. The same held-open scenario driven against the upstream
  reference payload (compiled only inside the fixture via
  `compile_reference`): must report exactly `Conflict("Report.TXT")`, exit 0,
  remove only its staged temp, leave the old destination and the held
  reader's bytes intact. This is the regression that fails under the
  original payload and passes under the successor, with exact
  implementation bindings on both sides.
- `csharp_busy_destination_without_delete_share_fails_closed` — control.
  Holder with `share_mode(READ|WRITE)` (no delete sharing): even the
  successor must fail closed (conflict reply, old bytes intact, temp
  cleaned). Restrictive handles are not bypassed.
- `csharp_toctou_keep_and_atomic_replace_preserve_realname` — existing
  test preserved verbatim (keep-mode collision refusal + atomic-replace
  old/new complete-byte reader assertions + RealName). Not weakened.

## Local gates (this Mac, exact final tree; logs/ + exit-codes.txt)

| Gate | Exit | Note |
|---|---|---|
| cargo fmt --check | 0 | |
| cargo check --lib --tests (host) | 0 | |
| cargo clippy --all-targets -D warnings (host) | 0 | |
| cargo test --lib (host) | 0 | 319 passed, 0 failed (was 318 at base: one pin test became two) |
| cargo check --target x86_64-pc-windows-gnu --lib --tests | 0 | compiles the new cfg(windows) tests |
| cargo clippy --target x86_64-pc-windows-gnu --lib --tests -D warnings | 0 | |
| npx tsc --noEmit | 0 | |
| npm run build | 0 | |
| npx vitest run | 0 | 211/211 |
| npx playwright test | 0 | 66/66 |
| pin RED (upstream bytes as agent.cs → active pin) | 101 expected nonzero | hash assertion cfa1c1af… ≠ adefa12c… |
| pin GREEN (successor restored) | 0 | sha re-verified after restore |

Reproduce: `bash docs/browser-m3/receipts/busy-replace/run-gates.sh`.
Disk at run time: 93 GiB free (floor 60). One pre-existing target dir reused
(`src-tauri/target`), `CARGO_INCREMENTAL=0`, no new targets/worktrees/installs.

## Round-end consistency fixes (same turn, doc-only; code-final unchanged)

- `docs/browser-m3/SMOKE.md` (living peer-smoke procedure, not a historical
  receipt): expected/inventory executable updated
  `agent-cfa1c1af6b161718.exe` → `agent-adefa12c41424c98.exe`, with a note
  that legacy exes are retained on peers but capability binds the successor
  name; "unmodified C# receiver" wording corrected to name the reviewed
  rename divergence.
- `src-tauri/Cargo.toml` sha2 dev-dependency comment: no longer claims every
  pinned body is byte-identical (upstream reference is; the active successor
  is sha256-pinned with a reviewed delta).

## OPEN — not claimed

- Native Windows C#/PowerShell runtime of the successor and of the RED
  witness: NOT run on this Mac (no C# runtime claim made). windows-gnu runs
  are compile gates only. Expected Windows CI deltas: +3 native tests in
  `win_agent::tests::windows`, +1 net pure test.
- Exact-head CI on this branch, actual Codie review, publication/PR, and
  the separately bounded final peer smokes remain with the parent.
- The hosted diagnostic (run 45084283852) measured LEGACY statuses
  (closed=0x0, held RWD/RW=0xC0000022) — the Ex/POSIX successor has not
  executed on any Windows machine yet; MS-FSCC semantics + Rust std's
  equivalent fallback are the design basis, CI is the proof.
