import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Peer, IncomingFile, TransferRecord, AppSettings } from "../types";

const DEFAULT_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "",
  autoAccept: false,
  showOfflineNodes: false,
  showExitNodes: false,
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
  }, []);

  // Fetch incoming files
  const refreshIncoming = useCallback(async () => {
    try {
      const result = await invoke<IncomingFile[]>("get_incoming_files");
      setIncomingFiles(result);
      // Bug #4: auto-accept if enabled
      if (settingsRef.current.autoAccept && result.length > 0) {
        autoAcceptFiles(result);
      }
    } catch {
      // silently fail polling — daemon might be briefly unavailable
    }
  }, [autoAcceptFiles]);

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
          peerName: peer.hostname,
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

      try {
        const savedPath = await invoke<string>("accept_file", {
          name,
          saveDir: settings.saveDirectory,
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
        throw e;
      }
    },
    [settings.saveDirectory, refreshIncoming]
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
