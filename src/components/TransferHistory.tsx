import type { TransferRecord, IncomingFile, TransferReceipt } from "../types";
import { formatTime, formatSize, shortenError, statusIcon, getStatusLabel } from "../lib/format";

interface TransferHistoryProps {
  transfers: TransferRecord[];
  incomingFiles: IncomingFile[];
  receipts: TransferReceipt[];
  onAcceptFile: (name: string) => void;
  onShowInFolder?: (path: string) => void;
  onRetryInbox?: (name: string) => void;
  onRecoverStaging?: (path: string) => void;
}

export function TransferHistory({
  transfers,
  incomingFiles,
  receipts,
  onAcceptFile,
  onShowInFolder,
  onRetryInbox,
  onRecoverStaging,
}: TransferHistoryProps) {
  // Combine transfers and receipts, sorted by timestamp descending
  const allItems = [
    ...transfers.map((t) => ({ ...t, type: "transfer" as const })),
    ...receipts.map((r) => ({ ...r, type: "receipt" as const })),
  ].sort((a, b) => b.timestamp - a.timestamp);

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
        {allItems.length === 0 && incomingFiles.length === 0 && (
          <div className="empty-state">No transfers yet</div>
        )}
        {allItems.map((item) => {
          if (item.type === "transfer") {
            const t = item;
            return (
              <div key={t.id} className={`transfer-item ${t.status}`}>
                <span className={`transfer-status ${t.status}`}>
                  {statusIcon(t.status)}
                </span>
                <div className="transfer-info">
                  <div className="transfer-filename">{t.filename}</div>
                  <div className="transfer-meta">
                    {getStatusLabel(t.status)} · {t.direction === "sent" ? "→" : "←"}{" "}
                    {t.peerName && t.peerName !== "incoming"
                      ? t.peerName
                      : "Sender unavailable"}{" "}
                    · {formatTime(t.timestamp)}
                  </div>
                  {t.error && <div className="transfer-error" title={t.error}>{shortenError(t.error)}</div>}
                </div>
              </div>
            );
          } else {
            const r = item;
            const statusClass = r.status === "saved" ? "success" : r.status === "failed" ? "error" : "salvaged";
            const icon = r.status === "saved" ? "✓" : r.status === "failed" ? "✗" : "⚠";
            const label = r.status === "saved" ? "Saved" : r.status === "failed" ? "Failed" : "Salvaged";
            const sender = r.peerName && r.peerName !== "incoming" ? r.peerName : "Sender unavailable";

            return (
              <div key={r.id} className={`transfer-item ${statusClass}`}>
                <span className={`transfer-status ${statusClass}`}>{icon}</span>
                <div className="transfer-info">
                  <div className="transfer-filename">{r.savedName}</div>
                  <div className="transfer-meta">
                    {label} · ← {sender} · {formatTime(r.timestamp)}
                  </div>
                  {r.status === "saved" && onShowInFolder && (
                    <button
                      className="btn-link btn-show-folder"
                      onClick={() => onShowInFolder(r.savedPath)}
                      aria-label={`Show ${r.savedName} in folder`}
                    >
                      Show in folder
                    </button>
                  )}
                  {r.status === "salvaged" && (
                    <div className="transfer-warning" title="Recovered from staging; download may be incomplete">
                      ⚠ Unverified recovery — check file integrity
                    </div>
                  )}
                  {r.status === "failed" && r.error && (
                    <div className="transfer-error" title={r.error}>{shortenError(r.error)}</div>
                  )}
                  {r.status === "failed" && r.recovery && (
                    <div className="transfer-actions">
                      {r.recovery.kind === "inbox" && onRetryInbox && (
                        <button
                          className="btn-retry"
                          onClick={() => onRetryInbox(r.filename)}
                          aria-label={`Retry ${r.filename}`}
                        >
                          Retry from inbox
                        </button>
                      )}
                      {r.recovery.kind === "staging" && onRecoverStaging && (() => {
                        const rec = r.recovery;
                        return rec && rec.kind === "staging" ? (
                          <button
                            className="btn-recover"
                            onClick={() => onRecoverStaging(rec.stagingPath)}
                            aria-label={`Recover ${r.filename} from staging`}
                          >
                            Recover from staging
                          </button>
                        ) : null;
                      })()}
                    </div>
                  )}
                </div>
              </div>
            );
          }
        })}
      </div>
    </div>
  );
}
