/**
 * Typed IPC wrappers for the browser commands (src-tauri/src/browser/
 * commands.rs). Tauri maps camelCase JS args to snake_case Rust params.
 *
 * Every result passes through a sanitizer (lib/browser/guards.ts): the IPC
 * boundary is a trust boundary, and a malformed payload must degrade, never
 * crash. Errors are normalized to the typed BrowserError shape — commands
 * reject with either the serialized struct or a plain string.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  BrowserConfig,
  BrowserListing,
  BrowserPeer,
  CopyEvent,
  CopyJobSnapshot,
  CopyRequest,
  DeleteOutcome,
  KnownHostPin,
  ProbeInfo,
  ServerFlavor,
} from "../../types/browser";
import {
  sanitizeBrowserConfig,
  sanitizeCopyEvent,
  sanitizeJobs,
  sanitizeKnownHosts,
  sanitizeListing,
  sanitizePeers,
  sanitizeProbe,
  toBrowserError,
} from "./guards";

export const COPY_EVENT = "browser-copy-event";

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw toBrowserError(e);
  }
}

export async function browserPeers(): Promise<BrowserPeer[]> {
  return sanitizePeers(await call<unknown>("browser_peers"));
}

export async function browserProbe(
  host: string,
  asUser: string | null,
): Promise<ProbeInfo | null> {
  return sanitizeProbe(await call<unknown>("browser_probe", { host, asUser }));
}

export async function browserLs(
  target: string,
  path: string,
  asUser: string | null,
): Promise<BrowserListing> {
  const raw = await call<unknown>("browser_ls", { target, path, asUser });
  const listing = sanitizeListing(raw);
  if (!listing) {
    throw toBrowserError(new Error("The folder listing came back malformed"));
  }
  return listing;
}

/** Enqueue a copy; resolves with the job id (the queue is serial in Rust). */
export async function browserCopy(job: CopyRequest): Promise<number> {
  return call<number>("browser_copy", { job });
}

export async function browserCancelCopy(jobId: number): Promise<void> {
  await call<void>("browser_cancel_copy", { jobId });
}

export async function browserCopyJobs(): Promise<CopyJobSnapshot[]> {
  return sanitizeJobs(await call<unknown>("browser_copy_jobs"));
}

export async function browserMkdir(
  target: string,
  path: string,
  asUser: string | null,
): Promise<void> {
  await call<void>("browser_mkdir", { target, path, asUser });
}

export async function browserRename(
  target: string,
  oldPath: string,
  newPath: string,
  asUser: string | null,
): Promise<void> {
  // Rust params are literally `old` / `new`.
  await call<void>("browser_rename", {
    target,
    old: oldPath,
    new: newPath,
    asUser,
  });
}

export async function browserDelete(
  target: string,
  paths: string[],
  asUser: string | null,
): Promise<DeleteOutcome> {
  return call<DeleteOutcome>("browser_delete", { target, paths, asUser });
}

/** Open a file with the default app; remote files download to cache first. */
export async function browserOpen(
  target: string,
  path: string,
  asUser: string | null,
): Promise<string> {
  return call<string>("browser_open", { target, path, asUser });
}

export async function browserSetUser(
  host: string,
  user: string | null,
): Promise<void> {
  await call<void>("browser_set_user", { host, user });
}

export async function browserSetLast(host: string): Promise<void> {
  await call<void>("browser_set_last", { host });
}

export async function browserConfig(): Promise<BrowserConfig> {
  return sanitizeBrowserConfig(await call<unknown>("browser_config"));
}

/** M2a (M1 gap 4): pinned host keys across the three flavor stores. */
export async function browserKnownHosts(): Promise<KnownHostPin[]> {
  return sanitizeKnownHosts(await call<unknown>("browser_known_hosts"));
}

/** Forget one pin (rotation recovery; next connect is accept-new again). */
export async function browserForgetHostKey(
  host: string,
  flavor: ServerFlavor,
): Promise<void> {
  await call<void>("browser_forget_host_key", { host, flavor });
}

/** Subscribe to copy progress/lifecycle events (sanitized). */
export async function listenCopy(
  cb: (e: CopyEvent) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(COPY_EVENT, (ev) => {
    const parsed = sanitizeCopyEvent(ev.payload);
    if (parsed) cb(parsed);
  });
}
