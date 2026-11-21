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
import * as api from "../../lib/browser/api";
import { newNameProblem, toBrowserError } from "../../lib/browser/guards";
import type { KnownHostPin, ServerFlavor } from "../../types/browser";

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

// ---------------------------------------------------------------------------
// M2a: "?" help overlay — the list is TRUTHFUL TO THE CODE: every row maps to
// a real `case` in TwoPaneView's keyboard router (App.qml:435-490 port), not
// to the plugin's fuller set. Adding a binding to the router means adding it
// here; the TwoPaneView vitest pins the two lists together.
// ---------------------------------------------------------------------------

const HELP_GROUPS: Array<{ title: string; rows: Array<[string, string]> }> = [
  {
    title: "Navigation",
    rows: [
      ["j / ↓", "Move cursor down"],
      ["k / ↑", "Move cursor up"],
      ["g / Home", "Top of the list"],
      ["G / End", "Bottom of the list"],
      ["PgDn / PgUp", "Page down / up"],
      ["Enter / l / →", "Open the folder or file"],
      ["Backspace / h / ←", "Up to the parent folder"],
      ["~", "Go to the home folder"],
      ["Tab", "Switch between the panes"],
    ],
  },
  {
    title: "Selection",
    rows: [
      ["Space / Ins", "Mark the row and advance"],
      ["Ctrl+A", "Mark everything"],
      ["Esc", "Clear marks, then filter, then status"],
    ],
  },
  {
    title: "Actions",
    rows: [
      ["c / y / F5", "Copy the selection to the other pane"],
      ["n / F7", "New folder"],
      ["F2 / R", "Rename"],
      ["D / Del / F8", "Delete"],
      ["o", "Open with the default app"],
      ["r", "Refresh the folder"],
      ["Ctrl+R", "Refresh + reload the machine list"],
      ["s", "Cycle the sort order"],
      [". / Ctrl+H", "Show or hide dotfiles"],
      ["/", "Filter names"],
      ["Ctrl+L", "Edit the folder path"],
      ["m", "Open the machine menu"],
      ["=", "Mirror this pane onto the other pane"],
      ["? / F1", "This help"],
    ],
  },
  {
    title: "Dialogs",
    rows: [
      ["Enter", "Confirm"],
      ["Esc", "Close"],
      ["y / n", "Confirm / cancel a destructive dialog"],
    ],
  },
];

export function HelpDialog({ onClose }: { onClose: () => void }) {
  const headingId = useId();
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, headingId);
  return (
    <div className="browser-overlay" ref={overlayRef} {...overlayProps} onClick={onClose}>
      <div
        className="browser-dialog browser-help-dialog"
        data-testid="help-dialog"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={headingId}>Keyboard shortcuts</h2>
        <div className="help-groups">
          {HELP_GROUPS.map((g) => (
            <section key={g.title} aria-label={g.title}>
              <h3>{g.title}</h3>
              <dl>
                {g.rows.map(([keys, what]) => (
                  <div className="help-row" key={keys}>
                    <dt>
                      <kbd>{keys}</kbd>
                    </dt>
                    <dd>{what}</dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
        <div className="browser-dialog-buttons">
          <button type="button" className="btn-primary" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// M2a: Known-hosts pin management (M1 gap 4) — every pinned host key across
// the three flavor stores, with Forget per row (TrustStore::forget on the
// Rust side). Forgetting is rotation recovery: the next connect to that
// host+flavor is accept-new again.
// ---------------------------------------------------------------------------

const FLAVOR_LABELS: Record<ServerFlavor, string> = {
  "tailscale-ssh": "Tailscale SSH",
  openssh: "OpenSSH",
  "openssh-windows": "OpenSSH (Windows)",
};

export function KnownHostsDialog({ onClose }: { onClose: () => void }) {
  const headingId = useId();
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, headingId);
  const [pins, setPins] = useState<KnownHostPin[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .browserKnownHosts()
      .then((p) => {
        if (!cancelled) setPins(p);
      })
      .catch((e) => {
        if (!cancelled) setError(toBrowserError(e).message);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const forget = (pin: KnownHostPin) => {
    const key = `${pin.host}|${pin.flavor}`;
    setBusy(key);
    setError(null);
    api
      .browserForgetHostKey(pin.host, pin.flavor)
      .then(() => {
        setPins((prev) =>
          (prev ?? []).filter((p) => !(p.host === pin.host && p.flavor === pin.flavor)),
        );
      })
      .catch((e) => setError(toBrowserError(e).message))
      .finally(() => setBusy(null));
  };

  return (
    <div className="browser-overlay" ref={overlayRef} {...overlayProps} onClick={onClose}>
      <div
        className="browser-dialog browser-hosts-dialog"
        data-testid="known-hosts-dialog"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={headingId}>Known hosts</h2>
        <p className="browser-dialog-body">
          Pinned host keys. A changed key refuses the connection until you verify the change
          out of band; forgetting a pin lets the next connection accept a new key.
        </p>
        {error && (
          <p className="browser-dialog-error" role="alert">
            {error}
          </p>
        )}
        {pins === null && !error && <p className="browser-dialog-hint">Loading…</p>}
        {pins !== null && pins.length === 0 && (
          <p className="browser-dialog-hint" data-testid="known-hosts-empty">
            No pinned host keys yet — pins are recorded the first time you connect to a machine.
          </p>
        )}
        {pins !== null && pins.length > 0 && (
          <ul className="known-hosts-list">
            {pins.map((p) => (
              <li className="known-host-row" data-testid="known-host-row" key={`${p.host}|${p.flavor}`}>
                <span className="known-host-name">{p.host}</span>
                <span className="known-host-flavor">{FLAVOR_LABELS[p.flavor]}</span>
                <code className="known-host-fp" title="SHA256 fingerprint of the pinned key">
                  {p.fingerprint || "—"}
                </code>
                <button
                  type="button"
                  className="known-host-forget"
                  aria-label={`Forget the ${FLAVOR_LABELS[p.flavor]} key for ${p.host}`}
                  disabled={busy === `${p.host}|${p.flavor}`}
                  onClick={() => forget(p)}
                >
                  {busy === `${p.host}|${p.flavor}` ? "Forgetting…" : "Forget"}
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="browser-dialog-buttons">
          <button type="button" className="btn-primary" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
