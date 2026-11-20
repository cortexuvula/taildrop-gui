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
| `trust.rs` | Per-flavor pin stores (`pins.tailscale-ssh.json` / `pins.openssh.json` / `pins.openssh-windows.json`), accept-new → refuse-on-change, atomic 0600 writes in 0700 dir, `.tmp` recovery, corrupt-store fail-closed |
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
7. Non-UTF-8 remote names are unrepresentable over SFTP (M0 fact) — counted
   as skipped server-side; local non-UTF-8 names skipped+counted by the walk.
8. Playwright runs chromium only (existing config); WebKit/WKWebView
   container-query support assumed (Safari 16+) — worth a manual smoke on
   the packaged macOS app before release.
9. `browser_open` on the local side shells to the platform opener; remote
   opens download to the app cache dir 0600 (no exec bits, fresh copy per
   open, `path_within` scope-checked). Tested via mock e2e, not on a real
   peer (no-fleet rule).
10. Fake-server test keys are embedded PEMs in `fake.rs` — generated for
    this fixture, authenticate nothing beyond loopback tests.

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
