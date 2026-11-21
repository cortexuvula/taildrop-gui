// @vitest-environment jsdom
/**
 * S-2 (owner decision): copy-finish notifications fire from the RUST pump
 * (they must survive a suspended webview), so they have to honor the
 * webview-owned "Desktop notifications" Settings toggle. useTailscale — the
 * settings owner — mirrors the toggle into the Rust browser config via
 * `browser_set_finish_notifications`: once after hydration (carrying the
 * PERSISTED value, never pre-hydration defaults — TD-03) and on every
 * change. These tests pin that wiring; the Rust-side gate
 * (notify_enabled → fire/suppress) is pinned in commands.rs tripwire +
 * mod.rs ConfigStore tests.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor, cleanup } from "@testing-library/react";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...a: unknown[]) => invokeMock(...a),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {} }),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: async () => false,
  requestPermission: async () => "denied",
  sendNotification: () => {},
}));

import { useTailscale } from "../useTailscale";
import { DEFAULT_SETTINGS } from "../useSettings";

let store: Record<string, string> = {};

function selfStatus() {
  return [
    {
      id: "self",
      public_key: "pk",
      hostname: "this-machine",
      dns_name: "this-machine.tail.ts.net.",
      display_name: "This Machine",
      machine_name: "this-machine",
      os: "macos",
      ips: ["100.64.0.1"],
      online: true,
      is_self: true,
      is_exit_node: false,
    },
  ];
}

function notifyCalls(): Array<Record<string, unknown> | undefined> {
  return invokeMock.mock.calls
    .filter((c) => c[0] === "browser_set_finish_notifications")
    .map((c) => c[1] as Record<string, unknown> | undefined);
}

beforeEach(() => {
  vi.clearAllMocks();
  store = {};
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store[k] ?? null,
    setItem: (k: string, v: string) => {
      store[k] = v;
    },
    removeItem: (k: string) => {
      delete store[k];
    },
  });
  invokeMock.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "get_tailscale_status":
        return selfStatus();
      case "get_incoming_files":
        return [];
      case "get_default_download_dir":
        return "/Users/me/Downloads";
      default:
        // validate_save_dir, browser_set_finish_notifications, ...
        return undefined;
    }
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("S-2: Desktop-notifications toggle mirrors to Rust", () => {
  it("startup push carries the PERSISTED toggle value (true)", async () => {
    store["taildrop-settings"] = JSON.stringify({
      ...DEFAULT_SETTINGS,
      notifications: true,
    });
    renderHook(() => useTailscale());
    await waitFor(() => expect(notifyCalls().length).toBeGreaterThanOrEqual(1));
    // TD-03: the FIRST push already carries the persisted value — hydration
    // gated, so defaults never race the stored toggle into Rust.
    expect(notifyCalls()[0]).toEqual({ enabled: true });
  });

  it("startup push defaults to false, and every toggle change re-pushes", async () => {
    const { result } = renderHook(() => useTailscale());
    await waitFor(() => expect(notifyCalls().length).toBeGreaterThanOrEqual(1));
    expect(notifyCalls()[0]).toEqual({ enabled: false });

    act(() => result.current.updateSettings({ notifications: true }));
    await waitFor(() =>
      expect(notifyCalls().some((c) => c?.enabled === true)).toBe(true),
    );

    act(() => result.current.updateSettings({ notifications: false }));
    await waitFor(() =>
      expect(
        notifyCalls().filter((c) => c?.enabled === false).length,
      ).toBeGreaterThanOrEqual(2),
    );
  });
});
