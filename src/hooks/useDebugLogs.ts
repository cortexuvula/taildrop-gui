import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getFrontendLogs, subscribe, type LogEntry } from "../lib/logger";

// Raw shape returned by the Rust get_debug_logs command.
interface BackendLogEntry {
  timestamp_ms: number;
  level: string;
  target: string;
  message: string;
}

export type MergedLogEntry = LogEntry;

export function useDebugLogs(enabled: boolean): MergedLogEntry[] {
  const [logs, setLogs] = useState<MergedLogEntry[]>([]);

  useEffect(() => {
    if (!enabled) return;

    let cancelled = false;

    const refresh = async () => {
      const [frontend, backend] = await Promise.all([
        Promise.resolve(getFrontendLogs()),
        invoke<BackendLogEntry[]>("get_debug_logs").catch(() => []),
      ]);
      if (cancelled) return;

      const merged: MergedLogEntry[] = [
        ...backend.map((b) => ({
          timestampMs: b.timestamp_ms,
          level: b.level as LogEntry["level"],
          source: "backend" as const,
          target: b.target,
          message: b.message,
        })),
        ...frontend,
      ].sort((a, b) => a.timestampMs - b.timestampMs);

      setLogs(merged);
    };

    void refresh();
    // Re-fetch on every frontend log event (instant frontend updates).
    const unsub = subscribe(() => {
      void refresh();
    });
    // Poll backend every 1s (backend has no per-line event).
    const interval = setInterval(() => {
      void refresh();
    }, 1000);

    return () => {
      cancelled = true;
      unsub();
      clearInterval(interval);
    };
  }, [enabled]);

  return logs;
}
