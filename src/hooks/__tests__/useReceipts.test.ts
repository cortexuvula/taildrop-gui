// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useReceipts } from "../useReceipts";

const mockInvoke = vi.fn();
const mockListen = vi.fn();

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
    const { result } = renderHook(() => useReceipts());
    expect(result.current.receipts).toEqual([]);
    expect(result.current.stagingDirs).toBeNull();
  });

  it("calls get_recent_receipts on mount", async () => {
    renderHook(() => useReceipts());

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("get_recent_receipts", {
        sinceSeq: 0,
        limit: 50,
      });
    });
  });

  it("calls staging_recovery_scan on mount", async () => {
    renderHook(() => useReceipts());

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("staging_recovery_scan");
    });
  });

  it("updates receipts when transfer-receipt event fires", async () => {
    const { result } = renderHook(() => useReceipts());

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
    const { result } = renderHook(() => useReceipts());

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
    const { result } = renderHook(() => useReceipts());

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
    const { result } = renderHook(() => useReceipts());

    await act(async () => {
      await result.current.recoverStaging("/tmp/staging-1");
    });

    expect(mockInvoke).toHaveBeenCalledWith("recover_staging_files", {
      path: "/tmp/staging-1",
    });
  });

  it("calls discard_staging_dir when discardStaging is invoked", async () => {
    const { result } = renderHook(() => useReceipts());

    await act(async () => {
      await result.current.discardStaging("/tmp/staging-1");
    });

    expect(mockInvoke).toHaveBeenCalledWith("discard_staging_dir", {
      path: "/tmp/staging-1",
    });
  });

  it("calls show_in_folder when showInFolder is invoked", async () => {
    const { result } = renderHook(() => useReceipts());

    await act(async () => {
      await result.current.showInFolder("/tmp/test.txt");
    });

    expect(mockInvoke).toHaveBeenCalledWith("show_in_folder", {
      path: "/tmp/test.txt",
    });
  });
});
