import { useState, useEffect, useCallback, useRef, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "../types";
import { logger } from "../lib/logger";
import { toErrorMsg } from "../lib/toErrorMsg";
import { loadStored, saveStored } from "../lib/storage";
import { sanitizeAppSettings } from "../lib/guards";

export const DEFAULT_SETTINGS: AppSettings = {
  hiddenNodes: [],
  saveDirectory: "",
  autoAccept: false,
  showOfflineNodes: false,
  showExitNodes: false,
  notifications: false,
};

export interface UseSettingsResult {
  settings: AppSettings;
  settingsRef: RefObject<AppSettings>;
  updateSettings: (update: Partial<AppSettings>) => void;
  /**
   * Non-null when the configured save directory is unusable (not absolute,
   * missing, or read-only). Surfaced as a visible error in Settings so an
   * invalid saveDirectory is rejected instead of silently receiving files
   * into a broken path.
   */
  saveDirError: string | null;
}

/**
 * Owns user settings state, its localStorage persistence, and the ref mirror
 * used by other hooks for synchronous reads inside async handlers.
 */
export function useSettings(): UseSettingsResult {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [saveDirError, setSaveDirError] = useState<string | null>(null);

  // Mirror settings into a ref so peer/transfer/incoming closures can read
  // the latest values without joining their dependency arrays. Updated in an
  // effect (not during render) to stay StrictMode/concurrent-safe.
  const settingsRef = useRef<AppSettings>(DEFAULT_SETTINGS);
  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  // Load settings from localStorage (sanitized — corrupted or old-schema
  // data falls back to defaults per-key) and resolve the default download
  // directory on mount.
  useEffect(() => {
    const stored = loadStored<unknown>("taildrop-settings");
    if (stored != null) {
      setSettings(sanitizeAppSettings(stored, DEFAULT_SETTINGS));
    }
    // Get default download dir
    invoke<string>("get_default_download_dir")
      .then((dir) => {
        setSettings((prev) => ({
          ...prev,
          saveDirectory: prev.saveDirectory || dir,
        }));
      })
      .catch((e) => {
        logger.warn("useSettings", "Could not get default download dir:", e);
      });
  }, []);

  // Validate the save directory on mount and after edits (debounced while
  // typing). The backend checks absolute/existing/writable; failures are
  // surfaced to the UI instead of failing silently at receive time.
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      invoke<string>("validate_save_dir", { saveDir: settings.saveDirectory })
        .then(() => {
          if (!cancelled) setSaveDirError(null);
        })
        .catch((e) => {
          if (!cancelled) setSaveDirError(toErrorMsg(e));
        });
    }, 400);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [settings.saveDirectory]);

  const updateSettings = useCallback((update: Partial<AppSettings>) => {
    setSettings((prev) => ({ ...prev, ...update }));
  }, []);

  // Persist settings (debounced) outside the state updater so a quota error
  // can't break the state transition.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      try {
        saveStored("taildrop-settings", settings);
      } catch {
        // Settings are tiny; quota failures here are not actionable.
        logger.warn("useSettings", "failed to persist settings");
      }
    }, 300);
    return () => window.clearTimeout(timer);
  }, [settings]);

  return { settings, settingsRef, updateSettings, saveDirError };
}
