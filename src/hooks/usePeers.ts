import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Peer, AppSettings } from "../types";
import { toErrorMsg } from "../lib/toErrorMsg";
import { sanitizePeers } from "../lib/guards";

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

  // Lifecycle guard: a slow in-flight status call must not setState after
  // unmount (React 18+ tolerates it, but concurrent tearing is free to avoid).
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refreshPeers = useCallback(async () => {
    try {
      const result = await invoke<unknown>("get_tailscale_status");
      if (!mountedRef.current) return;
      // Guard the IPC boundary: malformed entries are dropped rather than
      // crashing renderers downstream.
      setPeers(sanitizePeers(result));
      setError(null);
    } catch (e) {
      if (!mountedRef.current) return;
      setError(toErrorMsg(e));
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, []);

  // Poll peers every 10s.
  useEffect(() => {
    refreshPeers();
    const peerInterval = setInterval(refreshPeers, 10000);
    return () => clearInterval(peerInterval);
  }, [refreshPeers]);

  // Immediate refresh on window visibility/focus change (covers both
  // minimize/un-minimize via visibilitychange and occlusion via Tauri focus).
  useEffect(() => {
    const handleVisible = () => {
      if (!document.hidden) refreshPeers();
    };
    document.addEventListener("visibilitychange", handleVisible);

    let unlistenFocus: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }: { payload: boolean }) => {
        if (focused) refreshPeers();
      })
      .then((fn: () => void) => {
        unlistenFocus = fn;
      })
      .catch(() => {});

    return () => {
      document.removeEventListener("visibilitychange", handleVisible);
      unlistenFocus?.();
    };
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
