import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import type { Peer } from "../types";
import { useDebugLogs } from "../hooks/useDebugLogs";
import { useModal } from "../hooks/useModal";
import { clearFrontendLogs } from "../lib/logger";

interface DebugPanelProps {
  peers: Peer[];
  onClose: () => void;
}

export function DebugPanel({ peers, onClose }: DebugPanelProps) {
  const [copied, setCopied] = useState(false);
  const [copiedLogs, setCopiedLogs] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [envInfo, setEnvInfo] = useState("");
  const logs = useDebugLogs(true);
  const { overlayRef, overlayProps } = useModal(onClose);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("?"));
  }, []);

  useEffect(() => {
    invoke<string>("get_env_info").then(setEnvInfo).catch(() => setEnvInfo(""));
  }, []);

  const debugData = {
    timestamp: new Date().toISOString(),
    totalPeers: peers.length,
    onlinePeers: peers.filter((p) => p.online).length,
    offlinePeers: peers.filter((p) => !p.online).length,
    selfNode: peers.find((p) => p.is_self),
    peers: peers.map((p) => ({
      display_name: p.display_name,
      hostname: p.hostname,
      dns_name: p.dns_name,
      os: p.os,
      online: p.online,
      is_self: p.is_self,
      ips: p.ips,
      id: p.id.slice(0, 12) + "…",
    })),
  };

  const jsonText = JSON.stringify(debugData, null, 2);

  const handleCopy = () => {
    navigator.clipboard.writeText(jsonText).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const logsText = logs
    .map(
      (l) =>
        `${new Date(l.timestampMs).toISOString()} [${l.source[0].toUpperCase()}] ${l.level.padEnd(5)} ${l.target}: ${l.message}`,
    )
    .join("\n");

  const handleCopyLogs = () => {
    const header = `TailDrop v${appVersion} | ${envInfo}\nCaptured: ${new Date().toISOString()}\n${"=".repeat(60)}\n`;
    navigator.clipboard.writeText(header + logsText).then(() => {
      setCopiedLogs(true);
      setTimeout(() => setCopiedLogs(false), 2000);
    });
  };

  const handleClearFe = () => {
    clearFrontendLogs();
  };

  return (
    <div className="settings-overlay" ref={overlayRef} {...overlayProps} onClick={onClose}>
      <div
        className="settings-panel debug-panel"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-header">
          <h2>🔍 Debug — Peer List</h2>
          <div className="debug-header-actions">
            <button className="icon-btn" onClick={handleCopy} title="Copy JSON">
              {copied ? "✓" : "📋"}
            </button>
            <button className="icon-btn" onClick={onClose}>
              ✕
            </button>
          </div>
        </div>

        <div className="settings-section">
          <label className="settings-label">Environment</label>
          <div className="debug-env">
            TailDrop v{appVersion} | {envInfo}
          </div>
        </div>

        <div className="settings-section">
          <div className="debug-stats">
            <span>
              <strong>Total:</strong> {debugData.totalPeers}
            </span>
            <span className="online">
              <strong>Online:</strong> {debugData.onlinePeers}
            </span>
            <span className="offline">
              <strong>Offline:</strong> {debugData.offlinePeers}
            </span>
          </div>

          <table className="debug-table">
            <thead>
              <tr>
                <th className="debug-th">Name</th>
                <th className="debug-th">OS</th>
                <th className="debug-th">Online</th>
                <th className="debug-th">Self</th>
                <th className="debug-th">IPs</th>
              </tr>
            </thead>
            <tbody>
              {peers.map((p) => (
                <tr
                  key={p.id || p.public_key}
                  className={`debug-row${p.online ? "" : " offline"}`}
                >
                  <td className={`debug-td${p.is_self ? " self" : ""}`}>
                    {p.display_name}
                    {p.is_self && <span className="debug-you">(you)</span>}
                  </td>
                  <td className="debug-td muted">{p.os}</td>
                  <td className="debug-td">
                    <span className={p.online ? "debug-online-dot" : "debug-offline-dot"}>
                      {p.online ? "●" : "○"}
                    </span>
                  </td>
                  <td className="debug-td muted">{p.is_self ? "yes" : "—"}</td>
                  <td className="debug-td small">{p.ips.join(", ")}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <div className="settings-section">
          <label className="settings-label">Raw JSON</label>
          <pre className="debug-raw-json">{jsonText}</pre>
        </div>

        <div className="settings-section">
          <label className="settings-label">
            Logs ({logs.length})
            <span className="debug-header-actions">
              <button className="btn-secondary" onClick={handleClearFe}>
                Clear FE
              </button>
              <button className="btn-secondary" onClick={handleCopyLogs}>
                {copiedLogs ? "✓ Copied" : "📋 Copy logs"}
              </button>
            </span>
          </label>
          <pre className="debug-logs">{logsText}</pre>
        </div>
      </div>
    </div>
  );
}
