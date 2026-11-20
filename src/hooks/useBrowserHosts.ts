/**
 * Browser host registry: peers + per-host accounts + last-remote selection.
 *
 * Peers refresh on an interval (same 10 s cadence as the Taildrop pane's
 * usePeers); config is persisted by the Rust ConfigStore (atomic writes),
 * so this hook only reads/writes through the browser_config /
 * browser_set_user / browser_set_last commands — never localStorage.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import type { BrowserConfig, BrowserPeer } from "../types/browser";
import { LOCAL_TARGET } from "../types/browser";
import * as api from "../lib/browser/api";
import { toBrowserError } from "../lib/browser/guards";
import { logger } from "../lib/logger";

const PEER_REFRESH_MS = 10_000;

/** Synthetic "This computer" row for the machine picker. */
export const LOCAL_MACHINE: BrowserPeer = {
  host: LOCAL_TARGET,
  name: "This computer",
  dns: "",
  ip: "",
  os: "",
  online: true,
  user: "",
  tailscaleSsh: false,
  browsable: true,
  isSelf: true,
};

export function useBrowserHosts() {
  const [peers, setPeers] = useState<BrowserPeer[]>([]);
  const [config, setConfig] = useState<BrowserConfig>({ hosts: {}, last: "" });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [p, c] = await Promise.all([api.browserPeers(), api.browserConfig()]);
      if (!mounted.current) return;
      setPeers(p);
      setConfig(c);
      setError(null);
    } catch (e) {
      if (!mounted.current) return;
      setError(toBrowserError(e).message);
      logger.debug("useBrowserHosts", "refresh failed:", e);
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), PEER_REFRESH_MS);
    return () => window.clearInterval(id);
  }, [refresh]);

  /** Machine-picker list: local first, then browsable peers (phones/TVs are
   *  Taildrop-only and stay out of the browser — BrowserPeer.browsable). */
  const machines = useCallback((): BrowserPeer[] => {
    return [LOCAL_MACHINE, ...peers.filter((p) => p.browsable && !p.isSelf)];
  }, [peers]);

  const peerFor = useCallback(
    (host: string): BrowserPeer | undefined => peers.find((p) => p.host === host),
    [peers],
  );

  /** Saved account for a host (null = unset; the server resolves the default
   *  and the listing PINS the result in loadedUser). */
  const userFor = useCallback(
    (host: string): string | null => {
      if (host === LOCAL_TARGET) return null;
      const u = config.hosts[host]?.user;
      return u ?? null;
    },
    [config],
  );

  const setUser = useCallback(
    async (host: string, user: string | null) => {
      try {
        await api.browserSetUser(host, user);
        if (!mounted.current) return;
        setConfig((prev) => {
          const hosts = { ...prev.hosts };
          if (user === null) delete hosts[host];
          else hosts[host] = { user };
          return { ...prev, hosts };
        });
      } catch (e) {
        logger.warn("useBrowserHosts", "setUser failed:", toBrowserError(e).message);
        throw toBrowserError(e);
      }
    },
    [],
  );

  const setLast = useCallback(async (host: string) => {
    try {
      await api.browserSetLast(host);
    } catch (e) {
      logger.debug("useBrowserHosts", "setLast failed:", e);
    }
  }, []);

  return {
    peers,
    machines,
    peerFor,
    config,
    userFor,
    setUser,
    setLast,
    loading,
    error,
    refresh,
  };
}

export type BrowserHosts = ReturnType<typeof useBrowserHosts>;
