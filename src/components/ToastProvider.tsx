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
