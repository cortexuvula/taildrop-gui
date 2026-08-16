import { useState, useEffect, useCallback, useRef, useMemo, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { IncomingFile, TransferRecord, AppSettings } from "../types";
import { toErrorMsg } from "../lib/toErrorMsg";
import { logger } from "../lib/logger";
import { sanitizeIncomingFiles } from "../lib/guards";
import { newId } from "../lib/id";

const MAX_TRANSFER_HISTORY = 200;
/** After this many consecutive failed polls, surface a persistent warning —
 * a dead daemon must not masquerade as "no incoming files". */
const POLL_FAILURE_THRESHOLD = 3;

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
  /**
   * Set when polling has failed repeatedly (daemon unreachable, unusable save
   * dir, …). Rendered as a persistent banner so silent "no files" states are
   * impossible.
   */
  pollError: string | null;
}

/**
 * Polls the Tailscale incoming-files list (adaptive: 2s when transfers are
 * active, 8s when idle), emits desktop notifications for new files, and
 * auto-accepts when the user has enabled it.
 */
export function useIncomingFiles(options: UseIncomingFilesOptions): UseIncomingFilesResult {
  const { settingsRef, transfers, appendTransfers } = options;

  const [incomingFiles, setIncomingFiles] = useState<IncomingFile[]>([]);
  const [pollError, setPollError] = useState<string | null>(null);

  const autoAcceptingRef = useRef(false);
  const seenIncomingRef = useRef(new Set<string>());
  const recentlyAcceptedRef = useRef(new Map<string, number>());
  // Mirror of incomingFiles for synchronous lookup from the bridge.
  const incomingFilesRef = useRef<IncomingFile[]>([]);
  useEffect(() => {
    incomingFilesRef.current = incomingFiles;
  }, [incomingFiles]);

  // Lifecycle guard: async poll callbacks must not touch state after unmount.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

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
              id: newId(),
              filename: file.name,
              peerName,
              direction: "received" as const,
              timestamp: Date.now(),
              status: "success" as const,
            });
          } catch (e) {
            // Surface auto-accept failures to transfer history
            errorRecords.push({
              id: newId(),
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
          if (!mountedRef.current) return;
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
  }, [settingsRef]);

  // Fetch incoming files. Tracks consecutive failures so a dead daemon or an
  // unusable save dir surfaces as a persistent error instead of an
  // indistinguishable-from-success empty list.
  const consecutiveFailuresRef = useRef(0);
  const refreshIncoming = useCallback(async () => {
    try {
      const result = await invoke<unknown>("get_incoming_files", {
        saveDir: settingsRef.current.saveDirectory,
      });
      if (!mountedRef.current) return;
      // Sanitize the IPC boundary: malformed entries are dropped, and a
      // non-array payload becomes an empty list rather than a crash.
      const files = sanitizeIncomingFiles(result);
      consecutiveFailuresRef.current = 0;
      setPollError(null);

      // Filter out files that were recently accepted (poll race prevention)
      const now = Date.now();
      const filtered = files.filter((f) => {
        const acceptedAt = recentlyAcceptedRef.current.get(f.name);
        return !(acceptedAt && now - acceptedAt < 30000);
      });
      // Clean up stale entries
      for (const [name, time] of recentlyAcceptedRef.current) {
        if (now - time > 30000) recentlyAcceptedRef.current.delete(name);
      }
      // Only notify for non-auto-accept mode — when auto-accept is on,
      // files are handled silently and a desktop notification would be
      // noisy for something the user doesn't need to act on.
      if (filtered.length > 0 && !settingsRef.current.autoAccept) {
        void notifyIncoming(filtered);
      }
      if (settingsRef.current.autoAccept && filtered.length > 0) {
        setIncomingFiles([]);
        void autoAcceptFiles(filtered);
      } else {
        setIncomingFiles(filtered);
      }
    } catch (e) {
      if (!mountedRef.current) return;
      const msg = toErrorMsg(e);
      consecutiveFailuresRef.current += 1;
      logger.debug("useIncomingFiles", `poll failed (${consecutiveFailuresRef.current}):`, msg);
      if (consecutiveFailuresRef.current >= POLL_FAILURE_THRESHOLD) {
        setPollError(msg);
      }
    }
  }, [autoAcceptFiles, notifyIncoming, settingsRef]);

  // Adaptive polling: faster when transfers are active, slower when idle.
  const hasActiveTransfers = useMemo(
    () =>
      transfers.some(
        (t) =>
          t.status === "sending" ||
          t.status === "pending" ||
          t.status === "receiving",
      ),
    [transfers],
  );

  useEffect(() => {
    refreshIncoming();
    const incomingMs = hasActiveTransfers ? 2000 : 8000;
    const interval = setInterval(refreshIncoming, incomingMs);
    return () => clearInterval(interval);
  }, [refreshIncoming, hasActiveTransfers]);

  // When the window regains visibility or focus, fire an immediate poll.
  // WKWebView throttles setInterval when minimized/occluded, so we need
  // both: visibilitychange covers minimize/un-minimize, and the Tauri
  // window focus event covers occlusion (another window on top) — which
  // visibilitychange does NOT fire for (known Tauri bug #6864).
  useEffect(() => {
    const handleVisible = () => {
      if (!document.hidden) refreshIncoming();
    };
    document.addEventListener("visibilitychange", handleVisible);

    let unlistenFocus: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }: { payload: boolean }) => {
        if (focused) refreshIncoming();
      })
      .then((fn: () => void) => {
        unlistenFocus = fn;
      })
      .catch(() => {});

    return () => {
      document.removeEventListener("visibilitychange", handleVisible);
      unlistenFocus?.();
    };
  }, [refreshIncoming]);

  // Expose incoming-state operations via a ref so useTransfers' accept handler
  // can read/mutate incoming state without owning it. The facade hands this
  // ref to useTransfers. The methods are stable (they read from refs / use
  // state setters); refreshIncoming is rebound in an effect (not during
  // render) to stay StrictMode/concurrent-safe while keeping its closure fresh.
  const bridgeRef = useRef<IncomingBridge>({
    peerNameFor: (name: string) =>
      incomingFilesRef.current.find((f) => f.name === name)?.peerName,
    removeIncoming: (name: string) =>
      setIncomingFiles((prev) => prev.filter((f) => f.name !== name)),
    markRecentlyAccepted: (name: string) =>
      recentlyAcceptedRef.current.set(name, Date.now()),
    refreshIncoming: () => {},
  });
  const refresh = refreshIncoming;
  useEffect(() => {
    bridgeRef.current.refreshIncoming = () => refresh();
  }, [refresh]);

  return { incomingFiles, bridgeRef, pollError };
}

export { MAX_TRANSFER_HISTORY };
