/**
 * TwoPaneView — the Files view: two FilePanes, center toolbar, keyboard
 * router (port of App.qml:435-490), dialog host with SCOPE-LOCK capture +
 * revalidation (App.qml:371-383 — blocker-level invariant), status line
 * with 6 s auto-clear (App.qml:114-120), and the copy queue.
 *
 * Copy direction: the ACTIVE pane is the source, the other is the
 * destination (plugin "c" behavior); toolbar buttons make both directions
 * explicit. Windows destinations refuse writes in the UI (M1: browse+pull)
 * and the server refuses independently.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CopyJobSnapshot, CopyRequest } from "../../types/browser";
import { LOCAL_TARGET } from "../../types/browser";
import { useBrowserHosts } from "../../hooks/useBrowserHosts";
import { useBrowserPane, type BrowserPane } from "../../hooks/useBrowserPane";
import { useBrowserTransfers } from "../../hooks/useBrowserTransfers";
import * as api from "../../lib/browser/api";
import {
  canWriteTo,
  toBrowserError,
} from "../../lib/browser/guards";
import { join as joinPath } from "../../lib/browser/paths";
import { FilePane } from "./FilePane";
import { TransferQueue } from "./TransferQueue";
import { ConfirmDialog, HelpDialog, KnownHostsDialog, NameDialog } from "./dialogs";

type PaneId = "left" | "right";

/** Context captured when a dialog opens; revalidated on confirm. */
interface DialogScope {
  target: string;
  path: string;
  user: string | null;
}

type DialogState =
  | { kind: "mkdir"; paneId: PaneId; scope: DialogScope }
  | { kind: "rename"; paneId: PaneId; scope: DialogScope; name: string }
  | { kind: "delete"; paneId: PaneId; scope: DialogScope; names: string[] }
  | { kind: "user"; paneId: PaneId; host: string; initial: string }
  | { kind: "help" }
  | { kind: "hosts" }
  | {
      kind: "replace";
      job: CopyJobSnapshot;
      dstPaneId: PaneId;
      dstScope: DialogScope;
      srcPaneId: PaneId;
    };

export interface TwoPaneViewProps {
  /** Sidebar selection seeds the RIGHT pane (report §7.3). */
  seedPeer?: {
    id: string;
    hostname: string;
    dns_name: string;
    ips: string[];
  } | null;
}

const STATUS_MS = 6000;

export function TwoPaneView({ seedPeer }: TwoPaneViewProps) {
  const hosts = useBrowserHosts();
  const transfers = useBrowserTransfers();
  const [activePane, setActivePane] = useState<PaneId>("left");
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [status, setStatus] = useState<{ msg: string; isError: boolean } | null>(null);
  const [filterPane, setFilterPane] = useState<PaneId | null>(null);
  const statusTimer = useRef<number | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  const notice = useCallback((msg: string, isError = false) => {
    setStatus({ msg, isError });
    if (statusTimer.current) window.clearTimeout(statusTimer.current);
    statusTimer.current = window.setTimeout(() => setStatus(null), STATUS_MS);
  }, []);

  const userFor = useCallback((host: string) => hosts.userFor(host), [hosts]);

  const left = useBrowserPane({
    initialTarget: LOCAL_TARGET,
    userFor,
    onNotice: notice,
  });
  const right = useBrowserPane({
    initialTarget: LOCAL_TARGET,
    userFor,
    onNotice: notice,
  });

  const paneById = useCallback(
    (id: PaneId): BrowserPane => (id === "left" ? left : right),
    [left, right],
  );
  const otherId = (id: PaneId): PaneId => (id === "left" ? "right" : "left");

  // Seed the right pane from the persisted last-remote (once, on config load).
  const rightSeeded = useRef(false);
  useEffect(() => {
    if (rightSeeded.current) return;
    if (hosts.loading) return;
    rightSeeded.current = true;
    const last = hosts.config.last;
    if (last && last !== LOCAL_TARGET) {
      right.setPaneTarget(last);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hosts.loading, hosts.config.last]);

  // Sidebar peer selection seeds the right pane (match by hostname/dns/ip).
  const lastSeedId = useRef<string | null>(null);
  useEffect(() => {
    if (!seedPeer || seedPeer.id === lastSeedId.current) return;
    if (hosts.loading) return; // wait for peers, effect re-runs on load flip
    lastSeedId.current = seedPeer.id;
    const match = hosts.peers.find(
      (p) =>
        p.name === seedPeer.hostname ||
        p.dns === seedPeer.dns_name ||
        (p.ip !== "" && seedPeer.ips.includes(p.ip)),
    );
    if (match) {
      right.setPaneTarget(match.host);
      void hosts.setLast(match.host);
      setActivePane("right");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seedPeer, hosts.loading, hosts.peers]);

  const machines = useMemo(() => hosts.machines(), [hosts]);
  const nameFor = useCallback(
    (target: string): string => {
      if (target === LOCAL_TARGET) return "This computer";
      const m = machines.find((x) => x.host === target);
      return m?.name || target;
    },
    [machines],
  );

  // ---- dialog openers (capture scope NOW) ----
  const scopeOf = (p: BrowserPane): DialogScope => ({
    target: p.loadedTarget,
    path: p.loadedPath,
    user: p.loadedUser,
  });

  const openMkdir = useCallback(
    (paneId: PaneId) => {
      const p = paneById(paneId);
      if (p.blocked || p.error) return;
      setDialog({ kind: "mkdir", paneId, scope: scopeOf(p) });
    },
    [paneById],
  );

  const openRename = useCallback(
    (paneId: PaneId) => {
      const p = paneById(paneId);
      if (p.blocked || p.error) return;
      if (p.selection.length !== 1) {
        notice("Select exactly one item to rename", true);
        return;
      }
      setDialog({ kind: "rename", paneId, scope: scopeOf(p), name: p.selection[0] });
    },
    [paneById, notice],
  );

  const openDelete = useCallback(
    (paneId: PaneId) => {
      const p = paneById(paneId);
      if (p.blocked || p.error) return;
      if (p.selection.length === 0) {
        notice("Nothing selected", true);
        return;
      }
      setDialog({ kind: "delete", paneId, scope: scopeOf(p), names: [...p.selection] });
    },
    [paneById, notice],
  );

  const openChangeUser = useCallback(
    (paneId: PaneId, host: string) => {
      if (host === LOCAL_TARGET) return;
      setDialog({
        kind: "user",
        paneId,
        host,
        initial: hosts.userFor(host) ?? "",
      });
    },
    [hosts],
  );

  const openReplace = useCallback(
    (job: CopyJobSnapshot) => {
      // The replace dialog acts through the panes it was opened over:
      // dst pane must still show the destination folder; the SOURCE pane
      // must still sit on the job's source (App.qml replace staleness).
      const dstIdx = (["left", "right"] as PaneId[]).find((id) => {
        const p = paneById(id);
        return (
          p.loadedTarget === job.request.dstTarget && p.loadedPath === job.request.dstDir
        );
      });
      const srcIdx = (["left", "right"] as PaneId[]).find((id) => {
        const p = paneById(id);
        return p.loadedTarget === job.request.srcTarget;
      });
      if (!dstIdx || !srcIdx) {
        notice("That dialog no longer applies — its folder isn't open anymore", true);
        return;
      }
      setDialog({
        kind: "replace",
        job,
        dstPaneId: dstIdx,
        dstScope: scopeOf(paneById(dstIdx)),
        srcPaneId: srcIdx,
      });
    },
    [paneById, notice],
  );

  // ---- scope revalidation (the blocker-level invariant) ----
  const scopeStale = (paneId: PaneId, scope: DialogScope): boolean => {
    const p = paneById(paneId);
    return (
      p.loadedTarget !== scope.target ||
      p.loadedPath !== scope.path ||
      p.loadedUser !== scope.user
    );
  };

  const closeDialog = useCallback(() => setDialog(null), []);

  // ---- confirmed actions (all revalidate, then act, then reload) ----
  const confirmDialog = (value?: string) => {
    if (!dialog) return;
    const d = dialog;
    setDialog(null);
    if (d.kind === "user") {
      const user = (value ?? "").trim();
      void hosts
        .setUser(d.host, user === "" ? null : user)
        .then(() => {
          notice(
            user === ""
              ? `Using the default account for ${d.host}`
              : `Account for ${d.host} set to ${user}`,
          );
          const p = paneById(d.paneId);
          if (p.target === d.host) void p.load(d.host, p.loadedPath || "");
        })
        .catch((e) => notice(toBrowserError(e).message, true));
      return;
    }
    if (d.kind === "replace") {
      // Revalidate BOTH ends against the job request (App.qml replace path).
      const src = paneById(d.srcPaneId);
      const req = d.job.request;
      const stale =
        scopeStale(d.dstPaneId, d.dstScope) ||
        src.loadedTarget !== req.srcTarget ||
        src.loadedPath !== req.srcDir ||
        (src.loadedUser ?? null) !== (req.srcUser ?? null);
      if (stale) {
        notice("That dialog no longer applies — a folder moved", true);
        return;
      }
      void transfers
        .replaceJob(d.job)
        .then(() => {
          src.clearMarks();
          notice("Replace queued");
        })
        .catch((e) => notice(toBrowserError(e).message, true));
      return;
    }
    // help / hosts dialogs close on their own (no confirm path) — never reach
    // the scope-stale revalidation below, which needs paneId/scope.
    if (d.kind === "help" || d.kind === "hosts") return;
    // mkdir / rename / delete: pane must still sit where the dialog opened.
    if (scopeStale(d.paneId, d.scope)) {
      notice("That dialog no longer applies — the folder changed", true);
      return;
    }
    const p = paneById(d.paneId);
    const asUser = d.scope.user;
    if (d.kind === "mkdir") {
      const name = (value ?? "").trim();
      void api
        .browserMkdir(d.scope.target, joinPath(d.scope.path, name), asUser)
        .then(() => {
          p.setSelectAfterLoad(name);
          return p.load(d.scope.target, d.scope.path);
        })
        .catch((e) => notice(toBrowserError(e).message, true));
    } else if (d.kind === "rename") {
      const next = (value ?? "").trim();
      void api
        .browserRename(
          d.scope.target,
          joinPath(d.scope.path, d.name),
          joinPath(d.scope.path, next),
          asUser,
        )
        .then(() => {
          p.setSelectAfterLoad(next);
          return p.load(d.scope.target, d.scope.path);
        })
        .catch((e) => notice(toBrowserError(e).message, true));
    } else if (d.kind === "delete") {
      const full = d.names.map((n) => joinPath(d.scope.path, n));
      void api
        .browserDelete(d.scope.target, full, asUser)
        .then((out) => {
          p.clearMarks();
          notice(
            out.trashed
              ? `Moved ${out.done} ${out.done === 1 ? "item" : "items"} to Trash`
              : `Deleted ${out.done} ${out.done === 1 ? "item" : "items"}`,
          );
          return p.load(d.scope.target, d.scope.path);
        })
        .catch((e) => {
          const err = toBrowserError(e);
          notice(err.message, true);
          // Partial progress: still refresh so the pane shows the truth.
          void p.load(d.scope.target, d.scope.path);
        });
    }
  };

  // ---- copy (active pane → other) ----
  const copyFrom = useCallback(
    (srcId: PaneId) => {
      const src = paneById(srcId);
      const dst = paneById(otherId(srcId));
      if (src.blocked || src.error) {
        notice("Fix the source pane's error first", true);
        return;
      }
      if (dst.blocked || dst.error) {
        notice("Fix the destination pane's error first", true);
        return;
      }
      if (src.selection.length === 0) {
        notice("Nothing selected", true);
        return;
      }
      const srcTarget = src.loadedTarget || src.target;
      const dstTarget = dst.loadedTarget || dst.target;
      // S-3 (Codie M2a): instant UI mirror of the engine's same-host-two-
      // account refusal — one connection can't be two accounts. The engine
      // preflight (transfer/mod.rs) + queue row stay the authoritative path.
      if (
        srcTarget === dstTarget &&
        srcTarget !== LOCAL_TARGET &&
        (src.loadedUser ?? null) !== (dst.loadedUser ?? null)
      ) {
        notice(
          `${nameFor(srcTarget)} is shown as two different accounts (${src.loadedUser ?? ""} and ${dst.loadedUser ?? ""}) — a copy needs one account for both sides`,
          true,
        );
        return;
      }
      // M2a: remote→remote is ALLOWED — the engine streams it through the
      // app on two independent sessions (the M1 B5 mirror refusal is gone;
      // the server preflight still enforces same-host-two-account and
      // Windows-destination refusals authoritatively).
      const dstPeer = machines.find((m) => m.host === dstTarget);
      if (!canWriteTo(dstTarget, dstPeer)) {
        notice(
          `Copying INTO ${nameFor(dstTarget)} is disabled until the M3 safety gate — browse and pull only`,
          true,
        );
        return;
      }
      if (
        src.loadedTarget === dstTarget &&
        src.loadedPath === (dst.loadedPath || dst.path)
      ) {
        notice("Source and destination are the same folder", true);
        return;
      }
      const req: CopyRequest = {
        srcTarget,
        srcUser: src.loadedUser,
        srcDir: src.loadedPath || src.path,
        dstTarget,
        dstUser: dst.loadedUser,
        dstDir: dst.loadedPath || dst.path,
        names: [...src.selection],
        replace: false,
      };
      void transfers
        .startCopy(req)
        .then(() => {
          src.clearMarks();
          notice(
            `Queued ${req.names.length} ${req.names.length === 1 ? "item" : "items"} → ${nameFor(dstTarget)}`,
          );
        })
        .catch((e) => notice(toBrowserError(e).message, true));
    },
    [paneById, machines, nameFor, notice, transfers],
  );

  // ---- keyboard router (App.qml:435-490 port) ----
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (dialog) return; // dialogs own their keys
    const t = e.target as HTMLElement | null;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) {
      return; // editing — the router stays out
    }
    const v = paneById(activePane);
    const key = e.key;

    if (key === "Tab") {
      // Tab switches panes unless focus is on an interactive widget (those
      // keep native tab order for accessibility).
      if (t && (t.tagName === "BUTTON" || t.tagName === "A" || t.tagName === "SELECT")) return;
      e.preventDefault();
      setActivePane(otherId(activePane));
      return;
    }
    if (key === "Escape") {
      // Cascade: marks → filter → status (App.qml:455-460).
      if (v.marks.size > 0) v.clearMarks();
      else if (filterPane) {
        paneById(filterPane).setFilterText("");
        setFilterPane(null);
      } else if (status) setStatus(null);
      return;
    }
    if (e.ctrlKey || e.metaKey) {
      if (key === "a" || key === "A") {
        e.preventDefault();
        v.markAll();
      } else if (key === "l" || key === "L") {
        e.preventDefault();
        v.setPathEditing(true);
      } else if (key === "r" || key === "R") {
        e.preventDefault();
        void v.refresh();
        void hosts.refresh();
      } else if (key === "h" || key === "H") {
        e.preventDefault();
        v.setHidden();
      }
      return;
    }
    if (e.altKey) return;

    switch (key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        v.moveCursor(1);
        break;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        v.moveCursor(-1);
        break;
      case "PageDown":
        e.preventDefault();
        v.moveCursor(10);
        break;
      case "PageUp":
        e.preventDefault();
        v.moveCursor(-10);
        break;
      case "Home":
      case "g":
        e.preventDefault();
        v.setCursor(0);
        break;
      case "End":
      case "G":
        e.preventDefault();
        v.setCursor(Math.max(0, v.rows.length - 1));
        break;
      case "Enter":
      case "ArrowRight":
      case "l":
        e.preventDefault();
        if (v.cursorRow) v.activateRow(v.cursorRow);
        break;
      case "Backspace":
      case "ArrowLeft":
      case "h":
        e.preventDefault();
        v.goUp();
        break;
      case " ":
      case "Insert":
        e.preventDefault();
        v.toggleMarkAtCursor(true);
        break;
      case "F5":
      case "c":
      case "y":
        e.preventDefault();
        copyFrom(activePane);
        break;
      case "F7":
      case "n":
        e.preventDefault();
        openMkdir(activePane);
        break;
      case "F2":
      case "R":
        e.preventDefault();
        openRename(activePane);
        break;
      case "Delete":
      case "F8":
      case "D":
        e.preventDefault();
        openDelete(activePane);
        break;
      case "r":
        e.preventDefault();
        void v.refresh();
        break;
      case "o": {
        e.preventDefault();
        const row = v.cursorRow;
        if (row && row.rowKind === "entry" && row.entry.type === "file") {
          void api
            .browserOpen(v.loadedTarget, v.fullPath(row.entry.name), v.loadedUser)
            .catch((err) => notice(toBrowserError(err).message, true));
        }
        break;
      }
      case ".":
        e.preventDefault();
        v.setHidden();
        break;
      case "s":
        e.preventDefault();
        v.cycleSort();
        break;
      case "m": {
        e.preventDefault();
        const btn = rootRef.current?.querySelector<HTMLButtonElement>(
          `.file-pane[data-active="true"] .machine-picker-button`,
        );
        btn?.focus();
        break;
      }
      case "~":
        e.preventDefault();
        v.goHome();
        break;
      case "/":
        e.preventDefault();
        setFilterPane(activePane);
        break;
      case "=": {
        e.preventDefault();
        const other = paneById(otherId(activePane));
        other.setPaneTarget(v.loadedTarget || v.target, v.loadedPath || v.path);
        break;
      }
      case "?":
      case "F1":
        // M2a: help overlay (plugin App.qml:487 — "?" or F1).
        e.preventDefault();
        setDialog({ kind: "help" });
        break;
      default:
        break;
    }
  };

  const paneProps = (id: PaneId) => {
    const p = paneById(id);
    return {
      paneId: id,
      pane: p,
      machines,
      active: activePane === id,
      userFor,
      onFocusPane: () => setActivePane(id),
      onPickMachine: (host: string) => {
        p.setPaneTarget(host);
        if (host !== LOCAL_TARGET) void hosts.setLast(host);
      },
      onChangeUser: (host: string) => openChangeUser(id, host),
      onMkdir: () => openMkdir(id),
      onRename: () => openRename(id),
      onDelete: () => openDelete(id),
      filterOpen: filterPane === id,
      onFilterClose: () => setFilterPane((f) => (f === id ? null : f)),
    };
  };

  return (
    <div className="browser-view" ref={rootRef} onKeyDown={handleKeyDown} data-testid="browser-view">
      <div className="browser-panes">
        <FilePane {...paneProps("left")} />
        <div className="browser-toolbar" role="toolbar" aria-label="Copy direction">
          <button
            type="button"
            className="toolbar-btn"
            title="Copy from left to right (c with left active)"
            onClick={() => copyFrom("left")}
          >
            Copy →
          </button>
          <button
            type="button"
            className="toolbar-btn"
            title="Copy from right to left"
            onClick={() => copyFrom("right")}
          >
            ← Copy
          </button>
          <button
            type="button"
            className="toolbar-btn"
            data-testid="known-hosts-button"
            title="Manage pinned host keys"
            onClick={() => setDialog({ kind: "hosts" })}
          >
            Known hosts
          </button>
        </div>
        <FilePane {...paneProps("right")} />
      </div>

      <div className="browser-status" role="status" aria-live="polite">
        {status && <span className={status.isError ? "status-error" : "status-ok"}>{status.msg}</span>}
      </div>

      <TransferQueue
        jobs={transfers.jobs}
        nameFor={nameFor}
        onCancel={(id) => void transfers.cancel(id)}
        onReplace={openReplace}
        onDismiss={transfers.dismiss}
      />

      {dialog?.kind === "mkdir" && (
        <NameDialog
          title="New folder"
          initial=""
          confirmLabel="Create"
          placeholder="Folder name"
          onConfirm={(v) => confirmDialog(v)}
          onClose={closeDialog}
        />
      )}
      {dialog?.kind === "rename" && (
        <NameDialog
          title={`Rename "${dialog.name}"`}
          initial={dialog.name}
          confirmLabel="Rename"
          onConfirm={(v) => confirmDialog(v)}
          onClose={closeDialog}
        />
      )}
      {dialog?.kind === "user" && (
        <NameDialog
          title={`Account on ${dialog.host}`}
          initial={dialog.initial}
          confirmLabel="Save"
          allowEmpty
          placeholder="empty = default account"
          onConfirm={(v) => confirmDialog(v)}
          onClose={closeDialog}
        />
      )}
      {dialog?.kind === "delete" && (
        <ConfirmDialog
          title={`Delete ${dialog.names.length} ${dialog.names.length === 1 ? "item" : "items"}?`}
          body={
            paneById(dialog.paneId).target === LOCAL_TARGET
              ? `${dialog.names.slice(0, 3).join(", ")}${dialog.names.length > 3 ? ` +${dialog.names.length - 3} more` : ""} will be moved to the Trash. You can restore them from there.`
              : `${dialog.names.slice(0, 3).join(", ")}${dialog.names.length > 3 ? ` +${dialog.names.length - 3} more` : ""} will be PERMANENTLY deleted on ${nameFor(dialog.scope.target)}. This cannot be undone.`
          }
          confirmLabel="Delete"
          danger
          onConfirm={() => confirmDialog()}
          onClose={closeDialog}
        />
      )}
      {dialog?.kind === "help" && <HelpDialog onClose={closeDialog} />}
      {dialog?.kind === "hosts" && <KnownHostsDialog onClose={closeDialog} />}
      {dialog?.kind === "replace" && (
        <ConfirmDialog
          title="Replace existing items?"
          body={`${
            dialog.job.names?.length
              ? dialog.job.names.slice(0, 3).join(", ") +
                (dialog.job.names.length > 3 ? ` +${dialog.job.names.length - 3} more` : "")
              : dialog.job.label
          } already exist in ${nameFor(dialog.job.request.dstTarget)}:${
            dialog.job.request.dstDir
          }. Replacing overwrites them — for folders, matching files inside are replaced and the rest merged.`}
          confirmLabel="Replace"
          danger
          onConfirm={() => confirmDialog()}
          onClose={closeDialog}
        />
      )}
    </div>
  );
}
