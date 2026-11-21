/**
 * One file pane: MachinePicker + path bar (Ctrl+L, ~ expansion) + listing
 * with responsive column drop (FilePane.qml:67-71 — narrow panes drop the
 * date first, then the size) + footer counts + error pane with kind-specific
 * recovery (FilePane.qml:732-808: auth/account → Change user, all → Retry).
 *
 * Remote-supplied names render ONLY as React text children — the plugin's
 * PlainText rule (no rich-text interpretation of file names).
 *
 * Windows peers in M1: browse + pull only. Write actions are disabled in
 * the UI with an explicit note; the server independently refuses (the UI
 * hint is os-string based, the server's decision is probe-flavor based).
 */
import { useEffect, useRef, useState } from "react";
import type { BrowserPeer, BrowserError } from "../../types/browser";
import type { BrowserPane, PaneRow } from "../../hooks/useBrowserPane";
import { MachinePicker } from "./MachinePicker";
import { canWriteTo } from "../../lib/browser/guards";
import { expandHome } from "../../lib/browser/paths";
import { formatSize } from "../../lib/format";

export interface FilePaneProps {
  paneId: "left" | "right";
  pane: BrowserPane;
  machines: BrowserPeer[];
  active: boolean;
  userFor: (host: string) => string | null;
  onFocusPane: () => void;
  onPickMachine: (host: string) => void;
  onChangeUser: (host: string) => void;
  onMkdir: () => void;
  onRename: () => void;
  onDelete: () => void;
  /** Inline filter input (the "/" key) — filters rows by substring. */
  filterOpen: boolean;
  onFilterClose: () => void;
}

function formatDate(mtimeSecs: number): string {
  if (!Number.isFinite(mtimeSecs) || mtimeSecs <= 0) return "";
  const d = new Date(mtimeSecs * 1000);
  const now = new Date();
  const sameYear = d.getFullYear() === now.getFullYear();
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

function rowGlyph(row: PaneRow): string {
  if (row.rowKind === "up") return "↑";
  if (row.entry.link) return "🔗";
  return row.entry.type === "dir" ? "🗀" : "🗎";
}

export function FilePane({
  paneId,
  pane,
  machines,
  active,
  userFor,
  onFocusPane,
  onPickMachine,
  onChangeUser,
  onMkdir,
  onRename,
  onDelete,
  filterOpen,
  onFilterClose,
}: FilePaneProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const pathInputRef = useRef<HTMLInputElement>(null);
  const [pathDraft, setPathDraft] = useState("");

  const writable = canWriteTo(pane.loadedTarget || pane.target, 
    machines.find((m) => m.host === (pane.loadedTarget || pane.target)));
  const windowsBlocked = !writable && pane.target !== "local";

  // Keep the cursor row visible.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-index="${pane.cursor}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [pane.cursor, pane.rows]);

  // Focus the path input when path editing starts (Ctrl+L).
  useEffect(() => {
    if (pane.pathEditing) {
      setPathDraft(pane.loadedPath || pane.path || "~");
      pathInputRef.current?.focus();
      pathInputRef.current?.select();
    }
  }, [pane.pathEditing, pane.loadedPath, pane.path]);

  const commitPathEdit = () => {
    const expanded = expandHome(pathDraft, pane.home);
    pane.setPathEditing(false);
    void pane.load(undefined, expanded);
  };

  const entryCount = pane.rows.filter((r) => r.rowKind === "entry").length;

  return (
    <section
      className={`file-pane ${active ? "active" : ""}`}
      data-testid={`pane-${paneId}`}
      data-active={active}
      onMouseDown={onFocusPane}
      aria-label={`File pane ${paneId}`}
    >
      <header className="pane-header">
        <MachinePicker
          machines={machines}
          target={pane.target}
          userFor={userFor}
          onPick={onPickMachine}
          onChangeUser={onChangeUser}
        />
        {pane.pathEditing ? (
          <input
            ref={pathInputRef}
            className="path-input"
            type="text"
            value={pathDraft}
            aria-label="Folder path"
            onChange={(e) => setPathDraft(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation(); // pane router must not see edit keys
              if (e.key === "Enter") {
                e.preventDefault();
                commitPathEdit();
              } else if (e.key === "Escape") {
                e.preventDefault();
                pane.setPathEditing(false);
              }
            }}
            onBlur={() => pane.setPathEditing(false)}
          />
        ) : (
          <button
            type="button"
            className="path-display"
            title={`${pane.loadedPath || "/"} — Ctrl+L to edit`}
            onClick={() => {
              onFocusPane();
              pane.setPathEditing(true);
            }}
          >
            {pane.loadedPath || "/"}
          </button>
        )}
        <button
          type="button"
          className="pane-icon-btn"
          title="Refresh (r)"
          aria-label="Refresh"
          onClick={() => {
            onFocusPane();
            void pane.refresh();
          }}
          disabled={pane.blocked && !pane.error}
        >
          ⟳
        </button>
      </header>

      {filterOpen && (
        <div className="pane-filter">
          <span aria-hidden="true">/</span>
          <input
            autoFocus
            type="text"
            aria-label="Filter names"
            value={pane.filterText}
            onChange={(e) => pane.setFilterText(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Escape") {
                e.preventDefault();
                pane.setFilterText("");
                onFilterClose();
              } else if (e.key === "Enter") {
                e.preventDefault();
                onFilterClose();
              }
            }}
            onBlur={onFilterClose}
          />
        </div>
      )}

      {windowsBlocked && (
        <div className="pane-notice" role="status">
          Windows peers: browse and pull only (writes return in M3).
        </div>
      )}

      {pane.error ? (
        <ErrorPane
          error={pane.error}
          machineName={machines.find((m) => m.host === pane.target)?.name ?? pane.target}
          onRetry={() => void pane.load(pane.target, pane.path)}
          onChangeUser={() => onChangeUser(pane.target)}
          showChangeUser={
            pane.target !== "local" &&
            (pane.error.kind === "auth" ||
              pane.error.kind === "account" ||
              pane.error.kind === "hostkey" ||
              pane.error.kind === "unreachable")
          }
        />
      ) : (
        <div
          className="pane-list"
          ref={listRef}
          role="listbox"
          aria-multiselectable="true"
          aria-label={`Files on ${pane.target}`}
          tabIndex={-1}
        >
          {pane.loading && <div className="pane-loading" aria-hidden="true">Loading…</div>}
          {!pane.loading && pane.rows.length === 0 && (
            <div className="pane-empty">This folder is empty</div>
          )}
          {pane.rows.map((row, i) => {
            const isEntry = row.rowKind === "entry";
            const name = isEntry ? row.entry.name : row.name;
            const marked = isEntry && pane.marks.has(name);
            return (
              <div
                key={`${isEntry ? "e" : "u"}-${name}`}
                className={`pane-row ${i === pane.cursor ? "cursor" : ""} ${marked ? "marked" : ""}`}
                role="option"
                aria-selected={marked || (i === pane.cursor && pane.marks.size === 0)}
                data-index={i}
                data-name={name}
                data-kind={isEntry ? row.entry.type : "up"}
                onClick={(e) => {
                  onFocusPane();
                  if (e.detail === 2) {
                    pane.activateRow(row); // double click activates
                  } else if (marked) {
                    pane.toggleMark(name); // click on a marked row unmarks
                    pane.setCursor(i);
                  } else {
                    pane.setCursor(i);
                  }
                }}
              >
                <span className="col-glyph" aria-hidden="true">
                  {rowGlyph(row)}
                </span>
                <span className="col-name">{name}</span>
                {isEntry && row.entry.type === "file" && (
                  <span className="col-size">{formatSize(row.entry.size)}</span>
                )}
                {isEntry && <span className="col-time">{formatDate(row.entry.mtime)}</span>}
              </div>
            );
          })}
        </div>
      )}

      <footer className="pane-footer">
        <span>
          {entryCount} {entryCount === 1 ? "item" : "items"}
          {pane.marks.size > 0 && ` · ${pane.marks.size} marked`}
        </span>
        {pane.skipped > 0 && (
          <span className="footer-skipped" title="Names that can't be read safely (control characters, duplicates, or non-UTF-8)">
            {pane.skipped} hidden from listing
          </span>
        )}
        {pane.loadedUser && pane.target !== "local" && (
          <span className="footer-user">{pane.loadedUser}@</span>
        )}
        {!writable && pane.target !== "local" && <span className="footer-ro">read-only</span>}
        <span className="pane-actions">
          <button type="button" className="pane-action" onClick={() => { onFocusPane(); onMkdir(); }} disabled={!writable} title="New folder (n)">
            New folder
          </button>
          <button type="button" className="pane-action" onClick={() => { onFocusPane(); onRename(); }} disabled={!writable || pane.selection.length !== 1} title="Rename (R)">
            Rename
          </button>
          <button type="button" className="pane-action danger" onClick={() => { onFocusPane(); onDelete(); }} disabled={pane.selection.length === 0 || (pane.target !== "local" && !writable)} title="Delete (D)">
            Delete
          </button>
        </span>
      </footer>
    </section>
  );
}

/** Kind-specific error pane (FilePane.qml:732-808 port). */
export function ErrorPane({
  error,
  machineName,
  onRetry,
  onChangeUser,
  showChangeUser,
}: {
  error: BrowserError;
  machineName: string;
  onRetry: () => void;
  onChangeUser: () => void;
  showChangeUser: boolean;
}) {
  let title = "Can't show this folder";
  if (error.kind === "auth") title = `Sign-in refused by ${machineName}`;
  else if (error.kind === "unreachable") title = `Can't reach ${machineName}`;
  else if (error.kind === "hostkey") title = `Security: ${machineName}'s host key changed`;
  else if (error.kind === "account") title = "Account problem";
  else if (error.kind === "denied") title = "Permission denied";
  else if (error.kind === "notfound") title = "Folder not found";

  return (
    <div className="pane-error" role="alert" data-kind={error.kind}>
      <div className="pane-error-icon" aria-hidden="true">⚠</div>
      <p className="pane-error-title">{title}</p>
      <p className="pane-error-message">{error.message}</p>
      {error.kind === "hostkey" && (
        <p className="pane-error-hint">
          The key this machine presents no longer matches the one you accepted.
          Refusing to connect is intentional — verify the change out of band
          before trusting it, then remove the old pin via Known hosts in the
          toolbar and reconnect.
        </p>
      )}
      <div className="pane-error-actions">
        {showChangeUser && (
          <button type="button" className="btn-secondary" onClick={onChangeUser}>
            Change user
          </button>
        )}
        <button type="button" className="btn-primary" onClick={onRetry}>
          Retry
        </button>
      </div>
    </div>
  );
}
