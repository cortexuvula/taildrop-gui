// @vitest-environment jsdom
/**
 * useBrowserPane — the pane state machine: serial requests with the
 * stale-result guard (FilePane.qml:110-115), account pinning, and
 * marks-else-cursor selection (FilePane.qml:318-331).
 *
 * The Tauri IPC layer is mocked at @tauri-apps/api/core so the api.ts
 * wrappers AND the guards sanitizers run for real.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
import { invoke } from "@tauri-apps/api/core";
import { useBrowserPane } from "../useBrowserPane";

const mockInvoke = vi.mocked(invoke);

function listing(path: string, names: string[], user = "") {
  return {
    path,
    entries: names.map((n) => ({
      name: n,
      type: "file",
      link: false,
      size: 1,
      mtime: 1700000000,
      perm: 0o644,
    })),
    skipped: 0,
    user,
    home: "/home/u",
  };
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const opts = {
  initialTarget: "local",
  userFor: () => null,
};

/** Row display names (up-rows keep their "..", entries live under .entry). */
function rowNames(rows: ReturnType<typeof useBrowserPane>["rows"]): string[] {
  return rows.map((r) => (r.rowKind === "entry" ? r.entry.name : r.name));
}

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("useBrowserPane loading", () => {
  it("loads the initial listing and pins the listing's user", async () => {
    mockInvoke.mockResolvedValue(listing("/", ["a.txt"], "resolved-user"));
    const { result } = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(rowNames(result.current.rows)).toEqual(["a.txt"]);
    // The listing's user is the PIN for later actions (server resolved it).
    expect(result.current.loadedUser).toBe("resolved-user");
    expect(result.current.loadedTarget).toBe("local");
    expect(result.current.loadedPath).toBe("/");
    expect(result.current.error).toBeNull();
  });

  it("records a typed error and blocks the pane on auth failures", async () => {
    mockInvoke.mockRejectedValue({ message: "denied: bad account", kind: "auth" });
    const { result } = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(result.current.error).not.toBeNull());
    expect(result.current.error!.kind).toBe("auth");
    expect(result.current.blocked).toBe(true);
  });

  it("drops a stale result: the newer load wins even if it resolves first", async () => {
    // Two loads in flight; /b resolves BEFORE /a — /a must be dropped.
    const dA = deferred<unknown>();
    const dB = deferred<unknown>();
    mockInvoke.mockImplementation(((_cmd: string, args?: Record<string, unknown>) => {
      const p = args?.path as string;
      return p === "/a" ? dA.promise : dB.promise;
    }) as unknown as typeof invoke);
    const { result } = renderHook(() => useBrowserPane({ ...opts, autoLoad: false }));
    let pA!: Promise<void>;
    let pB!: Promise<void>;
    act(() => {
      pA = result.current.load("local", "/a");
      pB = result.current.load("local", "/b");
    });
    await act(async () => {
      dB.resolve(listing("/b", ["b.txt"]));
      await pB;
    });
    expect(result.current.loadedPath).toBe("/b");
    await act(async () => {
      dA.resolve(listing("/a", ["a-stale.txt"]));
      await pA;
    });
    // The stale /a response must NOT have overwritten /b ("/b" is non-root,
    // so the up-row leads).
    expect(result.current.loadedPath).toBe("/b");
    expect(rowNames(result.current.rows)).toEqual(["..", "b.txt"]);
  });

  it("drops a stale error too", async () => {
    const dA = deferred<unknown>();
    mockInvoke.mockImplementation(((_c: string, args?: Record<string, unknown>) =>
      args?.path === "/a" ? dA.promise : Promise.resolve(listing("/b", ["b.txt"]))) as unknown as typeof invoke);
    const { result } = renderHook(() => useBrowserPane({ ...opts, autoLoad: false }));
    let pA!: Promise<void>;
    let pB!: Promise<void>;
    act(() => {
      pA = result.current.load("local", "/a");
      pB = result.current.load("local", "/b");
    });
    await act(async () => {
      await pB;
    });
    expect(result.current.error).toBeNull();
    await act(async () => {
      dA.reject({ message: "late failure", kind: "unreachable" });
      await pA;
    });
    // Stale failure must not poison the newer successful view.
    expect(result.current.error).toBeNull();
    expect(result.current.loadedPath).toBe("/b");
  });
});

describe("useBrowserPane selection (marks-else-cursor)", () => {
  async function loaded() {
    mockInvoke.mockResolvedValue(listing("/", ["a.txt", "b.txt", "c.txt"]));
    const h = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(h.result.current.loading).toBe(false));
    return h;
  }

  it("selection is the cursor row when nothing is marked", async () => {
    const { result } = await loaded();
    expect(result.current.selection).toEqual(["a.txt"]);
    act(() => result.current.moveCursor(1));
    expect(result.current.selection).toEqual(["b.txt"]);
  });

  it("marks override the cursor; clearing restores cursor selection", async () => {
    const { result } = await loaded();
    act(() => result.current.toggleMark("c.txt"));
    act(() => result.current.toggleMark("a.txt"));
    expect([...result.current.selection].sort()).toEqual(["a.txt", "c.txt"]);
    act(() => result.current.moveCursor(1));
    expect([...result.current.selection].sort()).toEqual(["a.txt", "c.txt"]);
    act(() => result.current.clearMarks());
    expect(result.current.selection).toEqual(["b.txt"]); // cursor row again
  });

  it("toggleMarkAtCursor marks and advances (Space behavior)", async () => {
    const { result } = await loaded();
    act(() => result.current.toggleMarkAtCursor(true));
    expect(result.current.marks.has("a.txt")).toBe(true);
    expect(result.current.cursor).toBe(1);
    act(() => result.current.toggleMarkAtCursor(true));
    expect(result.current.marks.has("b.txt")).toBe(true);
  });

  it("markAll marks every entry row", async () => {
    const { result } = await loaded();
    act(() => result.current.markAll());
    expect(result.current.marks.size).toBe(3);
  });

  it("an up-row appears for non-root paths and is not selectable", async () => {
    mockInvoke.mockResolvedValue(listing("/sub", ["x"]));
    const { result } = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.rows[0].rowKind).toBe("up");
    // Cursor starts on the up-row: nothing is selected (up is not a name).
    expect(result.current.selection).toEqual([]);
    act(() => result.current.setCursor(1));
    expect(result.current.selection).toEqual(["x"]);
  });
});

describe("useBrowserPane rows", () => {
  it("hidden toggle filters dotfiles; text filter narrows", async () => {
    mockInvoke.mockResolvedValue(listing("/", [".hidden", "keep.txt", "other.md"]));
    const { result } = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(rowNames(result.current.rows)).toEqual(["keep.txt", "other.md"]);
    act(() => result.current.setHidden());
    expect(result.current.rows).toHaveLength(3);
    act(() => result.current.setFilterText("keep"));
    expect(rowNames(result.current.rows)).toEqual(["keep.txt"]);
  });

  it("sort cycles name → size → mtime and flips direction", async () => {
    mockInvoke.mockImplementation(() =>
      Promise.resolve({
        path: "/",
        entries: [
          { name: "b", type: "file", link: false, size: 1, mtime: 10, perm: 420 },
          { name: "a", type: "file", link: false, size: 9, mtime: 5, perm: 420 },
        ],
        skipped: 0,
        user: "",
        home: "/",
      }),
    );
    const { result } = renderHook(() => useBrowserPane(opts));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(rowNames(result.current.rows)).toEqual(["a", "b"]); // name asc
    act(() => result.current.cycleSort()); // size asc
    expect(rowNames(result.current.rows)).toEqual(["b", "a"]);
    act(() => result.current.cycleSort()); // mtime asc
    expect(rowNames(result.current.rows)).toEqual(["a", "b"]);
    act(() => result.current.cycleSort()); // name desc
    expect(rowNames(result.current.rows)).toEqual(["b", "a"]);
  });
});
