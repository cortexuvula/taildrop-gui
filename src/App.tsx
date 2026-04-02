import { useState, useEffect } from "react";
import { Sidebar } from "./components/Sidebar";
import { DropZone } from "./components/DropZone";
import { TransferHistory } from "./components/TransferHistory";
import { Settings } from "./components/Settings";
import { DebugPanel } from "./components/DebugPanel";
import { useTailscale } from "./hooks/useTailscale";
import "./App.css";

function App() {
  const {
    peers,
    visiblePeers,
    incomingFiles,
    transfers,
    settings,
    loading,
    error,
    sendFile,
    acceptFile,
    updateSettings,
  } = useTailscale();

  // Diagnostic: log state changes to console (visible in Safari Web Inspector)
  useEffect(() => {
    console.log("[taildrop] state:", {
      loading,
      error,
      totalPeers: peers.length,
      visiblePeers: visiblePeers.length,
      selfNode: peers.find((p) => p.is_self)?.dns_name ?? "none",
      samplePeer: peers.find((p) => !p.is_self),
      hiddenNodes: settings.hiddenNodes.length,
      showOffline: settings.showOfflineNodes,
      showExit: settings.showExitNodes,
    });
  }, [loading, error, peers, visiblePeers, settings]);

  // Bug #5: store only the ID, derive the peer object from current peers
  // so it stays in sync when peers refresh (online/offline, IP changes, etc.)
  const [selectedPeerId, setSelectedPeerId] = useState<string | null>(null);
  const selectedPeer = selectedPeerId
    ? visiblePeers.find((p) => p.id === selectedPeerId) ?? null
    : null;

  const [showSettings, setShowSettings] = useState(false);
  const [showDebug, setShowDebug] = useState(false);

  return (
    <div className="app">
      <Sidebar
        peers={visiblePeers}
        selectedPeer={selectedPeer}
        onSelectPeer={(peer) => setSelectedPeerId(peer.id)}
        incomingCount={incomingFiles.length}
        onShowSettings={() => setShowSettings(true)}
        onShowDebug={() => setShowDebug(true)}
      />

      <div className="main">
        {loading ? (
          <div className="loading-state">
            <div className="spinner" />
            <p>Connecting to Tailscale...</p>
          </div>
        ) : error ? (
          <div className="error-state">
            <div className="error-icon">⚠</div>
            <p>Could not connect to Tailscale</p>
            <p className="error-detail">{error}</p>
            <p className="error-hint">
              Make sure Tailscale is running and you have permission to access
              the local API socket.
            </p>
          </div>
        ) : (
          <DropZone
            selectedPeer={selectedPeer}
            onSendFiles={sendFile}
            peers={visiblePeers}
          />
        )}

        <TransferHistory
          transfers={transfers}
          incomingFiles={incomingFiles}
          onAcceptFile={acceptFile}
        />
      </div>

      {showSettings && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={updateSettings}
          onClose={() => setShowSettings(false)}
        />
      )}

      {showDebug && (
        <DebugPanel
          peers={peers}
          onClose={() => setShowDebug(false)}
        />
      )}
    </div>
  );
}

export default App;
