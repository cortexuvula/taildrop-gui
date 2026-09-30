// E2E wired-App acceptance (UI Consultant's requirement): render the PRODUCTION
// App with native integrations mocked, then drive the actual UI through:
//   Phase A: disconnected receipt backlog → mount → replay pagination →
//            receipts VISIBLE in the rendered transfer history.
//   Phase B: empty poll (zero-receipt replay) → history must NOT be erased.
//   Phase C: reconnect (focus event) → receipts still visible.
//   Phase D: Rust-shaped startup staging scan response ({dirs:[...]}) →
//            recovery banner RENDERS with filenames and both actions.
//   Phase E: Rust-shaped staging-recovery-found event payload → banner
//            renders through the event route too.
// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, waitFor, cleanup } from "@testing-library/react";
import type { TransferReceipt } from "../../types";

// ---- Mock every native integration before importing App ----
type ReceiptListener = (e: { payload: unknown }) => void;
let receiptListener: ReceiptListener | null = null;
void receiptListener;
let stagingListener: ReceiptListener | null = null;
let focusCallbacks: ((e: { payload: boolean }) => void)[] = [];

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => mockInvoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: ReceiptListener) => {
    if (event === "transfer-receipt") receiptListener = cb;
    if (event === "staging-recovery-found") stagingListener = cb;
    return () => {};
  },
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async (cb: (e: { payload: boolean }) => void) => {
      focusCallbacks.push(cb);
      return () => {};
    },
  }),
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: async () => null }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: async () => {} }));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: async () => false,
  requestPermission: async () => "denied",
  sendNotification: () => {},
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: async () => null }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: async () => {},
  disable: async () => {},
  isEnabled: async () => false,
}));

import AppWithToast from "../../App";
import { DEFAULT_SETTINGS } from "../../hooks/useSettings";

// Disconnected backlog: 75 receipts recorded while the frontend was absent.
const BACKLOG: TransferReceipt[] = Array.from({ length: 75 }, (_, i) => ({
  seq: i + 1,
  id: `recv-${i}`,
  filename: `backlog-${i}.txt`,
  savedName: `backlog-${i}.txt`,
  savedPath: `/Users/me/Documents/Custom/backlog-${i}.txt`,
  status: "saved" as const,
  timestamp: 1727654321000 + i,
  error: undefined,
  recovery: undefined,
  peerName: null,
  size: 1234,
  direction: "received" as const,
}));

// Rust-shaped staging payload (as serialized by StagingRecoveryFound).
const RUST_STAGING = {
  dirs: [
    {
      path: "/tmp/taildrop-accept-deadbeef",
      files: [{ name: "vacation-photo.jpg", size: 40960 }],
    },
  ],
};

function receiptPage(records: TransferReceipt[], next: number, hasMore: boolean) {
  return { receipts: records, nextSinceSeq: next, hasMore, reset: false };
}

let localStorageStore: Record<string, string> = {};

beforeEach(() => {
  vi.clearAllMocks();
  receiptListener = null;
  stagingListener = null;
  focusCallbacks = [];
  localStorageStore = {};
  // Persisted settings so hydration resolves immediately with a custom dir.
  const persisted = { ...DEFAULT_SETTINGS, saveDirectory: "/Users/me/Documents/Custom", autoAccept: false };
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => localStorageStore[k] ?? null,
    setItem: (k: string, v: string) => { localStorageStore[k] = v; },
    removeItem: (k: string) => { delete localStorageStore[k]; },
  });
  localStorageStore["taildrop-settings"] = JSON.stringify(persisted);

  mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "get_recent_receipts": {
        const since = ((args?.sinceSeq as number) || 0);
        const slice = BACKLOG.filter((r) => r.seq > since).slice(0, 50);
        const lastSeq = slice.length ? slice[slice.length - 1].seq : since;
        return receiptPage(slice, lastSeq, BACKLOG.some((r) => r.seq > lastSeq));
      }
      case "staging_recovery_scan":
        return RUST_STAGING;
      case "get_tailscale_status":
        return [{
        id: "self", public_key: "pk", hostname: "this-machine",
        dns_name: "this-machine.tail.ts.net.", display_name: "This Machine",
        machine_name: "this-machine", os: "macos", ips: ["100.64.0.1"],
        online: true, is_self: true, is_exit_node: false,
      }];
      case "get_incoming_files":
        return [];
      case "get_default_download_dir":
        return "/Users/me/Downloads";
      default:
        return undefined;
    }
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Wired App: disconnected receipts → visible history; staging banner renders", () => {
  it("replays the backlog paginated and shows it in the rendered transfer history; empty poll does not erase it", async () => {
    render(<AppWithToast />);

    // Phase A: after replay pagination, receipts must be VISIBLE in the DOM.
    await waitFor(
      () => {
        expect(screen.getByText("backlog-0.txt")).toBeTruthy();
        expect(screen.getByText("backlog-74.txt")).toBeTruthy();
      },
      { timeout: 4000 },
    );
    // Full backlog replayed across pages (50 + 25): both pagination pages ran.
    const replayCalls = mockInvoke.mock.calls.filter((c) => c[0] === "get_recent_receipts");
    expect(replayCalls.length).toBe(2);
    // "Show in folder" affordance rendered for a saved receipt.
    expect(screen.getAllByRole("button", { name: /Show backlog-3\.txt in folder/ }).length).toBe(1);

    // Phase B: empty poll must not erase visible history.
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_recent_receipts") return receiptPage([], 75, false);
      if (cmd === "get_tailscale_status") return [{
        id: "self", public_key: "pk", hostname: "this-machine",
        dns_name: "this-machine.tail.ts.net.", display_name: "This Machine",
        machine_name: "this-machine", os: "macos", ips: ["100.64.0.1"],
        online: true, is_self: true, is_exit_node: false,
      }];
      if (cmd === "get_incoming_files") return [];
      return undefined;
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 30)); });
    expect(screen.getByText("backlog-0.txt")).toBeTruthy();

    // Phase C: reconnect (focus) replays empty again — history survives.
    await act(async () => {
      focusCallbacks.forEach((cb) => cb({ payload: true }));
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(screen.getByText("backlog-74.txt")).toBeTruthy();
  });

  it("renders the recovery banner with filenames and both actions from the Rust-shaped scan response", async () => {
    render(<AppWithToast />);
    // Phase D: startup scan returned {dirs:[...]} — the banner must RENDER.
    await waitFor(
      () => {
        expect(screen.getByText("Recovered files found from a previous session")).toBeTruthy();
      },
      { timeout: 4000 },
    );
    expect(screen.getByText(/vacation-photo\.jpg/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Recover 1 files from staging/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Discard staged files" })).toBeTruthy();
  });

  it("renders the banner through the event route with the Rust-shaped payload", async () => {
    // Scan returns nothing; the startup EVENT delivers the payload instead.
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "staging_recovery_scan") return { dirs: [] };
      if (cmd === "get_recent_receipts") return receiptPage([], 0, false);
      if (cmd === "get_tailscale_status") return [{
        id: "self", public_key: "pk", hostname: "this-machine",
        dns_name: "this-machine.tail.ts.net.", display_name: "This Machine",
        machine_name: "this-machine", os: "macos", ips: ["100.64.0.1"],
        online: true, is_self: true, is_exit_node: false,
      }];
      if (cmd === "get_incoming_files") return [];
      return undefined;
    });
    render(<AppWithToast />);
    await act(async () => {
      stagingListener?.({ payload: RUST_STAGING });
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(screen.getByText(/vacation-photo\.jpg/)).toBeTruthy();
    expect(screen.getByText("Recovered files found from a previous session")).toBeTruthy();
  });
});
