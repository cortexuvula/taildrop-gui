// @vitest-environment jsdom
//
// TAILDROP-FIXES-20261104 audit regressions for useReceipts:
//  - A5: the reset indication must accumulate across EVERY page of a
//    paginated replay and reconcile once the replay finishes — a stale
//    previously-displayed receipt must disappear even when the FINAL page
//    reports reset:false, while all retained rows and any legitimately
//    live receipts arriving mid-replay survive.
//  - A3: preserved-staging reconciliation must run on refocus (fallback),
//    not only at mount.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useReceipts } from "../useReceipts";
import type { AppSettings, TransferReceipt } from "../../types";

const mockInvoke = vi.hoisted(() => vi.fn());
let focusHandler: ((p: { payload: boolean }) => void) | undefined;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_event: string, handler: (e: { payload: unknown }) => void) => {
    listeners[String(_event)] = handler;
    return () => {};
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async (cb: (p: { payload: boolean }) => void) => {
      focusHandler = cb;
      return () => {};
    },
  }),
}));

const listeners: Record<string, (e: { payload: unknown }) => void> = {};

const CUSTOM_SAVE_DIR = "/Users/me/Documents/TailDrop";

function settings(): AppSettings {
  return {
    saveDirectory: CUSTOM_SAVE_DIR,
    showDesktopNotifications: true,
    acceptedPeers: [],
    autoAcceptFrom: [],
    theme: "dark",
  } as unknown as AppSettings;
}

function receipt(seq: number, name: string, status: "saved" | "failed" | "salvaged" = "saved"): TransferReceipt {
  return {
    seq,
    id: `recv-${seq}`,
    filename: name,
    savedName: name,
    savedPath: `/tmp/${name}`,
    size: 10,
    peerName: null,
    direction: "received",
    status,
    timestamp: Date.now(),
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  focusHandler = undefined;
  Object.keys(listeners).forEach((k) => delete listeners[k]);
  // Default mount behavior: empty store, no staging dirs.
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

describe("A5: paginated reset accumulation", () => {
  it("drops a previously-displayed stale receipt when only an early page resets", async () => {
    // Retained backlog: seq 4..68 (65 rows > one page of 50).
    const retained = Array.from({ length: 65 }, (_, i) => receipt(4 + i, `kept-${4 + i}.txt`));
    const page1 = retained.slice(0, 50);
    const page2 = retained.slice(50);

    const { result } = renderHook(() => useReceipts({ settings: settings() }));
    await waitFor(() => {
      expect(listeners["transfer-receipt"]).toBeDefined();
    });

    // A stale receipt was previously displayed (cursor predates pruning).
    act(() => {
      listeners["transfer-receipt"]?.({
        payload: receipt(3, "stale.txt"),
      });
    });
    await waitFor(() => {
      expect(result.current.receipts.some((r) => r.id === "recv-3")).toBe(true);
    });

    // Replay over the backlog: FIRST page reset:true, FINAL page reset:false.
    const calls = { n: 0 };
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_recent_receipts") {
        calls.n += 1;
        if (calls.n === 1) {
          return Promise.resolve({ receipts: page1, nextSinceSeq: 53, hasMore: true, reset: true });
        }
        return Promise.resolve({ receipts: page2, nextSinceSeq: 68, hasMore: false, reset: false });
      }
      if (cmd === "staging_recovery_scan") return Promise.resolve({ dirs: [] });
      return Promise.resolve(undefined);
    });

    act(() => {
      focusHandler?.({ payload: true });
    });
    await waitFor(() => {
      expect(result.current.receipts.some((r) => r.id === "recv-3")).toBe(false);
    });

    expect(result.current.receipts).toHaveLength(65);
    const seqs = result.current.receipts.map((r) => r.seq);
    expect(seqs[0]).toBe(4);
    expect(seqs[seqs.length - 1]).toBe(68);
    expect(new Set(seqs).size).toBe(65);
  });

  it("keeps a legitimately live receipt that arrives between replay pages", async () => {
    const retained = Array.from({ length: 55 }, (_, i) => receipt(10 + i, `kept-${10 + i}.txt`));
    const page1 = retained.slice(0, 50);
    const page2 = retained.slice(50);

    const { result } = renderHook(() => useReceipts({ settings: settings() }));
    await waitFor(() => {
      expect(listeners["transfer-receipt"]).toBeDefined();
    });
    act(() => {
      listeners["transfer-receipt"]?.({ payload: receipt(5, "stale.txt") });
    });
    await waitFor(() => {
      expect(result.current.receipts.some((r) => r.id === "recv-5")).toBe(true);
    });

    const calls = { n: 0 };
    let resolvePage2: ((v: unknown) => void) | undefined;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_recent_receipts") {
        calls.n += 1;
        if (calls.n === 1) {
          return Promise.resolve({ receipts: page1, nextSinceSeq: 59, hasMore: true, reset: true });
        }
        return new Promise((resolve) => {
          resolvePage2 = resolve;
        });
      }
      if (cmd === "staging_recovery_scan") return Promise.resolve({ dirs: [] });
      return Promise.resolve(undefined);
    });

    act(() => {
      focusHandler?.({ payload: true });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    // A REAL-TIME receipt (seq 200, beyond the replay cursor) arrives while
    // the replay is still paging.
    act(() => {
      listeners["transfer-receipt"]?.({ payload: receipt(200, "live.txt") });
    });

    await act(async () => {
      resolvePage2?.({ receipts: page2, nextSinceSeq: 64, hasMore: false, reset: false });
      await new Promise((r) => setTimeout(r, 10));
    });

    // Stale pre-reset row gone; all retained rows AND the live row survive.
    expect(result.current.receipts.some((r) => r.id === "recv-5")).toBe(false);
    expect(result.current.receipts.some((r) => r.id === "recv-200")).toBe(true);
    expect(result.current.receipts).toHaveLength(56);
  });
});

describe("A5 rework (terminal R3): overlapping replays must never resurrect pruned rows", () => {
  function deferredReplay() {
    const resolvers: Array<(v: unknown) => void> = [];
    const respondIndex = (index: number, payload: unknown) => {
      const r = resolvers[index];
      if (r) r(payload);
    };
    return { resolvers, respondIndex };
  }

  it("a delayed OLDER replay cannot resurrect rows pruned by a newer reset, nor regress the cursor", async () => {
    // Startup: the backend retains history; the mount replay lands a
    // legitimate NONZERO cursor (the backend does not reset on cursor 0).
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_recent_receipts") {
        return Promise.resolve({ receipts: [receipt(1, "startup.txt")], nextSinceSeq: 1, hasMore: false, reset: false });
      }
      if (cmd === "staging_recovery_scan") return Promise.resolve({ dirs: [] });
      return Promise.resolve(undefined);
    });
    const { result } = renderHook(() => useReceipts({ settings: settings() }));
    await waitFor(() => {
      expect(result.current.receipts.some((r) => r.seq === 1)).toBe(true);
    });

    // Two overlapping focus replays: the OLDER starts first and resolves
    // LAST, carrying a retention window (seq 10) that the newer replay's
    // reset PRUNES.
    const fresh = deferredReplay();
    const sinceSeqs: number[] = [];
    mockInvoke.mockImplementation((cmd: string, args?: { sinceSeq?: number }) => {
      if (cmd === "get_recent_receipts") {
        sinceSeqs.push(args?.sinceSeq ?? -1);
        return new Promise((resolve) => fresh.resolvers.push(resolve));
      }
      if (cmd === "staging_recovery_scan") return Promise.resolve({ dirs: [] });
      return Promise.resolve(undefined);
    });

    act(() => {
      focusHandler?.({ payload: true });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
    act(() => {
      focusHandler?.({ payload: true });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(sinceSeqs).toEqual([1, 1]); // both replays start from cursor 1

    // The NEWER replay (second invoke, start order) completes FIRST with a
    // reset to seq 20 — its retention window is the authoritative one.
    await act(async () => {
      fresh.respondIndex(1, { receipts: [receipt(20, "new.txt")], nextSinceSeq: 20, hasMore: false, reset: true });
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(result.current.receipts.map((r) => r.seq)).toEqual([20]);

    // The OLDER replay (first invoke) finishes LATE with an expired window:
    // seq 10 was pruned by the newer reset. It must neither resurrect the
    // row nor regress the retention cursor.
    await act(async () => {
      fresh.respondIndex(0, { receipts: [receipt(10, "old.txt")], nextSinceSeq: 10, hasMore: false, reset: true });
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(result.current.receipts.map((r) => r.seq)).toEqual([20]);

    // Cursor regression: the NEXT replay pages from the winning reset's
    // cursor (20), never back to the stale replay's 10.
    act(() => {
      focusHandler?.({ payload: true });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(sinceSeqs[sinceSeqs.length - 1]).toBe(20);
  });

  it("never applies a replay result after the hook unmounts mid-flight", async () => {
    const { unmount } = renderHook(() => useReceipts({ settings: settings() }));
    await waitFor(() => {
      expect(focusHandler).toBeDefined();
    });

    const fresh = deferredReplay();
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_recent_receipts") {
        return new Promise((resolve) => fresh.resolvers.push(resolve));
      }
      if (cmd === "staging_recovery_scan") return Promise.resolve({ dirs: [] });
      return Promise.resolve(undefined);
    });

    act(() => {
      focusHandler?.({ payload: true });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(fresh.resolvers).toHaveLength(1);

    // Unmount while the replay is in flight; resolving afterwards must not
    // update state (React logs a console.error when it does, failing the
    // test) or throw.
    unmount();
    await act(async () => {
      fresh.respondIndex(0, { receipts: [receipt(2, "late.txt")], nextSinceSeq: 2, hasMore: false, reset: false });
      await new Promise((r) => setTimeout(r, 10));
    });
  });
});

describe("A3: staging reconciliation on refocus", () => {
  it("re-scans preserved staging on window focus (fallback), not only at mount", async () => {
    const { result } = renderHook(() => useReceipts({ settings: settings() }));

    await waitFor(() => {
      expect(focusHandler).toBeDefined();
    });
    const mountScans = mockInvoke.mock.calls.filter((c) => c[0] === "staging_recovery_scan").length;
    expect(mountScans).toBeGreaterThanOrEqual(1);

    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_recent_receipts") {
        return Promise.resolve({ receipts: [], nextSinceSeq: 0, hasMore: false, reset: false });
      }
      if (cmd === "staging_recovery_scan") {
        return Promise.resolve({
          dirs: [{ path: "/tmp/taildrop-accept-abc", files: [{ name: "partial.bin", size: 512 }] }],
        });
      }
      return Promise.resolve(undefined);
    });

    act(() => {
      focusHandler?.({ payload: true });
    });

    await waitFor(() => {
      expect(result.current.stagingDirs?.dirs.some((d) => d.path === "/tmp/taildrop-accept-abc")).toBe(
        true,
      );
    });
    // The scan must have run on focus (mount scan alone would have returned []).
    const focusScans = mockInvoke.mock.calls.filter((c) => c[0] === "staging_recovery_scan").length;
    expect(focusScans).toBeGreaterThan(mountScans);
  });
});