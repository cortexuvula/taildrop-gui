import { useState, useEffect, useCallback, useRef, type RefObject } from "react";
import type React from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Peer, TransferRecord, AppSettings } from "../types";
import { logger } from "../lib/logger";
import { toErrorMsg } from "../lib/toErrorMsg";
import { loadStored, saveStored } from "../lib/storage";
import { sanitizeTransfers } from "../lib/guards";
import { newId } from "../lib/id";

const MAX_TRANSFER_HISTORY = 200;
/** Upper bound on simultaneous in-flight sends; extra drops queue instead of
 * firing every file at once (which saturates the daemon and the network). */
const MAX_CONCURRENT_SENDS = 3;

export interface SendErrorInfoLike {
  filename: string;
  error: string;
  direction: "sent" | "received";
}

/**
 * Bridge into useIncomingFiles so useTransfers' async accept handler can read
 * and mutate incoming state without owning it. All methods are no-ops until
 * the facade wires the concrete implementations in.
 */
export interface IncomingBridge {
  /** Synchronously look up the peer name for an incoming file. */
  peerNameFor: (name: string) => string | undefined;
  /** Remove an accepted file from the incoming list. */
  removeIncoming: (name: string) => void;
  /** Mark a file as just-accepted so the incoming poller doesn't re-show it. */
  markRecentlyAccepted: (name: string) => void;
  /** Trigger an immediate refresh of the incoming list. */
  refreshIncoming: () => void | Promise<void>;
}

export interface UseTransfersOptions {
  settingsRef: RefObject<AppSettings>;
  onSendErrorRef: RefObject<((info: SendErrorInfoLike) => void) | undefined>;
  /** Wired by the facade to incoming-state operations. */
  incomingBridgeRef: RefObject<IncomingBridge | null>;
}

export interface UseTransfersResult {
  transfers: TransferRecord[];
  /** Exposed so the facade can wire useIncomingFiles' auto-accept path. */
  setTransfers: React.Dispatch<React.SetStateAction<TransferRecord[]>>;
  sendFile: (peer: Peer, filePaths: string[]) => Promise<void>;
  acceptFile: (name: string) => Promise<string | undefined>;
}

/**
 * Owns transfer history state and the send/accept actions.
 *
 * - Initial state is loaded lazily via `loadStored` and SANITIZED — corrupt
 *   or non-conforming entries (e.g. a null element from a damaged
 *   localStorage) are dropped instead of white-screening the app on mount.
 * - In-flight records from a previous session are removed rather than
 *   rewritten as errors: their real outcome is unknown (the transfer may
 *   have completed after the app closed), and inventing a failure that may
 *   not have happened is worse than dropping the row.
 * - Persists debounced state to localStorage on change (pruning on quota
 *   errors).
 * - Listens for backend `transfer-progress` events and updates progress.
 */
export function useTransfers(options: UseTransfersOptions): UseTransfersResult {
  const { settingsRef, onSendErrorRef, incomingBridgeRef } = options;

  const [transfers, setTransfers] = useState<TransferRecord[]>(() => {
    const stored = loadStored<unknown>("taildrop-transfers");
    return sanitizeTransfers(stored).filter(
      (t) => t.status !== "sending" && t.status !== "pending" && t.status !== "receiving",
    );
  });

  // Lifecycle guard: send/accept callbacks settle asynchronously and must not
  // touch state after unmount.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Persist transfer history (debounced to avoid jank during rapid transfers).
  useEffect(() => {
    const timeout = setTimeout(() => {
      try {
        saveStored("taildrop-transfers", transfers);
      } catch (e) {
        if (e instanceof DOMException && e.name === "QuotaExceededError") {
          logger.warn("useTransfers", "localStorage quota exceeded, pruning oldest transfers");
          const pruned = transfers.slice(0, Math.floor(transfers.length / 2));
          try {
            saveStored("taildrop-transfers", pruned);
            setTransfers(pruned);
          } catch {
            logger.warn(
              "useTransfers",
              "localStorage quota still exceeded after pruning — history will not persist",
            );
          }
        }
      }
    }, 500);
    return () => clearTimeout(timeout);
  }, [transfers]);

  // Listen for transfer progress events from the Rust backend.
  useEffect(() => {
    const unlistenPromise = listen<{ transferId: string; progress: number }>(
      "transfer-progress",
      (event) => {
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === event.payload.transferId
              ? { ...t, progress: event.payload.progress }
              : t,
          ),
        );
      },
    );
    return () => {
      // Swallow rejections from a failed listen() or a throwing unlisten fn.
      unlistenPromise.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Bug #2: send file paths instead of file data
  // Bug #3: send both peer.id (for localapi) and peer.hostname (for CLI)
  // Bug #10: cap transfer history
  const sendFile = useCallback(
    async (peer: Peer, filePaths: string[]) => {
      // Create transfer records upfront
      const records = filePaths.map((filePath) => {
        const filename = filePath.split(/[\\/]/).pop() || "file";
        const id = newId();
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
        [...records.map((r) => r.record), ...prev].slice(0, MAX_TRANSFER_HISTORY),
      );

      const sendOne = async ({
        record,
        filePath,
      }: {
        record: TransferRecord;
        filePath: string;
      }) => {
        try {
          await invoke("send_file", {
            transferId: record.id,
            peerId: peer.id,
            peerName: peer.machine_name,
            filePath,
          });
          if (!mountedRef.current) return;
          setTransfers((prev) =>
            prev.map((t) =>
              t.id === record.id ? { ...t, status: "success" } : t,
            ),
          );
        } catch (e) {
          if (!mountedRef.current) return;
          const errorStr = toErrorMsg(e);
          setTransfers((prev) =>
            prev.map((t) =>
              t.id === record.id
                ? { ...t, status: "error", error: errorStr }
                : t,
            ),
          );
          onSendErrorRef.current?.({
            filename: record.filename,
            error: errorStr,
            direction: "sent",
          });
        }
      };

      // Bounded worker pool: fire at most MAX_CONCURRENT_SENDS at once and
      // pull the rest from the queue as slots free up.
      let next = 0;
      const workerCount = Math.min(MAX_CONCURRENT_SENDS, records.length);
      await Promise.all(
        Array.from({ length: workerCount }, async () => {
          for (;;) {
            const index = next++;
            if (index >= records.length) break;
            await sendOne(records[index]);
          }
        }),
      );
    },
    [onSendErrorRef],
  );

  // Accept an incoming file.
  // Bridges into incoming state (peer-name lookup, removal, recently-accepted
  // mark, refresh) via incomingBridgeRef so this hook needn't own the incoming
  // list or its poller.
  const acceptFile = useCallback(
    async (name: string) => {
      const bridge = incomingBridgeRef.current;
      const id = newId();
      const peerName = bridge?.peerNameFor(name) ?? "incoming";
      const record: TransferRecord = {
        id,
        filename: name,
        peerName,
        direction: "received",
        timestamp: Date.now(),
        status: "receiving",
      };
      setTransfers((prev) => [record, ...prev].slice(0, MAX_TRANSFER_HISTORY));
      bridge?.removeIncoming(name);
      bridge?.markRecentlyAccepted(name);

      try {
        const savedPath = await invoke<string>("accept_file", {
          name,
          saveDir: settingsRef.current.saveDirectory,
        });
        if (!mountedRef.current) return savedPath;
        setTransfers((prev) =>
          prev.map((t) => (t.id === id ? { ...t, status: "success" } : t)),
        );
        // Refresh incoming list
        await bridge?.refreshIncoming();
        return savedPath;
      } catch (e) {
        if (!mountedRef.current) return;
        const errorStr = toErrorMsg(e);
        setTransfers((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, status: "error", error: errorStr } : t,
          ),
        );
        onSendErrorRef.current?.({
          filename: name,
          error: errorStr,
          direction: "received",
        });
      }
    },
    [incomingBridgeRef, onSendErrorRef, settingsRef],
  );

  return { transfers, setTransfers, sendFile, acceptFile };
}
