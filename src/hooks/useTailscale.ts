import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { Peer, IncomingFile, TransferRecord, AppSettings } from "../types";

const DEFAULT_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "",
  autoAccept: false,
  showOfflineNodes: false,
  showExitNodes: false,
  notifications: false,
};

const MAX_TRANSFER_HISTORY = 200;

export function useTailscale() {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [incomingFiles, setIncomingFiles] = useState<IncomingFile[]>([]);
  const [transfers, setTransfers] = useState<TransferRecord[]>([]);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const autoAcceptingRef = useRef(false);
  const seenIncomingRef = useRef(new Set<string>());

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
    invoke<string>("get_default_download_dir").then((dir) => {
      setSettings((prev) => ({
        ...prev,
        saveDirectory: prev.saveDirectory || dir,
      }));
    });
  }, []);

  // Save settings to localStorage
  const updateSettings = useCallback((update: Partial<AppSettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...update };
      localStorage.setItem("taildrop-settings", JSON.stringify(next));
      return next;
    });
  }, []);

  // Fetch peers
  const refreshPeers = useCallback(async () => {
    try {
      const result = await invoke<Peer[]>("get_tailscale_status");
      setPeers(result);
      setError(null);
    } catch (e) {
      setError(String(e));
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
        try {
          await invoke<string>("accept_file", {
            name: file.Name,
            saveDir: settingsRef.current.saveDirectory,
          });
          const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
          setTransfers((prev) =>
            [
              {
                id,
                filename: file.Name,
                peerName: "incoming",
                direction: "received" as const,
                timestamp: Date.now(),
                status: "success" as const,
              },
              ...prev,
            ].slice(0, MAX_TRANSFER_HISTORY)
          );
        } catch {
          // silently fail individual auto-accepts
        }
      }
    } finally {
      autoAcceptingRef.current = false;
    }
  }, []);

  // Send desktop notification for new incoming files
  const notifyIncoming = useCallback(async (files: IncomingFile[]) => {
    if (!settingsRef.current.notifications) return;
    const newFiles = files.filter((f) => !seenIncomingRef.current.has(f.Name));
    if (newFiles.length === 0) return;

    for (const f of newFiles) {
      seenIncomingRef.current.add(f.Name);
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
          body: newFiles[0].Name,
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
      const result = await invoke<IncomingFile[]>("get_incoming_files");
      if (result.length > 0) {
        notifyIncoming(result);
      } else {
        seenIncomingRef.current.clear();
      }
      if (settingsRef.current.autoAccept && result.length > 0) {
        setIncomingFiles([]);
        autoAcceptFiles(result);
      } else {
        setIncomingFiles(result);
      }
    } catch {
      // silently fail polling — daemon might be briefly unavailable
    }
  }, [autoAcceptFiles, notifyIncoming]);

  // Initial load + polling
  useEffect(() => {
    refreshPeers();
    refreshIncoming();
    const peerInterval = setInterval(refreshPeers, 10000);
    pollRef.current = setInterval(refreshIncoming, 5000);
    return () => {
      clearInterval(peerInterval);
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refreshPeers, refreshIncoming]);

  // Bug #2: send file paths instead of file data
  // Bug #3: send both peer.id (for localapi) and peer.hostname (for CLI)
  // Bug #10: cap transfer history
  const sendFile = useCallback(
    async (peer: Peer, filePaths: string[]) => {
      for (const filePath of filePaths) {
        const filename =
          filePath.split(/[\\/]/).pop() || "file";
        const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
        const record: TransferRecord = {
          id,
          filename,
          peerName: peer.display_name,
          direction: "sent",
          timestamp: Date.now(),
          status: "sending",
        };
        setTransfers((prev) => [record, ...prev].slice(0, MAX_TRANSFER_HISTORY));

        try {
          await invoke("send_file", {
            peerId: peer.id,
            peerName: peer.hostname,
            filePath,
          });
          setTransfers((prev) =>
            prev.map((t) => (t.id === id ? { ...t, status: "success" } : t))
          );
        } catch (e) {
          setTransfers((prev) =>
            prev.map((t) =>
              t.id === id ? { ...t, status: "error", error: String(e) } : t
            )
          );
        }
      }
    },
    []
  );

  // Accept an incoming file
  const acceptFile = useCallback(
    async (name: string) => {
      const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const record: TransferRecord = {
        id,
        filename: name,
        peerName: "incoming",
        direction: "received",
        timestamp: Date.now(),
        status: "pending",
      };
      setTransfers((prev) => [record, ...prev].slice(0, MAX_TRANSFER_HISTORY));
      setIncomingFiles((prev) => prev.filter((f) => f.Name !== name));

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
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, status: "error", error: String(e) } : t
          )
        );
      }
    },
    [refreshIncoming]
  );

  // Detect own tailnet domain from self node
  const selfNode = peers.find((p) => p.is_self);
  const tailnetDomain = selfNode
    ? selfNode.dns_name.split(".").slice(1).join(".")
    : null;

  // Visible peers: same tailnet only, excluding self + hidden + optionally offline
  const visiblePeers = peers.filter(
    (p) =>
      !p.is_self &&
      !settings.hiddenNodes.includes(p.id) &&
      (settings.showOfflineNodes || p.online) &&
      (!tailnetDomain || p.dns_name.endsWith(tailnetDomain) || settings.showExitNodes)
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
