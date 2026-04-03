import { useState } from "react";
import type { Peer } from "../types";

interface SidebarProps {
  peers: Peer[];
  totalPeerCount: number;
  selectedPeer: Peer | null;
  onSelectPeer: (peer: Peer) => void;
  incomingCount: number;
  onShowSettings: () => void;
  onShowDebug: () => void;
}

function getOsIcon(os: string): string {
  const lower = os.toLowerCase();
  if (lower.includes("windows")) return "🪟";
  if (lower.includes("macos") || lower.includes("darwin") || lower.includes("ios")) return "🍎";
  if (lower.includes("linux")) return "🐧";
  if (lower.includes("android")) return "🤖";
  return "💻";
}

export function Sidebar({
  peers,
  totalPeerCount,
  selectedPeer,
  onSelectPeer,
  incomingCount,
  onShowSettings,
  onShowDebug,
}: SidebarProps) {
  const [search, setSearch] = useState("");

  const filtered = search
    ? peers.filter((p) => {
        const q = search.toLowerCase();
        return (
          p.display_name.toLowerCase().includes(q) ||
          p.hostname.toLowerCase().includes(q) ||
          p.ips.some((ip) => ip.includes(q))
        );
      })
    : peers;

  const allOnlineCount = peers.filter((p) => p.online).length;
  const allOfflineCount = peers.filter((p) => !p.online).length;
  const onlinePeers = filtered.filter((p) => p.online);
  const offlinePeers = filtered.filter((p) => !p.online);

  return (
    <div className="sidebar">
      <div className="sidebar-header">
        <h2>Nodes</h2>
        <div className="sidebar-actions">
          {incomingCount > 0 && (
            <span className="badge">{incomingCount}</span>
          )}
          <button className="icon-btn" onClick={onShowDebug} title="Debug">
            🔍
          </button>
          <button className="icon-btn" onClick={onShowSettings} title="Settings">
            ⚙
          </button>
        </div>
      </div>

      <div className="sidebar-search">
        <div className="search-wrap">
          <input
            type="text"
            className="search-input"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search nodes..."
          />
          {search && (
            <button className="search-clear" onClick={() => setSearch("")}>
              ✕
            </button>
          )}
        </div>
      </div>

      <div className="peer-list">
        {onlinePeers.length > 0 && (
          <div className="peer-group">
            <div className="peer-group-label">
              Online ({search ? `${onlinePeers.length} of ${allOnlineCount}` : onlinePeers.length})
            </div>
            {onlinePeers.map((peer) => (
              <button
                key={peer.id || peer.public_key}
                className={`peer-card ${selectedPeer?.id === peer.id ? "selected" : ""}`}
                onClick={() => onSelectPeer(peer)}
              >
                <span className="status-dot online" />
                <span className="peer-os">{getOsIcon(peer.os)}</span>
                <div className="peer-info">
                  <div className="peer-name">{peer.display_name}</div>
                  <div className="peer-ip">{peer.ips[0] || peer.dns_name}</div>
                </div>
              </button>
            ))}
          </div>
        )}

        {offlinePeers.length > 0 && (
          <div className="peer-group">
            <div className="peer-group-label">
              Offline ({search ? `${offlinePeers.length} of ${allOfflineCount}` : offlinePeers.length})
            </div>
            {offlinePeers.map((peer) => (
              <button
                key={peer.id || peer.public_key}
                className={`peer-card offline ${selectedPeer?.id === peer.id ? "selected" : ""}`}
                onClick={() => onSelectPeer(peer)}
              >
                <span className="status-dot" />
                <span className="peer-os">{getOsIcon(peer.os)}</span>
                <div className="peer-info">
                  <div className="peer-name">{peer.display_name}</div>
                  <div className="peer-ip">{peer.ips[0] || peer.dns_name}</div>
                </div>
              </button>
            ))}
          </div>
        )}

        {peers.length === 0 && (
          <div className="empty-state">
            {totalPeerCount > 0
              ? "All nodes are hidden or offline. Check Settings."
              : "No nodes found. Is Tailscale running?"}
          </div>
        )}
      </div>
    </div>
  );
}
