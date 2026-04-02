import { useState } from "react";
import type { Peer } from "../types";

interface DebugPanelProps {
  peers: Peer[];
  onClose: () => void;
}

export function DebugPanel({ peers, onClose }: DebugPanelProps) {
  const [copied, setCopied] = useState(false);

  const debugData = {
    timestamp: new Date().toISOString(),
    totalPeers: peers.length,
    onlinePeers: peers.filter((p) => p.online).length,
    offlinePeers: peers.filter((p) => !p.online).length,
    selfNode: peers.find((p) => p.is_self),
    peers: peers.map((p) => ({
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

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div
        className="settings-panel"
        style={{ maxWidth: 700, width: "90vw" }}
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
                <th style={{ padding: "4px 8px" }}>Hostname</th>
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
                    {p.hostname}
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
      </div>
    </div>
  );
}
