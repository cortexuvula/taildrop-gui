import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Peer, IncomingFile, TransferRecord, AppSettings } from "../types";

const DEFAULT_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "",
  autoAccept: false,
  showOfflineNodes: false,
};

export function useTailscale() {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [incomingFiles, setIncomingFiles] = useState<IncomingFile[]>([]);
  const [transfers, setTransfers] = useState<TransferRecord[]>([]);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Load settings from localStorage
  useEffect(() => {
    const saved = localStorage.getItem("taildrop-settings");
    if (saved) {
      try {
        setSettings({ ...DEFAULT_SETTINGS, ...JSON.parse(saved) });
      } catch {
        // ignore
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

  // Fetch incoming files
  const refreshIncoming = useCallback(async () => {
    try {
      const result = await invoke<IncomingFile[]>("get_incoming_files");
      setIncomingFiles(result);
    } catch {
      // silently fail polling — daemon might be briefly unavailable
    }
  }, []);

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

  // Send a file to a peer
  const sendFile = useCallback(
    async (peer: Peer, file: File) => {
      const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const record: TransferRecord = {
        id,
        filename: file.name,
        peerName: peer.hostname,
        direction: "sent",
        timestamp: Date.now(),
        status: "sending",
      };
      setTransfers((prev) => [record, ...prev]);

      try {
        const arrayBuffer = await file.arrayBuffer();
        const data = Array.from(new Uint8Array(arrayBuffer));
        await invoke("send_file", {
          peerId: peer.id,
          filename: file.name,
          data,
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
      setTransfers((prev) => [record, ...prev]);

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

  // Visible peers (excluding hidden + self + optionally offline)
  const visiblePeers = peers.filter(
    (p) =>
      !p.is_self &&
      !settings.hiddenNodes.includes(p.id) &&
      (settings.showOfflineNodes || p.online)
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
