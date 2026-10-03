import { useState, useEffect } from "react";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { useToast } from "./ToastProvider";
import { useModalWithLabel } from "../hooks/useModal";
import { logger } from "../lib/logger";
import { ToggleSwitch } from "./ToggleSwitch";
import type { UseUpdaterApi } from "../hooks/useUpdater";
import type { Peer, AppSettings } from "../types";

interface SettingsProps {
  settings: AppSettings;
  allPeers: Peer[];
  onUpdate: (update: Partial<AppSettings>) => void;
  onClose: () => void;
  updater: UseUpdaterApi;
  saveDirError?: string | null;
}

const OS_ICON: Record<string, string> = {
  windows: "🪟",
  macos: "🍎",
  darwin: "🍎",
  ios: "🍎",
  linux: "🐧",
  android: "🤖",
};

function getOsIcon(os: string): string {
  const lower = os.toLowerCase();
  for (const [key, icon] of Object.entries(OS_ICON)) {
    if (lower.includes(key)) return icon;
  }
  return "💻";
}

function deviceMatches(peer: Peer, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    peer.display_name.toLowerCase().includes(q) ||
    peer.hostname.toLowerCase().includes(q) ||
    peer.os.toLowerCase().includes(q) ||
    peer.ips.some((ip) => ip.includes(q))
  );
}

export function Settings({
  settings,
  allPeers,
  onUpdate,
  onClose,
  updater,
  saveDirError,
}: SettingsProps) {
  const [deviceSearch, setDeviceSearch] = useState("");
  const [autoStart, setAutoStart] = useState(false);
  const [autoStartBusy, setAutoStartBusy] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const toast = useToast();
  const { overlayRef, overlayProps } = useModalWithLabel(onClose, true, "modal-heading");

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
      const actual = await isEnabled();
      setAutoStart(actual);
    } finally {
      setAutoStartBusy(false);
    }
  };

  const devices = allPeers.filter((p) => !p.is_self);
  const filteredDevices = devices.filter((p) => deviceMatches(p, deviceSearch));

  const toggleHidden = (id: string) => {
    const hidden = settings.hiddenNodes.includes(id)
      ? settings.hiddenNodes.filter((h) => h !== id)
      : [...settings.hiddenNodes, id];
    onUpdate({ hiddenNodes: hidden });
  };

  const handleCheckUpdates = async () => {
    const result = await updater.check();
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
          <h2 id="modal-heading">Settings</h2>
          <button
            className="icon-btn icon-btn-close"
            onClick={onClose}
            aria-label="Close settings"
          >
            ✕
          </button>
        </div>

        <div className="settings-body">
          {/* ── Receiving ── */}
          <section className="settings-card" aria-label="Receiving settings">
            <h3 className="settings-card-heading">
              <span className="settings-card-icon" aria-hidden="true">📥</span>
              Receiving
            </h3>

            <div className="settings-field">
              <label className="settings-field-label" htmlFor="save-directory">
                Save directory
              </label>
              <div className="settings-directory-row">
                <input
                  id="save-directory"
                  type="text"
                  className="settings-input"
                  value={settings.saveDirectory}
                  onChange={(e) => onUpdate({ saveDirectory: e.target.value })}
                  placeholder="Downloads folder"
                  spellCheck={false}
                  aria-invalid={saveDirError ? true : undefined}
                  aria-describedby={saveDirError ? "save-directory-error" : undefined}
                />
                <button
                  className="btn-secondary btn-browse"
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
              {saveDirError && (
                <p className="settings-field-error" id="save-directory-error" role="alert">
                  <span className="settings-error-icon" aria-hidden="true">⚠</span>
                  {saveDirError}
                </p>
              )}
            </div>

            <ToggleSwitch
              checked={settings.autoAccept}
              onChange={(c) => onUpdate({ autoAccept: c })}
              label="Auto-accept incoming files"
            />
          </section>

          {/* ── Application ── */}
          <section className="settings-card" aria-label="Application settings">
            <h3 className="settings-card-heading">
              <span className="settings-card-icon" aria-hidden="true">⚙</span>
              Application
            </h3>

            <ToggleSwitch
              checked={settings.notifications ?? false}
              onChange={(c) => onUpdate({ notifications: c })}
              label="Desktop notifications"
            />
            <ToggleSwitch
              checked={autoStart}
              onChange={toggleAutoStart}
              disabled={autoStartBusy}
              label="Start on boot"
            />
          </section>

          {/* ── Device visibility ── */}
          <section className="settings-card" aria-label="Device visibility settings">
            <h3 className="settings-card-heading">
              <span className="settings-card-icon" aria-hidden="true">👁</span>
              Device visibility
            </h3>

            <ToggleSwitch
              checked={settings.showOfflineNodes ?? false}
              onChange={(c) => onUpdate({ showOfflineNodes: c })}
              label="Show offline nodes"
            />
            <ToggleSwitch
              checked={settings.showExitNodes ?? false}
              onChange={(c) => onUpdate({ showExitNodes: c })}
              label="Show Mullvad / exit nodes"
            />

            <p className="settings-hint">
              Toggled devices appear in the sidebar, subject to the filters above.
            </p>

            <div className="settings-field">
              <label className="settings-field-label" htmlFor="device-search">
                Search devices
              </label>
              <div className="settings-search-row">
                <input
                  id="device-search"
                  type="text"
                  className="settings-input"
                  value={deviceSearch}
                  onChange={(e) => setDeviceSearch(e.target.value)}
                  placeholder="Search devices…"
                />
                {deviceSearch && (
                  <button
                    className="settings-search-clear"
                    onClick={() => setDeviceSearch("")}
                    aria-label="Clear search"
                  >
                    ✕
                  </button>
                )}
              </div>
            </div>

            {devices.length === 0 ? (
              <p className="settings-empty">No devices discovered yet</p>
            ) : filteredDevices.length === 0 ? (
              <p className="settings-empty">No matching devices</p>
            ) : (
              <div className="settings-device-list">
                {filteredDevices.map((device) => {
                  const visible = !settings.hiddenNodes.includes(device.id);
                  return (
                    <div
                      key={`${device.public_key}:${device.id}`}
                      className="settings-device-row"
                      title={device.display_name}
                    >
                      <span className={`settings-device-dot ${device.online ? "online" : ""}`} />
                      <span className="settings-device-os">{getOsIcon(device.os)}</span>
                      <span className="settings-device-name">{device.display_name}</span>
                      <span className="settings-device-visible">Visible</span>
                      <ToggleSwitch
                        checked={visible}
                        onChange={() => toggleHidden(device.id)}
                        label={`${device.display_name} visibility`}
                        compact
                      />
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </div>

        <div className="settings-footer">
          <div className="settings-footer-left">
            <span className="settings-version">
              {appVersion ? `TailDrop v${appVersion}` : "TailDrop"}
            </span>
            <span className="settings-dot-sep" aria-hidden="true">·</span>
          </div>
          <button
            className="btn-update"
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