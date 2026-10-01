import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { TransferReceipt, ReceiptPage, StagingRecoveryFoundEvent, AppSettings } from "../types";
import { mergeReceipts, isTransferReceipt, isStagingRecoveryEvent } from "../lib/receipts";
import { logger } from "../lib/logger";
import { toErrorMsg } from "../lib/toErrorMsg";

const RECEIPT_PAGE_LIMIT = 50;

/** Per-path in-flight recovery operation kind (TD-UI-06 shared op state). */
export type StagingOpKind = "recovering" | "discarding";

/** Per-path in-flight op state, keyed by the full backend staging path. */
export type StagingOps = Record<string, StagingOpKind>;

/** Per-path error message from the last settled recover/discard operation. */
export type StagingOpErrors = Record<string, string>;

export interface UseReceiptsOptions {
  settings: AppSettings;
}

export interface UseReceiptsResult {
  receipts: TransferReceipt[];
  stagingDirs: StagingRecoveryFoundEvent | null;
  /**
   * TD-UI-06: per-path in-flight recover/discard ops. Owned HERE, not in the
   * Review dialog, so closing the dialog (Done/Escape/backdrop) never
   * destroys it while the backend operation is still running — reopening
   * Review shows the retained pending state.
   */
  stagingOps: StagingOps;
  /** Retained per-path errors from settled ops; cleared on the next retry. */
  stagingOpErrors: StagingOpErrors;
  recoverStaging: (path: string) => Promise<void>;
  discardStaging: (path: string) => Promise<void>;
  /** Clears a path's retained error (e.g. when opening the discard confirm). */
  clearStagingOpError: (path: string) => void;
  retryInbox: (name: string) => Promise<void>;
  showInFolder: (path: string) => Promise<void>;
}

/**
 * TD-05: Subscribes to transfer-receipt events from the backend, replays
 * missed receipts on mount/focus (paginated via sinceSeq cursor), and
 * exposes recovery/show-in-folder actions.
 *
 * Contract (docs/td05-receipt-contract.md):
 * - `transfer-receipt` event fires for every new receipt (saved/failed/salvaged).
 * - `get_recent_receipts(sinceSeq, limit)` pages through history; hasMore
 *   forces the client to keep paging until backlog is drained.
 * - `staging-recovery-found` fires at startup for preserved staging dirs.
 */
export function useReceipts({ settings }: UseReceiptsOptions): UseReceiptsResult {
  const [receipts, setReceipts] = useState<TransferReceipt[]>([]);
  const [stagingDirs, setStagingDirs] = useState<StagingRecoveryFoundEvent | null>(null);
  const [stagingOps, setStagingOps] = useState<StagingOps>({});
  const [stagingOpErrors, setStagingOpErrors] = useState<StagingOpErrors>({});
  const mountedRef = useRef(true);
  const highestSeqRef = useRef(0);
  // Synchronous source of truth for the one-op-per-path dispatch guard
  // (TD-UI-06). Maps a staging path to its in-flight promise so racing
  // callers — double-clicks, Review close/reopen, TransferHistory — JOIN the
  // existing operation instead of dispatching a second backend request.
  const stagingInFlightRef = useRef(new Map<string, Promise<void>>());

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  /** Page through the receipt store until hasMore is false. */
  const replayReceipts = useCallback(async () => {
    try {
      let sinceSeq = highestSeqRef.current;
      let allNew: TransferReceipt[] = [];
      let hasMore = true;

      while (hasMore) {
        const page = await invoke<ReceiptPage>("get_recent_receipts", {
          sinceSeq,
          limit: RECEIPT_PAGE_LIMIT,
        });
        allNew = allNew.concat(page.receipts);
        hasMore = page.hasMore;
        sinceSeq = page.nextSinceSeq;
        if (!hasMore && page.reset) {
          // Cursor predates retained history — replace all receipts
          setReceipts(allNew);
          highestSeqRef.current = sinceSeq;
          return;
        }
      }

      if (allNew.length > 0) {
        setReceipts((prev) => mergeReceipts(prev, { receipts: allNew, nextSinceSeq: sinceSeq, hasMore: false, reset: false }));
        highestSeqRef.current = sinceSeq;
      }
    } catch (e) {
      logger.warn("useReceipts", "replay failed:", toErrorMsg(e));
    }
  }, []);

  // Listen for real-time receipt events
  useEffect(() => {
    const unlistenReceipt = listen<unknown>("transfer-receipt", (event) => {
      if (!mountedRef.current) return;
      if (!isTransferReceipt(event.payload)) {
        logger.warn("useReceipts", "invalid receipt event payload");
        return;
      }
      const receipt = event.payload;
      setReceipts((prev) => {
        // Dedupe: if we already have this ID, replace it (e.g. failed → salvaged)
        const filtered = prev.filter((r) => r.id !== receipt.id);
        return [...filtered, receipt].sort((a, b) => a.seq - b.seq);
      });
    });

    const unlistenStaging = listen<unknown>("staging-recovery-found", (event) => {
      if (!mountedRef.current) return;
      if (isStagingRecoveryEvent(event.payload)) {
        setStagingDirs(event.payload);
      }
    });

    return () => {
      unlistenReceipt.then((fn) => fn()).catch(() => {});
      unlistenStaging.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Replay on mount and on window focus/visibility
  useEffect(() => {
    void replayReceipts();

    // Check for preserved staging dirs at startup
    invoke<StagingRecoveryFoundEvent>("staging_recovery_scan")
      .then((result) => {
        if (mountedRef.current && result.dirs.length > 0) {
          setStagingDirs(result);
        }
      })
      .catch((e) => logger.debug("useReceipts", "staging scan failed:", toErrorMsg(e)));

    const handleVisible = () => {
      if (!document.hidden) void replayReceipts();
    };
    document.addEventListener("visibilitychange", handleVisible);

    let unlistenFocus: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }: { payload: boolean }) => {
        if (focused) void replayReceipts();
      })
      .then((fn: () => void) => { unlistenFocus = fn; })
      .catch(() => {});

    return () => {
      document.removeEventListener("visibilitychange", handleVisible);
      unlistenFocus?.();
    };
  }, [replayReceipts]);

  const clearStagingOpError = useCallback((path: string) => {
    setStagingOpErrors((prev) => {
      if (!(path in prev)) return prev;
      const next = { ...prev };
      delete next[path];
      return next;
    });
  }, []);

  /**
   * Runs one staging operation under the TD-UI-06 shared in-flight guard:
   * a path that already has a Recover or Discard in flight can never dispatch
   * a second request — the racing caller simply awaits the existing promise.
   * Busy/error state lives in this hook, so it survives the Review dialog
   * closing while the backend operation is still unresolved, and is cleared
   * promise-scoped in `finally` when the operation settles. Failures are
   * recorded in `stagingOpErrors` (these functions never reject), so fire-
   * and-forget callers can't leak unhandled rejections.
   */
  const runStagingOp = useCallback(
    (path: string, kind: StagingOpKind, run: () => Promise<void>) => {
      const existing = stagingInFlightRef.current.get(path);
      if (existing) return existing; // synchronous guard: one op per path

      clearStagingOpError(path);
      setStagingOps((prev) => ({ ...prev, [path]: kind }));

      const op = (async () => {
        try {
          await run();
        } catch (e) {
          const msg = toErrorMsg(e);
          logger.error("useReceipts", `${kind} staging failed for ${path}:`, msg);
          setStagingOpErrors((prev) => ({ ...prev, [path]: msg }));
        } finally {
          // Promise-scoped cleanup: ALWAYS drop the in-flight mark when the
          // operation settles, so a mid-flight staging-recovery-found event
          // that re-inserts the path (setStagingDirs replaces `dirs`
          // wholesale) cannot leave the row permanently disabled with no
          // in-flight request. This runs even while the Review dialog is
          // closed — op state outlives the dialog (TD-UI-06).
          stagingInFlightRef.current.delete(path);
          setStagingOps((prev) => {
            if (!(path in prev)) return prev;
            const next = { ...prev };
            delete next[path];
            return next;
          });
        }
      })();

      stagingInFlightRef.current.set(path, op);
      return op;
    },
    [clearStagingOpError],
  );

  const recoverStaging = useCallback(
    (path: string) =>
      runStagingOp(path, "recovering", async () => {
        await invoke("recover_staging_files", { path });
        // Backend emits receipts for each recovered file; replay to catch them.
        await replayReceipts();
        // Remove only the handled directory, not all staging dirs
        setStagingDirs((prev) => {
          if (!prev) return prev;
          const remaining = prev.dirs.filter((d) => d.path !== path);
          return remaining.length > 0 ? { ...prev, dirs: remaining } : null;
        });
      }),
    [runStagingOp, replayReceipts],
  );

  const discardStaging = useCallback(
    (path: string) =>
      runStagingOp(path, "discarding", async () => {
        await invoke("discard_staging_dir", { path });
        // Remove only the handled directory, not all staging dirs
        setStagingDirs((prev) => {
          if (!prev) return prev;
          const remaining = prev.dirs.filter((d) => d.path !== path);
          return remaining.length > 0 ? { ...prev, dirs: remaining } : null;
        });
      }),
    [runStagingOp],
  );

  const retryInbox = useCallback(async (name: string) => {
    try {
      // Retry re-invokes accept_file which the backend handles idempotently
      // via acknowledge_already_received (TD05-A).
      // Use the configured save directory, not the default Downloads fallback.
      await invoke("accept_file", { name, saveDir: settings.saveDirectory });
      await replayReceipts();
    } catch (e) {
      logger.error("useReceipts", "retry inbox failed:", toErrorMsg(e));
      throw e;
    }
  }, [replayReceipts, settings.saveDirectory]);

  const showInFolder = useCallback(async (path: string) => {
    try {
      await invoke("show_in_folder", { path });
    } catch (e) {
      logger.error("useReceipts", "show in folder failed:", toErrorMsg(e));
      throw e;
    }
  }, []);

  return {
    receipts,
    stagingDirs,
    stagingOps,
    stagingOpErrors,
    recoverStaging,
    discardStaging,
    clearStagingOpError,
    retryInbox,
    showInFolder,
  };
}
