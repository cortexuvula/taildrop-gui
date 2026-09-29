export interface Peer {
  id: string;
  public_key: string;
  hostname: string;
  dns_name: string;
  display_name: string;
  machine_name: string;
  os: string;
  ips: string[];
  online: boolean;
  is_self: boolean;
  is_exit_node: boolean;
}

export interface IncomingFile {
  name: string;
  size: number;
  /** Peer that sent the file, when the Tailscale API exposes it. */
  peerName?: string;
}

export type TransferStatus =
  | "pending"
  | "sending"
  | "receiving"
  | "success"
  | "error"
  | "cancelled";

export interface TransferRecord {
  id: string;
  filename: string;
  peerName: string;
  direction: "sent" | "received";
  timestamp: number;
  status: TransferStatus;
  error?: string;
  progress?: number;
}

export interface AppSettings {
  hiddenNodes: string[];
  saveDirectory: string;
  autoAccept: boolean;
  showOfflineNodes: boolean;
  showExitNodes: boolean;
  notifications: boolean;
}

/**
 * TD-05: Durable receipt record emitted by backend after a receive completes
 * or terminally fails. Merged by `id` (dedupe), not by filename — collisions
 * and double-accepts produce distinct receipts.
 */
export interface TransferReceipt {
  /** Backend-assigned, unique, strictly increasing sequence number. */
  seq: number;
  /** Backend-generated stable ID. */
  id: string;
  /** Name as it appeared in the inbox. */
  filename: string;
  /** Name it landed under (collision-resolved). */
  savedName: string;
  /** Absolute path the content actually landed at. */
  savedPath: string;
  /** Bytes on disk after completion. */
  size: number;
  /** Peer that sent the file, when the daemon exposes it. */
  peerName: string | null;
  direction: "received";
  /**
   * "salvaged": bytes recovered from a preserved staging dir after a failed
   * batch (TD05-B) — the move succeeded but download completeness could not
   * be verified. NOT a successful download; render differently and never
   * treat as verified history.
   */
  status: "saved" | "failed" | "salvaged";
  /** Present only when status === "failed". */
  error?: string;
  /** Completion time, ms since epoch. */
  timestamp: number;
  /** Present only when status === "failed"; describes recovery options. */
  recovery?: RecoveryInfo;
}

/**
 * TD-05: Recovery discriminator for failed receipts. The backend decides
 * which kind applies at failure time and re-evaluates on retry.
 * - "inbox": daemon inbox still lists the file; retry = existing accept_file.
 * - "staging": bytes preserved in staging dir; recover via recover_staging_files.
 * - "none": no recovery action available; show error detail only.
 */
export type RecoveryInfo =
  | { kind: "inbox" }
  | { kind: "staging"; stagingPath: string }
  | { kind: "none" };

/**
 * TD-05: Response shape for `get_recent_receipts` command. Cursor is a
 * monotonically increasing sequence number, not a timestamp.
 */
export interface ReceiptPage {
  receipts: TransferReceipt[];
  /** Highest seq in this page; 0 if page empty. */
  nextSinceSeq: number;
  /** True when more records remain; client must keep paging. */
  hasMore: boolean;
  /** True when sinceSeq predates retained history; re-sync from returned page. */
  reset: boolean;
}

/**
 * TD-05: Staging recovery directory emitted in `staging-recovery-found` event.
 * Backend scans temp dir for preserved `taildrop-accept-*` dirs at startup.
 */
export interface StagingRecoveryDir {
  path: string;
  files: Array<{ name: string; size: number }>;
}

export interface StagingRecoveryFoundEvent {
  dirs: StagingRecoveryDir[];
}
