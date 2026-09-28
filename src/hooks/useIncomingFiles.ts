import { useState, useEffect, useCallback, useRef, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

export interface UseIncomingFilesOptions {
  settings: AppSettings;
  settingsRef: RefObject<AppSettings>;
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
   * Set when the backend receive loop has failed repeatedly (daemon
   * unreachable, unusable save dir, …). Rendered as a persistent banner so
   * silent "no files" states are impossible.
   */
  pollError: string | null;
}

/**
 * Tracks Tailscale incoming files from the backend's background receive
 * loop: `incoming-files-changed` is the single source of truth for the list
 * and `incoming-files-error` for persistent failures. The timed polling
 * lives in Rust now — WKWebView suspends DOM timers in a minimized window,
 * which used to stop the download on socket-less macOS/Windows until
 * refocus. This hook keeps the backend's receive settings in sync, emits
 * desktop notifications for new files, auto-accepts when enabled, and does a
 * one-shot catch-up fetch on mount/refocus for an instant list.
 */
export function useIncomingFiles(options: UseIncomingFilesOptions): UseIncomingFilesResult {
  const { settings, settingsRef, appendTransfers } = options;

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

  // Lifecycle guard: async callbacks must not touch state after unmount.
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

  // Apply a fresh incoming-files list — from a backend
  // `incoming-files-changed` event or a one-shot catch-up fetch. Sanitizes
  // the boundary, drops recently-accepted names (poll race prevention), then
  // notifies / auto-accepts / updates state.
  const applyIncoming = useCallback(
    (rawList: unknown) => {
      if (!mountedRef.current) return;
      // Sanitize the boundary: malformed entries are dropped, and a
      // non-array payload becomes an empty list rather than a crash.
      const files = sanitizeIncomingFiles(rawList);
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
    },
    [autoAcceptFiles, notifyIncoming, settingsRef],
  );

  // The backend receive loop is the single source of truth for the incoming
  // list and for persistent receive failures (emitted after repeated
  // failures, mirroring the old 3-strike poll threshold).
  useEffect(() => {
    const unlistenChanged = listen<unknown>("incoming-files-changed", (event) => {
      applyIncoming(event.payload);
    });
    const unlistenError = listen<string>("incoming-files-error", (event) => {
      if (!mountedRef.current) return;
      setPollError(
        typeof event.payload === "string" ? event.payload : toErrorMsg(event.payload),
      );
    });
    return () => {
      // Swallow rejections from a failed listen() or a throwing unlisten fn.
      unlistenChanged.then((fn) => fn()).catch(() => {});
      unlistenError.then((fn) => fn()).catch(() => {});
    };
  }, [applyIncoming]);

  // Keep the backend's shared receive settings in sync — its loop uses these
  // for every poll, including the ones that happen while this webview is
  // suspended in a minimized window.
  useEffect(() => {
    invoke("set_receive_settings", {
      saveDir: settings.saveDirectory,
      autoAccept: settings.autoAccept,
    }).catch((e) => {
      logger.warn("useIncomingFiles", "set_receive_settings failed:", toErrorMsg(e));
    });
  }, [settings.saveDirectory, settings.autoAccept]);

  // One-shot catch-up fetch: on mount and when the window regains
  // visibility/focus, ask the backend for the current list so the UI is
  // instant. This is NOT the periodic receive loop — that lives in Rust —
  // but it is the same list/download call, so files the backend already
  // received while minimized appear immediately on refocus.
  const refreshIncoming = useCallback(async () => {
    try {
      const result = await invoke<unknown>("get_incoming_files", {
        saveDir: settingsRef.current.saveDirectory,
      });
      applyIncoming(result);
    } catch (e) {
      // A single failed catch-up is not proof the daemon is dead; the
      // backend emits the persistent error via incoming-files-error.
      logger.debug("useIncomingFiles", "catch-up fetch failed:", toErrorMsg(e));
    }
  }, [applyIncoming, settingsRef]);

  // When the window regains visibility or focus, fire an immediate catch-up.
  // visibilitychange covers minimize/un-minimize, and the Tauri window focus
  // event covers occlusion (another window on top) — which
  // visibilitychange does NOT fire for (known Tauri bug #6864).
  useEffect(() => {
    void refreshIncoming();
    const handleVisible = () => {
      if (!document.hidden) void refreshIncoming();
    };
    document.addEventListener("visibilitychange", handleVisible);

    let unlistenFocus: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }: { payload: boolean }) => {
        if (focused) void refreshIncoming();
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
