/**
 * Browser (M1) wire types — mirror the Rust `browser` module shapes
 * (src-tauri/src/browser/mod.rs). Kept in a separate module from the
 * Taildrop types so the existing flow's types stay untouched.
 */

/** Typed error taxonomy (plugin port): drives error panes + Replace flow. */
export type BrowserErrorKind =
  | "auth"
  | "hostkey"
  | "unreachable"
  | "notfound"
  | "denied"
  | "exists"
  | "account"
  | "unsupported"
  | "other";

export interface BrowserError {
  message: string;
  kind: BrowserErrorKind;
  /** Present only when kind === "exists": the colliding names. */
  names?: string[];
  /**
   * Raw SSH_FXP_STATUS code when the error came from an SFTP status packet
   * (4 = generic FAILURE, 5 = BAD_MESSAGE, ...). Backend-internal detail
   * (B6-R Windows junction probe); the UI renders from `kind` only.
   */
  sftpStatus?: number;
}

export type EntryKind = "dir" | "file";

export interface BrowserEntry {
  name: string;
  type: EntryKind;
  link: boolean;
  size: number;
  mtime: number;
  perm: number;
}

export interface BrowserListing {
  path: string;
  entries: BrowserEntry[];
  /** Names omitted because they can't be read safely (shown in the footer). */
  skipped: number;
  /** Account the listing was made under — panes pin later actions to it. */
  user: string;
  home: string;
}

export type ServerFlavor = "tailscale-ssh" | "openssh" | "openssh-windows";

export interface ProbeInfo {
  system: string;
  posix: boolean;
  home: string;
  flavor: ServerFlavor;
  user: string;
  extensions: Record<string, string>;
}

export interface BrowserPeer {
  /** Connect target: MagicDNS short name or first Tailscale IP. */
  host: string;
  name: string;
  dns: string;
  ip: string;
  os: string;
  online: boolean;
  /** Saved per-host account ("" = unset). */
  user: string;
  /** Capability HINT only — cannot classify the serving endpoint (M0). */
  tailscaleSsh: boolean;
  /** False for phones/tablets/TVs: visible for Taildrop, not browsable. */
  browsable: boolean;
  isSelf: boolean;
}

export interface CopyRequest {
  srcTarget: string;
  srcUser: string | null;
  srcDir: string;
  dstTarget: string;
  dstUser: string | null;
  dstDir: string;
  names: string[];
  replace: boolean;
}

export type JobState = "queued" | "running" | "done" | "failed" | "cancelled";

export interface CopyJobSnapshot {
  id: number;
  state: JobState;
  label: string;
  pct: number;
  rate: string;
  file: string;
  skipped: number;
  error?: string;
  kind?: BrowserErrorKind;
  /** Colliding names when kind === "exists" — drives the row Replace button. */
  names?: string[];
  request: CopyRequest;
  created: number;
}

/** `browser-copy-event` payload (camelCase on the wire). */
export interface CopyEvent {
  jobId: number;
  event: "start" | "progress" | "done";
  count?: number;
  pct?: number;
  rate?: string;
  file?: string;
  ok?: boolean;
  skipped?: number;
  kind?: BrowserErrorKind;
  names?: string[];
  error?: string;
  cancelled?: boolean;
}

export interface DeleteOutcome {
  trashed: boolean;
  done: number;
}

export interface HostConfig {
  user?: string | null;
}

/** One pinned host key for the Known-hosts dialog (M2a, M1 gap 4). */
export interface KnownHostPin {
  host: string;
  flavor: ServerFlavor;
  /** ssh-keygen-style SHA256 fingerprint ("" when the stored pin can't be parsed). */
  fingerprint: string;
}

export interface BrowserConfig {
  hosts: Record<string, HostConfig>;
  last: string;
}

/** Pane target: the local machine or a peer host string. */
export const LOCAL_TARGET = "local";
