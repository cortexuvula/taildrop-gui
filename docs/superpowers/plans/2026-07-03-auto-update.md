# Auto-Update Feature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Tauri-updater-based auto-update: check GitHub on launch + via a Settings button, surface updates as a persistent morphing toast, download + verify + relaunch in one click.

**Architecture:** `tauri-plugin-updater` + `tauri-plugin-process` (Rust, no custom commands). A pure `useUpdater` hook owns the check/download/install state machine. The component layer (App) wires hook state to the existing toast system, which is extended additively (action buttons, persistent duration, in-place `update`). Release workflow gains signing env vars + `latest.json` generation.

**Tech Stack:** Tauri v2 (Rust), React 19 + TypeScript. New deps: `@tauri-apps/plugin-updater`, `@tauri-apps/plugin-process`, `@tauri-apps/api` (already present), Rust crates `tauri-plugin-updater`, `tauri-plugin-process`.

**Reference spec:** `docs/superpowers/specs/2026-07-03-auto-update-design.md`

**Prerequisite (manual, user-only):** GitHub repo secret `TAURI_SIGNING_PRIVATE_KEY` must be set before the first signed release can be cut. The pubkey is already committed in `tauri.conf.json`. This plan does not block on the secret for code tasks, but the release will fail to sign artifacts without it.

**Test infrastructure note:** No frontend test framework exists (no vitest/jest). The automated gate is `npx tsc --noEmit` + `npm run build`. Backend compilation is `cargo check` in `src-tauri/`. Behavior is verified manually (matrix in Task 10).

---

## File Structure

**Create:**
- `src/hooks/useUpdater.ts` — updater state machine + actions (check/download/install/dismiss).

**Modify:**
- `src/components/ToastProvider.tsx` — add `action`, persistent `durationMs: 0`, and `toast.update()` (additive).
- `src/App.tsx` — call `useUpdater()`; wire state to persistent morphing toast.
- `src/components/Settings.tsx` — footer with version + "Check for updates" button; accept `updater` + `appVersion` props.
- `src/App.css` — toast action button styling + settings footer styling.
- `src-tauri/Cargo.toml` — add `tauri-plugin-updater`, `tauri-plugin-process`.
- `src-tauri/src/lib.rs` — register both plugins.
- `src-tauri/capabilities/default.json` — add `updater:default`, `process:allow-relaunch`, `core:app:default`.
- `src-tauri/tauri.conf.json` — add `plugins.updater`, `bundle.createUpdaterArtifacts`.
- `.github/workflows/release.yml` — add signing env vars + `updaterJsonPreferWorkspace` to all three build steps.

**Untouched:** `useTailscale`, transfer paths, transfer-history rendering.

---

## Task 1: Extend the toast system (action button + persistent + update)

**Files:**
- Modify: `src/components/ToastProvider.tsx`

This task is purely additive. Existing send-failure toasts keep working unchanged.

- [ ] **Step 1: Add the `ToastAction` type and extend `Toast`**

Open `src/components/ToastProvider.tsx`. After the existing `export interface Toast` block, add a new action type and extend the Toast interface. Replace:

```ts
export interface Toast {
  id: string;
  title: string;
  message?: string;
  variant: ToastVariant;
  durationMs: number;
}
```

with:

```ts
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  title: string;
  message?: string;
  variant: ToastVariant;
  durationMs: number;           // 0 = persistent (no auto-dismiss)
  action?: ToastAction;
}

export interface ToastPushOptions {
  durationMs?: number;
  action?: ToastAction;
}

export interface ToastPatch {
  title?: string;
  message?: string;
  action?: ToastAction | undefined;
}
```

- [ ] **Step 2: Extend the `ToastApi` interface**

Replace:

```ts
interface ToastApi {
  /** Show an error toast. Returns the toast id. */
  error: (title: string, message?: string) => string;
  /** Show an info toast (reserved for future use). Returns the toast id. */
  info: (title: string, message?: string) => string;
  /** Manually dismiss a toast by id. */
  dismiss: (id: string) => void;
}
```

with:

```ts
interface ToastApi {
  /** Show an error toast. Returns the toast id. */
  error: (title: string, message?: string) => string;
  /** Show an info toast. Returns the toast id. */
  info: (title: string, message?: string, opts?: ToastPushOptions) => string;
  /** Manually dismiss a toast by id. */
  dismiss: (id: string) => void;
  /** Mutate an existing toast in place. No-op if id is unknown. */
  update: (id: string | null, patch: ToastPatch) => void;
}
```

Note: `error` keeps its 2-arg signature (callers that use it don't pass opts). `info` gains an optional 3rd arg used by the updater.

- [ ] **Step 3: Extend `push` to accept options and skip auto-dismiss when durationMs === 0**

Replace the existing `push` definition:

```ts
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
          // The dropped toast's timer is stale; clear it. The timer map is a
          // ref, safe to mutate outside React's render via queueMicrotask.
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
```

with:

```ts
  const push = useCallback(
    (
      variant: ToastVariant,
      title: string,
      message?: string,
      opts?: ToastPushOptions,
    ): string => {
      const id =
        typeof crypto !== "undefined" && "randomUUID" in crypto
          ? crypto.randomUUID()
          : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const durationMs = opts?.durationMs ?? DEFAULT_DURATION_MS;
      const toast: Toast = {
        id,
        title,
        message,
        variant,
        durationMs,
        action: opts?.action,
      };
      setToasts((prev) => {
        // Cap visible toasts: drop the oldest if we'd exceed MAX_VISIBLE.
        const next = [...prev, toast];
        if (next.length > MAX_VISIBLE) {
          const dropped = next.shift()!;
          // The dropped toast's timer is stale; clear it. The timer map is a
          // ref, safe to mutate outside React's render via queueMicrotask.
          queueMicrotask(() => clearTimer(dropped.id));
        }
        return next;
      });
      // Auto-dismiss timer. durationMs === 0 means persistent (no timer).
      if (durationMs > 0) {
        timersRef.current.set(
          id,
          setTimeout(() => dismiss(id), durationMs),
        );
      }
      return id;
    },
    [clearTimer, dismiss],
  );
```

- [ ] **Step 4: Add the `update` method**

Add this `useCallback` inside `ToastProvider`, right after the `push` definition (before `const api = useMemo...`):

```ts
  const update = useCallback((id: string | null, patch: ToastPatch) => {
    if (!id) return;
    setToasts((prev) =>
      prev.map((t) =>
        t.id === id
          ? {
              ...t,
              ...(patch.title !== undefined ? { title: patch.title } : {}),
              ...(patch.message !== undefined ? { message: patch.message } : {}),
              // action is always overwritten (pass undefined to clear it)
              action: patch.action,
            }
          : t,
      ),
    );
  }, []);
```

- [ ] **Step 5: Wire `info` and `update` into the `api` memo**

Replace:

```ts
  const api = useMemo<ToastApi>(
    () => ({
      error: (title, message) => push("error", title, message),
      info: (title, message) => push("info", title, message),
      dismiss,
    }),
    [push, dismiss],
  );
```

with:

```ts
  const api = useMemo<ToastApi>(
    () => ({
      error: (title, message) => push("error", title, message),
      info: (title, message, opts) => push("info", title, message, opts),
      dismiss,
      update,
    }),
    [push, dismiss, update],
  );
```

- [ ] **Step 6: Render the action button in `ToastCard`**

Replace the `ToastCard` component:

```tsx
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
```

with:

```tsx
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
      {toast.action && (
        <button
          className="toast-action"
          onClick={toast.action.onClick}
        >
          {toast.action.label}
        </button>
      )}
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
```

- [ ] **Step 7: Add the `toast-action` CSS**

In `src/App.css`, inside the existing `/* ===== Toast Notifications ===== */` section, add after the `.toast-close:hover { ... }` block:

```css
.toast-action {
  background: var(--accent);
  color: #fff;
  border: none;
  cursor: pointer;
  font-size: 12px;
  font-weight: 600;
  padding: 5px 10px;
  border-radius: var(--radius);
  flex-shrink: 0;
  align-self: center;
  white-space: nowrap;
}

.toast-action:hover {
  background: var(--accent-hover);
}
```

- [ ] **Step 8: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS (no errors). The existing `error()` and `info(title, message)` calls still typecheck because `info`'s 3rd arg is optional and `error` is unchanged.

- [ ] **Step 9: Commit**

```bash
git add src/components/ToastProvider.tsx src/App.css
git commit -m "feat(toast): add action button, persistent duration, and update()"
```

---

## Task 2: Add Rust updater + process plugins

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/capabilities/default.json`

- [ ] **Step 1: Add the Rust crate dependencies**

In `src-tauri/Cargo.toml`, find the `[dependencies]` section. After the existing `tauri-plugin-notification = "2"` line, add:

```toml
tauri-plugin-updater = "2"
tauri-plugin-process = "2"
```

- [ ] **Step 2: Register the plugins in the builder**

In `src-tauri/src/lib.rs`, find the builder chain (around line 99-105):

```rust
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
```

Add after `.plugin(tauri_plugin_notification::init())`:

```rust
        .plugin(tauri_plugin_updater::init())
        .plugin(tauri_plugin_process::init())
```

- [ ] **Step 3: Add the capabilities (permissions)**

In `src-tauri/capabilities/default.json`, find the `permissions` array. After the existing `"notification:allow-show"` entry, add these three entries:

```json
,
        "updater:default",
        "process:allow-relaunch",
        "core:app:default"
```

The full array should end like:

```json
      "permissions": [
        "core:default",
        "core:event:default",
        "dialog:default",
        "autostart:allow-enable",
        "autostart:allow-disable",
        "autostart:allow-is-enabled",
        "notification:default",
        "notification:allow-is-permission-granted",
        "notification:allow-request-permission",
        "notification:allow-notify",
        "notification:allow-show",
        "updater:default",
        "process:allow-relaunch",
        "core:app:default"
      ]
```

- [ ] **Step 4: Compile the Rust backend**

Run: `cd src-tauri && cargo check 2>&1 | tail -20`
Expected: PASS. (This fetches and compiles the two new crates; first run may take a minute or two.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs src-tauri/capabilities/default.json
git commit -m "feat(backend): register tauri-plugin-updater and tauri-plugin-process"
```

---

## Task 3: Configure the updater in `tauri.conf.json`

**Files:**
- Modify: `src-tauri/tauri.conf.json`

- [ ] **Step 1: Enable updater artifact creation and add the updater plugin config**

In `src-tauri/tauri.conf.json`:

First, in the `bundle` object, add `"createUpdaterArtifacts": true`. Replace:

```json
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": [
```

with:

```json
  "bundle": {
    "active": true,
    "targets": "all",
    "createUpdaterArtifacts": true,
    "icon": [
```

Second, replace the empty `"plugins": {}` at the end of the file with the updater config (the pubkey here is the real generated key — it is public, not secret):

```json
  "plugins": {
    "updater": {
      "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IENCNkZDNTlEOEEwRTY5RQpSV1NlNXFEWVdmeTJETzJDVTRIMkh5ZHBrbDEzMys2ZjlrMFFGM0hERmtpUmhqSXBtT1NWdHR0TQo=",
      "endpoints": [
        "https://github.com/cortexuvula/taildrop-gui/releases/latest/download/latest.json"
      ]
    }
  }
```

- [ ] **Step 2: Validate the config compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`
Expected: PASS (Tauri validates the config at compile time; a malformed `plugins.updater` would error here).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/tauri.conf.json
git commit -m "feat(config): add updater pubkey, endpoint, and createUpdaterArtifacts"
```

---

## Task 4: Create the `useUpdater` hook

**Files:**
- Create: `src/hooks/useUpdater.ts`

- [ ] **Step 1: Add the JS plugin dependencies**

Run: `npm install @tauri-apps/plugin-updater @tauri-apps/plugin-process`
Expected: installs both packages; `package.json` updated.

- [ ] **Step 2: Create `src/hooks/useUpdater.ts`**

```ts
import { useCallback, useEffect, useRef, useState } from "react";
import { check as checkForUpdate, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "error";

export interface UseUpdaterApi {
  status: UpdateStatus;
  version?: string;
  date?: string;
  body?: string;
  progress?: number;
  error?: string;
  check: () => Promise<void>;
  download: () => Promise<void>;
  install: () => Promise<void>;
  dismiss: () => void;
}

export function useUpdater(): UseUpdaterApi {
  const [status, setStatus] = useState<UpdateStatus>("idle");
  const [version, setVersion] = useState<string | undefined>();
  const [date, setDate] = useState<string | undefined>();
  const [body, setBody] = useState<string | undefined>();
  const [progress, setProgress] = useState<number | undefined>();
  const [error, setError] = useState<string | undefined>();

  // Hold the resolved Update object so download() can act on it without a
  // re-check. Ref because it's not render-relevant state.
  const updateRef = useRef<Update | null>(null);
  // Guard against double-invocation (rapid button presses, StrictMode effects).
  const checkingRef = useRef(false);
  const downloadingRef = useRef(false);

  const check = useCallback(async () => {
    if (checkingRef.current) return;
    checkingRef.current = true;
    setStatus("checking");
    setError(undefined);
    try {
      const update = await checkForUpdate();
      if (update) {
        updateRef.current = update;
        setVersion(update.version);
        setDate(update.date);
        setBody(update.body);
        setStatus("available");
      } else {
        updateRef.current = null;
        setVersion(undefined);
        setDate(undefined);
        setBody(undefined);
        setStatus("idle");
      }
    } catch (e) {
      updateRef.current = null;
      setError(String(e));
      setStatus("error");
    } finally {
      checkingRef.current = false;
    }
  }, []);

  const download = useCallback(async () => {
    if (downloadingRef.current) return;
    const update = updateRef.current;
    if (!update) return; // no-op unless an update is available
    downloadingRef.current = true;
    setStatus("downloading");
    setProgress(0);
    setError(undefined);
    try {
      let total = 0;
      let downloaded = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            break;
          case "Progress":
            downloaded += event.data.chunkLength ?? 0;
            if (total > 0) {
              setProgress(Math.min(100, Math.round((downloaded / total) * 100)));
            }
            break;
          case "Finished":
            setProgress(100);
            break;
        }
      });
      // downloadAndInstall has written the new bundle; await relaunch.
      setStatus("ready");
    } catch (e) {
      setError(String(e));
      setStatus("error");
    } finally {
      downloadingRef.current = false;
    }
  }, []);

  const install = useCallback(async () => {
    if (status !== "ready") return; // no-op unless downloaded
    try {
      await relaunch();
    } catch (e) {
      setError(String(e));
      setStatus("error");
    }
  }, [status]);

  const dismiss = useCallback(() => {
    setStatus("idle");
    setVersion(undefined);
    setDate(undefined);
    setBody(undefined);
    setProgress(undefined);
    setError(undefined);
    updateRef.current = null;
  }, []);

  // Auto-check on launch (once per mount). Failures are silent here — the
  // caller decides whether to surface them (auto-check: no; manual: yes).
  useEffect(() => {
    void check();
  }, [check]);

  return {
    status,
    version,
    date,
    body,
    progress,
    error,
    check,
    download,
    install,
    dismiss,
  };
}
```

- [ ] **Step 3: Typecheck**

Run: `npx tsc --noEmit`
Expected: PASS. (If the `event.data` shape names differ in the installed plugin version, the compiler will flag it — adjust the two property names `contentLength`/`chunkLength` to match the plugin's actual types. These are the documented v2 names.)

- [ ] **Step 4: Commit**

```bash
git add src/hooks/useUpdater.ts package.json package-lock.json
git commit -m "feat(hook): add useUpdater state machine for check/download/install"
```

---

## Task 5: Wire `useUpdater` into `App.tsx` (persistent morphing toast)

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Import `useUpdater`, add the hook call, and wire state to a persistent toast**

In `src/App.tsx`, add the import (after the existing `useTailscale` import):

```ts
import { useUpdater } from "./hooks/useUpdater";
```

Add `useRef` to the React import (the current line is `import { useState, useEffect, useCallback } from "react";`):

```ts
import { useState, useEffect, useCallback, useRef } from "react";
```

Inside `function App()`, after the `const toast = useToast();` line and before `const onSendError = ...`, add:

```ts
  const updater = useUpdater();
  const updateToastId = useRef<string | null>(null);
```

- [ ] **Step 2: Add the effect that morphs the update toast**

After the existing `onSendError` `useCallback` (and before `const { ... } = useTailscale(...)`), add this effect:

```ts
  // Drive a single persistent toast through the update lifecycle:
  // available → downloading → ready. Dismissed on idle/error.
  useEffect(() => {
    const id = updateToastId.current;
    if (updater.status === "available") {
      updateToastId.current = toast.info(
        `TailDrop ${updater.version} is available`,
        "Click to download and install the update.",
        {
          durationMs: 0,
          action: { label: "Download & Install", onClick: () => void updater.download() },
        },
      );
    } else if (updater.status === "downloading") {
      toast.update(id, {
        title: "Downloading update…",
        message: `${updater.progress ?? 0}%`,
        action: undefined,
      });
    } else if (updater.status === "ready") {
      toast.update(id, {
        title: `Update ready — ${updater.version}`,
        message: "Relaunch to finish installing.",
        action: { label: "Relaunch now", onClick: () => void updater.install() },
      });
    } else if (updater.status === "idle" || updater.status === "error") {
      if (id) {
        toast.dismiss(id);
        updateToastId.current = null;
      }
    }
  }, [updater.status, updater.progress, updater.version, toast, updater]);
```

- [ ] **Step 3: Pass `updater` to Settings**

Find the `<Settings ... />` JSX. Add the `updater` prop and an `appVersion` prop (we'll add the version fetch in Task 6; for now pass a placeholder that Task 6 replaces). Replace:

```tsx
      {showSettings && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={updateSettings}
          onClose={() => setShowSettings(false)}
        />
      )}
```

with:

```tsx
      {showSettings && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={updateSettings}
          onClose={() => setShowSettings(false)}
          updater={updater}
        />
      )}
```

- [ ] **Step 4: Typecheck**

Run: `npx tsc --noEmit`
Expected: FAIL — `Settings` doesn't accept `updater` yet. That's expected; Task 6 adds the prop. Do not commit yet; proceed to Task 6 and commit both together.

---

## Task 6: Add version + "Check for updates" to Settings

**Files:**
- Modify: `src/components/Settings.tsx`

- [ ] **Step 1: Extend the Settings props and add version state**

In `src/components/Settings.tsx`, replace the imports and props interface (top of file):

```ts
import { useState, useEffect } from "react";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import type { Peer, AppSettings } from "../types";

interface SettingsProps {
  settings: AppSettings;
  allPeers: Peer[];
  onUpdate: (update: Partial<AppSettings>) => void;
  onClose: () => void;
}
```

with:

```ts
import { useState, useEffect } from "react";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { useToast } from "./ToastProvider";
import type { UseUpdaterApi } from "../hooks/useUpdater";
import type { Peer, AppSettings } from "../types";

interface SettingsProps {
  settings: AppSettings;
  allPeers: Peer[];
  onUpdate: (update: Partial<AppSettings>) => void;
  onClose: () => void;
  updater: UseUpdaterApi;
}
```

- [ ] **Step 2: Read the app version and add the toast hook**

In the `Settings` function, after the existing `const [autoStart, setAutoStart] = useState(false);` line, add:

```ts
  const [appVersion, setAppVersion] = useState("");
  const toast = useToast();

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(""));
  }, []);
```

- [ ] **Step 3: Accept the `updater` prop**

Change the function signature from:

```ts
export function Settings({ settings, allPeers, onUpdate, onClose }: SettingsProps) {
```

to:

```ts
export function Settings({ settings, allPeers, onUpdate, onClose, updater }: SettingsProps) {
```

- [ ] **Step 4: Add the manual-check handler**

After the existing `toggleHidden` function (before the `return (`), add:

```ts
  const handleCheckUpdates = async () => {
    const wasIdle = updater.status === "idle";
    await updater.check();
    // Surface terminal manual-check results via transient toasts.
    // "available" is handled by App's effect (persistent toast) — no duplicate.
    if (updater.status === "idle" && !wasIdle) {
      toast.info("You're up to date", "TailDrop is on the latest version.");
    } else if (updater.status === "error") {
      toast.error("Couldn't check for updates", updater.error);
    }
  };
```

- [ ] **Step 5: Add the footer JSX**

Find the closing of the last settings section (the Node Visibility `</div>` at the end, right before the panel's closing `</div>`). Insert this footer **inside** the `.settings-panel`, after the Node Visibility section and before the panel closes:

```tsx
        <div className="settings-section settings-footer">
          <div className="settings-version">
            {appVersion ? `TailDrop v${appVersion}` : "TailDrop"}
          </div>
          <button
            className="btn-secondary"
            onClick={handleCheckUpdates}
            disabled={
              updater.status === "checking" || updater.status === "downloading"
            }
          >
            {updater.status === "checking"
              ? "Checking…"
              : updater.status === "downloading"
                ? "Downloading…"
                : "Check for updates"}
          </button>
        </div>
```

- [ ] **Step 6: Add the footer CSS**

In `src/App.css`, append (after the Toast Notifications section):

```css
/* ===== Settings Footer ===== */
.settings-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  border-top: 1px solid var(--border);
  margin-top: 8px;
  padding-top: 14px;
}

.settings-version {
  color: var(--text-muted);
  font-size: 12px;
}
```

- [ ] **Step 7: Typecheck (Tasks 5 + 6 together)**

Run: `npx tsc --noEmit`
Expected: PASS (the `updater` prop now exists on Settings, satisfying Task 5's wiring).

- [ ] **Step 8: Production build**

Run: `npm run build`
Expected: builds without errors.

- [ ] **Step 9: Commit (Tasks 5 + 6 together)**

```bash
git add src/App.tsx src/components/Settings.tsx src/App.css
git commit -m "feat(update): wire useUpdater to persistent toast + Settings check button"
```

---

## Task 7: Update the release workflow

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Add signing env vars and updaterJsonPreferWorkspace to all three build steps**

There are three `tauri-action` steps: macOS (around lines 103-116), Windows (119-126), Linux (129-136).

For the **macOS** step, replace:

```yaml
      - name: Build and upload (macOS universal)
        if: matrix.os == 'macos-latest'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          args: --target universal-apple-darwin
```

with:

```yaml
      - name: Build and upload (macOS universal)
        if: matrix.os == 'macos-latest'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          updaterJsonPreferWorkspace: true
          args: --target universal-apple-darwin
```

For the **Windows** step, replace:

```yaml
      - name: Build and upload (Windows)
        if: matrix.os == 'windows-latest'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          args: --target x86_64-pc-windows-msvc
```

with:

```yaml
      - name: Build and upload (Windows)
        if: matrix.os == 'windows-latest'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          updaterJsonPreferWorkspace: true
          args: --target x86_64-pc-windows-msvc
```

For the **Linux** step, replace:

```yaml
      - name: Build and upload (Linux)
        if: matrix.os == 'ubuntu-22.04'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          args: --target x86_64-unknown-linux-gnu
```

with:

```yaml
      - name: Build and upload (Linux)
        if: matrix.os == 'ubuntu-22.04'
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          releaseId: ${{ needs.create-release.outputs.release_id }}
          updaterJsonPreferWorkspace: true
          args: --target x86_64-unknown-linux-gnu
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): sign updater artifacts and generate latest.json"
```

---

## Task 8: Final build + manual verification

**Files:** none (verification only)

- [ ] **Step 1: Full typecheck + build**

Run: `npx tsc --noEmit && npm run build`
Expected: both pass.

- [ ] **Step 2: Backend compile (final)**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 3: Remind the user of the manual prerequisite**

Before the next release tag is cut, the user MUST add the GitHub repo secret `TAURI_SIGNING_PRIVATE_KEY` (value = the private key generated during brainstorming) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (empty string). Without these, the release build's signing step fails and no updater artifacts are produced. The pubkey is already committed in `tauri.conf.json`.

- [ ] **Step 4: Manual verification matrix**

Run `npm run tauri dev`. Then verify (some scenarios require a newer GitHub release to exist; the up-to-date and offline cases can be tested immediately):

| Scenario | How | Expected |
|---|---|---|
| Up to date (auto) | Launch with no newer release than installed | No toast; Settings shows current version |
| Offline at launch | Disconnect network, launch | No toast (silent); status error internally |
| Manual check, up to date | Settings → "Check for updates" | Transient toast "You're up to date" |
| Manual check, error | Settings → "Check for updates" while offline | Error toast "Couldn't check for updates" |
| Update available | After cutting a newer signed release, launch | Persistent toast "TailDrop vX.Y.Z is available" with "Download & Install" |
| Download | Click "Download & Install" | Toast morphs to "Downloading… N%" |
| Install | After download, click "Relaunch now" | App quits and reopens on new version |
| Snooze | Click ✕ on update toast | Toast gone; not re-shown until next launch or manual check |
| Settings footer | Open Settings | Shows "TailDrop v<version>" and a "Check for updates" button |

- [ ] **Step 5: No-op unless manual testing surfaced fixups**

If manual testing found issues that were fixed, commit those. Otherwise the feature is complete as of Task 7.

---

## Spec coverage self-review

| Spec section | Implemented by |
|---|---|
| Toast extensions: `action`, persistent `durationMs: 0`, `toast.update` | Task 1 |
| Rust: `tauri-plugin-updater` + `tauri-plugin-process` registered | Task 2 |
| Capabilities: `updater:default`, `process:allow-relaunch`, `core:app:default` | Task 2 |
| `tauri.conf.json`: `plugins.updater` (pubkey + endpoint), `createUpdaterArtifacts` | Task 3 |
| `useUpdater` hook (check/download/install/dismiss + auto-check) | Task 4 |
| App wiring: persistent morphing toast via `toast.update` | Task 5 |
| Settings: version display (`getVersion`) + manual check button | Task 6 |
| Release workflow: signing env vars + `updaterJsonPreferWorkspace` | Task 7 |
| Auto-check failures silent; manual-check surfaces errors | Task 5 (effect) + Task 6 (handler) |
| Per-session snooze (dismiss → idle, no localStorage) | Task 4 (`dismiss`) + Task 5 (effect clears toast) |
| Concurrency guards (no double check/download) | Task 4 (`checkingRef`/`downloadingRef`) |

No placeholders. Type names consistent across tasks (`UseUpdaterApi`, `UpdateStatus`, `ToastAction`, `ToastPushOptions`, `ToastPatch` all defined where first used and reused verbatim). The `event.data.contentLength`/`chunkLength` property names are the documented v2 plugin names; Task 4 Step 3's note flags the fallback if the installed version differs.
