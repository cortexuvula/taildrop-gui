import { useState, useEffect, useCallback, useRef } from "react";
import { Sidebar } from "./components/Sidebar";
import { DropZone } from "./components/DropZone";
import { TransferHistory } from "./components/TransferHistory";
import { Settings } from "./components/Settings";
import { DebugPanel } from "./components/DebugPanel";
import { ToastProvider, useToast } from "./components/ToastProvider";
import { useTailscale, type SendErrorInfoLike } from "./hooks/useTailscale";
import { useUpdater } from "./hooks/useUpdater";
import "./App.css";

function App() {
  const toast = useToast();
  const updater = useUpdater();
  const updateToastId = useRef<string | null>(null);

  const onSendError = useCallback(
    (info: SendErrorInfoLike) => {
      const title =
        info.direction === "sent"
          ? `Send failed: ${info.filename}`
          : `Couldn't receive ${info.filename}`;
      toast.error(title, info.error);
    },
    [toast],
  );

  // Drive a single persistent toast through the update lifecycle:
  // available → downloading → ready. Dismissed on idle/error.
  useEffect(() => {
    const id = updateToastId.current;
    if (updater.status === "available") {
      updateToastId.current = toast.info(
        `TailDrop ${updater.version} is available`,
        "Click to download and install the update.",
        {
          durationMs: 0,
          action: { label: "Download & Install", onClick: () => void updater.download() },
        },
      );
    } else if (updater.status === "downloading") {
      toast.update(id, {
        title: "Downloading update…",
        message: `${updater.progress ?? 0}%`,
        action: undefined,
      });
    } else if (updater.status === "ready") {
      toast.update(id, {
        title: `Update ready — ${updater.version}`,
        message: "Relaunch to finish installing.",
        action: { label: "Relaunch now", onClick: () => void updater.install() },
      });
    } else if (updater.status === "idle" || updater.status === "error") {
      if (id) {
        toast.dismiss(id);
        updateToastId.current = null;
      }
    }
  }, [updater.status, updater.progress, updater.version, toast, updater]);

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
  } = useTailscale({ onSendError });

  // Diagnostic: log state changes in dev mode only (visible in Safari Web Inspector)
  useEffect(() => {
    if (import.meta.env.DEV) {
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
    }
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
        totalPeerCount={peers.filter((p) => !p.is_self).length}
        selectedPeer={selectedPeer}
        onSelectPeer={(peer) => setSelectedPeerId((prev) => (prev === peer.id ? null : peer.id))}
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
          updater={updater}
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

export default function AppWithToast() {
  return (
    <ToastProvider>
      <App />
    </ToastProvider>
  );
}
