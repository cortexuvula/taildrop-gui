/**
 * Pure formatting utilities extracted from TransferHistory for testability.
 */

export function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatSize(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB", "PB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + " " + sizes[i];
}

export function shortenError(err: string): string {
  const match = err.match(/Tailscale API error \([^)]+\): (.+)/);
  if (match) return match[1];
  const match2 = err.match(/tailscale file cp failed: (.+)/);
  if (match2) return match2[1];
  return err;
}

export function statusIcon(status: "pending" | "sending" | "success" | "error"): string {
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
