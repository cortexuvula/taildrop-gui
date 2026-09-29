import type { TransferRecord, IncomingFile } from "../types";
import { formatTime, formatSize, shortenError, statusIcon, getStatusLabel } from "../lib/format";

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
                {getStatusLabel(t.status)} · {t.direction === "sent" ? "→" : "←"}{" "}
                {/* peerName === "incoming" means the incoming-file listing didn't
                    expose a sender; don't display that sentinel as if it were real.
                    Per UI Consultant, show "Sender unavailable" rather than joining
                    on filename (ambiguous under collision renames). */}
                {t.peerName && t.peerName !== "incoming"
                  ? t.peerName
                  : "Sender unavailable"}{" "}
                · {formatTime(t.timestamp)}
              </div>
              {t.error && <div className="transfer-error" title={t.error}>{shortenError(t.error)}</div>}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
