import { useState, useEffect, useCallback, useRef, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "../types";
import { logger } from "../lib/logger";
import { loadStored, saveStored } from "../lib/storage";

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
}

/**
 * Owns user settings state, its localStorage persistence, and the ref mirror
 * used by other hooks for synchronous reads inside async handlers.
 */
export function useSettings(): UseSettingsResult {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);

  // Mirror settings into a ref so peer/transfer/incoming closures can read
  // the latest values without joining their dependency arrays.
  const settingsRef = useRef<AppSettings>(settings);
  settingsRef.current = settings;

  // Load settings from localStorage (with validation) and resolve the default
  // download directory on mount.
  useEffect(() => {
    const stored = loadStored<AppSettings>("taildrop-settings");
    if (stored) {
      // Validate critical fields to prevent crashes from corrupted data
      if (stored.hiddenNodes && !Array.isArray(stored.hiddenNodes)) {
        stored.hiddenNodes = [];
      }
      setSettings({ ...DEFAULT_SETTINGS, ...stored });
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

  const updateSettings = useCallback((update: Partial<AppSettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...update };
      saveStored("taildrop-settings", next);
      return next;
    });
  }, []);

  return { settings, settingsRef, updateSettings };
}
