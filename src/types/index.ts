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

export interface TransferRecord {
  id: string;
  filename: string;
  peerName: string;
  direction: "sent" | "received";
  timestamp: number;
  status: "pending" | "sending" | "success" | "error";
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
