/**
 * Copy-job queue view — listen("browser-copy-event") + a browser_copy_jobs
 * re-sync on mount and on window focus (the useReceipts pattern). The Rust
 * side owns the authoritative queue (serial pump, survives webview
 * suspension); this hook never fabricates job state — events nudge it, and
 * every `done` re-syncs the full snapshot so a missed event self-heals.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import type { CopyEvent, CopyJobSnapshot, CopyRequest } from "../types/browser";
import * as api from "../lib/browser/api";
import { toBrowserError } from "../lib/browser/guards";
import { logger } from "../lib/logger";

export function useBrowserTransfers() {
  const [jobs, setJobs] = useState<CopyJobSnapshot[]>([]);
  const [dismissed, setDismissed] = useState<ReadonlySet<number>>(new Set());
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const resync = useCallback(async () => {
    try {
      const snapshot = await api.browserCopyJobs();
      if (mounted.current) setJobs(snapshot);
    } catch (e) {
      logger.debug("useBrowserTransfers", "resync failed:", e);
    }
  }, []);

  // Event application: progress updates in place; start/done trigger resync
  // (the registry is the source of truth — cheap and always consistent).
  const applyEvent = useCallback(
    (ev: CopyEvent) => {
      if (ev.event === "progress") {
        setJobs((prev) =>
          prev.map((j) =>
            j.id === ev.jobId
              ? {
                  ...j,
                  state: "running" as const,
                  pct: ev.pct ?? j.pct,
                  rate: ev.rate ?? j.rate,
                  file: ev.file ?? j.file,
                }
              : j,
          ),
        );
      } else {
        void resync();
      }
    },
    [resync],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void api.listenCopy(applyEvent).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [applyEvent]);

  // Initial snapshot + re-sync on focus (webview suspension recovery).
  useEffect(() => {
    void resync();
    const onFocus = () => void resync();
    const onVisible = () => {
      if (document.visibilityState === "visible") void resync();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [resync]);

  /** Enqueue a copy. Returns the job id (throws the typed BrowserError). */
  const startCopy = useCallback(
    async (req: CopyRequest): Promise<number> => {
      try {
        const id = await api.browserCopy(req);
        await resync();
        return id;
      } catch (e) {
        throw toBrowserError(e);
      }
    },
    [resync],
  );

  const cancel = useCallback(
    async (jobId: number) => {
      try {
        await api.browserCancelCopy(jobId);
      } catch (e) {
        logger.debug("useBrowserTransfers", "cancel failed:", e);
      }
      await resync();
    },
    [resync],
  );

  /**
   * Replace-on-exists: re-enqueue the SAME request with replace=true. Scope
   * revalidation (the source pane may have moved) happens in the dialog
   * layer before this is called (App.qml:371-383 port).
   */
  const replaceJob = useCallback(
    async (job: CopyJobSnapshot) => {
      return startCopy({ ...job.request, replace: true });
    },
    [startCopy],
  );

  const dismiss = useCallback((jobId: number) => {
    setDismissed((prev) => {
      const next = new Set(prev);
      next.add(jobId);
      return next;
    });
  }, []);

  const visibleJobs = jobs.filter(
    (j) =>
      !dismissed.has(j.id) ||
      j.state === "running" ||
      j.state === "queued",
  );

  return { jobs: visibleJobs, startCopy, cancel, replaceJob, dismiss, resync };
}

export type BrowserTransfers = ReturnType<typeof useBrowserTransfers>;
