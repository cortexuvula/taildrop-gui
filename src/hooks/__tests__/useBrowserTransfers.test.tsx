// @vitest-environment jsdom
/**
 * useBrowserTransfers — the Rust registry is the source of truth: progress
 * events patch rows in place, start/done events trigger a full resync, and
 * dismissed terminal rows stay hidden until they run again.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useBrowserTransfers } from "../useBrowserTransfers";
import type { CopyEvent, CopyJobSnapshot } from "../../types/browser";

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

let eventCb: ((ev: { payload: unknown }) => void) | null = null;

function job(over: Partial<CopyJobSnapshot> = {}): CopyJobSnapshot {
  return {
    id: 1,
    state: "running",
    label: "a.txt",
    pct: 10,
    rate: "1 MB/s",
    file: "a.txt",
    skipped: 0,
    request: {
      srcTarget: "local",
      srcUser: null,
      srcDir: "/",
      dstTarget: "peer1",
      dstUser: null,
      dstDir: "/home/u",
      names: ["a.txt"],
      replace: false,
    },
    created: 1,
    ...over,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  eventCb = null;
  mockListen.mockImplementation((async (_ev: string, cb: unknown) => {
    eventCb = cb as (ev: { payload: unknown }) => void;
    return () => {
      eventCb = null;
    };
  }) as unknown as typeof listen);
  mockInvoke.mockImplementation((async (cmd: string) => {
    if (cmd === "browser_copy_jobs") return [job()];
    if (cmd === "browser_copy") return 7;
    return null;
  }) as unknown as typeof invoke);
});

function emit(payload: CopyEvent) {
  expect(eventCb, "listener registered").not.toBeNull();
  act(() => {
    eventCb!({ payload });
  });
}

describe("useBrowserTransfers", () => {
  it("snapshots the registry on mount", async () => {
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    expect(result.current.jobs[0].id).toBe(1);
  });

  it("progress events patch the row in place (no refetch)", async () => {
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    const callsBefore = mockInvoke.mock.calls.length;
    emit({ jobId: 1, event: "progress", pct: 55, rate: "2 MB/s", file: "b.bin" });
    await waitFor(() => expect(result.current.jobs[0].pct).toBe(55));
    expect(result.current.jobs[0].rate).toBe("2 MB/s");
    expect(result.current.jobs[0].file).toBe("b.bin");
    expect(mockInvoke.mock.calls.length).toBe(callsBefore); // in-place, no resync
  });

  it("done events trigger a registry resync (self-heals missed events)", async () => {
    mockInvoke.mockImplementation((async (cmd: string) => {
      if (cmd === "browser_copy_jobs") return [job({ state: "done", pct: 100 })];
      return null;
    }) as unknown as typeof invoke);
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    emit({ jobId: 1, event: "done", ok: true, skipped: 0 });
    await waitFor(() => expect(result.current.jobs[0].state).toBe("done"));
  });

  it("ignores malformed events (sanitizer guard)", async () => {
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    emit({ jobId: "bad", event: "progress" } as unknown as CopyEvent);
    emit({ jobId: 1, event: "explode" } as unknown as CopyEvent);
    expect(result.current.jobs[0].pct).toBe(10); // untouched
  });

  it("startCopy enqueues and resyncs; cancel and replace call through", async () => {
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    const req = job().request;
    const id = await act(async () => result.current.startCopy(req));
    expect(id).toBe(7);
    expect(mockInvoke).toHaveBeenCalledWith("browser_copy", { job: req });

    await act(async () => result.current.cancel(1));
    expect(mockInvoke).toHaveBeenCalledWith("browser_cancel_copy", { jobId: 1 });

    await act(async () => result.current.replaceJob(job()));
    expect(mockInvoke).toHaveBeenCalledWith("browser_copy", {
      job: { ...req, replace: true },
    });
  });

  it("dismiss never hides a running row", async () => {
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    expect(result.current.jobs[0].state).toBe("running");
    act(() => result.current.dismiss(1));
    expect(result.current.jobs).toHaveLength(1); // still visible
  });

  it("dismiss hides terminal (done) rows", async () => {
    mockInvoke.mockImplementation((async (cmd: string) => {
      if (cmd === "browser_copy_jobs") return [job({ state: "done", pct: 100 })];
      return null;
    }) as unknown as typeof invoke);
    const { result } = renderHook(() => useBrowserTransfers());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    act(() => result.current.dismiss(1));
    expect(result.current.jobs).toHaveLength(0);
  });
});
