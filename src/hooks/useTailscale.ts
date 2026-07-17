import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { Peer, IncomingFile, TransferRecord, AppSettings } from "../types";
import { logger } from "../lib/logger";
import { toErrorMsg } from "../lib/toErrorMsg";

const DEFAULT_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "",
  autoAccept: false,
  showOfflineNodes: false,
  showExitNodes: false,
  notifications: false,
};

const MAX_TRANSFER_HISTORY = 200;

export interface UseTailscaleOptions {
  /** Invoked when a manual send or accept fails. Pure notification; the hook
   * still writes the red transfer-history row. The component layer wires this
   * to a toast. */
  onSendError?: (info: SendErrorInfoLike) => void;
}

// Minimal shape to avoid importing from ToastProvider (keeps the hook pure).
export interface SendErrorInfoLike {
  filename: string;
  error: string;
  direction: "sent" | "received";
}

export function useTailscale(options?: UseTailscaleOptions) {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [incomingFiles, setIncomingFiles] = useState<IncomingFile[]>([]);
  const [transfers, setTransfers] = useState<TransferRecord[]>(() => {
    const saved = localStorage.getItem("taildrop-transfers");
    if (saved) {
      try {
        const parsed: TransferRecord[] = JSON.parse(saved);
        // Mark stale in-progress transfers from previous session
        return parsed.map((t) =>
          t.status === "sending" || t.status === "pending"
            ? { ...t, status: "error" as const, error: "Interrupted — app was closed" }
            : t
        );
      } catch {
        localStorage.removeItem("taildrop-transfers");
      }
    }
    return [];
  });
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const autoAcceptingRef = useRef(false);
  const seenIncomingRef = useRef(new Set<string>());
  const recentlyAcceptedRef = useRef(new Map<string, number>());
  // Mirror of incomingFiles for synchronous lookup in acceptFile (avoids
  // stale-closure issues without adding incomingFiles to its dep array).
  const incomingFilesRef = useRef<IncomingFile[]>([]);
  incomingFilesRef.current = incomingFiles;
  // Keep the latest onSendError callback in a ref so the send/accept
  // closures don't need it in their dependency arrays.
  const onSendErrorRef = useRef(options?.onSendError);
  onSendErrorRef.current = options?.onSendError;

  // Load settings from localStorage (with validation)
  useEffect(() => {
    const saved = localStorage.getItem("taildrop-settings");
    if (saved) {
      try {
        const parsed = JSON.parse(saved);
        // Validate critical fields to prevent crashes from corrupted data
        if (parsed.hiddenNodes && !Array.isArray(parsed.hiddenNodes)) {
          parsed.hiddenNodes = [];
        }
        setSettings({ ...DEFAULT_SETTINGS, ...parsed });
      } catch {
        localStorage.removeItem("taildrop-settings");
      }
    }
    // Get default download dir
    invoke<string>("get_default_download_dir")
      .then((dir) => {
        setSettings((prev) => ({
          ...prev,
          saveDirectory: prev.saveDirectory || dir,
        }));
      })
      .catch((e) => {
        logger.warn("useTailscale", "Could not get default download dir:", e);
      });
  }, []);

  // Persist transfer history (debounced to avoid jank during rapid transfers)
  useEffect(() => {
    const timeout = setTimeout(() => {
      try {
        localStorage.setItem("taildrop-transfers", JSON.stringify(transfers));
      } catch (e) {
        if (e instanceof DOMException && e.name === "QuotaExceededError") {
          logger.warn("useTailscale", "localStorage quota exceeded, pruning oldest transfers");
          const pruned = transfers.slice(0, Math.floor(transfers.length / 2));
          try {
            localStorage.setItem("taildrop-transfers", JSON.stringify(pruned));
            // Keep in-memory state in sync with the persisted list so the next
            // render doesn't re-attempt to persist the full (oversized) array.
            setTransfers(pruned);
          } catch {
            // still can't write after pruning — give up silently
          }
        }
      }
    }, 500);
    return () => clearTimeout(timeout);
  }, [transfers]);

  // Save settings to localStorage
  const updateSettings = useCallback((update: Partial<AppSettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...update };
      localStorage.setItem("taildrop-settings", JSON.stringify(next));
      return next;
    });
  }, []);

  // Listen for transfer progress events from the Rust backend
  useEffect(() => {
    const unlistenPromise = listen<{ transferId: string; progress: number }>(
      "transfer-progress",
      (event) => {
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === event.payload.transferId
              ? { ...t, progress: event.payload.progress }
              : t
          )
        );
      }
    );
    return () => {
      // Swallow rejections from a failed listen() or a throwing unlisten fn.
      unlistenPromise.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Fetch peers
  const refreshPeers = useCallback(async () => {
    try {
      const result = await invoke<Peer[]>("get_tailscale_status");
      setPeers(result);
      setError(null);
    } catch (e) {
      setError(toErrorMsg(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Bug #4: auto-accept incoming files when enabled
  const autoAcceptFiles = useCallback(async (files: IncomingFile[]) => {
    if (autoAcceptingRef.current) return;
    autoAcceptingRef.current = true;
    try {
      for (const file of files) {
        recentlyAcceptedRef.current.set(file.name, Date.now());
        const peerName = file.peerName ?? "incoming";
        try {
          await invoke<string>("accept_file", {
            name: file.name,
            saveDir: settingsRef.current.saveDirectory,
          });
          const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
          setTransfers((prev) =>
            [
              {
                id,
                filename: file.name,
                peerName,
                direction: "received" as const,
                timestamp: Date.now(),
                status: "success" as const,
              },
              ...prev,
            ].slice(0, MAX_TRANSFER_HISTORY)
          );
        } catch (e) {
          // Surface auto-accept failures to transfer history
          const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
          setTransfers((prev) =>
            [
              {
                id,
                filename: file.name,
                peerName,
                direction: "received" as const,
                timestamp: Date.now(),
                status: "error" as const,
                error: `Auto-accept failed: ${toErrorMsg(e)}`,
              },
              ...prev,
            ].slice(0, MAX_TRANSFER_HISTORY)
          );
        }
      }
    } finally {
      autoAcceptingRef.current = false;
    }
  }, []);

  // Send desktop notification for new incoming files
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
      } else {
        seenIncomingRef.current.clear();
      }
      if (settingsRef.current.autoAccept && filtered.length > 0) {
        setIncomingFiles([]);
        autoAcceptFiles(filtered);
      } else {
        setIncomingFiles(filtered);
      }
    } catch {
      // silently fail polling — daemon might be briefly unavailable
    }
  }, [autoAcceptFiles, notifyIncoming]);

  // Adaptive polling: faster when transfers are active, slower when idle.
  const hasActiveTransfers = useMemo(
    () => transfers.some((t) => t.status === "sending" || t.status === "pending"),
    [transfers]
  );

  useEffect(() => {
    refreshPeers();
    refreshIncoming();

    const peerInterval = setInterval(refreshPeers, 10000);
    const incomingMs = hasActiveTransfers ? 2000 : 8000;
    pollRef.current = setInterval(refreshIncoming, incomingMs);
    return () => {
      clearInterval(peerInterval);
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refreshPeers, refreshIncoming, hasActiveTransfers]);

  // Bug #2: send file paths instead of file data
  // Bug #3: send both peer.id (for localapi) and peer.hostname (for CLI)
  // Bug #10: cap transfer history
  const sendFile = useCallback(
    async (peer: Peer, filePaths: string[]) => {
      // Create transfer records upfront
      const records = filePaths.map((filePath) => {
        const filename = filePath.split(/[\\/]/).pop() || "file";
        const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
        return {
          record: {
            id,
            filename,
            peerName: peer.display_name,
            direction: "sent" as const,
            timestamp: Date.now(),
            status: "sending" as const,
          },
          filePath,
        };
      });
      setTransfers((prev) =>
        [...records.map((r) => r.record), ...prev].slice(0, MAX_TRANSFER_HISTORY)
      );

      // Send all files in parallel
      await Promise.allSettled(
        records.map(async ({ record, filePath }) => {
          try {
            await invoke("send_file", {
              transferId: record.id,
              peerId: peer.id,
              peerName: peer.machine_name,
              filePath,
            });
            setTransfers((prev) =>
              prev.map((t) =>
                t.id === record.id ? { ...t, status: "success" } : t
              )
            );
          } catch (e) {
            const errorStr = toErrorMsg(e);
            setTransfers((prev) =>
              prev.map((t) =>
                t.id === record.id
                  ? { ...t, status: "error", error: errorStr }
                  : t
              )
            );
            logger.debug("useTailscale", "send catch: errorStr =", errorStr, "| onSendErrorRef.current is", typeof onSendErrorRef.current === "function" ? "SET" : "NULL");
            onSendErrorRef.current?.({
              filename: record.filename,
              error: errorStr,
              direction: "sent",
            });
          }
        })
      );
    },
    []
  );

  // Accept an incoming file
  const acceptFile = useCallback(
    async (name: string) => {
      const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const peerName =
        incomingFilesRef.current.find((f) => f.name === name)?.peerName ??
        "incoming";
      const record: TransferRecord = {
        id,
        filename: name,
        peerName,
        direction: "received",
        timestamp: Date.now(),
        status: "pending",
      };
      setTransfers((prev) => [record, ...prev].slice(0, MAX_TRANSFER_HISTORY));
      setIncomingFiles((prev) => prev.filter((f) => f.name !== name));
      recentlyAcceptedRef.current.set(name, Date.now());

      try {
        const savedPath = await invoke<string>("accept_file", {
          name,
          saveDir: settingsRef.current.saveDirectory,
        });
        setTransfers((prev) =>
          prev.map((t) => (t.id === id ? { ...t, status: "success" } : t))
        );
        // Refresh incoming list
        refreshIncoming();
        return savedPath;
      } catch (e) {
        const errorStr = toErrorMsg(e);
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, status: "error", error: errorStr } : t
          )
        );
        onSendErrorRef.current?.({
          filename: name,
          error: errorStr,
          direction: "received",
        });
      }
    },
    [refreshIncoming]
  );

  // Detect own tailnet domain from self node
  const selfNode = peers.find((p) => p.is_self);
  const tailnetDomain = selfNode
    ? selfNode.dns_name.split(".").slice(1).join(".")
    : null;

  // Visible peers: exclude self + hidden + optionally offline.
  // Exit nodes (Mullvad, etc.) from other tailnets hidden unless toggle is on.
  // Non-exit shared peers from other tailnets always visible.
  const visiblePeers = peers.filter(
    (p) =>
      !p.is_self &&
      !settings.hiddenNodes.includes(p.id) &&
      (settings.showOfflineNodes || p.online) &&
      (!tailnetDomain ||
        p.dns_name.endsWith(tailnetDomain) ||
        !p.is_exit_node ||
        settings.showExitNodes)
  );

  return {
    peers,
    visiblePeers,
    incomingFiles,
    transfers,
    settings,
    loading,
    error,
    refreshPeers,
    refreshIncoming,
    sendFile,
    acceptFile,
    updateSettings,
  };
}
