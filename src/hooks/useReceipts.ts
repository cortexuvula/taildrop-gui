import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { TransferReceipt, ReceiptPage, StagingRecoveryFoundEvent, AppSettings } from "../types";
import { mergeReceipts, isTransferReceipt, isStagingRecoveryEvent } from "../lib/receipts";
import { logger } from "../lib/logger";
import { toErrorMsg } from "../lib/toErrorMsg";

const RECEIPT_PAGE_LIMIT = 50;

export interface UseReceiptsOptions {
  settings: AppSettings;
}

export interface UseReceiptsResult {
  receipts: TransferReceipt[];
  stagingDirs: StagingRecoveryFoundEvent | null;
  recoverStaging: (path: string) => Promise<void>;
  discardStaging: (path: string) => Promise<void>;
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
  const mountedRef = useRef(true);
  const highestSeqRef = useRef(0);

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

  const recoverStaging = useCallback(async (path: string) => {
    try {
      await invoke("recover_staging_files", { path });
      // Backend emits receipts for each recovered file; replay to catch them.
      await replayReceipts();
      // Remove only the handled directory, not all staging dirs
      setStagingDirs((prev) => {
        if (!prev) return prev;
        const remaining = prev.dirs.filter((d) => d.path !== path);
        return remaining.length > 0 ? { ...prev, dirs: remaining } : null;
      });
    } catch (e) {
      logger.error("useReceipts", "recover staging failed:", toErrorMsg(e));
      throw e;
    }
  }, [replayReceipts]);

  const discardStaging = useCallback(async (path: string) => {
    try {
      await invoke("discard_staging_dir", { path });
      // Remove only the handled directory, not all staging dirs
      setStagingDirs((prev) => {
        if (!prev) return prev;
        const remaining = prev.dirs.filter((d) => d.path !== path);
        return remaining.length > 0 ? { ...prev, dirs: remaining } : null;
      });
    } catch (e) {
      logger.error("useReceipts", "discard staging failed:", toErrorMsg(e));
      throw e;
    }
  }, []);

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

  return { receipts, stagingDirs, recoverStaging, discardStaging, retryInbox, showInFolder };
}
