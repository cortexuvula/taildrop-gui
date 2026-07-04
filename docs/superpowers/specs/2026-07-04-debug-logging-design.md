# In-App Debug Logging — Design

**Date:** 2026-07-04
**Status:** Approved (pending review)
**Motivator:** Diagnosing cross-platform issues (Linux/macOS/Windows) requires logs from both the Rust backend and the React frontend, but today the backend logs only go to stderr (invisible in a packaged app, and the user can't easily get them to me), and the frontend has 8 ad-hoc temporary `console.*` probes plus no structured logging. The user needs to be able to open the app, reproduce an issue, and copy-paste a complete, self-describing log dump.

## Problem

Audit findings (current state):
- **Backend:** `env_logger::init()` (bare, line 97 of `lib.rs`) writes to **stderr only**. Invisible in a packaged release. 18 `log::*` call sites (`lib.rs`, `tailscale.rs`) — all unreachable to the user.
- **Frontend:** 8 temporary `console.log("[toast] ...")` probes added during toast debugging, plus 2 legit `console.warn` calls. No structured logging util. All invisible to the user in a packaged release (no DevTools).
- **`DebugPanel`** exists (`src/components/DebugPanel.tsx`, 145 lines) — a peer-list modal with a dark `<pre>` and a working `navigator.clipboard` copy-to-clipboard button. Single-purpose (peers only); no logs, no version/env info.
- **No runtime OS/arch detection** anywhere (only compile-time `cfg` in `tailscale.rs`). Version is fetched in `Settings.tsx` via `getVersion()` but not shown in DebugPanel.

## Scope

**In scope:**
- Capture **backend** `log::*` output (debug and above) into a bounded in-memory buffer, with zero changes to existing `log::*` call sites.
- Capture **frontend** logs via a `logger.ts` util that replaces all 8 temp `console.*` probes (and the 2 legit `console.warn`s), buffering them in memory and mirroring to `console.*` in dev.
- Merge both buffers by timestamp into a single chronological view in `DebugPanel`.
- "Copy logs" button that puts a **self-describing** dump (version + OS/arch + capture timestamp + separator + merged logs) on the clipboard, ready to paste.
- Environment line in DebugPanel (version + OS + arch).

**Out of scope:** persistent log files on disk, log upload to a server, log filtering/search UI in DebugPanel (YAGNI — copy-paste is the export mechanism), `tracing` migration, capturing logs from third-party crates below `debug` level.

## Decisions (locked during brainstorming)

| Decision | Choice |
|---|---|
| Capture scope | Frontend + backend logs |
| Surface | Extend the existing DebugPanel modal |
| Backend log level | debug and above (debug, info, warn, error) |
| Backend architecture | Custom `log::Log` sink + IPC pull (coexists with env_logger) |

## Architecture

Three layers, each a clean boundary:

```
┌─ Backend (Rust) ─────────────────────────────────────────┐
│  log::debug!/info!/warn!/error!  (18 existing call sites) │
│         │                                                  │
│  ┌──────▼──────────────────┐    ┌──────────────────────┐ │
│  │ InMemorySink            │    │ env_logger (stderr)  │ │
│  │ (impl log::Log)         │    │ kept for `tauri dev` │ │
│  │ Mutex<VecDeque> cap 500 │    └──────────────────────┘ │
│  └──────┬──────────────────┘                              │
│         │ get_debug_logs() IPC ──────────┐                │
└─────────┼────────────────────────────────┼────────────────┘
          │                                │
┌─ Frontend (TS) ──────────────────────────┼────────────────┐
│  logger.ts util                          ▼                │
│  (replaces 8 temp console.log)        snapshot            │
│  → console.* (dev) + in-memory buffer  merge              │
│         │                                │                │
│  useDebugLogs hook ◄──── polls get_debug_logs when open   │
│         │                                                  │
│  DebugPanel ◄── + Environment section + Logs section       │
└────────────────────────────────────────────────────────────┘
```

**Boundary principle:** each layer has one responsibility. The backend sink captures `log::*` with zero call-site awareness. The frontend `logger.ts` is the single logging API for the frontend (no `console.*` calls remain in source). The hook merges; the component renders. No layer knows about the others' internals.

## Components

### 1. Backend `InMemorySink` + IPC (`src-tauri/src/debug_log.rs` + `lib.rs`)

A custom Rust logger implementing `log::Log` that appends each record into a bounded `Mutex<VecDeque<LogEntry>>` (cap 500, oldest evicted). Registered as the global logger; **forwards to `env_logger` internally** so stderr output is preserved in `tauri dev` (this is how a single global `log::set_logger` coexists with env_logger — the sink wraps it).

```rust
// src-tauri/src/debug_log.rs
pub struct LogEntry {
    pub timestamp_ms: u128,   // Unix epoch millis — merges with frontend Date.now()
    pub level: String,        // "debug" | "info" | "warn" | "error"
    pub target: String,       // module path (e.g. "tailscale", "taildrop_gui")
    pub message: String,
}
```

- **Buffer:** `Mutex<VecDeque<LogEntry>>`, capacity 500, `pop_front` when full.
- **Capture level:** `Level::Debug` and above (`set_max_level(LevelFilter::Debug)`).
- **Multi-logger strategy:** the `InMemorySink` holds an `Option<Box<dyn log::Log>>` (the built env_logger). On `log()`, it appends to its buffer *and* forwards to the inner logger if present. `init()` builds the env_logger via `env_logger::Builder::from_default_env()`, wraps it in the sink, and registers the sink as the global logger. This is the standard solution to `log::set_logger`'s single-logger constraint.
- **Timestamp:** `SystemTime::now().duration_since(UNIX_EPOCH).as_millis()` — same epoch-millis scale as the frontend's `Date.now()`, so the merge sort works.
- **`snapshot()`** clones the buffer into a `Vec<LogEntry>` for the IPC command.

**Two new IPC commands in `lib.rs`** (registered in `invoke_handler`):
```rust
#[tauri::command]
fn get_debug_logs() -> Vec<debug_log::LogEntry> {
    debug_log::snapshot()
}

#[tauri::command]
fn get_env_info() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}
```

`debug_log::init()` replaces the current bare `env_logger::init()` at `lib.rs:97`.

### 2. Frontend `logger.ts` (`src/lib/logger.ts`)

Single logging API for the frontend. Replaces all 8 temporary `console.*` probes and the 2 legit `console.warn`s. Buffers in memory; mirrors to `console.*` in dev only.

```ts
export type LogLevel = "debug" | "info" | "warn" | "error";

export interface LogEntry {
  timestampMs: number;   // Date.now() — merges with backend timestamp_ms
  level: LogLevel;
  source: "frontend" | "backend";
  target: string;        // component/module name (e.g. "DropZone", "useTailscale")
  message: string;
}

export const logger = {
  debug: (target: string, message: string, ...rest: unknown[]) => ...,
  info:  (target: string, message: string, ...rest: unknown[]) => ...,
  warn:  (target: string, message: string, ...rest: unknown[]) => ...,
  error: (target: string, message: string, ...rest: unknown[]) => ...,
};

export function getFrontendLogs(): LogEntry[];
export function clearFrontendLogs(): void;
export function subscribe(listener: () => void): () => void;
```

**Behavior of the internal `push`:**
- Builds a `LogEntry` with `Date.now()` timestamp.
- Pushes to a module-level `LogEntry[]` (cap 500, shift oldest).
- If `import.meta.env.DEV`, mirrors to the matching `console.*` method (preserving the existing dev-mode gating convention from `App.tsx:78`). The console call includes the `target` and structured `...rest` (JSON-stringified) for full diagnostic richness.
- Notifies all subscribers (the `Set<() => void>`).

**Migration of the 8 temp probes + 2 warns** — each becomes a `logger.<level>(target, message, ...)` call:
- `App.tsx:20` (`[toast] App onSendError callback RUN`) → `logger.debug("App", "onSendError callback RUN", info)`
- `DropZone.tsx:26` (`[toast] DropZone: useToast() returned`) → `logger.debug("DropZone", "useToast() returned", ...)`
- `DropZone.tsx:59` (`[toast] drag event`) → `logger.debug("DropZone", "drag event", event.payload.type)`
- `DropZone.tsx:72` (`[toast] empty drop`) → `logger.debug("DropZone", "empty drop → calling toast.error")`
- `ToastProvider.tsx:116` (`[toast] mounted`) → `logger.debug("ToastProvider", "mounted, toasts:", toasts.length)`
- `ToastProvider.tsx:123` (`[toast] post-commit probe`) → `logger.debug("ToastProvider", "post-commit probe", ...)`
- `ToastProvider.tsx:174` (`[toast] push`) → `logger.debug("ToastProvider", "push", { id, variant, title, ... })`
- `useTailscale.ts:357` (`[toast] send catch`) → `logger.debug("useTailscale", "send catch", ...)`
- `useTailscale.ts:98` (`console.warn` download dir) → `logger.warn("useTailscale", "Could not get default download dir", e)`
- `useTailscale.ts:109` (`console.warn` quota) → `logger.warn("useTailscale", "localStorage quota exceeded, pruning")`

Net: zero `console.*` calls remain in `src/`; all go through `logger`.

### 3. `useDebugLogs` hook (`src/hooks/useDebugLogs.ts`)

Pure logic hook. Fetches backend logs via IPC, reads the frontend buffer, merges by timestamp, and live-updates while the panel is open.

```ts
export function useDebugLogs(enabled: boolean): MergedLogEntry[];
```

- `enabled` param: DebugPanel only mounts the hook when open, so polling/subscription are idle when closed (no overhead during normal app use).
- On enable: fetches frontend (`getFrontendLogs()`) + backend (`invoke<BackendLogEntry[]>("get_debug_logs")`, wrapped in `.catch(() => [])` so a missing command never crashes DebugPanel).
- Normalizes backend entries (snake_case `timestamp_ms` → camelCase `timestampMs`; sets `source: "backend"`).
- Merges both arrays, sorts by `timestampMs` ascending (chronological interleaving of frontend + backend — essential for tracing cross-layer flows like "drop → IPC → backend error → frontend catch").
- **Live updates:** subscribes to the frontend logger (instant) AND polls the backend every 1s (backend has no per-line event; 1s is well within "good enough" for a debug viewer).

### 4. DebugPanel extension (`src/components/DebugPanel.tsx`)

The existing peer-list modal gains two new sections. The peer section and Raw JSON section are unchanged.

```tsx
interface DebugPanelProps {
  peers: Peer[];
  onClose: () => void;
}
```
(Props unchanged — `DebugPanel` fetches its own version/env/logs.)

**New sections:**
1. **Environment** (after the header, before the peer table): a single line `TailDrop v{appVersion} | {envInfo}` (e.g. "TailDrop v0.8.0 | macos aarch64"). `appVersion` via `getVersion()`; `envInfo` via `invoke<string>("get_env_info")`.
2. **Logs** (after the Raw JSON section): a label row showing the count and two buttons ("Clear FE" and "📋 Copy logs"), followed by a `<pre className="debug-logs">` rendering the merged logs.

**Logs rendering** — one line per entry:
```
2026-07-04T12:34:56.789Z [B] debug tailscale: Sent photo.jpg to noGNbeZPZb11
2026-07-04T12:34:56.801Z [F] debug useTailscale: send catch: errorStr = ...
```
- `[B]` = backend, `[F]` = frontend (first letter, uppercased).
- ISO timestamp, padded level, target, message.
- The `<pre>` is scrollable (maxHeight ~260px), dark-styled, reusing the existing Raw JSON `<pre>` styling pattern.

**Copy logs button** prepends a **self-describing header** so the pasted dump is immediately useful:
```
TailDrop v0.8.0 | macos aarch64
Captured: 2026-07-04T12:35:10.000Z
============================================================
<merged logs>
```
Uses `navigator.clipboard.writeText` (same pattern as the existing peer-copy button), flips a `copiedLogs` state for 2s of "✓ Copied" feedback.

**Clear FE button** calls `clearFrontendLogs()` from `logger.ts` — useful for resetting the frontend buffer before reproducing an issue (the backend buffer can't be cleared from the UI without an extra command; out of scope, YAGNI).

**New CSS** (`.debug-env`, `.debug-logs`, minor button spacing) appended to `App.css`, using existing CSS variables.

## Testing

**Automated gate:** `cargo check` (backend compiles), `npx tsc --noEmit` (frontend typecheck), `npm run build`. No frontend test framework exists (same constraint as prior features).

**Manual verification matrix:**

| Scenario | How | Expected |
|---|---|---|
| Backend logs captured | Open DebugPanel → Logs section shows entries like `[B] debug tailscale: Sent ...` after a transfer | Backend log::* calls appear in the merged view |
| Frontend logs captured | Trigger a toast-path log (e.g. drop a file) → Logs section shows `[F] debug DropZone: drag event ...` | Frontend logger calls appear |
| Merge order | Trigger an action that hits both layers (e.g. failed send) | Entries interleaved chronologically by timestamp |
| Copy logs | Click "📋 Copy logs" → paste into a text editor | Header (version + OS + capture time) + all logs, ready to share |
| Bounded buffer | Generate >500 log lines (e.g. many transfers) | Buffer caps at 500; oldest evicted; app memory stable |
| Dev console still works | `npm run tauri dev`, reproduce, check terminal stderr | env_logger stderr output preserved (sink forwards to it) |
| Panel-closed overhead | Close DebugPanel, use app normally | No backend polling, no frontend subscription (hook unmounted) |
| Reproduces the toast bug | Open DebugPanel, reproduce the send-error toast failure, copy logs | The dump shows the full chain: drop event → send_file IPC → backend error → frontend catch → whether onSendError fired |

## Files to be created/changed

**Create:**
- `src-tauri/src/debug_log.rs` — `InMemorySink`, `LogEntry`, `init()`, `snapshot()`.
- `src/lib/logger.ts` — frontend logging util + buffer + subscribe.
- `src/hooks/useDebugLogs.ts` — merge + live-update hook.

**Modify:**
- `src-tauri/src/lib.rs` — call `debug_log::init()` instead of `env_logger::init()`; add `get_debug_logs` + `get_env_info` commands to `invoke_handler`.
- `src/components/DebugPanel.tsx` — Environment section + Logs section + Copy/Clear buttons; `useDebugLogs(true)` + `getVersion()` + `get_env_info()`.
- `src/App.tsx`, `src/components/DropZone.tsx`, `src/components/ToastProvider.tsx`, `src/hooks/useTailscale.ts` — migrate the 8 temp `console.*` probes + 2 `console.warn`s to `logger.*` calls.
- `src/App.css` — `.debug-env`, `.debug-logs` styles.

**Untouched:** the transfer paths, the toast infrastructure logic (only its logging calls change), the existing peer-list section of DebugPanel.

## Non-goals

- No log file persistence to disk (in-memory only; resets on app restart).
- No log upload/telemetry.
- No filter/search UI in DebugPanel (the copy-paste export is the workflow; in-app search is YAGNI).
- No `tracing` migration (the custom `log::Log` sink is sufficient and lower-risk).
- No capturing of third-party-crate logs below debug level.
- No backend buffer clear from the UI (only frontend clear; backend clear is YAGNI).
