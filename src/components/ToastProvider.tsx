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
import { logger } from "../lib/logger";

// Maximum number of toasts shown at once. When a new toast would exceed this,
// the oldest is dropped. Prevents flooding during a multi-file batch failure.
const MAX_VISIBLE = 3;
const DEFAULT_DURATION_MS = 5000;

export type ToastVariant = "error" | "info";

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

export interface SendErrorInfo {
  filename: string;
  error: string;
  direction: "sent" | "received";
}

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
  // [DEBUG-TOAST] confirm provider mounted + viewport renders
  logger.debug("ToastProvider", "rendered, toasts in state:", toasts.length);

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
        logger.debug("ToastProvider", "push:", { id, variant, title, prevCount: prev.length, nextCount: next.length });
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

  const api = useMemo<ToastApi>(
    () => ({
      error: (title, message) => push("error", title, message),
      info: (title, message, opts) => push("info", title, message, opts),
      dismiss,
      update,
    }),
    [push, dismiss, update],
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
