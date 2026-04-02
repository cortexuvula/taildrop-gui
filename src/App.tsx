import { useState } from "react";
import { Sidebar } from "./components/Sidebar";
import { DropZone } from "./components/DropZone";
import { TransferHistory } from "./components/TransferHistory";
import { Settings } from "./components/Settings";
import { useTailscale } from "./hooks/useTailscale";
import type { Peer } from "./types";
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

  const [selectedPeer, setSelectedPeer] = useState<Peer | null>(null);
  const [showSettings, setShowSettings] = useState(false);

  return (
    <div className="app">
      <Sidebar
        peers={visiblePeers}
        selectedPeer={selectedPeer}
        onSelectPeer={setSelectedPeer}
        incomingCount={incomingFiles.length}
        onShowSettings={() => setShowSettings(true)}
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
            onSendFile={sendFile}
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
    </div>
  );
}

export default App;
