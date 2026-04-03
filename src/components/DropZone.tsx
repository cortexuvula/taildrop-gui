import { useState, useEffect, useCallback, useRef } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { Peer } from "../types";

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

  // Escape to close peer picker
  useEffect(() => {
    if (!showPeerPicker) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setShowPeerPicker(false);
        setPendingPaths([]);
      }
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [showPeerPicker]);

  // Tauri native drag-and-drop — gives file paths directly
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "enter") {
        setIsDragging(true);
      } else if (event.payload.type === "leave") {
        setIsDragging(false);
      } else if (event.payload.type === "drop") {
        setIsDragging(false);
        const paths = event.payload.paths;
        if (paths && paths.length > 0) {
          processRef.current?.(paths);
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
        <div className="peer-picker-overlay" onClick={() => setShowPeerPicker(false)}>
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
