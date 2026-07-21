import { useCallback, useEffect, useRef, useState } from "react";
import { check as checkForUpdate, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { toErrorMsg } from "../lib/toErrorMsg";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "error";

export interface UseUpdaterApi {
  status: UpdateStatus;
  version?: string;
  date?: string;
  body?: string;
  progress?: number;
  error?: string;
  check: () => Promise<UpdateStatus>;
  download: () => Promise<void>;
  install: () => Promise<void>;
  dismiss: () => void;
}

export function useUpdater(): UseUpdaterApi {
  const [status, setStatus] = useState<UpdateStatus>("idle");
  const [version, setVersion] = useState<string | undefined>();
  const [date, setDate] = useState<string | undefined>();
  const [body, setBody] = useState<string | undefined>();
  const [progress, setProgress] = useState<number | undefined>();
  const [error, setError] = useState<string | undefined>();

  // Hold the resolved Update object so download() can act on it without a
  // re-check. Ref because it's not render-relevant state.
  const updateRef = useRef<Update | null>(null);
  // Guard against double-invocation (rapid button presses, StrictMode effects).
  const checkingRef = useRef(false);
  const downloadingRef = useRef(false);

  const check = useCallback(async (): Promise<UpdateStatus> => {
    if (checkingRef.current) return status;
    checkingRef.current = true;
    setStatus("checking");
    setError(undefined);
    let result: UpdateStatus = "idle";
    try {
      const update = await checkForUpdate();
      if (update) {
        updateRef.current = update;
        setVersion(update.version);
        setDate(update.date);
        setBody(update.body);
        setStatus("available");
        result = "available";
      } else {
        updateRef.current = null;
        setVersion(undefined);
        setDate(undefined);
        setBody(undefined);
        setStatus("idle");
        result = "idle";
      }
    } catch (e) {
      updateRef.current = null;
      setError(toErrorMsg(e));
      setStatus("error");
      result = "error";
    } finally {
      checkingRef.current = false;
    }
    return result;
  }, []);

  const download = useCallback(async () => {
    if (downloadingRef.current) return;
    const update = updateRef.current;
    if (!update) return; // no-op unless an update is available
    downloadingRef.current = true;
    setStatus("downloading");
    setProgress(0);
    setError(undefined);
    try {
      let total = 0;
      let downloaded = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            break;
          case "Progress":
            downloaded += event.data.chunkLength ?? 0;
            if (total > 0) {
              setProgress(Math.min(100, Math.round((downloaded / total) * 100)));
            }
            break;
          case "Finished":
            setProgress(100);
            break;
        }
      });
      // downloadAndInstall has written the new bundle; await relaunch.
      setStatus("ready");
    } catch (e) {
      setError(toErrorMsg(e));
      setStatus("error");
    } finally {
      downloadingRef.current = false;
    }
  }, []);

  const install = useCallback(async () => {
    if (status !== "ready") return; // no-op unless downloaded
    try {
      await relaunch();
    } catch (e) {
      setError(toErrorMsg(e));
      setStatus("error");
    }
  }, [status]);

  const dismiss = useCallback(() => {
    setStatus("idle");
    setVersion(undefined);
    setDate(undefined);
    setBody(undefined);
    setProgress(undefined);
    setError(undefined);
    updateRef.current = null;
  }, []);

  // Auto-check on launch (once per mount). Failures are silent here — the
  // caller decides whether to surface them (auto-check: no; manual: yes).
  useEffect(() => {
    void check();
  }, [check]);

  return {
    status,
    version,
    date,
    body,
    progress,
    error,
    check,
    download,
    install,
    dismiss,
  };
}
