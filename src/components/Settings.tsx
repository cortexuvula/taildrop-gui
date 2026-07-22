import { useState, useEffect } from "react";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { useToast } from "./ToastProvider";
import { useModal } from "../hooks/useModal";
import { logger } from "../lib/logger";
import type { UseUpdaterApi } from "../hooks/useUpdater";
import type { Peer, AppSettings } from "../types";

interface SettingsProps {
  settings: AppSettings;
  allPeers: Peer[];
  onUpdate: (update: Partial<AppSettings>) => void;
  onClose: () => void;
  updater: UseUpdaterApi;
}

export function Settings({ settings, allPeers, onUpdate, onClose, updater }: SettingsProps) {
  const [nodeSearch, setNodeSearch] = useState("");
  const [autoStart, setAutoStart] = useState(false);
  const [autoStartBusy, setAutoStartBusy] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const toast = useToast();
  const { overlayRef, overlayProps } = useModal(onClose);

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch((e) => {
        logger.warn("Settings", "Could not get app version:", e);
        setAppVersion("?");
      });
  }, []);

  useEffect(() => {
    isEnabled().then(setAutoStart);
  }, []);

  const toggleAutoStart = async (checked: boolean) => {
    if (autoStartBusy) return;
    setAutoStartBusy(true);
    try {
      if (checked) {
        await enable();
      } else {
        await disable();
      }
      setAutoStart(checked);
    } catch {
      // Revert to actual state on failure
      const actual = await isEnabled();
      setAutoStart(actual);
    } finally {
      setAutoStartBusy(false);
    }
  };
  const nonSelfPeers = allPeers.filter((p) => !p.is_self);

  const toggleHidden = (id: string) => {
    const hidden = settings.hiddenNodes.includes(id)
      ? settings.hiddenNodes.filter((h) => h !== id)
      : [...settings.hiddenNodes, id];
    onUpdate({ hiddenNodes: hidden });
  };

  const handleCheckUpdates = async () => {
    const result = await updater.check();
    // "available" is handled by App's effect (persistent toast) — no duplicate.
    if (result === "idle") {
      toast.info("You're up to date", "TailDrop is on the latest version.");
    } else if (result === "error") {
      toast.error("Couldn't check for updates", updater.error);
    }
  };

  return (
    <div className="settings-overlay" ref={overlayRef} {...overlayProps} onClick={onClose}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <h2>Settings</h2>
          <button className="icon-btn" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="settings-section">
          <label className="settings-label">Save Directory</label>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              type="text"
              className="settings-input"
              value={settings.saveDirectory}
              onChange={(e) => onUpdate({ saveDirectory: e.target.value })}
              placeholder="Downloads folder"
              style={{ flex: 1 }}
            />
            <button
              className="btn-secondary"
              onClick={async () => {
                const selected = await open({ directory: true });
                if (selected) {
                  onUpdate({ saveDirectory: selected as string });
                }
              }}
            >
              Browse
            </button>
          </div>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Auto-accept incoming files</span>
            <input
              type="checkbox"
              checked={settings.autoAccept}
              onChange={(e) => onUpdate({ autoAccept: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Desktop notifications</span>
            <input
              type="checkbox"
              checked={settings.notifications ?? false}
              onChange={(e) => onUpdate({ notifications: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Start on boot</span>
            <input
              type="checkbox"
              checked={autoStart}
              disabled={autoStartBusy}
              onChange={(e) => toggleAutoStart(e.target.checked)}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Show offline nodes</span>
            <input
              type="checkbox"
              checked={settings.showOfflineNodes ?? false}
              onChange={(e) => onUpdate({ showOfflineNodes: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Show Mullvad/exit nodes</span>
            <input
              type="checkbox"
              checked={settings.showExitNodes ?? false}
              onChange={(e) => onUpdate({ showExitNodes: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label">Node Visibility</label>
          <div className="search-wrap" style={{ marginTop: 6 }}>
            <input
              type="text"
              className="settings-input"
              value={nodeSearch}
              onChange={(e) => setNodeSearch(e.target.value)}
              placeholder="Search nodes..."
            />
            {nodeSearch && (
              <button className="search-clear" onClick={() => setNodeSearch("")}>
                ✕
              </button>
            )}
          </div>
          <div className="node-visibility-list">
            {nonSelfPeers
              .filter((p) => {
                if (!nodeSearch) return true;
                const q = nodeSearch.toLowerCase();
                return (
                  p.display_name.toLowerCase().includes(q) ||
                  p.hostname.toLowerCase().includes(q) ||
                  p.os.toLowerCase().includes(q) ||
                  p.ips.some((ip) => ip.includes(q))
                );
              })
              .map((peer) => (
                <label key={`${peer.public_key}:${peer.id}`} className="toggle-row">
                  <span>
                    {peer.display_name}
                    <span className="peer-os-small">{peer.os}</span>
                  </span>
                  <input
                    type="checkbox"
                    checked={!settings.hiddenNodes.includes(peer.id)}
                    onChange={() => toggleHidden(peer.id)}
                  />
                </label>
              ))}
            {nonSelfPeers.length === 0 && (
              <div className="empty-state">No peers discovered yet</div>
            )}
          </div>
        </div>

        <div className="settings-section settings-footer">
          <div className="settings-version">
            {appVersion ? `TailDrop v${appVersion}` : "TailDrop"}
          </div>
          <button
            className="btn-secondary"
            onClick={handleCheckUpdates}
            disabled={
              updater.status === "checking" || updater.status === "downloading"
            }
          >
            {updater.status === "checking"
              ? "Checking…"
              : updater.status === "downloading"
                ? "Downloading…"
                : "Check for updates"}
          </button>
        </div>
      </div>
    </div>
  );
}
