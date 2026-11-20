/**
 * Transfer queue rows — port of App.qml:666-798. Row states: queued /
 * running (bar, %, rate, current file) / done (skipped note) / failed
 * (error + Replace offer when kind === "exists") / cancelled. Cancel works
 * on queued+running; Dismiss on terminal states.
 *
 * All remote-supplied strings (labels, file names, errors) render as plain
 * text through React escaping — never dangerouslySetInnerHTML.
 */
import type { CopyJobSnapshot } from "../../types/browser";

export interface TransferQueueProps {
  jobs: CopyJobSnapshot[];
  /** Display name for a target ("local" → "This computer"). */
  nameFor: (target: string) => string;
  onCancel: (jobId: number) => void;
  onReplace: (job: CopyJobSnapshot) => void;
  onDismiss: (jobId: number) => void;
}

function stateGlyph(state: CopyJobSnapshot["state"]): string {
  switch (state) {
    case "done":
      return "✓";
    case "failed":
      return "✕";
    case "cancelled":
      return "–";
    default:
      return "↑";
  }
}

export function TransferQueue({ jobs, nameFor, onCancel, onReplace, onDismiss }: TransferQueueProps) {
  if (jobs.length === 0) return null;
  return (
    <div className="browser-queue" role="list" aria-label="Copy queue">
      {jobs.map((job) => {
        const dstName = nameFor(job.request.dstTarget);
        return (
          <div
            key={job.id}
            className={`browser-queue-row state-${job.state}`}
            role="listitem"
            data-testid={`queue-row-${job.id}`}
            data-state={job.state}
          >
            <span className={`queue-glyph ${job.state}`} aria-hidden="true">
              {stateGlyph(job.state)}
            </span>
            <span className="queue-label" title={`${job.label} → ${dstName}:${job.request.dstDir}`}>
              {job.label} <span className="queue-arrow">→</span> {dstName}:{job.request.dstDir}
            </span>
            {(job.state === "running" || job.state === "queued") && (
              <span className="queue-progress">
                <span
                  className="queue-bar"
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={Math.round(job.pct)}
                >
                  <span className="queue-bar-fill" style={{ width: `${Math.max(0, Math.min(100, job.pct))}%` }} />
                </span>
                <span className="queue-pct">{Math.round(job.pct)}%</span>
                {job.state === "running" && job.rate !== "" && (
                  <span className="queue-rate">{job.rate}</span>
                )}
                {job.state === "running" && job.file !== "" && (
                  <span className="queue-file">{job.file}</span>
                )}
                <button type="button" className="queue-btn" onClick={() => onCancel(job.id)}>
                  Cancel
                </button>
              </span>
            )}
            {job.state === "done" && (
              <span className="queue-done">
                Done
                {job.skipped > 0 && <span className="queue-skipped"> · {job.skipped} skipped</span>}
                <button type="button" className="queue-btn" onClick={() => onDismiss(job.id)}>
                  Dismiss
                </button>
              </span>
            )}
            {job.state === "failed" && (
              <span className="queue-failed">
                <span className="queue-error" title={job.error ?? ""}>
                  {job.error ?? "Failed"}
                </span>
                {job.kind === "exists" && (
                  <button
                    type="button"
                    className="queue-btn queue-btn-replace"
                    onClick={() => onReplace(job)}
                  >
                    Replace
                  </button>
                )}
                <button type="button" className="queue-btn" onClick={() => onDismiss(job.id)}>
                  Dismiss
                </button>
              </span>
            )}
            {job.state === "cancelled" && (
              <span className="queue-cancelled">
                Cancelled
                <button type="button" className="queue-btn" onClick={() => onDismiss(job.id)}>
                  Dismiss
                </button>
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}
