/**
 * One browser pane's state machine — port of FilePane.qml (serial requests +
 * stale-result guard at :110-115, marks-else-cursor selection at :318-331,
 * blocked-pane semantics at :64).
 *
 * Serial + stale guard: every load takes a sequence number; when it resolves,
 * a newer request (or an unmounted pane) means the result is dropped — a slow
 * listing for an old folder can never overwrite the current view.
 *
 * Account pinning: the listing's `user` (resolved once server-side) is stored
 * as loadedUser and passed as as_user on EVERY later action from this pane,
 * so a saved-account change mid-dialog cannot retarget an in-flight pane
 * (report §4.7; the server enforces the same pin).
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { BrowserEntry, BrowserError } from "../types/browser";
import { LOCAL_TARGET } from "../types/browser";
import * as api from "../lib/browser/api";
import { toBrowserError } from "../lib/browser/guards";
import * as paths from "../lib/browser/paths";
import { logger } from "../lib/logger";

export type SortKey = "name" | "size" | "mtime";

/** A listing row: the ".." up-row or a wrapped entry (BrowserEntry.type is
 *  already "dir" | "file", so rows discriminate on rowKind). */
export type PaneRow =
  | { rowKind: "up"; name: string }
  | { rowKind: "entry"; entry: BrowserEntry };

export interface UseBrowserPaneOptions {
  initialTarget: string;
  initialPath?: string;
  /** Saved-account lookup, read at call time through a ref (never stale). */
  userFor: (host: string) => string | null;
  /** Transient status line (6 s auto-clear lives in TwoPaneView). */
  onNotice?: (msg: string, isError?: boolean) => void;
  /** Auto-load on mount (default true). */
  autoLoad?: boolean;
}

export function useBrowserPane(opts: UseBrowserPaneOptions) {
  const { initialTarget, initialPath = "", onNotice, autoLoad = true } = opts;

  const [target, setTarget] = useState(initialTarget);
  const [path, setPath] = useState(initialPath);
  // What the CURRENT listing was loaded from (the pin for later actions):
  const [loadedTarget, setLoadedTarget] = useState("");
  const [loadedPath, setLoadedPath] = useState("");
  const [loadedUser, setLoadedUser] = useState<string | null>(null);
  const [home, setHome] = useState("");
  const [entries, setEntries] = useState<BrowserEntry[]>([]);
  const [skipped, setSkipped] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<BrowserError | null>(null);
  const [hidden, setHidden] = useState(false);
  const [sortKey, setSortKey] = useState<SortKey>("name");
  const [sortAsc, setSortAsc] = useState(true);
  const [filterText, setFilterText] = useState("");
  const [cursor, setCursor] = useState(0);
  const [marks, setMarks] = useState<ReadonlySet<string>>(new Set());
  const [selectAfterLoad, setSelectAfterLoad] = useState<string | null>(null);
  const [pathEditing, setPathEditing] = useState(false);

  const reqSeq = useRef(0);
  const mounted = useRef(true);
  const userForRef = useRef(opts.userFor);
  useEffect(() => {
    userForRef.current = opts.userFor;
  }, [opts.userFor]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Live refs for keyboard handlers that must not re-subscribe per state tick.
  const stateRef = useRef({ target, path, loadedTarget, loadedPath, loadedUser, home });
  stateRef.current = { target, path, loadedTarget, loadedPath, loadedUser, home };

  const load = useCallback(
    async (nextTarget?: string, nextPath?: string) => {
      const t = nextTarget ?? stateRef.current.target;
      // "" means home: the server resolves it (realpath(".") for remotes).
      const p = nextPath ?? stateRef.current.path;
      const seq = ++reqSeq.current;
      const asUser = t === LOCAL_TARGET ? null : userForRef.current(t);
      setLoading(true);
      try {
        const listing = await api.browserLs(t, p, asUser);
        if (!mounted.current || seq !== reqSeq.current) return; // stale
        setTarget(t);
        setPath(listing.path);
        setLoadedTarget(t);
        setLoadedPath(listing.path);
        setLoadedUser(listing.user === "" ? null : listing.user);
        setHome(listing.home);
        setEntries(listing.entries);
        setSkipped(listing.skipped);
        setError(null);
        setMarks(new Set());
        // Select-after-load (goUp selects the folder we came from; mkdir /
        // rename select the new name — plugin selectAfterLoad).
        setSelectAfterLoad((want) => {
          if (want) {
            const idx = listing.entries.findIndex((e) => e.name === want);
            // +1 when an up-row is present (not at root).
            const offset = paths.isRoot(listing.path) ? 0 : 1;
            setCursor(idx >= 0 ? idx + offset : 0);
          } else {
            setCursor(0);
          }
          return null;
        });
      } catch (e) {
        if (!mounted.current || seq !== reqSeq.current) return; // stale
        const err = toBrowserError(e);
        setTarget(t);
        setError(err);
        logger.debug("useBrowserPane", `load ${t}:${p || "~"} failed:`, err.message);
      } finally {
        if (mounted.current && seq === reqSeq.current) setLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (autoLoad) void load(initialTarget, initialPath);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refresh = useCallback(() => {
    const { loadedTarget, loadedPath } = stateRef.current;
    return load(loadedTarget || target, loadedPath || path);
  }, [load, target, path]);

  const goUp = useCallback(() => {
    const { loadedPath, loadedTarget } = stateRef.current;
    if (paths.isRoot(loadedPath) || error) return;
    setSelectAfterLoad(paths.baseName(loadedPath));
    void load(loadedTarget, paths.parentOf(loadedPath));
  }, [error, load]);

  const goHome = useCallback(() => {
    void load(stateRef.current.target, stateRef.current.home || "");
  }, [load]);

  const openEntry = useCallback(
    (entry: BrowserEntry) => {
      const base = stateRef.current.loadedPath || "/";
      if (entry.type === "dir") {
        setSelectAfterLoad(null);
        void load(stateRef.current.loadedTarget, paths.join(base, entry.name));
      } else {
        // File: open with the default app (remote downloads to cache first).
        void api
          .browserOpen(stateRef.current.loadedTarget, paths.join(base, entry.name), stateRef.current.loadedUser)
          .catch((e) => {
            const err = toBrowserError(e);
            onNotice?.(err.message, true);
          });
      }
    },
    [load, onNotice],
  );

  const activateRow = useCallback(
    (row: PaneRow) => {
      if (row.rowKind === "up") goUp();
      else openEntry(row.entry);
    },
    [goUp, openEntry],
  );

  // ---- rows: hidden filter, text filter, sort, up-row ----
  const rows = useMemo<PaneRow[]>(() => {
    let list = entries;
    if (!hidden) list = list.filter((e) => !e.name.startsWith("."));
    if (filterText !== "") {
      const f = filterText.toLowerCase();
      list = list.filter((e) => e.name.toLowerCase().includes(f));
    }
    const sorted = [...list].sort((a, b) => {
      // Folders first, always (plugin behavior).
      if (a.type !== b.type) return a.type === "dir" ? -1 : 1;
      let c = 0;
      if (sortKey === "name") c = a.name.localeCompare(b.name, undefined, { numeric: true });
      else if (sortKey === "size") c = a.size - b.size;
      else c = a.mtime - b.mtime;
      return sortAsc ? c : -c;
    });
    const out: PaneRow[] = sorted.map((e) => ({ rowKind: "entry" as const, entry: e }));
    const atRoot = paths.isRoot(loadedPath || path);
    if (!atRoot) out.unshift({ rowKind: "up" as const, name: ".." });
    return out;
  }, [entries, hidden, filterText, sortKey, sortAsc, loadedPath, path]);

  // Clamp the cursor when rows shrink (filter/sort/load).
  useEffect(() => {
    setCursor((c) => Math.min(c, Math.max(0, rows.length - 1)));
  }, [rows.length]);

  // ---- selection: marks-else-cursor (FilePane.qml:318-331) ----
  const cursorRow = rows.length > 0 ? rows[Math.min(cursor, rows.length - 1)] : null;
  const selection = useMemo<string[]>(() => {
    if (marks.size > 0) return Array.from(marks);
    if (cursorRow && cursorRow.rowKind === "entry") return [cursorRow.entry.name];
    return [];
  }, [marks, cursorRow]);

  const toggleMark = useCallback(
    (name: string) => {
      setMarks((prev) => {
        const next = new Set(prev);
        if (next.has(name)) next.delete(name);
        else next.add(name);
        return next;
      });
    },
    [],
  );

  const toggleMarkAtCursor = useCallback(
    (advance: boolean) => {
      const row = rows[cursor];
      if (row && row.rowKind === "entry") toggleMark(row.entry.name);
      if (advance) setCursor((c) => Math.min(c + 1, Math.max(0, rows.length - 1)));
    },
    [rows, cursor, toggleMark],
  );

  const markAll = useCallback(() => {
    setMarks(
      new Set(
        rows
          .filter((r): r is Extract<PaneRow, { rowKind: "entry" }> => r.rowKind === "entry")
          .map((r) => r.entry.name),
      ),
    );
  }, [rows]);

  const clearMarks = useCallback(() => setMarks(new Set()), []);

  const moveCursor = useCallback(
    (delta: number) => {
      setCursor((c) => Math.max(0, Math.min(c + delta, rows.length - 1)));
    },
    [rows.length],
  );

  const cycleSort = useCallback(() => {
    setSortKey((k) => {
      if (k === "name") return "size";
      if (k === "size") return "mtime";
      setSortAsc((a) => !a);
      return "name";
    });
  }, []);

  /** Pane is blocked (FilePane.qml:64): fatal error kinds or a target the
   *  current listing never loaded — actions other than retry/machine-switch
   *  are refused until reload succeeds. */
  const blocked =
    error !== null &&
    ((error.kind === "auth" ||
      error.kind === "unreachable" ||
      error.kind === "hostkey" ||
      error.kind === "account") ||
      loadedTarget !== target);

  /** Switch the pane's machine (MachinePicker / "=" mirror uses load()). */
  const setPaneTarget = useCallback(
    (t: string, p?: string) => {
      setTarget(t);
      void load(t, p ?? "");
    },
    [load],
  );

  /** Absolute path of a row name within the loaded listing. */
  const fullPath = useCallback(
    (name: string): string => paths.join(loadedPath || "/", name),
    [loadedPath],
  );

  return {
    // state
    target,
    path,
    loadedPath,
    loadedTarget,
    loadedUser,
    home,
    rows,
    skipped,
    loading,
    error,
    blocked,
    hidden,
    sortKey,
    sortAsc,
    filterText,
    cursor,
    cursorRow,
    marks,
    selection,
    selectAfterLoad,
    pathEditing,
    isLocal: target === LOCAL_TARGET,
    // actions
    load,
    refresh,
    goUp,
    goHome,
    openEntry,
    activateRow,
    setCursor,
    moveCursor,
    toggleMark,
    toggleMarkAtCursor,
    markAll,
    clearMarks,
    setHidden: useCallback(() => setHidden((h) => !h), []),
    cycleSort,
    setFilterText,
    setSelectAfterLoad,
    setPathEditing,
    setPaneTarget,
    fullPath,
    paths,
  };
}

export type BrowserPane = ReturnType<typeof useBrowserPane>;
