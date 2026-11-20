/**
 * Machine picker for one pane — "This computer" + browsable peers with
 * online dots, OS, saved account, and the tailscale-ssh HINT badge (hint
 * only: capability cannot classify the serving endpoint — M0).
 *
 * Keyboard (App.qml:442-450 machineMenu block): Down/j Up/k move,
 * Enter/Space/l choose, Escape closes. A trailing "Change user…" row opens
 * the account dialog (the account is pinned per action server-side).
 */
import { useEffect, useId, useRef, useState } from "react";
import type { BrowserPeer } from "../../types/browser";
import { LOCAL_TARGET } from "../../types/browser";

export interface MachinePickerProps {
  machines: BrowserPeer[];
  target: string;
  userFor: (host: string) => string | null;
  onPick: (host: string) => void;
  onChangeUser: (host: string) => void;
}

export function MachinePicker({
  machines,
  target,
  userFor,
  onPick,
  onChangeUser,
}: MachinePickerProps) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  const current = machines.find((m) => m.host === target);
  const savedUser = target === LOCAL_TARGET ? null : userFor(target);
  // rows = machines + "Change user…" (only meaningful for remotes)
  const rowCount = machines.length + (target === LOCAL_TARGET ? 0 : 1);

  useEffect(() => {
    if (!open) return;
    const idx = machines.findIndex((m) => m.host === target);
    setCursor(idx >= 0 ? idx : 0);
    const onDocDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDocDown);
    return () => document.removeEventListener("mousedown", onDocDown);
  }, [open, machines, target]);

  const choose = (i: number) => {
    setOpen(false);
    if (i < machines.length) {
      const m = machines[i];
      if (m.host !== target) onPick(m.host);
    } else {
      onChangeUser(target);
    }
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        setOpen(true);
      }
      return;
    }
    e.stopPropagation(); // the pane router must not see menu keys
    if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
    } else if (e.key === "ArrowDown" || e.key === "j") {
      e.preventDefault();
      setCursor((c) => Math.min(c + 1, rowCount - 1));
    } else if (e.key === "ArrowUp" || e.key === "k") {
      e.preventDefault();
      setCursor((c) => Math.max(c - 1, 0));
    } else if (e.key === "Enter" || e.key === " " || e.key === "l") {
      e.preventDefault();
      choose(cursor);
    } else if (e.key === "Tab") {
      setOpen(false);
    }
  };

  const label = current
    ? current.host === LOCAL_TARGET
      ? "This computer"
      : current.name || current.host
    : target;

  return (
    <div className="machine-picker" ref={rootRef} onKeyDown={handleKey}>
      <button
        type="button"
        className="machine-picker-button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        onClick={() => setOpen((o) => !o)}
        title={savedUser ? `${label} (${savedUser})` : label}
      >
        <span
          className={`machine-dot ${
            target === LOCAL_TARGET || current?.online ? "online" : "offline"
          }`}
          aria-hidden="true"
        />
        <span className="machine-label">{label}</span>
        {savedUser && <span className="machine-user">{savedUser}@</span>}
        <span className="machine-caret" aria-hidden="true">
          ▾
        </span>
      </button>
      {open && (
        <ul className="machine-menu" id={listId} role="listbox" aria-label="Choose machine">
          {machines.map((m, i) => {
            const user = m.host === LOCAL_TARGET ? null : userFor(m.host);
            return (
              <li
                key={m.host}
                role="option"
                aria-selected={m.host === target}
                className={`machine-item ${i === cursor ? "cursor" : ""}`}
                onMouseEnter={() => setCursor(i)}
                onClick={() => choose(i)}
              >
                <span
                  className={`machine-dot ${m.online || m.host === LOCAL_TARGET ? "online" : "offline"}`}
                  aria-hidden="true"
                />
                <span className="machine-name">
                  {m.host === LOCAL_TARGET ? "This computer" : m.name || m.host}
                </span>
                {m.os && <span className="machine-os">{m.os}</span>}
                {m.tailscaleSsh && m.host !== LOCAL_TARGET && (
                  <span className="machine-badge" title="Tailscale SSH capability hint">
                    ssh
                  </span>
                )}
                {user && <span className="machine-user">{user}</span>}
              </li>
            );
          })}
          {target !== LOCAL_TARGET && (
            <li
              role="option"
              aria-selected={false}
              className={`machine-item machine-item-action ${
                cursor === machines.length ? "cursor" : ""
              }`}
              onMouseEnter={() => setCursor(machines.length)}
              onClick={() => choose(machines.length)}
            >
              Change user…
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
