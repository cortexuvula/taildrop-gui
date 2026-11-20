/**
 * Browser dialogs — NameDialog (mkdir/rename/change-user) and ConfirmDialog
 * (delete/replace). Both use the shared useModal focus trap (Tab cycles,
 * background inert, Escape closes, focus restored on unmount).
 *
 * SCOPE LOCK (blocker-level invariant, App.qml:371-383 port): dialogs are
 * opened with a captured {target, path, user} context and TwoPaneView
 * REVALIDATES it against the pane at confirm time — a pane that moved while
 * the dialog was open makes the action a no-op with a status notice. The
 * revalidation lives in TwoPaneView (the dialog host); these components are
 * deliberately dumb.
 *
 * ConfirmDialog binds y/n keys for destructive confirms (App.qml:837-842).
 * NameDialog keeps the dialog OPEN on an invalid name and shows the problem
 * inline (client mirror of the Rust guard; the server re-validates anyway).
 */
import { useEffect, useId, useRef, useState } from "react";
import { useModalWithLabel } from "../../hooks/useModal";
import { newNameProblem } from "../../lib/browser/guards";

export interface NameDialogProps {
  title: string;
  initial: string;
  confirmLabel: string;
  /** Hide the input (change-user uses it empty-allowed via `allowEmpty`). */
  allowEmpty?: boolean;
  placeholder?: string;
  onConfirm: (value: string) => void;
  onClose: () => void;
}

export function NameDialog({
  title,
  initial,
  confirmLabel,
  allowEmpty = false,
  placeholder,
  onConfirm,
  onClose,
}: NameDialogProps) {
  const headingId = useId();
  const errorId = useId();
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, headingId);
  const [value, setValue] = useState(initial);
  const [problem, setProblem] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Initial focus with rename-style stem preselect (App.qml:348: select up
  // to the last dot so the extension stays).
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    const dot = initial.lastIndexOf(".");
    if (dot > 0) el.setSelectionRange(0, dot);
    else el.select();
  }, [initial]);

  const submit = () => {
    const v = value.trim();
    if (allowEmpty) {
      onConfirm(v);
      return;
    }
    const p = newNameProblem(v);
    if (p) {
      setProblem(p); // keep the dialog open, show why (App.qml badName path)
      return;
    }
    onConfirm(v);
  };

  return (
    <div className="browser-overlay" ref={overlayRef} {...overlayProps} onClick={onClose}>
      <div className="browser-dialog" onClick={(e) => e.stopPropagation()}>
        <h2 id={headingId}>{title}</h2>
        <input
          ref={inputRef}
          type="text"
          value={value}
          placeholder={placeholder}
          onChange={(e) => {
            setValue(e.target.value);
            setProblem(null);
          }}
          onKeyDown={(e) => {
            // Keys inside the dialog never reach the pane router.
            e.stopPropagation();
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            }
          }}
          aria-invalid={problem ? true : undefined}
          aria-describedby={problem ? errorId : undefined}
        />
        {problem && (
          <p className="browser-dialog-error" id={errorId} role="alert">
            {problem}
          </p>
        )}
        <div className="browser-dialog-buttons">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn-primary" onClick={submit}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

export interface ConfirmDialogProps {
  title: string;
  body: string;
  confirmLabel: string;
  /** Destructive confirm: red border + y/n key binding (App.qml:837-842). */
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}

export function ConfirmDialog({
  title,
  body,
  confirmLabel,
  danger = false,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const headingId = useId();
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, headingId);

  const handleKey = (e: React.KeyboardEvent) => {
    if (!danger) return;
    if (e.key === "y" || e.key === "Y") {
      e.preventDefault();
      onConfirm();
    } else if (e.key === "n" || e.key === "N") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div
      className="browser-overlay"
      ref={overlayRef}
      {...overlayProps}
      onClick={onClose}
      onKeyDown={handleKey}
    >
      <div
        className={`browser-dialog ${danger ? "browser-dialog-danger" : ""}`}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={headingId}>{title}</h2>
        <p className="browser-dialog-body">{body}</p>
        {danger && <p className="browser-dialog-hint">Press y to confirm, n to cancel.</p>}
        <div className="browser-dialog-buttons">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className={danger ? "btn-danger" : "btn-primary"}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
