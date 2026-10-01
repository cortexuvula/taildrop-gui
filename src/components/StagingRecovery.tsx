import {
  useEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import type { StagingRecoveryDir } from "../types";
import type { StagingOpErrors, StagingOps } from "../hooks/useReceipts";
import { formatSize } from "../lib/format";
import { useModalWithLabel } from "../hooks/useModal";

interface StagingRecoveryProps {
  /** Remaining recovery groups; the parent removes a group once handled. */
  dirs: StagingRecoveryDir[];
  /**
   * TD-UI-06: per-path in-flight ops owned by useReceipts — NOT local state.
   * Closing this dialog must not destroy it while the backend operation is
   * still running; reopening reads the retained pending state from here.
   */
  busy: StagingOps;
  /** Per-path retained errors from settled ops, owned by useReceipts. */
  errors: StagingOpErrors;
  onRecover: (path: string) => Promise<void>;
  onDiscard: (path: string) => Promise<void>;
  onClearError: (path: string) => void;
  onClose: () => void;
}

/** Last path segment of a backend staging path (handles / and \ separators). */
function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

function groupSize(files: Array<{ size: number }>): number {
  return files.reduce((n, f) => n + f.size, 0);
}

function fileCountLabel(n: number): string {
  return `${n} file${n === 1 ? "" : "s"}`;
}

/** Stable, DOM-safe id suffix derived from the full backend path. */
function idSlug(path: string): string {
  return path.replace(/[^a-zA-Z0-9_-]/g, "_");
}

/**
 * Recovery review dialog. Opened explicitly via the compact notice's Review
 * button — never auto-opened. Close/Done/Escape only close the UI; discarding
 * requires the in-dialog confirmation whose default focus is Cancel.
 */
export function StagingRecovery({
  dirs,
  busy,
  errors,
  onRecover,
  onDiscard,
  onClearError,
  onClose,
}: StagingRecoveryProps) {
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, "staging-recovery-title");

  // Only truly ephemeral dialog UI lives here. In-flight ops and their
  // errors are owned by useReceipts (props above), so closing this dialog
  // dismisses the UI without resetting them (TD-UI-06).
  const [confirmingDiscard, setConfirmingDiscard] = useState<string | null>(null);
  // Group whose "Discard…" trigger should regain focus after its confirmation
  // is dismissed (set before re-render; consumed in an effect once the button
  // has remounted).
  const [focusDiscardPath, setFocusDiscardPath] = useState<string | null>(null);

  const listRef = useRef<HTMLDivElement>(null);
  const doneBtnRef = useRef<HTMLButtonElement>(null);
  const cancelBtnRefs = useRef(new Map<string, HTMLButtonElement>());
  const discardBtnRefs = useRef(new Map<string, HTMLButtonElement>());

  const setCancelBtnRef = (path: string) => (el: HTMLButtonElement | null) => {
    if (el) cancelBtnRefs.current.set(path, el);
    else cancelBtnRefs.current.delete(path);
  };
  const setDiscardBtnRef = (path: string) => (el: HTMLButtonElement | null) => {
    if (el) discardBtnRefs.current.set(path, el);
    else discardBtnRefs.current.delete(path);
  };

  // The confirmation's safe default: focus Cancel when it opens.
  useEffect(() => {
    if (confirmingDiscard) cancelBtnRefs.current.get(confirmingDiscard)?.focus();
  }, [confirmingDiscard]);

  // After dismissing a confirmation, return focus to that group's Discard… trigger.
  useEffect(() => {
    if (!focusDiscardPath) return;
    discardBtnRefs.current.get(focusDiscardPath)?.focus();
    setFocusDiscardPath(null);
  }, [focusDiscardPath]);

  // When a group's row disappears after a successful action, move focus to
  // the next remaining group's Recover button — or Done when none remain.
  const prevPathsRef = useRef(dirs.map((d) => d.path));
  useEffect(() => {
    const prevPaths = prevPathsRef.current;
    prevPathsRef.current = dirs.map((d) => d.path);
    if (dirs.length >= prevPaths.length) return; // nothing was removed
    const active = document.activeElement;
    const list = listRef.current;
    if (active && active.isConnected && list?.contains(active)) return;
    const next = list?.querySelector<HTMLElement>(
      ".staging-group-actions .staging-btn-recover:not([disabled])",
    );
    (next ?? doneBtnRef.current)?.focus();
  }, [dirs]);

  const beginConfirmDiscard = (path: string) => {
    if (busy[path]) return; // never confirm-discards a group with an op in flight
    onClearError(path);
    setConfirmingDiscard(path);
  };

  const cancelConfirmDiscard = (path: string) => {
    setConfirmingDiscard(null);
    setFocusDiscardPath(path);
  };

  // Thin dispatchers: the UI fast-path below keeps the click harmless, but
  // the authoritative synchronous guard lives at the command-dispatch
  // boundary in useReceipts, so even a click that races a re-render can
  // never dispatch a second request for the same path (TD-UI-06). Busy and
  // error state are maintained there and survive this dialog closing.
  const handleRecover = (dir: StagingRecoveryDir) => {
    if (busy[dir.path]) return; // one in-flight action per group
    setConfirmingDiscard((p) => (p === dir.path ? null : p));
    // Success is confirmed by the parent: the group leaves `dirs` and the
    // backend's receipts appear in the transfer history via replay.
    void onRecover(dir.path);
  };

  const handleDiscard = (dir: StagingRecoveryDir) => {
    if (busy[dir.path]) return;
    // The confirmation stays mounted while the discard is in flight (its
    // buttons disable and the confirm shows "Discarding…"); if the dialog is
    // closed mid-flight, the reopened actions block carries the same busy
    // state from props.
    void onDiscard(dir.path);
  };

  // Escape first dismisses an open discard confirmation (never discards);
  // a second Escape falls through to useModal's close-on-Escape.
  const handleKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && confirmingDiscard) {
      e.stopPropagation();
      cancelConfirmDiscard(confirmingDiscard);
    }
  };

  const recoverActionProps = (dir: StagingRecoveryDir): ButtonHTMLAttributes<HTMLButtonElement> => ({
    "aria-label": `Recover ${fileCountLabel(dir.files.length)} from ${baseName(dir.path)}`,
  });

  return (
    <div
      className="staging-overlay"
      ref={overlayRef}
      {...overlayProps}
      onKeyDown={handleKeyDown}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="staging-dialog">
        <div className="staging-dialog-header">
          <span className="staging-dialog-icon" aria-hidden="true">⚠</span>
          <div className="staging-dialog-heading">
            <h2 id="staging-recovery-title" className="staging-dialog-title">
              Recover incomplete files
            </h2>
            <p className="staging-dialog-subtitle">
              {dirs.length === 1
                ? "1 recovery group contains files that may be incomplete."
                : `${dirs.length} recovery groups contain files that may be incomplete.`}
            </p>
          </div>
        </div>

        <div className="staging-dialog-list" ref={listRef}>
          {dirs.length === 0 && (
            <div className="staging-empty-state">
              All recovery groups have been handled.
            </div>
          )}
          {dirs.map((dir) => {
            const busyState = busy[dir.path];
            const error = errors[dir.path];
            const confirming = confirmingDiscard === dir.path;
            const slug = idSlug(dir.path);
            const confirmTextId = `staging-confirm-text-${slug}`;
            return (
              <article key={dir.path} className="staging-group" data-staging-path={dir.path}>
                {/*
                  Details default to open so the full wrapping path and every
                  filename/size are disclosed without interaction (never
                  hover-only); the summary remains a keyboard toggle.
                */}
                <details className="staging-group-details" open>
                  <summary className="staging-group-summary">
                    <span className="staging-chevron" aria-hidden="true">▸</span>
                    <span className="staging-group-name">{baseName(dir.path)}</span>
                    <span className="staging-group-meta">
                      {fileCountLabel(dir.files.length)} · {formatSize(groupSize(dir.files))}
                    </span>
                  </summary>
                  <div className="staging-group-body">
                    <div className="staging-path">{dir.path}</div>
                    <ul className="staging-file-list">
                      {dir.files.map((f) => (
                        <li key={f.name} className="staging-file-item">
                          <span className="staging-file-name">{f.name}</span>
                          <span className="staging-file-size">{formatSize(f.size)}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                </details>

                {error && (
                  <p className="staging-error" role="alert">
                    Couldn’t handle this group: {error}
                  </p>
                )}

                {confirming ? (
                  <div className="staging-confirm" role="group" aria-labelledby={confirmTextId}>
                    <p className="staging-confirm-text" id={confirmTextId}>
                      Discard {fileCountLabel(dir.files.length)} (
                      {formatSize(groupSize(dir.files))}) from{" "}
                      <span className="staging-confirm-name">{baseName(dir.path)}</span>?
                    </p>
                    <p className="staging-confirm-warning">
                      These files will be deleted. They may be the only copies — this
                      cannot be undone.
                    </p>
                    <div className="staging-confirm-actions">
                      <button
                        type="button"
                        className="staging-btn-cancel"
                        ref={setCancelBtnRef(dir.path)}
                        onClick={() => cancelConfirmDiscard(dir.path)}
                        disabled={busyState === "discarding"}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        className="staging-btn-confirm-discard"
                        onClick={() => handleDiscard(dir)}
                        disabled={busyState === "discarding"}
                      >
                        {busyState === "discarding"
                          ? "Discarding…"
                          : `Discard ${fileCountLabel(dir.files.length)}`}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="staging-group-actions">
                    <button
                      type="button"
                      className="staging-btn-recover"
                      onClick={() => handleRecover(dir)}
                      disabled={busyState !== undefined}
                      {...recoverActionProps(dir)}
                    >
                      {busyState === "recovering" ? "Recovering…" : "Recover"}
                    </button>
                    <button
                      type="button"
                      className="staging-btn-discard"
                      ref={setDiscardBtnRef(dir.path)}
                      onClick={() => beginConfirmDiscard(dir.path)}
                      disabled={busyState !== undefined}
                    >
                      {busyState === "discarding" ? "Discarding…" : "Discard…"}
                    </button>
                  </div>
                )}
              </article>
            );
          })}
        </div>

        <div className="staging-dialog-footer">
          <button
            type="button"
            className="staging-btn-done"
            ref={doneBtnRef}
            onClick={onClose}
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
