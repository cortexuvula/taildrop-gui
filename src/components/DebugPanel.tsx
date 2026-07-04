import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import type { Peer } from "../types";
import { useDebugLogs } from "../hooks/useDebugLogs";
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

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("?"));
  }, []);

  useEffect(() => {
    invoke<string>("get_env_info").then(setEnvInfo).catch(() => setEnvInfo(""));
  }, []);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [onClose]);

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
    <div className="settings-overlay" onClick={onClose}>
      <div
        className="settings-panel"
        style={{ maxWidth: 800, width: "90vw" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-header">
          <h2>🔍 Debug — Peer List</h2>
          <div style={{ display: "flex", gap: 8 }}>
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
          <div style={{ display: "flex", gap: 16, marginBottom: 12, fontSize: 13 }}>
            <span>
              <strong>Total:</strong> {debugData.totalPeers}
            </span>
            <span style={{ color: "#4caf50" }}>
              <strong>Online:</strong> {debugData.onlinePeers}
            </span>
            <span style={{ color: "#888" }}>
              <strong>Offline:</strong> {debugData.offlinePeers}
            </span>
          </div>

          <table style={{ width: "100%", fontSize: 12, borderCollapse: "collapse" }}>
            <thead>
              <tr style={{ borderBottom: "1px solid #333", textAlign: "left" }}>
                <th style={{ padding: "4px 8px" }}>Name</th>
                <th style={{ padding: "4px 8px" }}>OS</th>
                <th style={{ padding: "4px 8px" }}>Online</th>
                <th style={{ padding: "4px 8px" }}>Self</th>
                <th style={{ padding: "4px 8px" }}>IPs</th>
              </tr>
            </thead>
            <tbody>
              {peers.map((p) => (
                <tr
                  key={p.id || p.public_key}
                  style={{
                    borderBottom: "1px solid #222",
                    opacity: p.online ? 1 : 0.45,
                  }}
                >
                  <td style={{ padding: "4px 8px", fontWeight: p.is_self ? 700 : 400 }}>
                    {p.display_name}
                    {p.is_self && (
                      <span style={{ marginLeft: 4, fontSize: 10, color: "#888" }}>(you)</span>
                    )}
                  </td>
                  <td style={{ padding: "4px 8px", color: "#aaa" }}>{p.os}</td>
                  <td style={{ padding: "4px 8px" }}>
                    <span style={{ color: p.online ? "#4caf50" : "#666" }}>
                      {p.online ? "●" : "○"}
                    </span>
                  </td>
                  <td style={{ padding: "4px 8px", color: "#888" }}>
                    {p.is_self ? "yes" : "—"}
                  </td>
                  <td style={{ padding: "4px 8px", color: "#aaa", fontSize: 11 }}>
                    {p.ips.join(", ")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <div className="settings-section">
          <label className="settings-label">Raw JSON</label>
          <pre
            style={{
              background: "#111",
              border: "1px solid #333",
              borderRadius: 6,
              padding: 12,
              fontSize: 11,
              maxHeight: 220,
              overflowY: "auto",
              whiteSpace: "pre-wrap",
              wordBreak: "break-all",
              color: "#ccc",
            }}
          >
            {jsonText}
          </pre>
        </div>

        <div className="settings-section">
          <label className="settings-label">
            Logs ({logs.length})
            <span style={{ float: "right", display: "flex", gap: 8 }}>
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
