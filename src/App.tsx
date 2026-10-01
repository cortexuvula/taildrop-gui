import { useState, useEffect, useCallback, useRef } from "react";
import { Sidebar } from "./components/Sidebar";
import { DropZone } from "./components/DropZone";
import { TransferHistory } from "./components/TransferHistory";
import { Settings } from "./components/Settings";
import { DebugPanel } from "./components/DebugPanel";
import { StagingRecovery } from "./components/StagingRecovery";
import { ToastProvider, useToast } from "./components/ToastProvider";
import { useTailscale, type SendErrorInfoLike } from "./hooks/useTailscale";
import { useUpdater } from "./hooks/useUpdater";
import { useReceipts } from "./hooks/useReceipts";
import { logger } from "./lib/logger";
import "./App.css";

function App() {
  const toast = useToast();
  const updater = useUpdater();
  const updateToastId = useRef<string | null>(null);

  const onSendError = useCallback(
    (info: SendErrorInfoLike) => {
      // [DEBUG-TOAST] link 2: did App's onSendError callback actually run?
      logger.debug("App", "onSendError callback RUN, calling toast.error:", info);
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
      // Only create the toast once — avoid duplicates when the effect re-runs.
      if (id) return;
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
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [updater.status, updater.progress, updater.version, toast]);

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
    saveDirError,
    pollError,
  } = useTailscale({ onSendError });

  // TD-05: durable receipt state — events + replay + recovery actions
  const {
    receipts,
    stagingDirs,
    stagingOps,
    stagingOpErrors,
    recoverStaging,
    discardStaging,
    clearStagingOpError,
    retryInbox,
    showInFolder,
  } = useReceipts({ settings });

  // Mount-time diagnostic: one summary log (not per-render noise).
  // Intentionally empty deps — we only want this on first mount.
  useEffect(() => {
    logger.debug("App", "started — online peers:", peers.filter((p) => p.online && !p.is_self).length);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Bug #5: store only the ID, derive the peer object from current peers
  // so it stays in sync when peers refresh (online/offline, IP changes, etc.)
  const [selectedPeerId, setSelectedPeerId] = useState<string | null>(null);
  const selectedPeer = selectedPeerId
    ? visiblePeers.find((p) => p.id === selectedPeerId) ?? null
    : null;

  const [showSettings, setShowSettings] = useState(false);
  const [showDebug, setShowDebug] = useState(false);
  // Review dialog for staging recovery — opened explicitly, never auto-opened.
  const [reviewOpen, setReviewOpen] = useState(false);

  // Staging recovery state — derived, null-safe. Groups keep their distinct
  // full backend paths as identity (no collapsing of look-alike file sets).
  const stagingDirList = stagingDirs?.dirs ?? [];
  const stagingDirCount = stagingDirList.length;

  return (
    <div className="app">
      <Sidebar
        peers={visiblePeers}
        totalPeerCount={peers.filter((p) => !p.is_self).length}
        selectedPeer={selectedPeer}
        onSelectPeer={(peer) => setSelectedPeerId((prev) => (prev === peer.id ? null : peer.id))}
        incomingCount={incomingFiles.length}
        onShowSettings={() => setShowSettings(true)}
        onShowDebug={import.meta.env.DEV ? () => setShowDebug(true) : undefined}
      />

      <div className={`main ${incomingFiles.length > 0 || transfers.some(t => t.status === "sending" || t.status === "receiving" || t.status === "pending") ? "has-activity" : ""}`}>
        {pollError && (
          <div className="poll-error-banner" role="alert">
            <span className="poll-error-icon" aria-hidden="true">⚠</span>
            <div>
              <strong>Can’t check for incoming files</strong>
              <div className="poll-error-detail">{pollError}</div>
              <div className="poll-error-hint">
                Make sure Tailscale is running and your save directory is reachable.
              </div>
            </div>
          </div>
        )}

        {stagingDirCount > 0 && (
          <div className="recovery-notice" role="status">
            <span className="recovery-notice-icon" aria-hidden="true">⚠</span>
            <div className="recovery-notice-body">
              <div className="recovery-notice-title">Files need attention</div>
              <div className="recovery-notice-detail">
                {stagingDirCount === 1
                  ? "1 recovery group contains files that may be incomplete."
                  : `${stagingDirCount} recovery groups contain files that may be incomplete.`}
              </div>
            </div>
            <button
              type="button"
              className="btn-review"
              onClick={() => setReviewOpen(true)}
              aria-haspopup="dialog"
            >
              Review
            </button>
          </div>
        )}

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
          receipts={receipts}
          onAcceptFile={acceptFile}
          onShowInFolder={showInFolder}
          onRetryInbox={retryInbox}
          onRecoverStaging={recoverStaging}
        />

        {reviewOpen && (
          <StagingRecovery
            dirs={stagingDirList}
            busy={stagingOps}
            errors={stagingOpErrors}
            onRecover={recoverStaging}
            onDiscard={discardStaging}
            onClearError={clearStagingOpError}
            onClose={() => setReviewOpen(false)}
          />
        )}
      </div>

      {showSettings && (
        <Settings
          settings={settings}
          allPeers={peers}
          onUpdate={updateSettings}
          onClose={() => setShowSettings(false)}
          updater={updater}
          saveDirError={saveDirError}
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
