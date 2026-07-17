import type { TransferRecord, IncomingFile } from "../types";

interface TransferHistoryProps {
  transfers: TransferRecord[];
  incomingFiles: IncomingFile[];
  onAcceptFile: (name: string) => void;
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function formatSize(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB", "PB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + " " + sizes[i];
}

function shortenError(err: string): string {
  // Extract the useful part from verbose Tailscale API errors
  const match = err.match(/Tailscale API error \([^)]+\): (.+)/);
  if (match) return match[1];
  const match2 = err.match(/tailscale file cp failed: (.+)/);
  if (match2) return match2[1];
  return err;
}

function statusIcon(status: TransferRecord["status"]): string {
  switch (status) {
    case "sending":
    case "pending":
      return "⏳";
    case "success":
      return "✓";
    case "error":
      return "✗";
  }
}

export function TransferHistory({
  transfers,
  incomingFiles,
  onAcceptFile,
}: TransferHistoryProps) {
  return (
    <div className="transfer-panel">
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
