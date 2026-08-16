/**
 * Runtime validation for data crossing trust boundaries: localStorage
 * (may be corrupted or from an older schema) and Tauri IPC results
 * (untyped `invoke<T>` casts — a malformed row could white-screen the app).
 *
 * Each `sanitize*` function accepts `unknown` and returns only well-shaped
 * values, dropping or resetting anything invalid instead of throwing.
 */
import type {
  AppSettings,
  IncomingFile,
  Peer,
  TransferRecord,
  TransferStatus,
} from "../types";

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isString(v: unknown): v is string {
  return typeof v === "string";
}

function isBool(v: unknown): v is boolean {
  return typeof v === "boolean";
}

function isFiniteNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

const TRANSFER_STATUSES: readonly TransferStatus[] = [
  "pending",
  "sending",
  "receiving",
  "success",
  "error",
  "cancelled",
];

export function isTransferStatus(v: unknown): v is TransferStatus {
  return isString(v) && (TRANSFER_STATUSES as readonly string[]).includes(v);
}

export function isTransferRecord(v: unknown): v is TransferRecord {
  if (
    !isRecord(v) ||
    !isString(v.id) ||
    !isString(v.filename) ||
    !isString(v.peerName) ||
    (v.direction !== "sent" && v.direction !== "received") ||
    !isFiniteNumber(v.timestamp) ||
    !isTransferStatus(v.status)
  ) {
    return false;
  }
  if (v.error !== undefined && !isString(v.error)) return false;
  if (v.progress !== undefined && !isFiniteNumber(v.progress)) return false;
  return true;
}

/** Keep only well-formed transfer records; a single null/corrupt element
 * must not take the whole history (and the app) down. */
export function sanitizeTransfers(v: unknown): TransferRecord[] {
  if (!Array.isArray(v)) return [];
  return v.filter(isTransferRecord);
}

export function isPeer(v: unknown): v is Peer {
  return (
    isRecord(v) &&
    isString(v.id) &&
    isString(v.public_key) &&
    isString(v.hostname) &&
    isString(v.dns_name) &&
    isString(v.display_name) &&
    isString(v.machine_name) &&
    isString(v.os) &&
    Array.isArray(v.ips) &&
    v.ips.every(isString) &&
    isBool(v.online) &&
    isBool(v.is_self) &&
    isBool(v.is_exit_node)
  );
}

export function sanitizePeers(v: unknown): Peer[] {
  if (!Array.isArray(v)) return [];
  return v.filter(isPeer);
}

export function isIncomingFile(v: unknown): v is IncomingFile {
  return (
    isRecord(v) &&
    isString(v.name) &&
    isFiniteNumber(v.size) &&
    v.size >= 0 &&
    (v.peer_name === undefined || isString(v.peer_name))
  );
}

export function sanitizeIncomingFiles(v: unknown): IncomingFile[] {
  if (!Array.isArray(v)) return [];
  return v.filter(isIncomingFile);
}

/** Coerce arbitrary stored data to a plausible AppSettings shape. Unknown or
 * mistyped keys fall back to the provided defaults instead of crashing. */
export function sanitizeAppSettings(v: unknown, defaults: AppSettings): AppSettings {
  if (!isRecord(v)) return { ...defaults };
  const out: AppSettings = { ...defaults };
  if (Array.isArray(v.hiddenNodes) && v.hiddenNodes.every(isString)) {
    out.hiddenNodes = v.hiddenNodes;
  }
  if (isString(v.saveDirectory)) {
    out.saveDirectory = v.saveDirectory;
  }
  if (isBool(v.autoAccept)) out.autoAccept = v.autoAccept;
  if (isBool(v.showOfflineNodes)) out.showOfflineNodes = v.showOfflineNodes;
  if (isBool(v.showExitNodes)) out.showExitNodes = v.showExitNodes;
  if (isBool(v.notifications)) out.notifications = v.notifications;
  return out;
}
