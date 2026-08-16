import { useCallback, useEffect, useRef, type RefObject } from "react";
import type { TransferRecord } from "../types";
import { useSettings } from "./useSettings";
import { usePeers } from "./usePeers";
import { useTransfers, type SendErrorInfoLike } from "./useTransfers";
import {
  useIncomingFiles,
  MAX_TRANSFER_HISTORY,
} from "./useIncomingFiles";

export { type SendErrorInfoLike } from "./useTransfers";

export interface UseTailscaleOptions {
  /** Invoked when a manual send or accept fails. Pure notification; the hook
   * still writes the red transfer-history row. The component layer wires this
   * to a toast. */
  onSendError?: (info: SendErrorInfoLike) => void;
}

/**
 * Thin facade over the four focused sub-hooks. The return shape matches the
 * properties the monolithic hook exposed so existing consumers (App.tsx) need
 * no changes.
 *
 * Wiring notes:
 * - `incomingBridgeRef` is created here and passed to useTransfers, then
 *   populated from useIncomingFiles' `bridgeRef`. This lets useTransfers'
 *   async accept handler read/mutate incoming state without owning it. The
 *   handlers only fire post-render, so the ref is always populated before use.
 * - `appendTransfers` lets useIncomingFiles push auto-accept outcomes into
 *   transfer history without owning that state.
 */
export function useTailscale(options?: UseTailscaleOptions) {
  const { settings, settingsRef, updateSettings, saveDirError } = useSettings();
  const { peers, loading, error, visiblePeers } = usePeers(settings);

  // Keep the latest onSendError callback in a ref so the send/accept closures
  // don't need it in their dependency arrays. Updated in an effect (not
  // during render) to stay StrictMode/concurrent-safe.
  const onSendErrorRef: RefObject<((info: SendErrorInfoLike) => void) | undefined> =
    useRef(options?.onSendError);
  const onSendError = options?.onSendError;
  useEffect(() => {
    onSendErrorRef.current = onSendError;
  }, [onSendError]);

  // Bridge between useIncomingFiles (owner) and useTransfers (consumer).
  // Created here as a mutable ref, populated below once useIncomingFiles
  // returns its own bridge ref.
  const incomingBridgeRef = useRef<{
    peerNameFor: (name: string) => string | undefined;
    removeIncoming: (name: string) => void;
    markRecentlyAccepted: (name: string) => void;
    refreshIncoming: () => void | Promise<void>;
  } | null>(null);

  const { transfers, setTransfers, sendFile, acceptFile } = useTransfers({
    settingsRef,
    onSendErrorRef,
    incomingBridgeRef,
  });

  const appendTransfers = useCallback(
    (records: TransferRecord[]) => {
      setTransfers((prev) => [...records, ...prev].slice(0, MAX_TRANSFER_HISTORY));
    },
    [],
  );

  const { incomingFiles, bridgeRef, pollError } = useIncomingFiles({
    settingsRef,
    transfers,
    appendTransfers,
  });
  // Wire the incoming bridge into the ref useTransfers reads. The methods are
  // stable across renders; refreshIncoming is rebound each render, so the
  // assignment happens in an effect rather than during render.
  const bridge = bridgeRef.current;
  useEffect(() => {
    incomingBridgeRef.current = bridge;
  }, [bridge]);

  return {
    peers,
    visiblePeers,
    incomingFiles,
    transfers,
    settings,
    loading,
    error,
    sendFile,
    acceptFile,
    updateSettings,
    saveDirError,
    pollError,
  };
}
