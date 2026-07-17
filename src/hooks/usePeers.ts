import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Peer, AppSettings } from "../types";
import { toErrorMsg } from "../lib/toErrorMsg";

export interface UsePeersResult {
  peers: Peer[];
  loading: boolean;
  error: string | null;
  visiblePeers: Peer[];
}

/**
 * Fetches and polls the Tailscale peer list (every 10s) and derives the
 * visible-peers view based on settings (hidden nodes, offline filter, exit
 * nodes from other tailnets).
 */
export function usePeers(settings: AppSettings): UsePeersResult {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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

  // Poll peers every 10s.
  useEffect(() => {
    refreshPeers();
    const peerInterval = setInterval(refreshPeers, 10000);
    return () => clearInterval(peerInterval);
  }, [refreshPeers]);

  // Detect own tailnet domain from self node.
  const selfNode = peers.find((p) => p.is_self);
  const tailnetDomain = selfNode
    ? selfNode.dns_name.split(".").slice(1).join(".")
    : null;

  // Visible peers: exclude self + hidden + optionally offline.
  // Exit nodes (Mullvad, etc.) from other tailnets hidden unless toggle is on.
  // Non-exit shared peers from other tailnets always visible.
  const visiblePeers = useMemo(
    () =>
      peers.filter(
        (p) =>
          !p.is_self &&
          !settings.hiddenNodes.includes(p.id) &&
          (settings.showOfflineNodes || p.online) &&
          (!tailnetDomain ||
            p.dns_name.endsWith(tailnetDomain) ||
            !p.is_exit_node ||
            settings.showExitNodes),
      ),
    [peers, settings.hiddenNodes, settings.showOfflineNodes, settings.showExitNodes, tailnetDomain],
  );

  return { peers, loading, error, visiblePeers };
}
