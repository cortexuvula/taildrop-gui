/**
 * Runtime validation for browser data crossing the IPC boundary, in the
 * exact style of src/lib/guards.ts: sanitizers accept `unknown`, return
 * only well-shaped values, and never throw — a malformed row must not
 * white-screen the app.
 *
 * Also holds the CLIENT-SIDE MIRRORS of the Rust guards (new-name rules,
 * Windows write-disable heuristic). The mirrors exist for instant UI
 * feedback only; the Rust side re-validates every mutation (invariant:
 * the server is authoritative, dialogs re-check scope on confirm).
 */
import type {
  BrowserConfig,
  BrowserEntry,
  BrowserError,
  BrowserErrorKind,
  BrowserListing,
  BrowserPeer,
  CopyEvent,
  CopyJobSnapshot,
  JobState,
  ProbeInfo,
  ServerFlavor,
} from "../../types/browser";

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

const ENTRY_KINDS: readonly string[] = ["dir", "file"];
const JOB_STATES: readonly string[] = [
  "queued",
  "running",
  "done",
  "failed",
  "cancelled",
];
const ERROR_KINDS: readonly string[] = [
  "auth",
  "hostkey",
  "unreachable",
  "notfound",
  "denied",
  "exists",
  "account",
  "unsupported",
  "other",
];
const FLAVORS: readonly string[] = [
  "tailscale-ssh",
  "openssh",
  "openssh-windows",
];

export function isBrowserEntry(v: unknown): v is BrowserEntry {
  return (
    isRecord(v) &&
    isString(v.name) &&
    v.name.length > 0 &&
    ENTRY_KINDS.includes(v.type as string) &&
    isBool(v.link) &&
    isFiniteNumber(v.size) &&
    isFiniteNumber(v.mtime) &&
    isFiniteNumber(v.perm)
  );
}

export function sanitizeListing(v: unknown): BrowserListing | null {
  if (
    !isRecord(v) ||
    !isString(v.path) ||
    !Array.isArray(v.entries) ||
    !isFiniteNumber(v.skipped) ||
    !isString(v.user) ||
    !isString(v.home)
  ) {
    return null;
  }
  // Drop malformed rows rather than rejecting the whole listing; a dropped
  // row is indistinguishable from a server-side skip, which the footer
  // already reports.
  const entries = (v.entries as unknown[]).filter(isBrowserEntry);
  return {
    path: v.path,
    entries,
    skipped: Math.max(0, Math.trunc(v.skipped)),
    user: v.user,
    home: v.home,
  };
}

export function isBrowserPeer(v: unknown): v is BrowserPeer {
  return (
    isRecord(v) &&
    isString(v.host) &&
    v.host.length > 0 &&
    isString(v.name) &&
    isString(v.dns) &&
    isString(v.ip) &&
    isString(v.os) &&
    isBool(v.online) &&
    isString(v.user) &&
    isBool(v.tailscaleSsh) &&
    isBool(v.browsable) &&
    isBool(v.isSelf)
  );
}

/** Filter (never throw): bad rows are dropped, good rows pass through. */
export function sanitizePeers(v: unknown): BrowserPeer[] {
  if (!Array.isArray(v)) return [];
  return v.filter(isBrowserPeer);
}

export function isJobState(v: unknown): v is JobState {
  return isString(v) && JOB_STATES.includes(v);
}

export function sanitizeJobs(v: unknown): CopyJobSnapshot[] {
  if (!Array.isArray(v)) return [];
  const out: CopyJobSnapshot[] = [];
  for (const j of v) {
    if (
      !isRecord(j) ||
      !isFiniteNumber(j.id) ||
      !isJobState(j.state) ||
      !isString(j.label) ||
      !isFiniteNumber(j.pct) ||
      !isString(j.rate) ||
      !isString(j.file) ||
      !isFiniteNumber(j.skipped) ||
      !isRecord(j.request) ||
      !isFiniteNumber(j.created)
    ) {
      continue;
    }
    if (j.error !== undefined && !isString(j.error)) continue;
    if (
      j.kind !== undefined &&
      !(isString(j.kind) && ERROR_KINDS.includes(j.kind))
    ) {
      continue;
    }
    if (j.names !== undefined && !Array.isArray(j.names)) continue;
    out.push(j as unknown as CopyJobSnapshot);
  }
  return out;
}

export function sanitizeCopyEvent(v: unknown): CopyEvent | null {
  if (
    !isRecord(v) ||
    !isFiniteNumber(v.jobId) ||
    !(v.event === "start" || v.event === "progress" || v.event === "done")
  ) {
    return null;
  }
  if (v.pct !== undefined && !isFiniteNumber(v.pct)) return null;
  if (v.rate !== undefined && !isString(v.rate)) return null;
  if (v.file !== undefined && !isString(v.file)) return null;
  if (v.ok !== undefined && !isBool(v.ok)) return null;
  if (v.skipped !== undefined && !isFiniteNumber(v.skipped)) return null;
  if (v.cancelled !== undefined && !isBool(v.cancelled)) return null;
  if (v.error !== undefined && !isString(v.error)) return null;
  if (
    v.kind !== undefined &&
    !(isString(v.kind) && ERROR_KINDS.includes(v.kind))
  ) {
    return null;
  }
  if (v.names !== undefined && !Array.isArray(v.names)) return null;
  return v as unknown as CopyEvent;
}

export function sanitizeBrowserConfig(v: unknown): BrowserConfig {
  const fallback: BrowserConfig = { hosts: {}, last: "" };
  if (!isRecord(v)) return fallback;
  const hosts: BrowserConfig["hosts"] = {};
  if (isRecord(v.hosts)) {
    for (const [host, cfg] of Object.entries(v.hosts)) {
      if (isRecord(cfg) && (cfg.user === null || isString(cfg.user))) {
        hosts[host] = { user: cfg.user as string | null };
      }
    }
  }
  return { hosts, last: isString(v.last) ? v.last : "" };
}

export function sanitizeProbe(v: unknown): ProbeInfo | null {
  if (
    !isRecord(v) ||
    !isString(v.system) ||
    !isBool(v.posix) ||
    !isString(v.home) ||
    !(isString(v.flavor) && FLAVORS.includes(v.flavor)) ||
    !isString(v.user) ||
    !isRecord(v.extensions)
  ) {
    return null;
  }
  const extensions: Record<string, string> = {};
  for (const [k, val] of Object.entries(v.extensions)) {
    if (isString(val)) extensions[k] = val;
  }
  return {
    system: v.system,
    posix: v.posix,
    home: v.home,
    flavor: v.flavor as ServerFlavor,
    user: v.user,
    extensions,
  };
}

/**
 * Normalize an invoke() rejection into the typed error shape. Rust browser
 * commands reject with the serialized BrowserError struct; a few legacy-style
 * commands reject with a plain string.
 */
export function toBrowserError(e: unknown): BrowserError {
  if (isRecord(e) && isString(e.message)) {
    const kind: BrowserErrorKind =
      isString(e.kind) && ERROR_KINDS.includes(e.kind)
        ? (e.kind as BrowserErrorKind)
        : "other";
    const names =
      Array.isArray(e.names) && e.names.every(isString)
        ? (e.names as string[])
        : undefined;
    return { message: e.message, kind, names };
  }
  if (typeof e === "string") return { message: e, kind: "other" };
  if (e instanceof Error) return { message: e.message, kind: "other" };
  return { message: String(e), kind: "other" };
}

// ---------------------------------------------------------------------------
// Client-side mirrors of the Rust guards (UI pre-checks only — the server
// re-validates everything; see commands.rs new_name_problem call sites).
// ---------------------------------------------------------------------------

/** Mirror of guards::control_char_problem (C0 + DEL). */
export function controlCharProblem(s: string): string | null {
  for (const ch of s) {
    const c = ch.codePointAt(0) ?? 0;
    if (c < 0x20 || c === 0x7f) {
      return "contains a control character, so it can't be handled safely";
    }
  }
  return null;
}

/** Mirror of guards::new_name_problem — mkdir/rename dialog pre-check. */
export function newNameProblem(name: string): string | null {
  if (name === "") return "The name can't be empty";
  if (name.includes("/")) return "The name can't contain a slash";
  if (name === "." || name === "..") return "That name is reserved";
  return controlCharProblem(name);
}

/**
 * Windows write-disable heuristic for the UI (M1: Windows peers are
 * browse + pull only until the M3 gate). The server refuses authoritatively
 * by PROBED FLAVOR; the client only has the status `os` string before a
 * probe, so this is intentionally a hint, not a guarantee.
 */
export function isWindowsPeer(peer: Pick<BrowserPeer, "os"> | null): boolean {
  return (peer?.os ?? "").toLowerCase() === "windows";
}

/** Can the UI offer write actions (New folder / Rename / Delete / copy-INTO)
 *  for this pane target? Local always; Windows peers never in M1. */
export function canWriteTo(
  target: string,
  peer: Pick<BrowserPeer, "os"> | null | undefined,
): boolean {
  if (target === "local") return true;
  return !isWindowsPeer(peer ?? null);
}
