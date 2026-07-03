# Send-Failure Toast Notifications — Design

**Date:** 2026-07-03
**Status:** Approved (pending review)
**Motivator:** Dropping a file from the Windows Recycle Bin produced a silent failure — no UI reaction, no transfer row, no error. The user had no way to know the drop was rejected. This design adds an in-app toast system so manual transfer failures are never silent.

## Problem

The app has two distinct classes of silent failure in the manual-transfer path:

- **(A) Drop yields nothing sendable** — `DropZone.tsx:58` skips silently when `paths.length === 0`. No transfer record is ever created. The Recycle-Bin case lands here.
- **(B/C/D) Send or accept errors** — `useTailscale.ts` *does* create a red error row in transfer history (`:329-337`, `:374-380`), but the row is passive and easy to miss, especially while the user's attention is on the dropzone.

There is no global toast/banner system in the frontend today. The only feedback channels are: the transfer-history error row (passive), the big "Could not connect to Tailscale" banner (peer-status only), and OS desktop notifications (incoming files only, gated behind a setting).

## Scope

**In scope:** Surface a toast for every *manual* transfer failure:
- (A) Drop yields zero usable file paths.
- (B) Drop yields paths but a file is unreadable / non-existent (backend `send_file` error).
- (C) Send-to-peer error (peer offline/unreachable, daemon rejects, etc.).
- (D) Manual accept-file error.

**Out of scope:** Auto-accept failures (case E), the swallowed incoming-poll errors (`useTailscale.ts:265`), and the OS-desktop-notification channel. These may be added later by routing through the same toast API.

## Decisions (locked during brainstorming)

| Decision | Choice |
|---|---|
| Channel | In-app toast (bottom-right, fixed position) |
| Coverage | A–D (all manual transfer failures) |
| Empty-drop policy | Always notify |
| Architecture | Approach 1 — React context/provider |

## Architecture

A new self-contained **Toast system** as a React context/provider, plus targeted hook usage at the four failure points. No new dependencies.

```
App.tsx
 └─ <ToastProvider>                ← wraps the whole tree
     └─ App()
         ├─ const { toast } = useToast()
         ├─ DropZone (case A)      ← fires toast on empty drop
         ├─ <ToastViewport />      ← renders the toast stack (fixed, bottom-right)
         └─ useTailscale(...)      ← returns error info; App fires toasts for B/C/D
```

**Key boundary:** the toast system is a pure UI concern. It knows nothing about transfers or Tailscale. `useTailscale` stays free of UI dependencies; instead the **component layer** (App/DropZone) is responsible for turning transfer failures into toast calls. This keeps `useTailscale` testable in isolation.

## Components

### 1. `ToastProvider` + `useToast()` (`src/components/ToastProvider.tsx`)

Holds the toast queue in React state. Exposes a `toast` object via context:

```ts
interface ToastInput {
  title: string;
  message?: string;        // detail line, shown smaller/muted
  variant: "error" | "info";  // only "error" used initially; "info" reserved
  durationMs?: number;     // default 5000
}
interface Toast extends ToastInput { id: string; }

interface ToastApi {
  error: (title: string, message?: string) => void;
  info: (title: string, message?: string) => void;   // reserved, unused initially
  dismiss: (id: string) => void;
}
```

Behavior:
- Each toast gets a unique `id` (`crypto.randomUUID()`).
- Auto-dismiss after `durationMs` (default 5000ms). A dismiss timer is cleared on manual ✕ and on unmount.
- **Queue cap of 3:** if a 4th toast is pushed while 3 are visible, the oldest is dropped. This prevents toast spam during a multi-file failed batch send (e.g. 10 dropped files all unreadable → at most 3 toasts cycle through, not 10 stacked).
- The provider is wrapped in `ToastProvider` and rendered once at the app root.

### 2. `ToastViewport` (rendered inside `ToastProvider`)

A fixed-position container (`position: fixed; bottom: 16px; right: 16px; z-index: 1000`) that renders the active toasts stacked vertically, newest on top. Each toast card shows:
- An icon (`⚠` for error).
- `title` (bold) and optional `message` (muted, smaller).
- A `✕` dismiss button.

Styling uses the existing CSS variables (`--bg-tertiary`, `--border`, `--text-primary`, `--text-secondary`, `--red`, `--radius`), appended to `App.css`. Dark-theme consistent. Because `body` has `overflow: hidden`, the viewport must be `position: fixed` (confirmed against `App.css`).

### 3. Toast wiring at the four call sites

**Case A — empty drop (`DropZone.tsx:55-60`):**
```ts
} else if (event.payload.type === "drop") {
  setIsDragging(false);
  const paths = event.payload.paths;
  if (paths && paths.length > 0) {
    processRef.current?.(paths);
  } else {
    // NEW: empty drop — no sendable files
    toast.error(
      "No files to send",
      "Drop files from Finder/Explorer, not the Recycle Bin."
    );
  }
}
```
`DropZone` calls `useToast()` directly (the provider wraps the whole tree, so no prop threading is needed).

**Cases B & C — send errors (`useTailscale.ts:329-337`):**
The hook already catches the error and writes a red transfer row. To fire a toast without coupling the hook to the UI provider, **the hook returns a lightweight signal** and `App.tsx` fires the toast. Concretely: `useTailscale` exposes an `onSendError` option (a callback set by `App.tsx`), invoked with `{ filename, error }` in the existing catch block. `App.tsx` wires it to `toast.error(...)`.

This keeps the hook pure (it just calls a callback if one is provided; no import of toast internals, no context dependency) and makes the failure → toast mapping live in the component layer where it belongs.

**Case D — accept error (`useTailscale.ts:374-380`):**
Same pattern — the existing catch block also invokes `onSendError` (or a parallel `onAcceptError`). One callback covers both B/C and D since the toast shape is identical ("Transfer failed: <filename> — <reason>").

### 4. Message mapping

Toasts show a friendly title + the filename. The raw backend error string goes in the `message` line (muted), so the user sees *what* failed and a hint of *why* without a wall of text. Examples:

| Case | Trigger | Title | Message |
|---|---|---|---|
| A | empty drop | "No files to send" | "Drop files from Finder/Explorer, not the Recycle Bin." |
| B | `Failed to stat/open file ...` | "Couldn't read <filename>" | the backend error string |
| C | `Tailscale API error (4xx)`, `Failed to connect to daemon`, daemon reject | "Send failed: <filename>" | the backend error string |
| D | accept error | "Couldn't receive <filename>" | the backend error string |

No new error-string normalization is needed beyond what `TransferHistory.tsx`'s `shortenError` already does; the toast shows the raw string (truncated visually by CSS if very long). Keeping it raw preserves diagnostic value, which the user explicitly wants for cross-platform troubleshooting.

## Data flow

1. **Case A:** drop event → `DropZone` detects `paths.length === 0` → `toast.error(...)` directly via `useToast()`.
2. **Cases B/C/D:** `invoke("send_file"|"accept_file")` rejects → `useTailscale` catch block writes the error transfer row (unchanged) **and** calls the `onSendError` callback → `App.tsx` calls `toast.error(...)`.

The existing red error row in transfer history is **kept** — the toast is an additional, ephemeral alert; the history row is the persistent record.

## Error handling & edge cases

- **Spurious empty-drop toast for non-file drags** (text, image from webpage): accepted as a trade-off per the "always notify" decision. The message ("Drop files from Finder/Explorer") is correct feedback even for these.
- **Multi-file batch failures:** queue cap of 3 prevents toast flooding. Each failed file still gets its own (capped) toast + its own history row.
- **Provider missing:** if `useToast()` is called outside `<ToastProvider>`, it throws a clear error (`ToastProvider missing`) — fail loud, not silent, since this is a wiring bug.
- **Dismiss timer leaks:** each toast's timer is cleared on dismiss and on viewport unmount.
- **localStorage persistence:** toasts are purely ephemeral — never persisted. (Contrast with transfer records, which are.)

## Testing

- **Unit (ToastProvider):** pushing N toasts caps at 3 (oldest dropped); `dismiss(id)` removes one; auto-dismiss fires after `durationMs`. Render with `@testing-library/react` if a test setup exists; otherwise a small manual test script. *(Check for existing test infra during implementation.)*
- **Manual verification matrix:**
  - Case A: drop from Recycle Bin (or drop a non-file) → toast appears, auto-dismisses in ~5s.
  - Case B: drop a file, delete it before the send lands → toast + red history row.
  - Case C: select an offline peer, send → toast + red history row.
  - Case D: corrupt the incoming-file path / force an accept error → toast + red history row.
  - Cap: drop 5 unreadable files at once → at most 3 toasts cycle through; 5 history rows appear.
  - Dismiss: click ✕ → toast removed immediately; timer does not fire later.

## Files to be created/changed

**New:**
- `src/components/ToastProvider.tsx` — provider, context, `useToast()`, `ToastViewport`.

**Changed:**
- `src/App.tsx` — wrap tree in `<ToastProvider>`; wire `onSendError` callback from `useTailscale` to `toast.error()`.
- `src/components/DropZone.tsx` — call `useToast()`; fire toast on empty drop (`:58` else-branch).
- `src/hooks/useTailscale.ts` — accept an optional `onSendError?: (info: { filename: string; error: string; direction: "sent" | "received" }) => void` option; invoke it in the send catch block (`:329`) and accept catch block (`:374`). No other changes to the hook.
- `src/App.css` — append toast viewport/card styles using existing CSS variables.

**Untouched:** backend (`src-tauri/`), types (no new exported types needed beyond the provider's internal ones), transfer-history rendering.

## Non-goals

- No success/info toasts for now (the `info` variant is reserved but unused).
- No OS-desktop-notification integration for failures (that's a separate, opt-in channel).
- No changes to the swallowed incoming-poll error (`useTailscale.ts:265`) — out of scope; tracked separately.
- No backend changes.
