import { useState, useEffect, useCallback, useRef, useMemo, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { IncomingFile, TransferRecord, AppSettings } from "../types";
import { toErrorMsg } from "../lib/toErrorMsg";
import { logger } from "../lib/logger";

const MAX_TRANSFER_HISTORY = 200;

export interface UseIncomingFilesOptions {
  settingsRef: RefObject<AppSettings>;
  /** Transfers list — used to compute `hasActiveTransfers` for adaptive polling. */
  transfers: TransferRecord[];
  /**
   * Append received transfer records (used by the auto-accept path). The
   * caller passes a state setter wrapper so this hook needn't own transfers.
   */
  appendTransfers: (records: TransferRecord[]) => void;
}

export interface IncomingBridge {
  peerNameFor: (name: string) => string | undefined;
  removeIncoming: (name: string) => void;
  markRecentlyAccepted: (name: string) => void;
  refreshIncoming: () => void | Promise<void>;
}

export interface UseIncomingFilesResult {
  incomingFiles: IncomingFile[];
  /** Incoming-state operations exposed for useTransfers via the facade bridge. */
  bridgeRef: RefObject<IncomingBridge>;
}

/**
 * Polls the Tailscale incoming-files list (adaptive: 2s when transfers are
 * active, 8s when idle), emits desktop notifications for new files, and
 * auto-accepts when the user has enabled it.
 */
export function useIncomingFiles(options: UseIncomingFilesOptions): UseIncomingFilesResult {
  const { settingsRef, transfers, appendTransfers } = options;

  const [incomingFiles, setIncomingFiles] = useState<IncomingFile[]>([]);

  const autoAcceptingRef = useRef(false);
  const seenIncomingRef = useRef(new Set<string>());
  const recentlyAcceptedRef = useRef(new Map<string, number>());
  // Mirror of incomingFiles for synchronous lookup from the bridge.
  const incomingFilesRef = useRef<IncomingFile[]>([]);
  incomingFilesRef.current = incomingFiles;

  // Bug #4: auto-accept incoming files when enabled.
  const autoAcceptFiles = useCallback(
    async (files: IncomingFile[]) => {
      if (autoAcceptingRef.current) return;
      autoAcceptingRef.current = true;
      try {
        const records: TransferRecord[] = [];
        const errorRecords: TransferRecord[] = [];
        for (const file of files) {
          recentlyAcceptedRef.current.set(file.name, Date.now());
          const peerName = file.peerName ?? "incoming";
          try {
            await invoke<string>("accept_file", {
              name: file.name,
              saveDir: settingsRef.current.saveDirectory,
            });
            records.push({
              id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
              filename: file.name,
              peerName,
              direction: "received" as const,
              timestamp: Date.now(),
              status: "success" as const,
            });
          } catch (e) {
            // Surface auto-accept failures to transfer history
            errorRecords.push({
              id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
              filename: file.name,
              peerName,
              direction: "received" as const,
              timestamp: Date.now(),
              status: "error" as const,
              error: `Auto-accept failed: ${toErrorMsg(e)}`,
            });
          }
        }
        // Commit all auto-accept outcomes in a single capped update.
        if (records.length > 0 || errorRecords.length > 0) {
          const all = [...records, ...errorRecords];
          appendTransfers(all);
        }
      } finally {
        autoAcceptingRef.current = false;
      }
    },
    [appendTransfers, settingsRef],
  );

  // Send desktop notification for new incoming files.
  const notifyIncoming = useCallback(async (files: IncomingFile[]) => {
    if (!settingsRef.current.notifications) return;
    const fileKey = (f: IncomingFile) => `${f.name}:${f.size}`;
    const newFiles = files.filter((f) => !seenIncomingRef.current.has(fileKey(f)));
    if (newFiles.length === 0) return;

    for (const f of newFiles) {
      seenIncomingRef.current.add(fileKey(f));
    }
    // Prune keys that are no longer in the incoming list
    const currentKeys = new Set(files.map(fileKey));
    for (const key of seenIncomingRef.current) {
      if (!currentKeys.has(key)) seenIncomingRef.current.delete(key);
    }

    try {
      let granted = await isPermissionGranted();
      if (!granted) {
        const perm = await requestPermission();
        granted = perm === "granted";
      }
      if (!granted) return;

      if (newFiles.length === 1) {
        sendNotification({
          title: "TailDrop — Incoming File",
          body: newFiles[0].name,
        });
      } else {
        sendNotification({
          title: "TailDrop — Incoming Files",
          body: `${newFiles.length} files waiting to be accepted`,
        });
      }
    } catch {
      // notifications not supported or permission denied
    }
  }, []);

  // Fetch incoming files
  const refreshIncoming = useCallback(async () => {
    try {
      const result = await invoke<IncomingFile[]>("get_incoming_files", {
        saveDir: settingsRef.current.saveDirectory,
      });
      // Filter out files that were recently accepted (poll race prevention)
      const now = Date.now();
      const filtered = result.filter((f) => {
        const acceptedAt = recentlyAcceptedRef.current.get(f.name);
        return !(acceptedAt && now - acceptedAt < 30000);
      });
      // Clean up stale entries
      for (const [name, time] of recentlyAcceptedRef.current) {
        if (now - time > 30000) recentlyAcceptedRef.current.delete(name);
      }
      if (filtered.length > 0) {
        notifyIncoming(filtered);
      }
      // Note: do NOT clear seenIncomingRef on empty polls — a transient
      // empty response would reset the dedup set and cause duplicate
      // notifications on the next non-empty poll. Keys are pruned by
      // notifyIncoming itself when files leave the list.
      if (settingsRef.current.autoAccept && filtered.length > 0) {
        setIncomingFiles([]);
        autoAcceptFiles(filtered);
      } else {
        setIncomingFiles(filtered);
      }
    } catch (e) {
      // Log but don't surface — daemon might be briefly unavailable.
      // Using logger so it appears in the DebugPanel for diagnosis.
      logger.debug("useIncomingFiles", "poll failed:", toErrorMsg(e));
    }
  }, [autoAcceptFiles, notifyIncoming, settingsRef]);

  // Adaptive polling: faster when transfers are active, slower when idle.
  const hasActiveTransfers = useMemo(
    () => transfers.some((t) => t.status === "sending" || t.status === "pending"),
    [transfers],
  );

  useEffect(() => {
    refreshIncoming();
    const incomingMs = hasActiveTransfers ? 2000 : 8000;
    const interval = setInterval(refreshIncoming, incomingMs);
    return () => clearInterval(interval);
  }, [refreshIncoming, hasActiveTransfers]);

  // Expose incoming-state operations via a ref so useTransfers' accept handler
  // can read/mutate incoming state without owning it. The facade hands this
  // ref to useTransfers. The methods are stable (they read from refs / use
  // state setters); refreshIncoming is rebound each render to keep the closure
  // fresh.
  const bridgeRef = useRef<IncomingBridge>({
    peerNameFor: (name: string) =>
      incomingFilesRef.current.find((f) => f.name === name)?.peerName,
    removeIncoming: (name: string) =>
      setIncomingFiles((prev) => prev.filter((f) => f.name !== name)),
    markRecentlyAccepted: (name: string) =>
      recentlyAcceptedRef.current.set(name, Date.now()),
    refreshIncoming: () => void refreshIncoming(),
  });
  bridgeRef.current.refreshIncoming = () => refreshIncoming();

  return { incomingFiles, bridgeRef };
}

export { MAX_TRANSFER_HISTORY };
