# M1 handoff — two-pane file browser over Tailscale SSH (pure SFTP)

Branch: `feature/browser-m1` (worktree, sole writer). Base: `89aeda3`.
Status: **M1 complete — all gates green, nothing committed is broken,
no push/merge/tag performed** (per brief; merge is Frikkie's call).

Spec sources: `report.md` (architecture, §7.1–7.6), M0 spike verdict
(`m0-report.md` — russh GO), owner decisions (`decisions.md`), safety
invariants (§7.4 v1 column — acceptance contract).

## Commits (oldest → newest)

| SHA | Subject |
|---|---|
| 7523452 | feat(browser): M1 backend skeleton — russh SFTP transport, trust pins, copy engine, serial job registry |
| af693a3 | chore(deps): sync package-lock version metadata |
| e126826 | fix(browser): hardlink placement distinguishes unsupported from failed |
| cff5c69 | test(browser): in-process fake SFTP server + 18 loopback transport/placement tests |
| 4044326 | test(browser): local-sink filesystem attack tests (planted stage, swap-before-unlink, EXDEV/non-atomic, case-clash, fidelity) |
| fdc11b8 | feat(browser): two-pane Files view — panes, queue, dialogs with scope-lock, keyboard router |
| e6db4d9 | test(browser): vitest suites for paths, guards, hooks, dialog scope-lock |
| 2c33425 | test(browser): playwright e2e — layout/keyboard/focus-trap/queue at both window sizes |
| 6c0e6d3 | docs(browser): M1 gate receipts + handoff |
| 136a14b | fix(browser): review batch 1 — B2 windows E0412, B4 sshHostKeys format, W1 file-mode mask, W2 pin tmp recovery |
| adbbc6c | fix(browser): B3 — lossy-decoded (U+FFFD) remote names are guesses, never actionable |
| 447dc29 | fix(browser): B5 remote→remote refusal + B6 Windows junction probe |
| 35755e9 | fix(browser): B1 — Windows browser_open never routes paths through cmd.exe |
| 5eea0ed | test(browser): W3 — end-to-end PULL tests through the real LocalSink |
| 6562689 | test(browser): e2e — remote→remote copy refused in the UI with the M2 note (B5) |
| 361eacd | docs(browser): fix-round receipts + corrected handoff (B1-B6, W1-W5; B7 pending owner) |
| 2dd151c | fix(browser): drop russh rsa feature — RUSTSEC-2023-0071 (owner decision; Ed25519/ECDSA retained) |
| (this commit) | fix(browser): B6-R — Windows read_link probe distinguishes plain dirs (generic FAILURE) from junctions (BAD_MESSAGE) |

## Gate receipts

`docs/browser-m1/receipts/` — SUMMARY.md (table) + raw logs: cargo test
209/209, clippy --all-targets -D warnings clean, fmt clean, tsc clean,
vitest 178/178 (117 pre-existing untouched), playwright 53/53 (43
pre-existing untouched), npm run build OK. All exits 0 at HEAD 2c33425.

## Module map — Rust (`src-tauri/src/browser/`)

| File | Role |
|---|---|
| `mod.rs` | Wire types (camelCase serde), 9-kind error taxonomy (plugin bin/tailfiles:221-236 port), ConfigStore (atomic 0600), caps (depth 512, field 64 KiB, number 15 digits), `STAGE_PREFIX` |
| `winnames.rs` | `windows_name_problem` port — MANDATORY client-side (M0: Windows OpenSSH accepted `CON` as a normal file) |
| `guards.rs` | Pure decision cores: guard_key, guard_delete (root/home-ancestor), rel-path, number-field, copy preconditions (one-parent, into-itself, Windows case-insensitive clash), exists_message, new_name |
| `transport.rs` | `SftpTransport`/`RemoteReader`/`RemoteWriter` seam (Send+Sync). Shell-out fallback stays documented-not-implemented (M0 verdict) |
| `trust.rs` | Per-flavor pin stores (`pins.tailscale-ssh.json` / `pins.openssh.json` / `pins.openssh-windows.json`), accept-new → refuse-on-change, atomic 0600 writes in 0700 dir, crash-leftover `.tmp` sweep (>1 h mtime, live writers untouched) + pid-nonce tmp names (W2, fix round), corrupt-store fail-closed |
| `session.rs` | Banner sniff → flavor; raw VERSION extension capture on a dedicated channel (high-level session drops unknown names); auth = none (Tailscale) / key-file / ssh-agent — never password/kbi; auth-banner refusal recorded; tailscale `sshHostKeys` cross-check (fail closed); `RusshTransport` |
| `probe.rs` | Exec-free home via `realpath(".")`; 600 s cache keyed host+account; fresh-probe path for delete guards |
| `listing.rs` | Local scandir + remote readdir; control-char/non-UTF-8/duplicate names skipped+counted exactly once; bounded dir-symlink resolution; `path_within` (used by browser_open cache scope check) |
| `transfer/plan.rs` | `PlanBuilder`: DFS-order stack discipline, all caps enforced, hostile-input tested |
| `transfer/place.rs` | `place_action` matrix + `remote_tier` selection; rename errno mapping (EXDEV/non-atomic fail closed) |
| `transfer/local_side.rs` | Unix handle-bound sink: dir-fds, `O_NOFOLLOW`, dev/ino re-check, exact-size reads, exclusive `mkdirat` 0700 + euid/mode/empty identity check, `renameat2 RENAME_NOREPLACE` / `renameatx_np RENAME_EXCL` behind the injectable `RenameNoReplace` seam, cleanup unlinks ONLY while lstat matches the held fd's dev/ino |
| `transfer/remote_side.rs` | SFTP walker→plan, chunked streaming (4 Hz real-byte progress), exclusive random-tag staging, **hardlink-no-replace tier with explicit collision verification**, unsupported→documented check-then-rename downgrade (e126826), replace/merge under consent, own-tag-only recursive cleanup |
| `transfer/mod.rs` | `CopySink` protocol, `run_copy` (pre-walk total → stream → place → aggregate), cleanup on EVERY exit path, plugin-exact failure aggregation, progress capped at 99 until done |
| `commands.rs` | 13 tauri commands (§7.2 + `browser_set_last`), serial Rust job registry + pump (survives webview suspension), account pinning (`as_user`) on every command, same-host two-account refusal, Windows write refusal by PROBED FLAVOR, local delete = trash-crate only, remote delete with fresh-home fail-closed guards (as-written AND resolved), `browser_open` cache download 0600 with `path_within` scope check |
| `fake.rs` | **cfg(test) only.** In-process fake SFTP server (russh server + russh-sftp Handler over canonicalized temp root): configurable banner/extensions/rename semantics/hardlink modes/write-failure/auth. Embedded test-only ed25519 keys. This is the M2/M3 regression harness — extend, don't replace |

Boundary compliance: `tailscale.rs` gained exactly one additive fn
(`fetch_status_json_raw`, read-only passthrough); `lib.rs` is
registration/wiring only. Existing Taildrop flow untouched.

## Component map — frontend

| File | Role |
|---|---|
| `src/types/browser.ts` | Wire types mirroring Rust serde shapes |
| `src/lib/browser/paths.ts` | FilePane.qml:157-183 port (incl. `/C:/` drive-root rule, `~` expansion) |
| `src/lib/browser/guards.ts` | IPC sanitizers (never throw, drop bad rows), typed-error normalization, client mirrors of the Rust name guards, Windows write-disable heuristic |
| `src/lib/browser/api.ts` | Typed invoke wrappers (all 13 commands) + sanitized `browser-copy-event` subscription |
| `src/hooks/useBrowserHosts.ts` | Peers (10 s poll) + Rust-persisted config; machines list filters non-browsable; per-host account; last-remote |
| `src/hooks/useBrowserPane.ts` | Pane state machine: serial + stale-result guard (FilePane.qml:110-115), `loadedUser` account pin, marks-else-cursor (:318-331), blocked semantics (:64), selectAfterLoad over DISPLAYED rows |
| `src/hooks/useBrowserTransfers.ts` | Events + registry resync on mount/focus/visibility; start/cancel/replace/dismiss |
| `src/components/browser/TwoPaneView.tsx` | Keyboard router (App.qml:435-490), 6 s status line, dialog host with **scope-lock capture + confirm-time revalidation** (App.qml:371-383) |
| `src/components/browser/FilePane.tsx` | Picker + path bar (Ctrl+L) + listbox with container-query column drop (FilePane.qml:67-71) + footer + per-kind error pane (FilePane.qml:732-808) |
| `src/components/browser/MachinePicker.tsx` | Online dots, os, saved user, ssh HINT badge, keyboard menu, Change user… |
| `src/components/browser/TransferQueue.tsx` | Row states queued/running/done/failed(+Replace when kind=exists)/cancelled (App.qml:666-798) |
| `src/components/browser/dialogs.tsx` | NameDialog (stem preselect, inline problems) + ConfirmDialog (y/n) on the shared useModal focus trap |
| `src/App.tsx` | Send/Files switch in the main column; Sidebar peer selection seeds the right pane; DropZone flow untouched |
| `src-tauri/tauri.conf.json` | Window default 1180×740, min 760×460 (Andre's decision: grow the single window) |

## How to run

```bash
npm install                      # deps (node >= 22.13)
npm run tauri dev                # app with hot reload
cd src-tauri && cargo test       # 209 Rust tests (fake server included; no network)
npx vitest run                   # 178 unit/hook/component tests
npx playwright test              # 53 e2e (starts vite on :1420 itself)
npm run build                    # tsc + vite production bundle
```

## Known gaps / assumptions (flagged, deliberate)

1. **Windows peers: browse + pull only.** Server refuses writes by probed
   flavor (`windows_write_refusal`); UI disables write actions by os-string
   hint + shows a note. Full Windows write safety = M3 gate.
2. **check-then-rename TOCTOU ceiling** (documented, §7.4 v1): on servers
   whose vanilla RENAME replaces (Tailscale SSH), a destination appearing
   between the existence check and the rename can be replaced. The
   hardlink-no-replace tier (fleet-proven) avoids this where advertised;
   unsupported≠failed is enforced and tested (e126826).
3. **posix_rename seam intentionally unwired** — trait method + fake support
   exist; Replace uses remove-then-place under explicit consent instead.
4. **Pin management UI** (view/forget pins) not in M1 — `TrustStore::forget`
   exists; host-key change refuses with an explanatory pane. M2.
5. **Taildrop "send to inbox" fallback** for SSH-less peers: phase-2 future
   work per decisions.md — not attempted.
6. **Help overlay (`?` key) not ported** from the plugin (minor; keyboard
   surface documented in TwoPaneView comments).
7. Non-UTF-8 remote names arrive LOSSILY decoded (russh-sftp
   `String::from_utf8_lossy`) as U+FFFD replacements — the true bytes are
   unrecoverable. CORRECTED in the fix round (B3, was previously described
   wrongly as "skipped server-side"): `safe_remote_name` now rejects any
   U+FFFD name at every remote boundary (listing, walker, merge_dirs,
   remove_tree, remote delete) — skipped + counted exactly once, never
   actionable. Local names keep their real bytes (OsStr) and are handled by
   the existing walk rules. A server LITERALLY serving U+FFFD is
   indistinguishable from the lossy case and treated the same (fail-closed).
8. Playwright runs chromium only (existing config); WebKit/WKWebView
   container-query support assumed (Safari 16+) — worth a manual smoke on
   the packaged macOS app before release.
9. `browser_open` never routes a path through a shell interpreter (B1 fix
   round): macOS `open` / Linux `xdg-open` via direct exec-family spawn
   (argv element, not shell text); Windows via tauri-plugin-opener
   (ShellExecuteExW, raw wide strings — the `open` crate's cmd launcher sits
   behind its disabled `insecure` feature, verified in vendored source).
   Remote opens download to the app cache dir 0600 (no exec bits, fresh copy
   per open, `path_within` scope-checked). Tested via mock e2e + source
   tripwire test; Windows RUNTIME behavior not executed on this Mac.
10. Fake-server test keys are embedded PEMs in `fake.rs` — generated for
    this fixture, authenticate nothing beyond loopback tests.
11. **W4 (added in the fix round): the local side fails closed on Windows
    builds.** `local_side.rs` is `cfg(unix)` (handle-bound dir-fd engine);
    every copy to/from "This computer" is refused on a Windows desktop with
    an explicit Unsupported error (transfer/mod.rs, commands.rs rename/delete
    included). Defensible M1 cut — failing closed is correct — but it means
    the Windows desktop app ships browse+pull-from-remote-only in BOTH
    directions' absence: a Windows user cannot copy at all until a Windows
    local sink exists (M2/M3).
12. **W5 (fix round): the delete guard is string-based — firmlink aliases
    pass it.** On macOS, `/System/Volumes/Data/Users/<u>` is the same
    directory as `/Users/<u>` and realpath keeps both spellings distinct, so
    a confirmed PERMANENT remote delete of a home-equivalent path via its
    firmlink alias passes the as-written and parent-resolved guards
    (plugin-parity ceiling, §7.4 row-10 gap). Local deletes are Trash-only,
    so exposure is remote-permanent-delete-only. Mitigation for M2: compare
    dev/ino locally in addition to strings.
    **M2b residual note (Codie M2a S-1, documented per owner instruction —
    no guard change):** the W5 dev/ino mitigation (`home_guard_ids`,
    commands.rs) collects identities for the STRING ancestors of the two
    known home spellings only. An alias of an ANCESTOR-of-home under a
    third spelling — e.g. `/System/Volumes/Data`, which transitively
    contains home via the firmlink — is in neither the string guards nor
    the identity set, so `local_delete` of such an alias passes both arms.
    Exposure is local-trash-only (reversible; macOS refuses to trash the
    data-volume root in practice). Cheap future hardening if ever wanted:
    canonicalize each ancestor before collecting its id.
13. **Windows compile + runtime are CI-gated, not locally provable.** B2
    (E0412) is fixed statically; B1's ShellExecuteW path cannot be executed
    on this Mac. The B6-R probe semantics below are SOURCE-VERIFIED against
    Win32-OpenSSH (`latestw_all`: sftp-server.c process_readlink →
    win32compat/fileio.c fileio_readlink → errno_from_Win32Error →
    errno_to_portable) and match the M0 room-6 junction evidence
    (BadMessage), but the deployed OpenSSH_for_Windows_9.5 build could in
    principle differ. `.github/workflows/ci.yml` windows-latest is the
    compile gate; an on-device Windows smoke — a folder pull from room-6
    containing a junction — is REQUIRED before release.
14. **B7 RESOLVED (owner decision, decisions.md #6): the russh `rsa`
    feature is DROPPED** — RUSTSEC-2023-0071 (Marvin timing side channel)
    is gone from the dependency tree (`cargo audit` receipt:
    cargo-audit.txt; rsa/pkcs1/crypto-primes removed from Cargo.lock, pins
    russh =0.65.2 / russh-sftp =3.0.1 untouched). Consequences: RSA key
    FILES (~/.ssh/id_rsa) can no longer sign in-process — auth detects them
    and returns a typed Unsupported error naming the key and the remedy;
    RSA identities held by ssh-agent still work (signing happens inside the
    agent; russh's hash negotiation is not feature-gated); RSA HOST keys
    were already refused by M0 policy (Ed25519/ECDSA qualified per M0 —
    every fleet host presents Ed25519). Ed25519/ECDSA user-key paths are
    unchanged.

## Fix round (continuation 4) — Codie review responses

All six blockers + W1–W5 of codie-m1-review.md (run 2, binding 6c0e6d3)
are addressed in commits 136a14b…6562689 (table above). Per-blocker:

- **B1** (Windows open = cmd injection): opener plugin / ShellExecuteExW;
  source tripwire + `&`-name tests (35755e9).
- **B2** (Windows E0412): offending `const _: Option<ConnectParams>` line
  deleted (136a14b); CI windows-latest is the compile gate (gap 13).
- **B3** (lossy U+FFFD names actionable): `safe_remote_name` at every
  remote boundary; fake-server injection knob; listing + pull + unit tests;
  module comment and this handoff corrected (adbbc6c).
- **B4** (sshHostKeys cross-check): (algorithm, blob) pair comparison;
  fixture switched to real full-line format; match + 3 mismatch-class tests
  incl. bare-blob and wrong-algorithm regressions (136a14b).
- **B5** (remote→remote unrefused): preflight Unsupported (after the more
  specific same-host-two-account Account error) + UI mirror + unit test +
  e2e (447dc29, 6562689).
- **B6** (Windows junctions followed): read_link probe before descending on
  Windows sources (447dc29). **Superseded by B6-R** (continuation 6): the
  first probe accepted only NotFound as "plain dir", but Win32-OpenSSH
  answers generic FAILURE for plain dirs — the corrected probe descends on
  raw SSH_FX_FAILURE and skips on BAD_MESSAGE/NAME/NO_SUCH_FILE/anything
  else (see the semantics table below).
- **W1**: file modes masked 0o777 at all three application points (136a14b).
- **W2**: stale-tmp sweep + pid-nonce names + regression test; this
  handoff's claim corrected (136a14b).
- **W3**: four end-to-end pull tests (symlink-as-link, size-changed,
  read-error-not-masked-by-close, remote-source cancel) + three new fake
  hostility knobs (5eea0ed). Walker skip counts asserted in the B3/B6 pulls.
- **W4/W5**: documented as gaps 11/12 above.
- **B7**: RESOLVED in continuation 5 (owner decision decisions.md #6) —
  rsa feature dropped, typed Unsupported for RSA key files, audit receipt
  captured; see gap 14.
- Suggestions applied: mkdir_remote no-op block removed, place() double
  lstat collapsed, dead-code seams already carried M2/M3 milestone tags.
- **W6** (flaky pre-existing master test): out of scope, untouched. It
  failed once across this round's ~10 full cargo-test runs and passed on
  immediate rerun — consistent with Codie's master-flake finding.

## Windows READLINK probe semantics (B6-R) — M3 inherits this table

For WINDOWS SOURCES ONLY (POSIX sources never probe — POSIX sftp-server
answers BAD_MESSAGE/EINVAL for plain dirs and lstat flags links reliably),
the walker probes every directory with READLINK before descending and
dispatches on the RAW SSH_FXP_STATUS (carried through
`BrowserError::sftp_status`, because `ErrorKind` merges FAILURE and
BAD_MESSAGE into `other`):

| READLINK answer | Meaning (Win32-OpenSSH source path) | Walker action |
|---|---|---|
| NAME (Ok) | real directory symlink | skip + count (Windows link policy: never followed) |
| BAD_MESSAGE (5) | junction / non-symlink reparse point (IO_REPARSE_TAG_MOUNT_POINT → EINVAL → SSH2_FX_BAD_MESSAGE; M0 room-6 evidence) | skip + count, NEVER descend (outside-tree data / ancestor loops) |
| generic FAILURE (4) | PLAIN directory (FSCTL_GET_REPARSE_POINT → ERROR_NOT_A_REPARSE_POINT 4390, unmapped by errno_from_Win32Error, errno_to_portable default) | **DESCEND** |
| NO_SUCH_FILE (2) | directory vanished since listing (race) | skip + count, tolerated |
| anything else | unknown | skip + count (fail closed) |

Provenance: Codie's R3 walk of openssh-portable `latestw_all`
(contrib/win32/win32compat/fileio.c) + their own POSIX
/usr/libexec/sftp-server probe; NOT yet observed on room-6 (gap 13 smoke).
The fake server answers with these exact semantics per flavor (plain dir:
FAILURE on Windows / BAD_MESSAGE on POSIX; junction knob → BAD_MESSAGE;
`vanish_readlink_for` → NO_SUCH_FILE; `dir_readlinks` counter proves POSIX
never probes). Tests: `windows_pull_descends_plain_dirs_answering_failure`
(the R3 regression), `windows_pull_skips_junctions_never_follows` (incl. a
nested ancestor-pointing junction — no loop),
`windows_pull_tolerates_dir_vanished_before_probe`,
`posix_pull_never_probes_dir_readlink`.

## Windows compile fix (continuation 7) — ported vs gated, semantic differences

CI `cargo test` on windows-latest failed to COMPILE at 07969c1 (run
44859283804, job 136709470314): 27 errors, all Unix-only APIs in the M1
browser modules used without platform gates. Fixed forward on master with
ZERO Unix behavior change — the host suite keeps all 224 existing tests
green and gains 4 platform-neutral ones (228 total).

Production — `session.rs` ssh-agent auth is now platform-split:

- Unix: unchanged, `AgentClient::connect_env()` (SSH_AUTH_SOCK).
- Windows: russh DOES expose a Windows agent API — `AgentClient::
  connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")`, the Windows-OpenSSH
  ssh-agent service pipe (russh 0.65.2, keys/agent/client.rs, cfg(windows),
  no feature gate). We USE it, so Windows gets real agent auth instead of a
  skip. The service is manual-start: when it is not running the pipe is
  absent, `agent_connect()` returns None, and auth falls through to the
  EXISTING typed "no working credential … tried tailnet identity, key files
  … and ssh-agent" error — unchanged text, still honest (an absent agent is
  never reported as an agent success). Key-file and tailnet-identity auth
  paths are untouched on every platform.
- Any other platform: `agent_connect()` returns None (documented skip).

Test fixture — `fake.rs` server side (compiled by `cargo test` everywhere):

- PORTED (behavior preserved): positional IO — Unix keeps
  `FileExt::read_at`/`write_all_at`; Windows uses seek+read/write on the
  fixture's fresh per-call handles (no shared offset exists, so the
  semantics are identical). `attrs_of` mode — Unix keeps the real st_mode;
  Windows synthesizes the OpenSSH-style mode (S_IFDIR/S_IFREG/S_IFLNK type
  bits + readonly→0o444 else 0o644, dirs 0o755), the same synthesis
  Win32-OpenSSH performs from ACLs — WITHOUT the type bits the client's
  `FileAttributes::file_type()` would mark every node unknown. Server-side
  SYMLINK op — Windows uses `symlink_file`/`symlink_dir`; where Developer
  Mode / SeCreateSymbolicLinkPrivilege is absent, creation fails and the
  copy engine's documented "destination can't hold links" skip+count path
  handles it (no test depends on link CREATION on a Windows host). READLINK
  handler — the "exists, not a link" branch now keys on existence
  (`symlink_metadata().is_ok()`) instead of `ErrorKind::InvalidInput`,
  because Windows reports ERROR_NOT_A_REPARSE_POINT (4390) as
  `Uncategorized`; per-flavor answers (FAILURE vs BAD_MESSAGE vs junction
  simulation) are unchanged.
- Windows SEMANTIC DIFFERENCES (documented; no test asserts on them, and
  the copy engine treats both as best-effort like a server that ignores
  SETSTAT): (1) the fixture ignores SETSTAT/open PERMISSION bits on Windows
  — there is no mode-bit equivalent, and setting the readonly flag would
  break staging cleanup because Windows refuses to delete read-only files;
  (2) DIRECTORY mtime application is best-effort via std `FileTimes` on a
  read-only dir handle (FILE mtimes are applied reliably).
- cfg(unix)-GATED (each site carries a one-line reason): every `run_copy`
  test touching `Side::Local` — the local engine is Unix-only BY M1 DESIGN
  (all of local_side.rs is `#[cfg(unix)]`; run_copy fails closed elsewhere
  via `local_unsupported()`), so those tests cannot exercise real behavior
  on Windows — plus the Unix-symlink fixture builder `fidelity_source` and
  the helpers used only by gated tests. NO test was deleted and NO
  assertion was weakened; the two previously-gated symlink tests
  (`windows_pull_skips_junctions_never_follows`,
  `pull_copies_remote_symlink_as_link`) were already `#[cfg(unix)]`.

Windows coverage KEPT for platform-neutral paths — 4 new tests in
`transfer/remote_side.rs` (they run on ALL platforms, Windows included;
every filesystem effect lands inside the fake server's temp root):

- `windows_plan_probe_descends_plain_dirs_skips_reparse_and_vanished` —
  the B6-R probe table at PLAN level: plain dirs (FAILURE) descend,
  junctions (BAD_MESSAGE) and vanished dirs (NO_SUCH_FILE) skip+count.
- `posix_plan_never_probes_dir_readlink` — POSIX never-probes invariant
  via the `dir_readlinks` counter.
- `remote_sink_stages_exclusively_places_and_cleans` — STAGING: exclusive
  stage mkdir, planted-lookalike refusal (Exists), hardlink/rename tier
  placement, mtime through placement, full cleanup.
- `engine_streams_plan_into_remote_sink_roundtrip` — COPY ENGINE:
  build_plan → stream_plan (chunking + exact-size enforcement) → place,
  multi-chunk byte fidelity.

`trust.rs`: `set_mtime_for_test` is now portable std `FileTimes`
(atime+mtime, same semantics as the old libc `utimensat` version), so the
W2 stale-tmp-sweep test runs on every platform.

Local Windows verification limit (unchanged from prior rounds): `cargo
check --target x86_64-pc-windows-msvc` cannot run on this Mac — ring's C
build fails first (no MSVC SDK; receipts/win-crosscheck-attempt.txt). The
Windows COMPILE+TEST gate is GitHub Actions windows-latest; see
receipts/SUMMARY.md for the windows-gnu cross-check status.

### Continuation 8 — C1 fix + Codie S1–S4

**C1 (the one blocker in Codie's review of continuation 7).** Windows CI run
44865268846 failed in `listing_skips_control_char_name_exactly_once`: the
fixture SETUP writes a real `"bad\nname"` file, and Win32 forbids 0x01–0x1F
in filenames (Os code 123 InvalidFilename) — production code was never
reached. Fixed per Codie's option (a): the real-file test is now
`#[cfg(unix)]` (unchanged otherwise — real-file coverage stays on Unix), a
new `FakeConfig::control_char_names` knob injects the name synthetically
into READDIR responses without touching the host filesystem (same pattern
as `non_utf8_names`), and a new ALL-PLATFORM twin
`listing_skips_synthetic_control_char_name_exactly_once` asserts the
skip-and-count rule (skipped exactly once, never returned, never guessed).

**Windows agent-auth scope (S2).** Windows ssh-agent auth = the
Windows-OpenSSH service pipe `\\.\pipe\openssh-ssh-agent` ONLY. Pageant
(PuTTY) is NOT supported in M1 — russh exposes `connect_pageant`, but it is
deliberately not wired; a user with only Pageant running gets the typed
no-credential error listing what was tried (never a silent skip-as-success).
russh's ERROR_PIPE_BUSY retry loop has no limit of its own but is bounded by
the 25 s AUTH_TIMEOUT around the whole auth phase, so a permanently busy
pipe ends in the typed auth-timeout error, not a hang (S3 comment at the
connect site in session.rs).

**Tests that cannot run on Windows hosts (S4).** 17 `#[cfg(unix)]` tests in
fake.rs, each gated with a one-line reason at the site:

- 16 drive `run_copy` through `Side::Local` — the local engine is Unix-only
  by M1 design (local_side.rs is whole-module `#[cfg(unix)]`; callers fail
  closed via `local_unsupported()`), so these cannot exercise real behavior
  on a Windows host:
  `hardlink_lies_end_to_end_copy_uses_documented_fallback`,
  `pull_skips_non_utf8_names_and_counts_them`,
  `copy_local_to_remote_fidelity_and_cleanup`,
  `copy_onto_existing_without_consent_refuses_and_cleans`,
  `copy_replace_consent_replaces_and_cleans`,
  `cancel_midcopy_leaves_no_stage_and_no_destination`,
  `windows_pull_skips_junctions_never_follows`,
  `windows_pull_descends_plain_dirs_answering_failure`,
  `windows_pull_tolerates_dir_vanished_before_probe`,
  `posix_pull_never_probes_dir_readlink`,
  `write_failure_midcopy_reports_primary_error_and_cleans`,
  `pre_cancelled_copy_creates_nothing`, `pull_copies_remote_symlink_as_link`,
  `pull_size_changed_fails_and_cleans`,
  `pull_read_error_not_masked_by_close_error`,
  `cancel_remote_source_leaves_no_stage`.
- 1 writes a real control-char filename in its setup (NTFS/Win32 forbids
  0x01–0x1F in names): `listing_skips_control_char_name_exactly_once` — the
  RULE stays covered on Windows by the knob-based twin above (C1).

Related Unix-only coverage by design (not new): the entire local_side.rs
test module (handle-bound local-engine attack tests) and trust.rs
`pin_file_is_0600` (0600/0700 mode bits are a Unix concept). What still
runs on Windows: all transport/auth/trust/extension/listing tests in
fake.rs, the 4 platform-neutral remote_side tests (probe semantics,
never-probe, staging/placement, stream roundtrip), the C1 synthetic twin,
and every pure-unit module (plan, guards, winnames, probe, session, trust,
place decision tables).

**S1 (hygiene, no behavior change).** `#[cfg_attr(not(unix),
allow(dead_code))]` with per-item reasons on the items that are dead in
Windows builds because their only consumers are Unix-gated: listing.rs
`local_home`; place.rs `ItemKind`/`RenameOutcome`/`PlaceAction`/
`place_action`; tailscale.rs `url_encode`/`parse_content_length`/
`validate_received_length` + the `TempFileForTest` framing-test helpers
(pre-M1 items; attribute/lint touches only). Also fixed the Windows-only
clippy `unnecessary_map_err` in tailscale.rs's Windows `accept_file`
(`map_err` that returned `e` unchanged → behavior-identical `inspect_err`).
Result: `cargo clippy --target x86_64-pc-windows-gnu --lib --tests
-- -D warnings` exits 0.

## What M2/M3 inherit

- `browser/fake.rs`: configurable hostile-server harness (banners,
  extension lies, rename semantics, write failures, auth modes) — the
  regression rig for the Windows agent (M2) and any transport change.
- `transport.rs` seam: a shell-out or agent-backed transport drops in
  without touching the copy engine or commands.
- `RenameNoReplace` seam (local_side): filesystem-behavior injection.
- Trust store + flavor classification: ready for pin-management UI (M2)
  and Windows-flavor pins (M3).
- CopySink protocol: the M2 Windows agent sink implements the same trait
  (dir_begin/file_chunk/place/remove stack discipline).
- Job registry + pump: queue survives webview suspension; UI resyncs via
  `browser_copy_jobs` — multi-job concurrency stays a Rust-side change.
