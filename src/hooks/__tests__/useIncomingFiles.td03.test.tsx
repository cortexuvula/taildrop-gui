// @vitest-environment jsdom
//
// TD-03 acceptance test (per repo-audititor): with a persisted custom save
// destination and DELAYED settings hydration, neither destructive route may
// fire before the destination is authoritative:
//   1. set_receive_settings must not be pushed with pre-hydration defaults
//   2. get_incoming_files (the catch-up that IS the download on CLI
//      fallback) must not run until hydration completes
// And once hydrated, exactly one push carrying the persisted directory.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useIncomingFiles } from "../useIncomingFiles";
import { DEFAULT_SETTINGS } from "../useSettings";
import type { AppSettings } from "../../types";

// The Tauri IPC/event surface — every call is recorded, none touch a real
// backend. Tauri APIs are ESM singletons; mock at the module level.
const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
  }),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: async () => false,
  requestPermission: async () => "denied",
  sendNotification: () => {},
}));

const PERSISTED_DIR = "/Users/tester/Documents/Inbox";

const NOOP_APPEND = () => {};

function makeSettings(overrides: Partial<AppSettings> = {}): AppSettings {
  return { ...DEFAULT_SETTINGS, saveDirectory: PERSISTED_DIR, ...overrides };
}

describe("TD-03: receive is gated until settings hydration", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd: string) => {
      // get_incoming_files returns an empty inbox; anything else is inert.
      if (cmd === "get_incoming_files") return [];
      return undefined;
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("pushes nothing and fetches nothing while dehydrated", async () => {
    // CRITICAL: appendTransfers must be referentially stable, exactly as
    // useTailscale provides it — an inline arrow re-triggers the effect
    // chain every render and loops (applyIncoming → refreshIncoming →
    // setState → re-render). Production memoizes it; the test must too.
    // Defined once at test scope: stable identity across renders (what
    // useTailscale's useCallback provides in production).
    const appendTransfers = NOOP_APPEND;
    const settingsRef = { current: DEFAULT_SETTINGS };
    const { rerender } = renderHook(
      ({ hydrated, settings }: { hydrated: boolean; settings: AppSettings }) =>
        useIncomingFiles({
          settings,
          settingsRef: settingsRef as never,
          hydrated,
          appendTransfers,
        }),
      { initialProps: { hydrated: false, settings: DEFAULT_SETTINGS } },
    );

    // Let every mount effect and microtask queue drain.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });

    const calls = invokeMock.mock.calls.map((c) => c[0] as string);
    expect(calls).not.toContain("set_receive_settings");
    expect(calls).not.toContain("get_incoming_files");

    // Hydration lands with the persisted directory → exactly one push with
    // the authoritative path, and the catch-up may run now.
    const persisted = makeSettings();
    settingsRef.current = persisted;
    rerender({ hydrated: true, settings: persisted });
    await waitFor(() => {
      expect(
        invokeMock.mock.calls.some(
          (c) =>
            c[0] === "set_receive_settings" &&
            (c[1] as { saveDir?: string }).saveDir === PERSISTED_DIR,
        ),
      ).toBe(true);
    });
    // The pre-hydration default ("" — resolves to Downloads) was never sent.
    const pushes = invokeMock.mock.calls.filter(
      (c) => c[0] === "set_receive_settings",
    );
    expect(pushes.length).toBe(1);
    expect((pushes[0][1] as { saveDir: string }).saveDir).toBe(PERSISTED_DIR);
  });

  it("runs the catch-up fetch only after hydration", async () => {
    const appendTransfers = NOOP_APPEND;
    const persisted = makeSettings();
    const settingsRef = { current: persisted };
    const { rerender } = renderHook(
      ({ hydrated }: { hydrated: boolean }) =>
        useIncomingFiles({
          settings: persisted,
          settingsRef: settingsRef as never,
          hydrated,
          appendTransfers,
        }),
      { initialProps: { hydrated: false } },
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });
    expect(
      invokeMock.mock.calls.some((c) => c[0] === "get_incoming_files"),
    ).toBe(false);

    rerender({ hydrated: true });
    await waitFor(() => {
      expect(
        invokeMock.mock.calls.some((c) => c[0] === "get_incoming_files"),
      ).toBe(true);
    });
  });
});
