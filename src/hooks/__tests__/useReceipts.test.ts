// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useReceipts } from "../useReceipts";
import type { Settings } from "../../types";

const mockInvoke = vi.fn();
const mockListen = vi.fn();

// Per-test overridable settings (distinct from Downloads to catch the saveDir bug)
const CUSTOM_SAVE_DIR = "/Users/me/Documents/TailDrop";
let testSettings: Settings = {
  saveDirectory: CUSTOM_SAVE_DIR,
  showDesktopNotifications: true,
  acceptedPeers: [],
  autoAcceptFrom: [],
  theme: "dark",
} as unknown as Settings;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => mockListen(...args),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: () => Promise.resolve(() => {}),
  }),
}));

// Capture event listeners so tests can fire them
const listeners: Record<string, (event: { payload: unknown }) => void> = {};

beforeEach(() => {
  vi.clearAllMocks();
  Object.keys(listeners).forEach((k) => delete listeners[k]);
  
  // Reset settings to default for each test
  testSettings = {
    saveDirectory: CUSTOM_SAVE_DIR,
    showDesktopNotifications: true,
    acceptedPeers: [],
    autoAcceptFrom: [],
    theme: "dark",
  } as unknown as Settings;

  // Default: listen() captures the handler and returns an unlisten fn
  mockListen.mockImplementation((event: string, handler: (e: { payload: unknown }) => void) => {
    listeners[event] = handler;
    return Promise.resolve(() => {});
  });

  // Default: invoke dispatches by command name
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === "get_recent_receipts") {
      return Promise.resolve({ receipts: [], nextSinceSeq: 0, hasMore: false, reset: false });
    }
    if (cmd === "staging_recovery_scan") {
      return Promise.resolve({ dirs: [] });
    }
    return Promise.resolve(undefined);
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useReceipts", () => {
  it("initializes with empty receipts and null stagingDirs", () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));
    expect(result.current.receipts).toEqual([]);
    expect(result.current.stagingDirs).toBeNull();
  });

  it("calls get_recent_receipts on mount", async () => {
    renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("get_recent_receipts", {
        sinceSeq: 0,
        limit: 50,
      });
    });
  });

  it("calls staging_recovery_scan on mount", async () => {
    renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("staging_recovery_scan");
    });
  });

  it("updates receipts when transfer-receipt event fires", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(mockListen).toHaveBeenCalledWith("transfer-receipt", expect.any(Function));
    });

    act(() => {
      listeners["transfer-receipt"]?.({
        payload: {
          seq: 1,
          id: "receipt-1",
          filename: "test.txt",
          savedName: "test.txt",
          savedPath: "/tmp/test.txt",
          size: 1024,
          peerName: "alice",
          direction: "received",
          status: "saved",
          timestamp: Date.now(),
        },
      });
    });

    await waitFor(() => {
      expect(result.current.receipts).toHaveLength(1);
      expect(result.current.receipts[0].filename).toBe("test.txt");
    });
  });

  it("deduplicates receipts by id (replaces with higher seq)", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(listeners["transfer-receipt"]).toBeDefined();
    });

    const base = {
      id: "receipt-1",
      filename: "test.txt",
      savedName: "test.txt",
      savedPath: "/tmp/test.txt",
      size: 1024,
      peerName: "alice",
      direction: "received" as const,
      timestamp: Date.now(),
    };

    act(() => {
      listeners["transfer-receipt"]?.({
        payload: { ...base, seq: 1, status: "saved" },
      });
    });

    await waitFor(() => {
      expect(result.current.receipts[0].status).toBe("saved");
    });

    act(() => {
      listeners["transfer-receipt"]?.({
        payload: { ...base, seq: 2, status: "salvaged" },
      });
    });

    await waitFor(() => {
      expect(result.current.receipts).toHaveLength(1);
      expect(result.current.receipts[0].status).toBe("salvaged");
      expect(result.current.receipts[0].seq).toBe(2);
    });
  });

  it("updates stagingDirs when staging-recovery-found event fires", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(listeners["staging-recovery-found"]).toBeDefined();
    });

    act(() => {
      listeners["staging-recovery-found"]?.({
        payload: {
          dirs: [
            {
              path: "/tmp/staging-1",
              files: [{ name: "partial.txt", size: 512 }],
            },
          ],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.stagingDirs).not.toBeNull();
      expect(result.current.stagingDirs!.dirs).toHaveLength(1);
      expect(result.current.stagingDirs!.dirs[0].files[0].name).toBe("partial.txt");
    });
  });

  it("calls recover_staging_files when recoverStaging is invoked", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await act(async () => {
      await result.current.recoverStaging("/tmp/staging-1");
    });

    expect(mockInvoke).toHaveBeenCalledWith("recover_staging_files", {
      path: "/tmp/staging-1",
    });
  });

  it("calls discard_staging_dir when discardStaging is invoked", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await act(async () => {
      await result.current.discardStaging("/tmp/staging-1");
    });

    expect(mockInvoke).toHaveBeenCalledWith("discard_staging_dir", {
      path: "/tmp/staging-1",
    });
  });

  it("calls show_in_folder when showInFolder is invoked", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await act(async () => {
      await result.current.showInFolder("/tmp/test.txt");
    });

    expect(mockInvoke).toHaveBeenCalledWith("show_in_folder", {
      path: "/tmp/test.txt",
    });
  });

  // Codie's blocker regression: retryInbox must send the configured saveDirectory,
  // not an empty string that would resolve to the default Downloads folder.
  it("retryInbox passes the configured saveDirectory to accept_file", async () => {
    // Override settings to a non-default directory
    testSettings = {
      ...testSettings,
      saveDirectory: "/Users/me/Documents/MyCustomDir",
    };

    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await act(async () => {
      await result.current.retryInbox("image.png");
    });

    // The critical assertion: saveDir must be the custom dir, NOT "" or Downloads
    expect(mockInvoke).toHaveBeenCalledWith("accept_file", {
      name: "image.png",
      saveDir: "/Users/me/Documents/MyCustomDir",
    });

    // Also assert it's NOT the empty string that the old code sent
    const call = mockInvoke.mock.calls.find(
      (c: unknown[]) => c[0] === "accept_file",
    );
    expect(call).toBeDefined();
    expect((call![1] as { saveDir: string }).saveDir).not.toBe("");
  });

  // UI Consultant fix: handling one staging dir must not hide remaining dirs.
  it("discardStaging removes only the handled directory, not all staging dirs", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    // Simulate two staging directories from staging-recovery-found event
    await waitFor(() => {
      expect(listeners["staging-recovery-found"]).toBeDefined();
    });

    act(() => {
      listeners["staging-recovery-found"]?.({
        payload: {
          dirs: [
            {
              path: "/tmp/staging-A",
              files: [{ name: "a.txt", size: 100 }],
            },
            {
              path: "/tmp/staging-B",
              files: [{ name: "b.txt", size: 200 }],
            },
          ],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.stagingDirs?.dirs).toHaveLength(2);
    });

    // Discard dir A
    await act(async () => {
      await result.current.discardStaging("/tmp/staging-A");
    });

    // Dir B must still be visible
    expect(result.current.stagingDirs).not.toBeNull();
    expect(result.current.stagingDirs!.dirs).toHaveLength(1);
    expect(result.current.stagingDirs!.dirs[0].path).toBe("/tmp/staging-B");
  });

  // Same fix for recover: handling one dir leaves the other visible.
  it("recoverStaging removes only the handled directory, not all staging dirs", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(listeners["staging-recovery-found"]).toBeDefined();
    });

    act(() => {
      listeners["staging-recovery-found"]?.({
        payload: {
          dirs: [
            { path: "/tmp/staging-A", files: [{ name: "a.txt", size: 100 }] },
            { path: "/tmp/staging-B", files: [{ name: "b.txt", size: 200 }] },
          ],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.stagingDirs?.dirs).toHaveLength(2);
    });

    await act(async () => {
      await result.current.recoverStaging("/tmp/staging-A");
    });

    expect(result.current.stagingDirs).not.toBeNull();
    expect(result.current.stagingDirs!.dirs).toHaveLength(1);
    expect(result.current.stagingDirs!.dirs[0].path).toBe("/tmp/staging-B");
  });

  // When the last dir is handled, stagingDirs should become null
  it("stagingDirs becomes null after handling the last directory", async () => {
    const { result } = renderHook(() => useReceipts({ settings: testSettings }));

    await waitFor(() => {
      expect(listeners["staging-recovery-found"]).toBeDefined();
    });

    act(() => {
      listeners["staging-recovery-found"]?.({
        payload: {
          dirs: [{ path: "/tmp/staging-only", files: [{ name: "x.txt", size: 50 }] }],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.stagingDirs?.dirs).toHaveLength(1);
    });

    await act(async () => {
      await result.current.discardStaging("/tmp/staging-only");
    });

    expect(result.current.stagingDirs).toBeNull();
  });
});
