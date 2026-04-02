import { useState, useCallback, useRef } from "react";
import type { Peer } from "../types";

interface DropZoneProps {
  selectedPeer: Peer | null;
  onSendFile: (peer: Peer, file: File) => void;
  peers: Peer[];
}

export function DropZone({ selectedPeer, onSendFile, peers }: DropZoneProps) {
  const [isDragging, setIsDragging] = useState(false);
  const [droppedFiles, setDroppedFiles] = useState<File[]>([]);
  const [showPeerPicker, setShowPeerPicker] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dragCounter = useRef(0);

  const handleDragEnter = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounter.current++;
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragCounter.current--;
    if (dragCounter.current === 0) {
      setIsDragging(false);
    }
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
  }, []);

  const processFiles = useCallback(
    (files: File[]) => {
      if (selectedPeer && selectedPeer.online) {
        files.forEach((f) => onSendFile(selectedPeer, f));
        setDroppedFiles([]);
        setShowPeerPicker(false);
      } else {
        setDroppedFiles(files);
        setShowPeerPicker(true);
      }
    },
    [selectedPeer, onSendFile]
  );

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setIsDragging(false);
      dragCounter.current = 0;

      const files = Array.from(e.dataTransfer.files);
      if (files.length > 0) {
        processFiles(files);
      }
    },
    [processFiles]
  );

  const handleFileSelect = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const files = Array.from(e.target.files || []);
      if (files.length > 0) {
        processFiles(files);
      }
    },
    [processFiles]
  );

  const handlePeerSelect = useCallback(
    (peer: Peer) => {
      droppedFiles.forEach((f) => onSendFile(peer, f));
      setDroppedFiles([]);
      setShowPeerPicker(false);
    },
    [droppedFiles, onSendFile]
  );

  const onlinePeers = peers.filter((p) => p.online);

  return (
    <div className="dropzone-container">
      <div
        className={`dropzone ${isDragging ? "dragging" : ""} ${selectedPeer ? "has-target" : ""}`}
        onDragEnter={handleDragEnter}
        onDragLeave={handleDragLeave}
        onDragOver={handleDragOver}
        onDrop={handleDrop}
        onClick={() => fileInputRef.current?.click()}
      >
        <input
          ref={fileInputRef}
          type="file"
          multiple
          onChange={handleFileSelect}
          style={{ display: "none" }}
        />

        <div className="dropzone-content">
          {isDragging ? (
            <>
              <div className="dropzone-icon">📥</div>
              <div className="dropzone-text">Drop files here</div>
            </>
          ) : selectedPeer ? (
            <>
              <div className="dropzone-icon">📤</div>
              <div className="dropzone-text">
                Drop files to send to <strong>{selectedPeer.hostname}</strong>
              </div>
              <div className="dropzone-hint">or click to browse</div>
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

      {showPeerPicker && droppedFiles.length > 0 && (
        <div className="peer-picker-overlay" onClick={() => setShowPeerPicker(false)}>
          <div className="peer-picker" onClick={(e) => e.stopPropagation()}>
            <h3>Send {droppedFiles.length} file(s) to:</h3>
            <div className="peer-picker-list">
              {onlinePeers.map((peer) => (
                <button
                  key={peer.id}
                  className="peer-picker-item"
                  onClick={() => handlePeerSelect(peer)}
                >
                  <span className="status-dot online" />
                  {peer.hostname}
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
                setDroppedFiles([]);
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
