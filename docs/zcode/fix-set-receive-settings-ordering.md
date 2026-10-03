# Zcode Prompt — Fix `set_receive_settings` ordering (receive-gate correctness)

## Context

Repo: `~/Development/taildrop-gui` (Tauri 2 + React 19 + Rust). Branch `master`, currently at `e48e7b7`. Focused backend fix in `src-tauri/src/lib.rs`.

## Hard facts (verified at file:line — reuse, do not reinvent or guess)

- `set_receive_settings` is a `#[tauri::command]` at `src-tauri/src/lib.rs:211-227`.
- Current body (lib.rs:216-226):
  1. writes `settings.save_dir = save_dir.clone()` and `settings.auto_accept = auto_accept` into the shared `SharedReceiveSettings` (lib.rs:217-221);
  2. sets `RECEIVE_READY.store(true, Release)` (lib.rs:225);
  3. `await`s `validate_save_dir_path(&save_dir)` and returns its result (lib.rs:226).
- `RECEIVE_READY` is a `static AtomicBool` documented **"Set once, never cleared"** (lib.rs:22-29). The background `receive_loop` (lib.rs:369) blocks on it (lib.rs:379-381) before reading `settings.save_dir` (lib.rs:382-388) and draining into `effective_save_dir(&save_dir)` (lib.rs:389).
- `validate_save_dir_path` (lib.rs:158-191) checks absolute path, existing dir, canonicalization, and a writability probe; returns the canonical path on success.
- `effective_save_dir` (lib.rs:139) creates the dir on demand if missing.
- **Deliberate contract to preserve** (doc comment lib.rs:206-209): values are stored even when validation fails so an unusable dir surfaces as a persistent `incoming-files-error` event, not a silent revert. The frontend relies on this (src/hooks/useIncomingFiles.ts:286 comment: "backend emits the persistent error via incoming-files-error").
- The `incoming-files-error` event is normally emitted by the receive loop only after `RECEIVE_FAILURE_THRESHOLD` consecutive fetch failures (lib.rs:417-421). If the loop is gated off, this emit never fires.
- A command can emit events directly via the global `APP_HANDLE` (`static Mutex<Option<tauri::AppHandle>>`, lib.rs:353; set at setup lib.rs:582; usage pattern in `recover_staging_files` at lib.rs:256-260).
- Frontend calls `set_receive_settings` once on hydration (src/hooks/useIncomingFiles.ts:247-252), gated on `hydrated` (TD-03).
- No Rust unit test currently pins `set_receive_settings` ordering (grep for `set_receive_settings|RECEIVE_READY|receive_ready` in `src-tauri` returns no test hits).

## The bug

`RECEIVE_READY` is set to `true` (lib.rs:225) BEFORE `validate_save_dir_path` completes (lib.rs:226). The receive loop can therefore unblock and drain one iteration into a path validation has not yet judged.

## Design decision — gate on validation COMPLETION, not success

**Do NOT** "set the flag only on success." `RECEIVE_READY` is set-once-never-cleared (lib.rs:28), and the invalid-dir error contract is delivered by the loop polling (lib.rs:417-421). If a validation failure left the flag unset, the loop would never poll, the error would never surface, and the user would silently get no receives — worse than the current one-iteration window.

Correct shape:

1. `await validate_save_dir_path(&save_dir)` FIRST — before any state write or flag set.
2. Store `settings.save_dir` and `settings.auto_accept` regardless of the validation result (preserve the store-on-failure contract, lib.rs:206-209).
3. If validation returned `Err`: emit `incoming-files-error` directly via the global `APP_HANDLE` (pattern at lib.rs:256-260) with the validation error message. This is what guarantees the documented persistent-error contract fires immediately — the loop's own emit only fires after `RECEIVE_FAILURE_THRESHOLD` consecutive failures and may never fire if the created dir polls as an empty inbox.
4. Set `RECEIVE_READY.store(true, Release)` unconditionally (set-once-never-cleared; gating on success would deadlock the loop and silently kill receives for users with invalid saved dirs).
5. Return the validation result.

This closes the "drains into a path validation has not yet judged" window (validation always completes before the flag opens) while keeping the loop alive and the error contract intact.

## Non-negotiable constraints

- No change to the frontend (`src/hooks/useIncomingFiles.ts`) — the command's contract (returns `Result<(), String>`, stores on failure) is unchanged.
- No change to `receive_loop`'s gating logic.
- Keep the doc comment on `set_receive_settings` accurate — update it to state the new ordering (validate → store → emit-on-failure → gate) while retaining the store-on-failure rationale.
- Match existing style: `unwrap_or_else(|poisoned| poisoned.into_inner())` for locks, `Ordering::Release`/`Acquire` for the atomic, `app.emit(...)` best-effort with a `log::debug!` on failure (as at lib.rs:419-421).

## Deliverable (numbered, each with an acceptance criterion)

1. `set_receive_settings` validates the save dir BEFORE setting `RECEIVE_READY=true`.
   - ACCEPT: in the rewritten body, `validate_save_dir_path` is awaited before `RECEIVE_READY.store(true, ...)` is reached.
2. On validation failure, `incoming-files-error` is emitted directly via `APP_HANDLE`.
   - ACCEPT: the `Err` branch of the validation result emits `incoming-files-error` with the error message (best-effort, `log::debug!` on emit failure).
3. Values are still stored when validation fails.
   - ACCEPT: on a validation `Err`, `settings.save_dir`/`auto_accept` are still written and the command returns the `Err`.
4. `RECEIVE_READY` is set unconditionally (never left false on a bad dir).
   - ACCEPT: `RECEIVE_READY.store(true, Release)` is reached on both the `Ok` and `Err` validation paths.
5. Doc comment on `set_receive_settings` reflects the new ordering.
   - ACCEPT: comment states validate-then-store-then-emit-on-failure-then-gate, and keeps the store-on-failure rationale.

## Verification gates (run individually; do not chain into one opaque failure)

1. `cd ~/Development/taildrop-gui/src-tauri && cargo test` — full suite green (currently 81 tests).
2. `cargo clippy --all-targets -- -D warnings` — clean.
3. `cargo fmt --all -- --check` — clean.
4. `cd ~/Development/taildrop-gui && npm test` — frontend unit suite green (105 tests; the TD-03 hydration test at `src/hooks/__tests__/useIncomingFiles.td03.test.tsx` must still pass).
5. `npm run test:e2e` — 43 e2e tests green.

## Report

Report back: the rewritten `set_receive_settings` body (verbatim), which gate(s) you ran and their results, and confirm all four acceptance criteria — especially that the flag is set on both paths and the error event fires on the `Err` path.
