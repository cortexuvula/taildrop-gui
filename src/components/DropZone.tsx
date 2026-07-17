import { useState, useEffect, useCallback, useRef } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { Peer } from "../types";
import { useToast } from "./ToastProvider";
import { useModal } from "../hooks/useModal";
import { logger } from "../lib/logger";

interface DropZoneProps {
  selectedPeer: Peer | null;
  onSendFiles: (peer: Peer, filePaths: string[]) => void;
  peers: Peer[];
}

// Bug #2: Uses Tauri native drag-drop (file paths) instead of HTML5 DnD (File blobs)
// This avoids the massive JSON serialization of file data over IPC.
export function DropZone({ selectedPeer, onSendFiles, peers }: DropZoneProps) {
  const [isDragging, setIsDragging] = useState(false);
  const [pendingPaths, setPendingPaths] = useState<string[]>([]);
  const [showPeerPicker, setShowPeerPicker] = useState(false);
  const processRef = useRef<((paths: string[]) => void) | undefined>(undefined);
  const toast = useToast();
  // Keep the latest toast in a ref so the event listener (registered once on
  // mount) always calls the current one without re-registering.
  const toastRef = useRef(toast);
  toastRef.current = toast;

  const processFiles = useCallback(
    (paths: string[]) => {
      if (selectedPeer && selectedPeer.online) {
        onSendFiles(selectedPeer, paths);
      } else {
        setPendingPaths(paths);
        setShowPeerPicker(true);
      }
    },
    [selectedPeer, onSendFiles]
  );

  // Keep ref in sync for the Tauri event listener
  processRef.current = processFiles;

  // Peer picker modal: useModal handles Escape + focus, onClose clears state.
  const closePeerPicker = useCallback(() => {
    setShowPeerPicker(false);
    setPendingPaths([]);
  }, []);
  const { overlayRef: pickerRef, overlayProps: pickerProps } = useModal(
    closePeerPicker,
    showPeerPicker,
  );

  // Tauri native drag-and-drop — gives file paths directly
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      // Log state transitions only; 'over' fires ~30×/sec while hovering.
      if (event.payload.type !== "over") {
        logger.debug("DropZone", "drag event =", event.payload.type);
      }
      if (event.payload.type === "enter") {
        setIsDragging(true);
      } else if (event.payload.type === "leave") {
        setIsDragging(false);
      } else if (event.payload.type === "drop") {
        setIsDragging(false);
        const paths = event.payload.paths;
        if (paths && paths.length > 0) {
          processRef.current?.(paths);
        } else {
          // Empty drop — no sendable file paths (e.g. file from Recycle Bin,
          // or non-file content dragged in).
          logger.debug("DropZone", "empty drop → calling toast.error");
          toastRef.current.error(
            "No files to send",
            "Drop files from Finder/Explorer, not the Recycle Bin.",
          );
        }
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // File picker using Tauri dialog plugin — returns file paths
  const handleBrowse = useCallback(async () => {
    const selected = await open({ multiple: true });
    if (selected) {
      const paths = Array.isArray(selected) ? selected : [selected];
      if (paths.length > 0) {
        processFiles(paths);
      }
    }
  }, [processFiles]);

  const handlePeerSelect = useCallback(
    (peer: Peer) => {
      onSendFiles(peer, pendingPaths);
      setPendingPaths([]);
      setShowPeerPicker(false);
    },
    [pendingPaths, onSendFiles]
  );

  const onlinePeers = peers.filter((p) => p.online);

  return (
    <div className="dropzone-container">
      <div
        className={`dropzone ${isDragging ? "dragging" : ""} ${selectedPeer ? "has-target" : ""}`}
        onClick={handleBrowse}
      >
        <div className="dropzone-content">
          {isDragging ? (
            <>
              <div className="dropzone-icon">📥</div>
              <div className="dropzone-text">Drop files here</div>
            </>
          ) : selectedPeer ? (
            <>
              <div className="dropzone-icon">{selectedPeer.online ? "📤" : "⚠️"}</div>
              <div className="dropzone-text">
                {selectedPeer.online ? (
                  <>Drop files to send to <strong>{selectedPeer.display_name}</strong></>
                ) : (
                  <><strong>{selectedPeer.display_name}</strong> is offline</>
                )}
              </div>
              <div className="dropzone-hint">
                {selectedPeer.online ? "or click to browse" : "select an online node to send files"}
              </div>
            </>
          ) : (
            <>
              <div className="dropzone-icon">📁</div>
              <div className="dropzone-text">
                Drop files here to send via Taildrop
              </div>
              <div className="dropzone-hint">
                Select a node first, or drop to choose
              </div>
            </>
          )}
        </div>
      </div>

      {showPeerPicker && pendingPaths.length > 0 && (
        <div className="peer-picker-overlay" ref={pickerRef} {...pickerProps} onClick={closePeerPicker}>
          <div className="peer-picker" onClick={(e) => e.stopPropagation()}>
            <h3>Send {pendingPaths.length} file(s) to:</h3>
            <div className="peer-picker-list">
              {onlinePeers.map((peer) => (
                <button
                  key={peer.id}
                  className="peer-picker-item"
                  onClick={() => handlePeerSelect(peer)}
                >
                  <span className="status-dot online" />
                  {peer.display_name}
                  <span className="peer-picker-os">{peer.os}</span>
                </button>
              ))}
              {onlinePeers.length === 0 && (
                <div className="empty-state">No online nodes available</div>
              )}
            </div>
            <button
              className="btn-secondary"
              onClick={() => {
                setShowPeerPicker(false);
                setPendingPaths([]);
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
