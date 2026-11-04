// @vitest-environment jsdom
//
// TAILDROP-FIXES-20261104 audit regressions for useIncomingFiles:
//  - A4: a changed snapshot that arrives while an auto-accept is running must
//    be retained and reconciled (accepted or visibly pending) after the
//    active accept settles — never silently dropped.
//  - A6: notification seen-keys must reconcile against EVERY inbox snapshot,
//    including EMPTY ones, so a later distinct same-name/same-size transfer
//    notifies again while an unchanged snapshot stays suppressed.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useIncomingFiles } from "../useIncomingFiles";
import { DEFAULT_SETTINGS } from "../useSettings";
import type { AppSettings, IncomingFile } from "../../types";

const invokeMock = vi.hoisted(() => vi.fn());
const sendNotificationMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_event: string, handler: (e: { payload: unknown }) => void) => {
    listeners[String(_event)] = handler;
    return () => {};
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
  }),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: async () => true,
  requestPermission: async () => "granted",
  sendNotification: (...args: unknown[]) => sendNotificationMock(...args),
}));

// Capture backend event listeners so tests can deliver snapshots on demand.
const listeners: Record<string, (e: { payload: unknown }) => void> = {};

function file(name: string, size = 10): IncomingFile {
  return { name, size };
}

function makeSettings(overrides: Partial<AppSettings> = {}): AppSettings {
  return { ...DEFAULT_SETTINGS, ...overrides };
}

/**
 * Render the hook with a controllable settingsRef and a stable
 * appendTransfers (referentially stable — matching production useCallback).
 */
function renderIncoming(settings: AppSettings, settingsRef: { current: AppSettings }) {
  const appendTransfers = NOOP_APPEND;
  return renderHook(() =>
    useIncomingFiles({
      settings,
      settingsRef: settingsRef as never,
      hydrated: true,
      appendTransfers,
    }),
  );
}

const NOOP_APPEND = () => {};

const PERSISTED_DIR = "/Users/tester/Documents/Inbox";

describe("A4: busy-time snapshots are retained and reconciled", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    acceptResolvers.length = 0;
    Object.keys(listeners).forEach((k) => delete listeners[k]);
    sendNotificationMock.mockReset();
    // Default: empty inbox on catch-up; accept_file held until the test
    // resolves it; set_receive_settings inert.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_incoming_files") return Promise.resolve([]);
      if (cmd === "accept_file") {
        return new Promise<string>((resolve, reject) => {
          acceptResolvers.push({ resolve, reject });
        });
      }
      return Promise.resolve(undefined);
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  // A manually-resolvable accept_file queue (FIFO per invoke order).
  const acceptResolvers: Array<{
    resolve: (v: string) => void;
    reject: (e: unknown) => void;
  }> = [];

  function settleNextAccept(value: string) {
    const r = acceptResolvers.shift();
    if (r) r.resolve(value);
  }

  function rejectNextAccept(err: unknown) {
    const r = acceptResolvers.shift();
    if (r) r.reject(err);
  }

  it("accepts the busy-time file after the active accept settles (no extra events)", async () => {
    const settingsRef = { current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }) };
    const { result } = renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    // File A arrives: auto-accept starts and is HELD.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(acceptResolvers).toHaveLength(1);

    // While busy, a changed snapshot adds file B — the latest pending input.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    // B must be VISIBLY pending while the accept is still running.
    expect(result.current.incomingFiles.some((f) => f.name === "b.txt")).toBe(true);

    // Active accept settles: B must now be accepted with NO further events.
    act(() => {
      settleNextAccept("/save/a.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    const accepted = invokeMock.mock.calls
      .filter((c) => c[0] === "accept_file")
      .map((c) => (c[1] as { name: string }).name);
    expect(accepted).toContain("b.txt");
    expect(accepted[0]).toBe("a.txt");
    expect(accepted[1]).toBe("b.txt");
  });

  it("keeps the busy-time file visibly pending when auto-accept is disabled mid-flight", async () => {
    const settingsRef = { current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }) };
    const { result } = renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(acceptResolvers).toHaveLength(1);

    // While accepting A, a snapshot with B arrives (B queued while busy)…
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    // …and THEN the user disables auto-accept mid-flight.
    settingsRef.current = makeSettings({ autoAccept: false, saveDirectory: PERSISTED_DIR });

    act(() => {
      settleNextAccept("/save/a.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    // B is not lost: visibly pending, never auto-accepted after the flip.
    expect(result.current.incomingFiles.some((f) => f.name === "b.txt")).toBe(true);
    const bAccepts = invokeMock.mock.calls.filter(
      (c) => c[0] === "accept_file" && (c[1] as { name: string }).name === "b.txt",
    );
    expect(bAccepts).toHaveLength(0);
  });

  it("still accepts the busy-time file when the active accept FAILS", async () => {
    const settingsRef = { current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }) };
    renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      rejectNextAccept("boom: first accept failed");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    const accepted = invokeMock.mock.calls
      .filter((c) => c[0] === "accept_file")
      .map((c) => (c[1] as { name: string }).name);
    expect(accepted).toContain("b.txt");
    // No tight retry of the failing file within this window: exactly one
    // accept attempt for a.txt, one for b.txt (no spinner loop).
    expect(accepted.filter((n) => n === "a.txt")).toHaveLength(1);
  });
});

describe("A6: notification keys reconcile against empty snapshots", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    Object.keys(listeners).forEach((k) => delete listeners[k]);
    sendNotificationMock.mockReset();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_incoming_files") return Promise.resolve([]);
      if (cmd === "accept_file") return Promise.resolve("/save/x.txt");
      return Promise.resolve(undefined);
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("notifies once, stays suppressed while unchanged, re-notifies after an empty snapshot", async () => {
    const settingsRef = {
      current: makeSettings({ notifications: true, autoAccept: false, saveDirectory: PERSISTED_DIR }),
    };
    const { result } = renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    // 1) First arrival: one notification.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("photo.jpg", 42)] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(sendNotificationMock).toHaveBeenCalledTimes(1);
    expect(result.current.incomingFiles).toHaveLength(1);

    // 2) Unchanged snapshot: no duplicate notification.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("photo.jpg", 42)] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(sendNotificationMock).toHaveBeenCalledTimes(1);

    // 3) Empty snapshot: key must be re-armed (no notification needed).
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(sendNotificationMock).toHaveBeenCalledTimes(1);

    // 4) A DISTINCT later transfer with the same name/size: must notify again.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("photo.jpg", 42)] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(sendNotificationMock).toHaveBeenCalledTimes(2);
    const bodies = sendNotificationMock.mock.calls.map((c) => (c[0] as { body?: string }).body);
    expect(bodies).toEqual(["photo.jpg", "photo.jpg"]);
  });
});

describe("A4 rework (terminal R2): overlapping batches and empty snapshots", () => {
  // A manually-resolvable accept_file queue (FIFO per invoke order) — one
  // entry per accept_file CALL, matching the sequential marking/accept loop.
  const acceptResolvers: Array<{
    resolve: (v: string) => void;
    reject: (e: unknown) => void;
  }> = [];

  function settleNextAccept(value: string) {
    const r = acceptResolvers.shift();
    if (r) r.resolve(value);
  }

  beforeEach(() => {
    invokeMock.mockReset();
    acceptResolvers.length = 0;
    Object.keys(listeners).forEach((k) => delete listeners[k]);
    sendNotificationMock.mockReset();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_incoming_files") return Promise.resolve([]);
      if (cmd === "accept_file") {
        return new Promise<string>((resolve, reject) => {
          acceptResolvers.push({ resolve, reject });
        });
      }
      return Promise.resolve(undefined);
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("never accepts a file twice when a changed snapshot overlaps an active multi-file batch", async () => {
    const settingsRef = {
      current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }),
    };
    const { result } = renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    // Batch [A,B] starts: A's accept is issued and HELD; B is marked for the
    // batch but not yet accepted (the loop awaits A first).
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt"), file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(acceptResolvers).toHaveLength(1); // A held; B not yet invoked

    // A changed snapshot overlapping the batch adds C. The active batch owns
    // A and B, so the queued busy-time input must be [C] ONLY — B (already
    // in the in-flight batch) must never be accepted a second time, and C
    // must not wait behind a duplicate accept of B.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt"), file("b.txt"), file("c.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    // C is VISIBLY pending while the batch is still running.
    expect(result.current.incomingFiles.some((f) => f.name === "c.txt")).toBe(true);
    expect(result.current.incomingFiles.some((f) => f.name === "b.txt")).toBe(false);

    // Original batch settles: A then B (in invoke order).
    act(() => {
      settleNextAccept("/save/a.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
    act(() => {
      settleNextAccept("/save/b.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    // A fresh queued batch accepts C, never B again.
    const accepted = invokeMock.mock.calls
      .filter((c) => c[0] === "accept_file")
      .map((c) => (c[1] as { name: string }).name);
    expect(accepted).toEqual(["a.txt", "b.txt", "c.txt"]);
  });

  it("an empty latest snapshot cancels a file queued while busy (never accepts a vanished file)", async () => {
    const settingsRef = {
      current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }),
    };
    const { result } = renderIncoming(settingsRef.current, settingsRef);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(acceptResolvers).toHaveLength(1);

    // While busy, B appears: queued and visibly pending.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(result.current.incomingFiles.some((f) => f.name === "b.txt")).toBe(true);

    // The LATEST snapshot is empty: the file vanished from the inbox. The
    // stale queued input must be REPLACED by the newest (empty) snapshot.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(result.current.incomingFiles).toHaveLength(0);

    // The active accept settles: vanished B must NEVER be accepted.
    act(() => {
      settleNextAccept("/save/a.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    const accepted = invokeMock.mock.calls
      .filter((c) => c[0] === "accept_file")
      .map((c) => (c[1] as { name: string }).name);
    expect(accepted).toEqual(["a.txt"]);
  });

  it("an empty snapshot while auto-accept is TEMPORARILY off cancels the queued busy-time file even if re-enabled before A settles", async () => {
    const settingsRef = {
      current: makeSettings({ autoAccept: true, saveDirectory: PERSISTED_DIR }),
    };
    const { result, rerender } = renderHook(
      (props: { autoAccept: boolean }) => {
        const current = makeSettings({ ...settingsRef.current, autoAccept: props.autoAccept });
        settingsRef.current = current;
        return useIncomingFiles({
          settings: current,
          settingsRef: settingsRef as never,
          hydrated: true,
          appendTransfers: NOOP_APPEND,
        });
      },
      { initialProps: { autoAccept: true } },
    );

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    // Auto-accept ON: file A starts and is HELD.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("a.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(acceptResolvers).toHaveLength(1);

    // While busy, B appears: queued and visibly pending.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [file("b.txt")] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(result.current.incomingFiles.some((f) => f.name === "b.txt")).toBe(true);

    // Auto-accept is TEMPORARILY turned OFF while A is still busy…
    rerender({ autoAccept: false });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    // …then an EMPTY snapshot arrives. The busy-time queue MUST still be
    // reconciled while the setting is off: B (now vanished from the inbox)
    // cancels instead of surviving the off→on toggle.
    act(() => {
      listeners["incoming-files-changed"]?.({ payload: [] });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(result.current.incomingFiles).toHaveLength(0);

    // Re-enabled BEFORE A settles: B must not be resurrected.
    rerender({ autoAccept: true });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    act(() => {
      settleNextAccept("/save/a.txt");
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 30));
    });

    const accepted = invokeMock.mock.calls
      .filter((c) => c[0] === "accept_file")
      .map((c) => (c[1] as { name: string }).name);
    expect(accepted).toEqual(["a.txt"]);
  });
});