# Send-Failure Toast Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an in-app toast system that surfaces every manual transfer failure (drop-yields-nothing, unreadable file, send-to-peer error, accept error) so failures are never silent.

**Architecture:** A self-contained React `ToastProvider` + `useToast()` context wraps the app tree. `DropZone` calls `useToast()` directly for the empty-drop case. `useTailscale` stays pure — it accepts an optional `onSendError` callback, and `App.tsx` wires that to `toast.error()` for send/accept failures. The existing red transfer-history row is kept as the persistent record.

**Tech Stack:** React 19 + TypeScript, Tauri v2. No new dependencies.

**Reference spec:** `docs/superpowers/specs/2026-07-03-send-failure-toast-design.md`

---

## File Structure

**Create:**
- `src/components/ToastProvider.tsx` — context, provider, `useToast()` hook, and `ToastViewport` (renders the stack). Single responsibility: ephemeral toast queue and rendering.

**Modify:**
- `src/App.tsx` — wrap tree in `<ToastProvider>`; read `onSendError` callback wiring into `useTailscale`; define the `onSendError` handler that calls `toast.error()`.
- `src/components/DropZone.tsx` — call `useToast()`; fire toast on empty drop.
- `src/hooks/useTailscale.ts` — accept optional `onSendError` option; invoke it in the send catch block (`:329`) and accept catch block (`:374`).
- `src/App.css` — append toast viewport/card styles using existing CSS variables.

**Untouched:** backend (`src-tauri/`), `src/types/index.ts`, transfer-history rendering.

**Test infrastructure note:** This project has **no frontend test framework** (no vitest/jest, no test script in `package.json`). The only automated gate is the TypeScript compiler (`tsc --noEmit`). The automated verification step in each task is therefore `npx tsc --noEmit` (typecheck) plus `npm run build`. Acceptance of behavior is by manual verification (matrix in the spec, repeated in Task 6). Adding a test framework is out of scope for this feature.

---

## Task 1: Create the `ToastProvider` + `useToast()` + viewport

**Files:**
- Create: `src/components/ToastProvider.tsx`

- [ ] **Step 1: Create `src/components/ToastProvider.tsx`**

```tsx
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

// Maximum number of toasts shown at once. When a new toast would exceed this,
// the oldest is dropped. Prevents flooding during a multi-file batch failure.
const MAX_VISIBLE = 3;
const DEFAULT_DURATION_MS = 5000;

export type ToastVariant = "error" | "info";

export interface Toast {
  id: string;
  title: string;
  message?: string;
  variant: ToastVariant;
  durationMs: number;
}

export interface SendErrorInfo {
  filename: string;
  error: string;
  direction: "sent" | "received";
}

interface ToastApi {
  /** Show an error toast. Returns the toast id. */
  error: (title: string, message?: string) => string;
  /** Show an info toast (reserved for future use). Returns the toast id. */
  info: (title: string, message?: string) => string;
  /** Manually dismiss a toast by id. */
  dismiss: (id: string) => void;
}

const ToastContext = createContext<ToastApi | null>(null);

function ToastCard({
  toast,
  onDismiss,
}: {
  toast: Toast;
  onDismiss: (id: string) => void;
}) {
  return (
    <div className={`toast toast-${toast.variant}`} role="alert">
      <div className="toast-icon">{toast.variant === "error" ? "⚠" : "ℹ"}</div>
      <div className="toast-body">
        <div className="toast-title">{toast.title}</div>
        {toast.message && <div className="toast-message">{toast.message}</div>}
      </div>
      <button
        className="toast-close"
        aria-label="Dismiss"
        onClick={() => onDismiss(toast.id)}
      >
        ✕
      </button>
    </div>
  );
}

function ToastViewport({
  toasts,
  onDismiss,
}: {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}) {
  return (
    <div className="toast-viewport" aria-live="polite">
      {/* Newest first: render reversed so the newest appears on top. */}
      {[...toasts].reverse().map((t) => (
        <ToastCard key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  // Track each toast's auto-dismiss timer so we can clear it on manual dismiss.
  const timersRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const clearTimer = useCallback((id: string) => {
    const timer = timersRef.current.get(id);
    if (timer) {
      clearTimeout(timer);
      timersRef.current.delete(id);
    }
  }, []);

  const dismiss = useCallback(
    (id: string) => {
      clearTimer(id);
      setToasts((prev) => prev.filter((t) => t.id !== id));
    },
    [clearTimer],
  );

  const push = useCallback(
    (variant: ToastVariant, title: string, message?: string): string => {
      const id =
        typeof crypto !== "undefined" && "randomUUID" in crypto
          ? crypto.randomUUID()
          : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const toast: Toast = {
        id,
        title,
        message,
        variant,
        durationMs: DEFAULT_DURATION_MS,
      };
      setToasts((prev) => {
        // Cap visible toasts: drop the oldest if we'd exceed MAX_VISIBLE.
        const next = [...prev, toast];
        if (next.length > MAX_VISIBLE) {
          const dropped = next.shift()!;
          // The dropped toast's timer is stale; clear it. Its id is captured
          // synchronously here so the ref cleanup is correct.
          // (Use queueMicrotask to avoid setState-during-render concerns; the
          // timer map is a ref, safe to mutate outside React's flow.)
          queueMicrotask(() => clearTimer(dropped.id));
        }
        return next;
      });
      // Auto-dismiss timer.
      timersRef.current.set(
        id,
        setTimeout(() => dismiss(id), DEFAULT_DURATION_MS),
      );
      return id;
    },
    [clearTimer, dismiss],
  );

  const api = useMemo<ToastApi>(
    () => ({
      error: (title, message) => push("error", title, message),
      info: (title, message) => push("info", title, message),
      dismiss,
    }),
    [push, dismiss],
  );

  // Clean up all timers on unmount.
  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      timers.forEach((t) => clearTimeout(t));
      timers.clear();
    };
  }, []);

  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastViewport toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error("useToast must be used within a <ToastProvider>");
  }
  return ctx;
}
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS (no errors). If `crypto.randomUUID` type is unavailable in the TS lib config, the `else` branch already provides a fallback string id, so the union type is fine.

- [ ] **Step 3: Commit**

```bash
git add src/components/ToastProvider.tsx
git commit -m "feat(toast): add ToastProvider, useToast, and viewport component"
```

---

## Task 2: Add toast CSS

**Files:**
- Modify: `src/App.css` (append at end)

- [ ] **Step 1: Append toast styles to `src/App.css`**

```css
/* ===== Toast Notifications ===== */
.toast-viewport {
  position: fixed;
  bottom: 16px;
  right: 16px;
  z-index: 1000;
  display: flex;
  flex-direction: column-reverse;
  gap: 8px;
  pointer-events: none;
  /* Allow the viewport itself to be pass-through but each card interactive. */
}

.toast {
  pointer-events: auto;
  display: flex;
  align-items: flex-start;
  gap: 10px;
  min-width: 280px;
  max-width: 380px;
  padding: 12px 14px;
  background: var(--bg-tertiary);
  border: 1px solid var(--border);
  border-left: 3px solid var(--red);
  border-radius: var(--radius);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
  color: var(--text-primary);
  font-size: 13px;
  animation: toast-slide-in 0.18s ease-out;
}

.toast-info {
  border-left-color: var(--accent);
}

.toast-icon {
  font-size: 16px;
  line-height: 1.3;
  flex-shrink: 0;
}

.toast-body {
  flex: 1;
  min-width: 0;
}

.toast-title {
  font-weight: 600;
  margin-bottom: 2px;
  word-break: break-word;
}

.toast-message {
  color: var(--text-secondary);
  font-size: 12px;
  word-break: break-word;
  /* Clamp very long backend error strings to 3 lines. */
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.toast-close {
  background: none;
  border: none;
  color: var(--text-muted);
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  padding: 2px 4px;
  border-radius: 4px;
  flex-shrink: 0;
}

.toast-close:hover {
  color: var(--text-primary);
  background: var(--bg-hover);
}

@keyframes toast-slide-in {
  from {
    opacity: 0;
    transform: translateX(12px);
  }
  to {
    opacity: 1;
    transform: translateX(0);
  }
}
```

- [ ] **Step 2: Commit**

```bash
git add src/App.css
git commit -m "style(toast): add toast viewport and card styles"
```

---

## Task 3: Add `onSendError` option to `useTailscale`

**Files:**
- Modify: `src/hooks/useTailscale.ts`

This task keeps the hook pure: it only *calls* a callback if one is provided. No toast imports.

- [ ] **Step 1: Add the `options` parameter and the send-error callback**

In `src/hooks/useTailscale.ts`, change the hook signature (currently at line 22: `export function useTailscale() {`) to accept an optional options object:

```ts
export interface UseTailscaleOptions {
  /** Invoked when a manual send or accept fails. Pure notification; the hook
   * still writes the red transfer-history row. The component layer wires this
   * to a toast. */
  onSendError?: (info: SendErrorInfoLike) => void;
}

// Minimal shape to avoid importing from ToastProvider (keeps the hook pure).
export interface SendErrorInfoLike {
  filename: string;
  error: string;
  direction: "sent" | "received";
}

export function useTailscale(options?: UseTailscaleOptions) {
```

Then add a ref that always holds the latest callback (avoids stale closures and re-renders), right after the existing refs (after line 54, `incomingFilesRef.current = incomingFiles;`):

```ts
  // Keep the latest onSendError callback in a ref so the send/accept
  // closures don't need it in their dependency arrays.
  const onSendErrorRef = useRef(options?.onSendError);
  onSendErrorRef.current = options?.onSendError;
```

- [ ] **Step 2: Invoke the callback in the send catch block**

Find the send catch block (around line 329). It currently looks like:

```ts
          } catch (e) {
            setTransfers((prev) =>
              prev.map((t) =>
                t.id === record.id
                  ? { ...t, status: "error", error: String(e) }
                  : t
              )
            );
          }
```

Replace it with:

```ts
          } catch (e) {
            const errorStr = String(e);
            setTransfers((prev) =>
              prev.map((t) =>
                t.id === record.id
                  ? { ...t, status: "error", error: errorStr }
                  : t
              )
            );
            onSendErrorRef.current?.({
              filename: record.filename,
              error: errorStr,
              direction: "sent",
            });
          }
```

- [ ] **Step 3: Invoke the callback in the accept catch block**

Find the accept catch block (around line 374). It currently looks like:

```ts
      } catch (e) {
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, status: "error", error: String(e) } : t
          )
        );
      }
```

Replace it with:

```ts
      } catch (e) {
        const errorStr = String(e);
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, status: "error", error: errorStr } : t
          )
        );
        onSendErrorRef.current?.({
          filename: name,
          error: errorStr,
          direction: "received",
        });
      }
```

- [ ] **Step 4: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS. (The hook is still pure — no toast import added.)

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useTailscale.ts
git commit -m "feat(hook): add onSendError callback to useTailscale for send/accept failures"
```

---

## Task 4: Wire `ToastProvider` + `onSendError` into `App.tsx`

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Wrap the tree in `ToastProvider` and wire `onSendError`**

The current `App.tsx` body returns a `<div className="app">…</div>`. We need to (a) import `ToastProvider` and `useToast`, (b) split into an inner component so we can call `useToast()` (it must be inside the provider), and (c) pass `onSendError` into `useTailscale`.

Replace the entire contents of `src/App.tsx` with:

```tsx
import { useState, useEffect, useCallback } from "react";
import { Sidebar } from "./components/Sidebar";
import { DropZone } from "./components/DropZone";
import { TransferHistory } from "./components/TransferHistory";
import { Settings } from "./components/Settings";
import { DebugPanel } from "./components/DebugPanel";
import { ToastProvider, useToast } from "./components/ToastProvider";
import { useTailscale, type SendErrorInfoLike } from "./hooks/useTailscale";
import "./App.css";

function App() {
  const toast = useToast();

  const onSendError = useCallback(
    (info: SendErrorInfoLike) => {
      const title =
        info.direction === "sent"
          ? `Send failed: ${info.filename}`
          : `Couldn't receive ${info.filename}`;
      toast.error(title, info.error);
    },
    [toast],
  );

  const {
    peers,
    visiblePeers,
    incomingFiles,
    transfers,
    settings,
    loading,
    error,
    sendFile,
    acceptFile,
    updateSettings,
  } = useTailscale({ onSendError });

  // Diagnostic: log state changes in dev mode only (visible in Safari Web Inspector)
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

  // Bug #5: store only the ID, derive the peer object from current peers
  // so it stays in sync when peers refresh (online/offline, IP changes, etc.)
  const [selectedPeerId, setSelectedPeerId] = useState<string | null>(null);
  const selectedPeer = selectedPeerId
    ? visiblePeers.find((p) => p.id === selectedPeerId) ?? null
    : null;

  const [showSettings, setShowSettings] = useState(false);
  const [showDebug, setShowDebug] = useState(false);

  return (
    <div className="app">
      <Sidebar
        peers={visiblePeers}
        totalPeerCount={peers.filter((p) => !p.is_self).length}
        selectedPeer={selectedPeer}
        onSelectPeer={(peer) => setSelectedPeerId((prev) => (prev === peer.id ? null : peer.id))}
        incomingCount={incomingFiles.length}
        onShowSettings={() => setShowSettings(true)}
        onShowDebug={() => setShowDebug(true)}
      />

      <div className="main">
        {loading ? (
          <div className="loading-state">
            <div className="spinner" />
            <p>Connecting to Tailscale...</p>
          </div>
        ) : error ? (
          <div className="error-state">
            <div className="error-icon">⚠</div>
            <p>Could not connect to Tailscale</p>
            <p className="error-detail">{error}</p>
            <p className="error-hint">
              Make sure Tailscale is running and you have permission to access
              the local API socket.
            </p>
          </div>
        ) : (
          <DropZone
            selectedPeer={selectedPeer}
            onSendFiles={sendFile}
            peers={visiblePeers}
          />
        )}

        <TransferHistory
          transfers={transfers}
          incomingFiles={incomingFiles}
          onAcceptFile={acceptFile}
        />
      </div>

      {showSettings && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={updateSettings}
          onClose={() => setShowSettings(false)}
        />
      )}

      {showDebug && (
        <DebugPanel
          peers={peers}
          onClose={() => setShowDebug(false)}
        />
      )}
    </div>
  );
}

export default function AppWithToast() {
  return (
    <ToastProvider>
      <App />
    </ToastProvider>
  );
}
```

Note: the default export changes from `App` to `AppWithToast` so the provider wraps everything. `main.tsx` imports the default, so no change there.

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/App.tsx
git commit -m "feat(app): wrap tree in ToastProvider and wire onSendError to toasts"
```

---

## Task 5: Fire a toast on empty drop in `DropZone`

**Files:**
- Modify: `src/components/DropZone.tsx`

- [ ] **Step 1: Import `useToast` and fire on empty drop**

In `src/components/DropZone.tsx`, add the import at the top (after the existing imports, around line 4):

```ts
import { useToast } from "./ToastProvider";
```

Inside the component (right after the existing state declarations, around line 18 — after `const processRef = ...`), add:

```ts
  const toast = useToast();
  // Keep the latest toast in a ref so the event listener (registered once on
  // mount) always calls the current one without re-registering.
  const toastRef = useRef(toast);
  toastRef.current = toast;
```

Then modify the `drop` branch of the drag-drop listener (currently around lines 55-61):

```ts
      } else if (event.payload.type === "drop") {
        setIsDragging(false);
        const paths = event.payload.paths;
        if (paths && paths.length > 0) {
          processRef.current?.(paths);
        } else {
          // Empty drop — no sendable file paths (e.g. file from Recycle Bin,
          // or non-file content dragged in).
          toastRef.current.error(
            "No files to send",
            "Drop files from Finder/Explorer, not the Recycle Bin.",
          );
        }
      }
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS. (`useRef` is already imported on line 1 of DropZone.tsx.)

- [ ] **Step 3: Commit**

```bash
git add src/components/DropZone.tsx
git commit -m "feat(dropzone): show toast when a drop yields no sendable files"
```

---

## Task 6: Build + manual verification

**Files:** none (verification only)

- [ ] **Step 1: Production build**

Run: `npm run build`
Expected: builds without errors (`tsc && vite build`).

- [ ] **Step 2: Run the dev app for manual testing**

Run: `npm run tauri dev`
Expected: app launches. (Tailscale must be running for full transfer tests; the empty-drop case A does not need a peer.)

- [ ] **Step 3: Manual verification matrix**

Perform each scenario and confirm the toast appears (bottom-right), auto-dismisses after ~5s, and a manual ✕ works. The existing red transfer-history row should ALSO appear for B/C/D (toast is additional, not a replacement).

| Case | How to trigger | Expected toast |
|---|---|---|
| A (empty drop) | Drag something that yields no file path (e.g. on Windows, drag from Recycle Bin; or drag a text selection from another app) | title "No files to send", message about Finder/Explorer |
| B (unreadable file) | Drop a file, then quickly delete/move it before the send lands (or drop a path that doesn't exist) | title "Send failed: <name>", message = backend error |
| C (send to offline peer) | Select a known-offline peer (toggle "show offline nodes" in Settings if needed), then drop a real file | title "Send failed: <name>", message = backend error |
| D (accept error) | Force an accept failure: temporarily make the save directory unwritable, or accept while the daemon is unreachable | title "Couldn't receive <name>", message = backend error |
| Cap (queue=3) | Drop 5+ unreadable files at once (e.g. a folder of 5 files then delete them mid-flight, or select 5 files that all fail) | at most 3 toasts visible at once; 5 history rows appear |

- [ ] **Step 4: Confirm no regressions**

- Normal send to an online peer succeeds and shows NO toast (only the success history row).
- Normal accept of an incoming file succeeds and shows NO toast.
- Clicking ✕ on a toast removes it immediately and its timer does not fire later (watch console for any state-after-unmount warnings).
- Toasts do not appear outside the window; they sit at bottom-right of the app window.

- [ ] **Step 5: Final commit (if any fixups were made)**

If manual testing surfaced issues that were fixed, commit those. Otherwise nothing to commit — the feature is complete as of Task 5.

---

## Spec coverage self-review

| Spec section | Implemented by |
|---|---|
| `ToastProvider` + `useToast()` + queue cap of 3 + auto-dismiss + ✕ | Task 1 |
| `ToastViewport` fixed bottom-right, dark-theme CSS vars | Task 1 (component) + Task 2 (CSS) |
| Case A: empty-drop toast in DropZone | Task 5 |
| Cases B/C: send-error via `onSendError` callback | Task 3 (hook) + Task 4 (App wiring) |
| Case D: accept-error via `onSendError` callback | Task 3 (hook) + Task 4 (App wiring) |
| Message mapping (sent vs received titles) | Task 4 (`onSendError` handler) |
| Keep existing red history row (toast is additional) | Tasks 3 & 4 do not remove the existing row |
| Pure `useTailscale` (no toast import) | Task 3 (uses a ref'd callback only) |
| Queue cap prevents batch spam | Task 1 (`MAX_VISIBLE`, oldest dropped) |
| `useToast` outside provider throws | Task 1 (`throw new Error`) |
| Out of scope: auto-accept (E), poll errors, OS notifications | Not implemented (correctly) |

No placeholders. Type names consistent (`SendErrorInfo` in spec → `SendErrorInfoLike` in the hook to keep it decoupled from the toast module; `App.tsx` imports the `Like` type — consistent). All steps have exact code and commands.
