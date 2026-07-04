# In-App Debug Logging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture frontend + backend logs into a bounded in-memory buffer and surface them in the existing DebugPanel with a "Copy logs" button that produces a self-describing, paste-ready dump.

**Architecture:** A custom Rust `log::Log` sink buffers all `log::*` output (forwarding to `env_logger` for stderr) and exposes it via a new IPC command. A frontend `logger.ts` util replaces all `console.*` calls with a buffered logger. A `useDebugLogs` hook merges both by timestamp while DebugPanel is open. DebugPanel gains Environment + Logs sections with copy/clear buttons.

**Tech Stack:** Rust (`log` crate, custom sink), React 19 + TypeScript, Tauri v2 IPC.

**Reference spec:** `docs/superpowers/specs/2026-07-04-debug-logging-design.md`

**Test infrastructure note:** No frontend test framework. Automated gates are `cargo check` (backend), `npx tsc --noEmit` (frontend typecheck), `npm run build`. Behavior is verified manually (matrix in Task 7).

---

## File Structure

**Create:**
- `src-tauri/src/debug_log.rs` — `InMemorySink` (`impl log::Log`), `LogEntry`, `init()`, `snapshot()`.
- `src/lib/logger.ts` — frontend logging util + buffer + subscribe.
- `src/hooks/useDebugLogs.ts` — merge + live-update hook.

**Modify:**
- `src-tauri/src/lib.rs` — call `debug_log::init()`; add `get_debug_logs` + `get_env_info` commands.
- `src/components/DebugPanel.tsx` — Environment + Logs sections; wire `useDebugLogs`, `getVersion`, `get_env_info`.
- `src/App.tsx`, `src/components/DropZone.tsx`, `src/components/ToastProvider.tsx`, `src/hooks/useTailscale.ts` — migrate 8 temp `console.*` + 2 `console.warn`s to `logger.*`.
- `src/App.css` — `.debug-env`, `.debug-logs` styles.

---

## Task 1: Backend `InMemorySink` + IPC commands

**Files:**
- Create: `src-tauri/src/debug_log.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Create `src-tauri/src/debug_log.rs`**

```rust
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use log::{Level, LevelFilter, Log, Metadata, Record};
use serde::Serialize;

const MAX_ENTRIES: usize = 500;
const CAPTURE_LEVEL: Level = Level::Debug;

#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub timestamp_ms: u128,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct InMemorySink {
    buffer: Mutex<VecDeque<LogEntry>>,
    /// Optional inner logger (env_logger) to forward to, preserving stderr
    /// output in `tauri dev`. None in release builds (env_logger is quiet).
    inner: Mutex<Option<Box<dyn Log>>>,
}

static SINK: InMemorySink = InMemorySink {
    buffer: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)),
    inner: Mutex::new(None),
};

impl Log for InMemorySink {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= CAPTURE_LEVEL
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Forward to the inner logger (env_logger) so dev stderr still works.
        if let Ok(inner) = self.inner.lock() {
            if let Some(logger) = inner.as_ref() {
                logger.log(record);
            }
        }
        // Append to the in-memory buffer.
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let entry = LogEntry {
            timestamp_ms,
            level: record.level().to_string(),
            target: record.target().to_string(),
            message: format!("{}", record.args()),
        };
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= MAX_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(entry);
        }
    }

    fn flush(&self) {
        if let Ok(inner) = self.inner.lock() {
            if let Some(logger) = inner.as_ref() {
                logger.flush();
            }
        }
    }
}

/// Initialize the in-memory sink as the global logger. Builds env_logger from
/// the default environment (RUST_LOG) and wraps it so both sinks receive every
/// record. Replaces the bare `env_logger::init()` call.
pub fn init() {
    // Build env_logger but don't install it as the global logger; instead wrap
    // it in our sink so we can capture + forward.
    let env_logger = env_logger::Builder::from_default_env().build();
    if let Ok(mut inner) = SINK.inner.lock() {
        *inner = Some(Box::new(env_logger));
    }
    // SAFETY: set_logger is single-threaded at startup; called once from run().
    let _ = log::set_logger(&SINK);
    log::set_max_level(CAPTURE_LEVEL.to_level_filter());
}

/// Return a snapshot of the current buffer (oldest-first).
pub fn snapshot() -> Vec<LogEntry> {
    SINK.buffer
        .lock()
        .map(|buf| buf.iter().cloned().collect())
        .unwrap_or_default()
}
```

- [ ] **Step 2: Register the module and init in `src-tauri/src/lib.rs`**

In `src-tauri/src/lib.rs`, add the module declaration at the very top (before `mod tailscale;`):

```rust
mod debug_log;
```

Replace the bare `env_logger::init();` call (line 97) with:

```rust
    debug_log::init();
```

- [ ] **Step 3: Add the two new IPC commands**

In `src-tauri/src/lib.rs`, add these two commands (after the existing `get_default_download_dir` command, before `pub fn run()`):

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

- [ ] **Step 4: Register the new commands in the invoke_handler**

Replace the `generate_handler!` block:

```rust
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
        ])
```

with:

```rust
        .invoke_handler(tauri::generate_handler![
            get_tailscale_status,
            send_file,
            get_incoming_files,
            accept_file,
            get_default_download_dir,
            get_debug_logs,
            get_env_info,
        ])
```

- [ ] **Step 5: Compile the backend**

Run: `cargo check --manifest-path src-tauri/Cargo.toml 2>&1 | tail -10`
Expected: PASS (compiles with no errors). The `env_logger::Builder::from_default_env().build()` returns a logger implementing `Log`; boxing it into `Option<Box<dyn Log>>` type-checks.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/debug_log.rs src-tauri/src/lib.rs
git commit -m "feat(backend): add in-memory log sink and debug IPC commands"
```

---

## Task 2: Frontend `logger.ts` util

**Files:**
- Create: `src/lib/logger.ts`

- [ ] **Step 1: Create `src/lib/logger.ts`**

```ts
export type LogLevel = "debug" | "info" | "warn" | "error";

export interface LogEntry {
  timestampMs: number;
  level: LogLevel;
  source: "frontend" | "backend";
  target: string;
  message: string;
}

const MAX_ENTRIES = 500;
const buffer: LogEntry[] = [];
const listeners = new Set<() => void>();

function safeStringify(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function push(level: LogLevel, target: string, message: string, ...rest: unknown[]) {
  const entry: LogEntry = {
    timestampMs: Date.now(),
    level,
    source: "frontend",
    target,
    message: rest.length
      ? `${message} ${rest.map(safeStringify).join(" ")}`
      : message,
  };
  buffer.push(entry);
  if (buffer.length > MAX_ENTRIES) buffer.shift();

  // Mirror to console in dev only (matches existing import.meta.env.DEV gating).
  if (import.meta.env.DEV) {
    const fn =
      level === "error" ? console.error
      : level === "warn" ? console.warn
      : level === "info" ? console.info
      : console.log;
    fn(`[${target}] ${message}`, ...rest);
  }

  listeners.forEach((l) => l());
}

export const logger = {
  debug: (target: string, message: string, ...rest: unknown[]) => push("debug", target, message, ...rest),
  info: (target: string, message: string, ...rest: unknown[]) => push("info", target, message, ...rest),
  warn: (target: string, message: string, ...rest: unknown[]) => push("warn", target, message, ...rest),
  error: (target: string, message: string, ...rest: unknown[]) => push("error", target, message, ...rest),
};

export function getFrontendLogs(): LogEntry[] {
  return [...buffer];
}

export function clearFrontendLogs(): void {
  buffer.length = 0;
  listeners.forEach((l) => l());
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit 2>&1 | grep -v "npm notice" | head -5`
Expected: PASS (no errors).

- [ ] **Step 3: Commit**

```bash
git add src/lib/logger.ts
git commit -m "feat(logger): add frontend logging util with buffer and subscribe"
```

---

## Task 3: `useDebugLogs` hook

**Files:**
- Create: `src/hooks/useDebugLogs.ts`

- [ ] **Step 1: Create `src/hooks/useDebugLogs.ts`**

```ts
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getFrontendLogs, subscribe, type LogEntry } from "../lib/logger";

// Raw shape returned by the Rust get_debug_logs command.
interface BackendLogEntry {
  timestamp_ms: number;
  level: string;
  target: string;
  message: string;
}

export type MergedLogEntry = LogEntry;

export function useDebugLogs(enabled: boolean): MergedLogEntry[] {
  const [logs, setLogs] = useState<MergedLogEntry[]>([]);

  useEffect(() => {
    if (!enabled) return;

    let cancelled = false;

    const refresh = async () => {
      const [frontend, backend] = await Promise.all([
        Promise.resolve(getFrontendLogs()),
        invoke<BackendLogEntry[]>("get_debug_logs").catch(() => []),
      ]);
      if (cancelled) return;

      const merged: MergedLogEntry[] = [
        ...backend.map((b) => ({
          timestampMs: b.timestamp_ms,
          level: b.level as LogEntry["level"],
          source: "backend" as const,
          target: b.target,
          message: b.message,
        })),
        ...frontend,
      ].sort((a, b) => a.timestampMs - b.timestampMs);

      setLogs(merged);
    };

    void refresh();
    // Re-fetch on every frontend log event (instant frontend updates).
    const unsub = subscribe(() => {
      void refresh();
    });
    // Poll backend every 1s (backend has no per-line event).
    const interval = setInterval(() => {
      void refresh();
    }, 1000);

    return () => {
      cancelled = true;
      unsub();
      clearInterval(interval);
    };
  }, [enabled]);

  return logs;
}
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit 2>&1 | grep -v "npm notice" | head -5`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/hooks/useDebugLogs.ts
git commit -m "feat(hook): add useDebugLogs to merge frontend+backend logs"
```

---

## Task 4: Extend DebugPanel with Environment + Logs sections

**Files:**
- Modify: `src/components/DebugPanel.tsx`
- Modify: `src/App.css`

- [ ] **Step 1: Replace the imports and add state/wiring in `src/components/DebugPanel.tsx`**

Replace lines 1-10 (imports through `const [copied, setCopied] = useState(false);`):

```tsx
import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import type { Peer } from "../types";
import { useDebugLogs } from "../hooks/useDebugLogs";
import { clearFrontendLogs } from "../lib/logger";

interface DebugPanelProps {
  peers: Peer[];
  onClose: () => void;
}

export function DebugPanel({ peers, onClose }: DebugPanelProps) {
  const [copied, setCopied] = useState(false);
  const [copiedLogs, setCopiedLogs] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [envInfo, setEnvInfo] = useState("");
  const logs = useDebugLogs(true);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("?"));
  }, []);

  useEffect(() => {
    invoke<string>("get_env_info").then(setEnvInfo).catch(() => setEnvInfo(""));
  }, []);
```

- [ ] **Step 2: Add the logs text builder and copy/clear handlers**

After the existing `handleCopy` function (before the `return (`), add:

```tsx
  const logsText = logs
    .map(
      (l) =>
        `${new Date(l.timestampMs).toISOString()} [${l.source[0].toUpperCase()}] ${l.level.padEnd(5)} ${l.target}: ${l.message}`,
    )
    .join("\n");

  const handleCopyLogs = () => {
    const header = `TailDrop v${appVersion} | ${envInfo}\nCaptured: ${new Date().toISOString()}\n${"=".repeat(60)}\n`;
    navigator.clipboard.writeText(header + logsText).then(() => {
      setCopiedLogs(true);
      setTimeout(() => setCopiedLogs(false), 2000);
    });
  };

  const handleClearFe = () => {
    clearFrontendLogs();
  };
```

- [ ] **Step 3: Widen the modal and add the Environment + Logs sections**

Replace the panel's `style` (line 51) to widen it:

```tsx
        style={{ maxWidth: 800, width: "90vw" }}
```

After the existing Raw JSON `</div>` section (line 140, before the panel's closing `</div>`), insert:

```tsx

        <div className="settings-section">
          <label className="settings-label">Environment</label>
          <div className="debug-env">
            TailDrop v{appVersion} | {envInfo}
          </div>
        </div>

        <div className="settings-section">
          <label className="settings-label">
            Logs ({logs.length})
            <span style={{ float: "right", display: "flex", gap: 8 }}>
              <button className="btn-secondary" onClick={handleClearFe}>
                Clear FE
              </button>
              <button className="btn-secondary" onClick={handleCopyLogs}>
                {copiedLogs ? "✓ Copied" : "📋 Copy logs"}
              </button>
            </span>
          </label>
          <pre className="debug-logs">{logsText}</pre>
        </div>
```

- [ ] **Step 4: Add the CSS for `.debug-env` and `.debug-logs`**

In `src/App.css`, append at the end:

```css
/* ===== Debug Panel: Env + Logs ===== */
.debug-env {
  color: var(--text-secondary);
  font-size: 12px;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", monospace;
  margin-top: 6px;
}

.debug-logs {
  background: #111;
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 12px;
  margin-top: 6px;
  font-size: 11px;
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  max-height: 260px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-all;
  color: #ccc;
}
```

- [ ] **Step 5: Typecheck + build**

Run: `npx tsc --noEmit 2>&1 | grep -v "npm notice" | head -5 && npm run build 2>&1 | tail -5`
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add src/components/DebugPanel.tsx src/App.css
git commit -m "feat(debug): add Environment and Logs sections to DebugPanel"
```

---

## Task 5: Migrate the 8 temp `console.*` probes + 2 `console.warn`s to `logger.*`

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/components/DropZone.tsx`
- Modify: `src/components/ToastProvider.tsx`
- Modify: `src/hooks/useTailscale.ts`

This task replaces every temporary/ad-hoc `console.*` call with the new `logger.*` API. After this task, `grep -rn "console\." src/` should return **zero** matches.

- [ ] **Step 1: Migrate `src/App.tsx`**

In `src/App.tsx`, add the import after the existing imports (after the `import "./App.css";` line):

```ts
import { logger } from "./lib/logger";
```

Find the temp log inside `onSendError` (currently):

```ts
      console.log("[toast] App onSendError callback RUN, calling toast.error:", info);
```

Replace with:

```ts
      logger.debug("App", "onSendError callback RUN, calling toast.error:", info);
```

Then find the existing dev-mode state log:

```ts
      console.log("[taildrop] state:", {
```

Replace with:

```ts
      logger.debug("App", "state:", {
```

(Keep the surrounding object literal and the `if (import.meta.env.DEV)` guard — the logger already gates console mirroring on DEV, but keeping the guard avoids buffering this noisy log in production. Actually, remove the `if (import.meta.env.DEV)` wrapper since the logger handles dev-mirroring; the buffer capture is desired in both modes for debug purposes. So replace the whole block.)

Find the dev-mode state effect (currently):

```ts
  useEffect(() => {
    if (import.meta.env.DEV) {
      console.log("[taildrop] state:", {
        loading,
        error,
        totalPeers: peers.length,
        visiblePeers: visiblePeers.length,
        selfNode: peers.find((p) => p.is_self)?.dns_name ?? "none",
        samplePeer: peers.find((p) => !p.is_self),
        hiddenNodes: settings.hiddenNodes.length,
        showOffline: settings.showOfflineNodes,
        showExit: settings.showExitNodes,
      });
    }
  }, [loading, error, peers, visiblePeers, settings]);
```

Replace with:

```ts
  useEffect(() => {
    logger.debug("App", "state:", {
      loading,
      error,
      totalPeers: peers.length,
      visiblePeers: visiblePeers.length,
      selfNode: peers.find((p) => p.is_self)?.dns_name ?? "none",
      samplePeer: peers.find((p) => !p.is_self),
      hiddenNodes: settings.hiddenNodes.length,
      showOffline: settings.showOfflineNodes,
      showExit: settings.showExitNodes,
    });
  }, [loading, error, peers, visiblePeers, settings]);
```

- [ ] **Step 2: Migrate `src/components/DropZone.tsx`**

In `src/components/DropZone.tsx`, add the import after the existing imports:

```ts
import { logger } from "../lib/logger";
```

Find the temp useToast log:

```ts
  console.log("[toast] DropZone: useToast() returned:", typeof toast?.error === "function" ? "valid API" : "INVALID");
```

Replace with:

```ts
  logger.debug("DropZone", "useToast() returned:", typeof toast?.error === "function" ? "valid API" : "INVALID");
```

Find the drag event log:

```ts
      console.log("[toast] DropZone: drag event =", event.payload.type);
```

Replace with:

```ts
      logger.debug("DropZone", "drag event =", event.payload.type);
```

Find the empty drop log:

```ts
          console.log("[toast] DropZone: empty drop → calling toast.error");
```

Replace with:

```ts
          logger.debug("DropZone", "empty drop → calling toast.error");
```

- [ ] **Step 3: Migrate `src/components/ToastProvider.tsx`**

In `src/components/ToastProvider.tsx`, add the import at the top (after the React import block):

```ts
import { logger } from "../lib/logger";
```

Find the mounted log:

```ts
  console.log("[toast] ToastProvider mounted, toasts in state:", toasts.length);
```

Replace with:

```ts
  logger.debug("ToastProvider", "rendered, toasts in state:", toasts.length);
```

Find the post-commit probe (the whole `useEffect`):

```ts
  // [DEBUG-TOAST] decisive DOM probe: runs AFTER React commits to the DOM.
  // If this finds the node, the viewport IS in the DOM (any "can't see it"
  // is a search/visibility issue). If it finds null, React did not commit it.
  useEffect(() => {
    const node = document.querySelector(".toast-viewport");
    console.log("[toast] post-commit DOM probe: viewport node =", node ? "FOUND" : "NULL", "| child toast count =", node ? node.children.length : "n/a");
  });
```

Replace with:

```ts
  // DOM probe: confirms the viewport node committed to the DOM after render.
  useEffect(() => {
    const node = document.querySelector(".toast-viewport");
    logger.debug(
      "ToastProvider",
      "post-commit DOM probe: viewport node =",
      node ? "FOUND" : "NULL",
      "| child toast count =",
      node ? node.children.length : "n/a",
    );
  });
```

Find the push log inside `push`'s `setToasts` updater:

```ts
        // [DEBUG-TOAST] confirm push reached state
        console.log("[toast] push:", { id, variant, title, prevCount: prev.length, nextCount: next.length });
```

Replace with:

```ts
        logger.debug("ToastProvider", "push:", { id, variant, title, prevCount: prev.length, nextCount: next.length });
```

- [ ] **Step 4: Migrate `src/hooks/useTailscale.ts`**

In `src/hooks/useTailscale.ts`, add the import at the top (after the other imports):

```ts
import { logger } from "../lib/logger";
```

Find the temp send-catch log:

```ts
            // [DEBUG-TOAST] link 1: did the send-error catch block run, and is the ref populated?
            console.log("[toast] useTailscale send catch: errorStr =", errorStr, "| onSendErrorRef.current is", typeof onSendErrorRef.current === "function" ? "SET" : "NULL");
```

Replace with:

```ts
            logger.debug("useTailscale", "send catch: errorStr =", errorStr, "| onSendErrorRef.current is", typeof onSendErrorRef.current === "function" ? "SET" : "NULL");
```

Find the two legit `console.warn` calls. The first (download dir):

```ts
        console.warn("[taildrop] Could not get default download dir:", e);
```

Replace with:

```ts
        logger.warn("useTailscale", "Could not get default download dir:", e);
```

The second (quota):

```ts
          console.warn("[taildrop] localStorage quota exceeded, pruning oldest transfers");
```

Replace with:

```ts
          logger.warn("useTailscale", "localStorage quota exceeded, pruning oldest transfers");
```

- [ ] **Step 5: Verify zero `console.*` remain**

Run: `grep -rn "console\." src/`
Expected: **no output** (zero matches). If any remain, migrate them too.

- [ ] **Step 6: Typecheck + build**

Run: `npx tsc --noEmit 2>&1 | grep -v "npm notice" | head -5 && npm run build 2>&1 | tail -5`
Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add src/App.tsx src/components/DropZone.tsx src/components/ToastProvider.tsx src/hooks/useTailscale.ts
git commit -m "refactor(log): migrate all console.* calls to logger util"
```

---

## Task 6: Backend compile (final) + verify env_logger still works

**Files:** none (verification only)

- [ ] **Step 1: Full backend compile**

Run: `cargo check --manifest-path src-tauri/Cargo.toml 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 2: Verify stderr still works in dev (manual)**

Run: `RUST_LOG=debug npm run tauri dev 2>&1 | head -30`
Expected: backend log lines still appear in the terminal stderr (the sink forwards to env_logger). Look for lines like `DEBUG tailscale: ...` or `INFO taildrop_gui: ...`. Confirm they ALSO appear in the DebugPanel Logs section after opening it.

- [ ] **Step 3: No commit needed** (verification only)

---

## Task 7: Manual verification matrix

**Files:** none (verification only)

- [ ] **Step 1: Run the dev app**

Run: `npm run tauri dev`

- [ ] **Step 2: Verify each scenario**

| Scenario | How | Expected |
|---|---|---|
| Backend logs captured | Open DebugPanel (gear → Debug) → scroll to Logs | Entries with `[B]` marker appear (e.g. `[B] debug tailscale: ...`) |
| Frontend logs captured | Trigger any action that logs (e.g. drop a file) → check Logs | Entries with `[F]` marker appear (e.g. `[F] debug DropZone: drag event`) |
| Merge order | Trigger a failed send (drop to offline peer) | `[B]` backend error and `[F]` frontend catch interleave chronologically |
| Environment line | Open DebugPanel | Shows `TailDrop v<version> \| <os> <arch>` |
| Copy logs | Click "📋 Copy logs" → paste into text editor | Header (version + OS + capture time + `====` separator) + all merged logs |
| Copy feedback | After clicking Copy logs | Button shows "✓ Copied" for ~2s |
| Clear FE | Click "Clear FE" | Frontend (`[F]`) entries disappear; backend (`[B]`) entries remain |
| Bounded buffer | Generate >500 lines (many actions) | Buffer caps at 500; app memory stable; oldest entries evicted |
| Panel-closed overhead | Close DebugPanel, use app | No 1s polling (hook unmounts); no frontend subscription active |
| Dev stderr preserved | `RUST_LOG=debug npm run tauri dev` | Backend logs still print to terminal stderr AND appear in DebugPanel |
| The toast bug | With DebugPanel open, reproduce the send-error toast failure, copy logs | The dump shows the full chain: drag event → send_file IPC → backend error → frontend catch → whether onSendError fired |

- [ ] **Step 3: Commit only if fixups were made**

If manual testing surfaced issues that were fixed, commit those. Otherwise the feature is complete as of Task 5.

---

## Spec coverage self-review

| Spec section | Implemented by |
|---|---|
| Backend `InMemorySink` (`impl log::Log`), bounded 500, debug+ | Task 1 |
| Multi-logger: sink forwards to env_logger | Task 1 (inner `Option<Box<dyn Log>>` + forward in `log()`) |
| `LogEntry` shape (timestamp_ms, level, target, message) | Task 1 |
| `get_debug_logs` IPC command | Task 1 |
| `get_env_info` IPC command (OS + arch) | Task 1 |
| Replaces bare `env_logger::init()` with `debug_log::init()` | Task 1 |
| Frontend `logger.ts` (debug/info/warn/error, buffer, subscribe) | Task 2 |
| Migration of 8 temp `console.*` + 2 `console.warn`s | Task 5 |
| `useDebugLogs` hook (merge by timestamp, 1s backend poll, FE subscription) | Task 3 |
| `enabled` param gates polling/subscription | Task 3 (early return if `!enabled`) |
| Backend invoke wrapped in `.catch(() => [])` | Task 3 |
| DebugPanel Environment section (version + OS/arch) | Task 4 |
| DebugPanel Logs section (`<pre>` + Copy + Clear FE) | Task 4 |
| Self-describing copy header (version + OS + capture time + separator) | Task 4 (`handleCopyLogs`) |
| `[B]`/`[F]` source markers in log rendering | Task 4 (`l.source[0].toUpperCase()`) |
| `.debug-env`, `.debug-logs` CSS | Task 4 |
| Out of scope: log files, upload, search UI, tracing migration | Not implemented (correctly) |

No placeholders. Type names consistent across tasks (`LogEntry`, `MergedLogEntry`, `LogLevel`, `BackendLogEntry` all defined where first used). Field names consistent (`timestampMs` frontend / `timestamp_ms` backend, normalized in Task 3). All steps have exact code and commands.
