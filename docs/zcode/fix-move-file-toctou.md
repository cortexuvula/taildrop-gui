# Zcode Prompt — Close cross-device TOCTOU in `move_file_into_dir`

## Context

Repo: `~/Development/taildrop-gui` (Tauri 2 + React 19 + Rust). Branch `master`, currently at `e48e7b7`. Focused backend fix in `src-tauri/src/tailscale.rs`.

## Hard facts (verified at file:line — reuse, do not reinvent or guess)

- `move_file_into_dir` is at `src-tauri/src/tailscale.rs:145-193`. It moves `src` into `dir` under `name`, NEVER overwriting an existing file.
- Current flow:
  1. `reserve_unique_file(dir, name)` (tailscale.rs:105-125) creates the destination with `create_new` (O_EXCL) and returns `(placeholder: File, dest: PathBuf)`.
  2. `drop(placeholder)` (tailscale.rs:153) — comment: "Windows cannot replace an open file — close our reservation first."
  3. `std::fs::rename(src, &dest)` (tailscale.rs:154). On `Ok` → return `dest`.
  4. On `Err` where `is_cross_device` (tailscale.rs:156; EXDEV on Unix / ERROR_NOT_SAME_DEVICE on Windows, tailscale.rs:129-137): the **copy fallback** (tailscale.rs:157-181):
     - opens `src` read-only (tailscale.rs:158);
     - re-opens `dest` by pathname with `.write(true).truncate(true)` (tailscale.rs:160-164) — **this is the TOCTOU**: the reservation handle was dropped, and the file is re-opened by pathname and truncated;
     - `io::copy` + `sync_all` (tailscale.rs:165-169);
     - on success: `let _ = std::fs::remove_file(src)` (tailscale.rs:174) — **silently swallows source-removal failure** (file then exists in both staging and destination with no log);
     - on failure: `let _ = std::fs::remove_file(&dest)` (tailscale.rs:178) — silently swallows cleanup failure.
  5. On other `Err`: `let _ = std::fs::remove_file(&dest)` (tailscale.rs:184), return error.
- `timestamp_tag()` (tailscale.rs:751) produces a unique u64 suffix — available for naming a temp file.
- `std::fs::rename` replaces an existing destination atomically on both Unix and Windows (std uses MOVEFILE_REPLACE_EXISTING on Windows).

## The bug

The cross-device copy fallback drops the reservation file handle, then re-opens `dest` by pathname with `.truncate(true)`. Between the drop and the reopen, the path is not exclusively held — a self-TOCTOU window (same-user, but the silent `let _ =` on cleanup failure at tailscale.rs:178 masks it). Additionally, the success path's `let _ = std::fs::remove_file(src)` (tailscale.rs:174) silently swallows a source-removal failure, leaving the file in both staging and destination with no log.

## Design decision — temp-then-rename (preferred direction)

Close the window by copying to a `create_new` temp file **in the destination directory** (same-filesystem rename is atomic), then rename onto the reservation's path:

1. In the cross-device branch, create a temp file in `dir` via `create_new` with a unique name (e.g. `dir/.taildrop-move-<timestamp_tag()>` — use `timestamp_tag()`, tailscale.rs:751). The `create_new` (O_EXCL) handle is exclusively ours.
2. Copy `src` into the temp file through the open handle (no pathname reopen/truncate), then `sync_all`.
3. On success: `std::fs::rename(tmp, &dest)` — atomic same-filesystem rename replaces the empty reservation at `dest` on both Unix and Windows.
4. On any failure: remove the temp file, and remove `dest` (the empty reservation) to keep current dest semantics (current code removes `dest` on copy failure, tailscale.rs:178). Log a `warn!` if either cleanup removal fails.
5. On the success path, replace `let _ = std::fs::remove_file(src)` (tailscale.rs:174) with a logged removal: `warn!` if removing `src` fails (the file then exists in both staging and destination — the "both copies, no evidence" state must not be silent).

Keep the non-cross-device path unchanged (rename is already atomic and safe).

## Non-negotiable constraints

- Never overwrite an existing file; the reservation + exclusive-create semantics are preserved.
- The temp file must be created in `dir` (the destination directory) so the final rename is same-filesystem and atomic.
- Match existing style: `map_err(|e| format!(...))` for errors, `log::warn!`/`log::debug!` for diagnostics, `timestamp_tag()` for unique names.
- No change to callers of `move_file_into_dir` (its signature and return contract are unchanged).

## Deliverable (numbered, each with an acceptance criterion)

1. The cross-device fallback copies to a `create_new` temp file in `dir` and renames it onto `dest`, never re-opening `dest` by pathname with truncate.
   - ACCEPT: the rewritten fallback contains no `.truncate(true)` reopen of `dest`; it copies into an exclusively-created temp file and `rename`s it onto `dest`.
2. On any copy failure, both the temp file and the empty reservation `dest` are removed, with a `warn!` if a removal fails.
   - ACCEPT: failure path removes `tmp` and `dest`; each removal failure is logged.
3. The success path logs a `warn!` if removing `src` fails.
   - ACCEPT: `remove_file(src)` failure on the success path produces a `log::warn!` (no silent `let _ =`).
4. The non-cross-device path is unchanged.
   - ACCEPT: `std::fs::rename(src, &dest)` on the same-device path is untouched.

## Verification gates (run individually; do not chain into one opaque failure)

1. `cd ~/Development/taildrop-gui/src-tauri && cargo test` — full suite green (currently 81 tests; the existing `move_file_into_dir` tests must still pass).
2. `cargo clippy --all-targets -- -D warnings` — clean.
3. `cargo fmt --all -- --check` — clean.
4. `cd ~/Development/taildrop-gui && npm test` — frontend unit suite green (105 tests).
5. `npm run test:e2e` — 43 e2e tests green.

## Report

Report back: the rewritten `move_file_into_dir` (verbatim), which gate(s) you ran and their results, and confirm all four acceptance criteria — especially that the cross-device branch no longer re-opens `dest` by pathname with truncate, and that the source-removal failure is logged.
