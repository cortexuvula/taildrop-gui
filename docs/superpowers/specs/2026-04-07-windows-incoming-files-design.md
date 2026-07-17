# Windows Incoming File Detection via Named Pipe

**Date:** 2026-04-07
**Status:** SUPERSEDED — see revision note below

> **Revision (2026-07-13):** This spec described the original design using
> `std::sync::Once`-gated logging and silent `[]` return on failure. The
> shipped implementation (v0.9.3+) intentionally diverged:
> - The `Once` gate was removed; failures are logged on every poll for
>   DebugPanel visibility.
> - A `try_cli_receive_files` CLI auto-receive fallback was added (v0.9.8)
>   for when the named pipe is inaccessible (non-admin), mirroring the macOS
>   fallback pattern.
> - `get_incoming_files` now takes a `save_dir` parameter, threaded through
>   from the frontend, used by the CLI fallback as the download target.
> - Polling is 8s idle / 2s during active transfers (adaptive), not 5s.
>
> This document is preserved as a historical design record.

## Problem

Windows `get_incoming_files()` in `src-tauri/src/tailscale.rs:440-442` is a hardcoded stub returning `[]`. This means:
- Incoming files are never detected on Windows
- Desktop notifications never fire (even when enabled)
- The incoming files panel is always empty

## Approach

Connect to the Tailscale daemon's local API via its Windows named pipe, mirroring the pattern used by macOS's `try_socket_get()`.

### Named Pipe Details

- **Path:** `\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled`
- **Protocol:** Raw HTTP/1.0 over the pipe (same as macOS over Unix socket)
- **Endpoint:** `GET /localapi/v0/files/` — returns JSON array of `{Name, Size}` objects

### Implementation

1. Add `try_pipe_get(path: &str) -> Result<Vec<u8>, String>` to the Windows `platform` module
   - Open named pipe via `std::fs::OpenOptions` (read + write)
   - Send `GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n`
   - Read full response, parse HTTP headers, extract body
   - Return body bytes on 200, error on anything else

2. Update `get_incoming_files()` to:
   - Call `try_pipe_get("/localapi/v0/files/")` inside `spawn_blocking`
   - On success: return the JSON bytes
   - On failure: log once (using `std::sync::Once`), return `[]`

### Graceful Fallback

Identical to macOS behavior — if the pipe is inaccessible (Tailscale not running, permissions, etc.), log a one-time warning to stderr and return `[]`. No crash, no user-visible error.

### Dependencies

No new crate dependencies. Uses only `std::fs::OpenOptions` and `std::io::{Read, Write}`.

## Files Changed

- `src-tauri/src/tailscale.rs` — Windows `platform` mod only

## What Doesn't Change

- Frontend notification logic (already correct)
- Polling intervals (5s incoming, 10s peers)
- Accept flow (`tailscale file get` CLI — stays as-is)
- Linux and macOS implementations
