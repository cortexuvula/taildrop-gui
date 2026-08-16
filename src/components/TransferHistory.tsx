import type { TransferRecord, IncomingFile } from "../types";
import { formatTime, formatSize, shortenError, statusIcon } from "../lib/format";

interface TransferHistoryProps {
  transfers: TransferRecord[];
  incomingFiles: IncomingFile[];
  onAcceptFile: (name: string) => void;
}

export function TransferHistory({
  transfers,
  incomingFiles,
  onAcceptFile,
}: TransferHistoryProps) {
  return (
    <div className="transfer-panel" aria-live="polite" aria-label="Transfers">
      <div className="transfer-header">
        <h3>Transfers</h3>
      </div>

      {incomingFiles.length > 0 && (
        <div className="incoming-section">
          <div className="section-label">Incoming Files</div>
          {incomingFiles.map((file) => (
            <div key={file.name} className="incoming-item">
              <div className="incoming-info">
                <span className="incoming-name">{file.name}</span>
                <span className="incoming-size">{formatSize(file.size)}</span>
              </div>
              <button
                className="btn-accept"
                onClick={() => { onAcceptFile(file.name); }}
                aria-label={`Accept ${file.name}`}
              >
                Accept
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="transfer-list">
        {transfers.length === 0 && incomingFiles.length === 0 && (
          <div className="empty-state">No transfers yet</div>
        )}
        {transfers.map((t) => (
          <div key={t.id} className={`transfer-item ${t.status}`}>
            <span className={`transfer-status ${t.status}`}>
              {statusIcon(t.status)}
            </span>
            <div className="transfer-info">
              <div className="transfer-filename">{t.filename}</div>
              <div className="transfer-meta">
                {t.direction === "sent" ? "→" : "←"} {t.peerName} ·{" "}
                {formatTime(t.timestamp)}
              </div>
              {t.error && <div className="transfer-error" title={t.error}>{shortenError(t.error)}</div>}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
