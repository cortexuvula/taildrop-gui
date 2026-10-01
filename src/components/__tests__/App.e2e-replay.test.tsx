// E2E wired-App acceptance (UI Consultant's requirement): render the PRODUCTION
// App with native integrations mocked, then drive the actual UI through:
//   Phase A: disconnected receipt backlog → mount → replay pagination →
//            receipts VISIBLE in the rendered transfer history.
//   Phase B: empty poll (zero-receipt replay) → history must NOT be erased.
//   Phase C: reconnect (focus event) → receipts still visible.
//   Phase D: Rust-shaped startup staging scan response ({dirs:[...]}) →
//            compact recovery notice RENDERS inside .main (not a third
//            column, not an auto-opened modal).
//   Phase E: Rust-shaped staging-recovery-found event payload → notice
//            renders through the event route too.
//   Phases F–K: Review dialog behavior — identity/disclosure, recover
//            success (salvaged receipts), recover/discard failure,
//            discard confirmation safety, focus/inertness semantics.
// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, waitFor, cleanup, fireEvent, within } from "@testing-library/react";
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

// Rust-shaped staging payloads (as serialized by StagingRecoveryFound).
// Two DISTINCT directories carrying IDENTICAL filenames — they must never be
// collapsed or deduplicated.
const RUST_STAGING_TWO_DIRS = {
  dirs: [
    {
      path: "/tmp/taildrop-accept-deadbeef",
      files: [
        { name: "vacation-photo.jpg", size: 40960 },
        { name: "tiny.txt", size: 4 },
      ],
    },
    {
      path: "/tmp/taildrop-accept-cafebabe",
      files: [
        { name: "vacation-photo.jpg", size: 40960 },
        { name: "empty.bin", size: 0 },
      ],
    },
  ],
};

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

// ---- Mutable mock backend state the recovery tests drive directly ----
interface MockBackend {
  staging: typeof RUST_STAGING_TWO_DIRS;
  extraReceipts: TransferReceipt[];
  failRecover: boolean;
  failDiscard: boolean;
  holdRecover: boolean;
  releaseRecover: (() => void) | null;
  holdDiscard: boolean;
  releaseDiscard: (() => void) | null;
  discarded: string[];
}

function freshBackend(): MockBackend {
  return {
    staging: { dirs: [] },
    extraReceipts: [],
    failRecover: false,
    failDiscard: false,
    holdRecover: false,
    releaseRecover: null,
    holdDiscard: false,
    releaseDiscard: null,
    discarded: [],
  };
}

let backend: MockBackend;

function selfStatus() {
  return [{
    id: "self", public_key: "pk", hostname: "this-machine",
    dns_name: "this-machine.tail.ts.net.", display_name: "This Machine",
    machine_name: "this-machine", os: "macos", ips: ["100.64.0.1"],
    online: true, is_self: true, is_exit_node: false,
  }];
}

beforeEach(() => {
  vi.clearAllMocks();
  receiptListener = null;
  stagingListener = null;
  focusCallbacks = [];
  localStorageStore = {};
  backend = freshBackend();
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
        const all = [...BACKLOG, ...backend.extraReceipts].sort((a, b) => a.seq - b.seq);
        const slice = all.filter((r) => r.seq > since).slice(0, 50);
        const lastSeq = slice.length ? slice[slice.length - 1].seq : since;
        return receiptPage(slice, lastSeq, all.some((r) => r.seq > lastSeq));
      }
      case "staging_recovery_scan":
        return backend.staging;
      case "recover_staging_files": {
        if (backend.failRecover) throw new Error("destination disk full");
        if (backend.holdRecover) {
          await new Promise<void>((resolve) => { backend.releaseRecover = resolve; });
        }
        const path = args?.path as string;
        const dir = backend.staging.dirs.find((d) => d.path === path);
        if (dir) {
          // Backend emits a SALVAGED receipt per file, then replay picks them up.
          backend.extraReceipts = backend.extraReceipts.concat(
            dir.files.map((f, i) => ({
              seq: 1000 + backend.extraReceipts.length + i,
              id: `salvage-${path}-${f.name}-${i}`,
              filename: f.name,
              savedName: f.name,
              savedPath: `/Users/me/Documents/Custom/${f.name}`,
              status: "salvaged" as const,
              timestamp: Date.now(),
              error: undefined,
              recovery: undefined,
              peerName: null,
              size: f.size,
              direction: "received" as const,
            })),
          );
          backend.staging = { dirs: backend.staging.dirs.filter((d) => d.path !== path) };
        }
        return "ok";
      }
      case "discard_staging_dir": {
        if (backend.failDiscard) throw new Error("cannot remove directory");
        if (backend.holdDiscard) {
          await new Promise<void>((resolve) => { backend.releaseDiscard = resolve; });
        }
        const path = args?.path as string;
        backend.discarded.push(path);
        backend.staging = { dirs: backend.staging.dirs.filter((d) => d.path !== path) };
        return "ok";
      }
      case "get_tailscale_status":
        return selfStatus();
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

function invokeCalls(cmd: string) {
  return mockInvoke.mock.calls.filter((c) => c[0] === cmd);
}

async function openReview() {
  await waitFor(
    () => expect(screen.getByRole("button", { name: "Review" })).toBeTruthy(),
    { timeout: 4000 },
  );
  const review = screen.getByRole("button", { name: "Review" });
  // A real browser click focuses the button first; jsdom's fireEvent does not.
  review.focus();
  fireEvent.click(review);
  await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
  return review;
}

describe("Wired App: disconnected receipts → visible history; recovery notice renders", () => {
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
      if (cmd === "get_tailscale_status") return selfStatus();
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

  it("renders the compact notice inside .main from the Rust-shaped scan response — no auto-opened modal, no third column", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    render(<AppWithToast />);

    // Phase D: startup scan returned {dirs:[...]} — the notice must RENDER.
    await waitFor(
      () => expect(screen.getByText("Files need attention")).toBeTruthy(),
      { timeout: 4000 },
    );
    expect(
      screen.getByText("2 recovery groups contain files that may be incomplete."),
    ).toBeTruthy();

    // No dialog auto-opens at startup.
    expect(screen.queryByRole("dialog")).toBeNull();

    // The notice lives INSIDE .main — .app stays a two-child flex row
    // (sidebar + main), so the notice can never become a third column.
    const notice = screen.getByText("Files need attention").closest(".recovery-notice");
    expect(notice).toBeTruthy();
    expect(notice!.closest(".main")).toBeTruthy();
    const app = document.querySelector(".app");
    expect(app).toBeTruthy();
    expect(app!.children.length).toBe(2);
    expect(app!.children[0].classList.contains("sidebar")).toBe(true);
    expect(app!.children[1].classList.contains("main")).toBe(true);

    // Nothing was auto-discarded or auto-recovered.
    expect(invokeCalls("discard_staging_dir").length).toBe(0);
    expect(invokeCalls("recover_staging_files").length).toBe(0);
  });

  it("renders the notice through the event route with the Rust-shaped payload", async () => {
    // Scan returns nothing; the startup EVENT delivers the payload instead.
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "staging_recovery_scan") return { dirs: [] };
      if (cmd === "get_recent_receipts") return receiptPage([], 0, false);
      if (cmd === "get_tailscale_status") return selfStatus();
      if (cmd === "get_incoming_files") return [];
      return undefined;
    });
    render(<AppWithToast />);
    await act(async () => {
      stagingListener?.({ payload: RUST_STAGING });
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(screen.getByText("Files need attention")).toBeTruthy();
    expect(
      screen.getByText("1 recovery group contains files that may be incomplete."),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Review" })).toBeTruthy();
  });
});

describe("Wired App: Review dialog (identity, disclosure, recover, discard)", () => {
  it("Review opens a labelled dialog disclosing full paths, filenames and exact sizes; distinct dirs are not deduplicated", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    render(<AppWithToast />);
    await openReview();

    const dialog = screen.getByRole("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBeTruthy();
    const labelledBy = dialog.getAttribute("aria-labelledby")!;
    expect(document.getElementById(labelledBy)).toBeTruthy();

    // Readable basenames AND full wrapping paths (never hover-only).
    expect(screen.getAllByText("taildrop-accept-deadbeef").length).toBeGreaterThan(0);
    expect(screen.getAllByText("taildrop-accept-cafebabe").length).toBeGreaterThan(0);
    expect(screen.getByText("/tmp/taildrop-accept-deadbeef")).toBeTruthy();
    expect(screen.getByText("/tmp/taildrop-accept-cafebabe")).toBeTruthy();

    // Exact sizes: 40960 → "40 KB", 4 bytes → "4 B", 0 bytes → "0 B".
    expect(screen.getAllByText("40 KB").length).toBe(2);
    expect(screen.getByText("4 B")).toBeTruthy();
    expect(screen.getByText("0 B")).toBeTruthy();

    // Two DISTINCT directories with identical filenames both render.
    expect(screen.getAllByText("vacation-photo.jpg").length).toBe(2);

    // Per-group actions exist; there are NO bulk actions.
    expect(screen.getAllByRole("button", { name: /^Recover 2 files from taildrop-accept-deadbeef$/ }).length).toBe(1);
    expect(screen.getAllByRole("button", { name: /^Recover 2 files from taildrop-accept-cafebabe$/ }).length).toBe(1);
    expect(screen.getAllByRole("button", { name: "Discard…" }).length).toBe(2);
    expect(screen.queryByRole("button", { name: /Recover All/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /Discard All/i })).toBeNull();
    expect(screen.getByRole("button", { name: "Done" })).toBeTruthy();
  });

  it("recover succeeds for one group only: the other survives, and salvaged receipts surface via replay with their warning", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-deadbeef" }),
    );

    // Success is confirmed by the backend receipt replay, not by guesswork.
    await waitFor(() => expect(invokeCalls("recover_staging_files").length).toBe(1));
    expect(invokeCalls("recover_staging_files")[0][1]).toEqual({
      path: "/tmp/taildrop-accept-deadbeef",
    });

    await waitFor(() => {
      // Group A is gone; group B survives untouched.
      expect(screen.queryByText("/tmp/taildrop-accept-deadbeef")).toBeNull();
      expect(screen.getByText("/tmp/taildrop-accept-cafebabe")).toBeTruthy();
    });
    // Notice count drops to one group (notice + open dialog subtitle).
    expect(
      screen.getAllByText("1 recovery group contains files that may be incomplete.").length,
    ).toBe(2);

    // Salvaged receipts appear in history and KEEP their unverified warning —
    // they must not become ordinary Saved rows.
    await waitFor(() => {
      const warnings = screen.getAllByText(/Unverified recovery/);
      expect(warnings.length).toBe(2);
    });
    // Group B's zero-byte file (0 B) is untouched — only group A was handled.
    expect(screen.getByText("0 B")).toBeTruthy();
  });

  it("recovering shows Recovering… with disabled conflicting controls; repeated clicks never duplicate the request", async () => {
    backend.staging = RUST_STAGING;
    backend.holdRecover = true;
    render(<AppWithToast />);
    await openReview();

    const recoverBtn = screen.getByRole("button", { name: "Recover 1 file from taildrop-accept-deadbeef" });
    fireEvent.click(recoverBtn);

    await waitFor(() => expect(screen.getByText("Recovering…")).toBeTruthy());
    const busyRecover = screen.getByText("Recovering…").closest("button")!;
    expect(busyRecover.hasAttribute("disabled")).toBe(true);
    const discardBtn = screen.getByRole("button", { name: "Discard…" });
    expect(discardBtn.hasAttribute("disabled")).toBe(true);

    // Hammer the disabled controls — the backend must see exactly one request.
    fireEvent.click(busyRecover);
    fireEvent.click(busyRecover);
    await act(async () => {
      backend.releaseRecover?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("recover_staging_files").length).toBe(1);

    // Group handled → dialog shows the empty state with Done still focused-able.
    await waitFor(() =>
      expect(screen.getByText("All recovery groups have been handled.")).toBeTruthy(),
    );
    expect(screen.getByRole("button", { name: "Done" })).toBeTruthy();
  });

  it("failed recover keeps the correct group visible with an inline error and allows retry", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.failRecover = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" }),
    );

    await waitFor(() =>
      expect(screen.getByText(/Couldn’t handle this group: destination disk full/)).toBeTruthy(),
    );
    // The failed group is still there — and so is the untouched one.
    expect(screen.getByText("/tmp/taildrop-accept-cafebabe")).toBeTruthy();
    expect(screen.getByText("/tmp/taildrop-accept-deadbeef")).toBeTruthy();

    // Controls re-enable; a retry issues a second request.
    const retryBtn = screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" });
    expect(retryBtn.hasAttribute("disabled")).toBe(false);
    backend.failRecover = false;
    fireEvent.click(retryBtn);
    await waitFor(() => expect(invokeCalls("recover_staging_files").length).toBe(2));
    await waitFor(() => expect(screen.queryByText("/tmp/taildrop-accept-cafebabe")).toBeNull());
    expect(screen.getByText("/tmp/taildrop-accept-deadbeef")).toBeTruthy();
  });

  it("discard requires an in-dialog confirmation naming group and files; Cancel is default focus and never discards", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    render(<AppWithToast />);
    await openReview();

    const discardButtons = screen.getAllByRole("button", { name: "Discard…" });
    fireEvent.click(discardButtons[1]); // target group B (cafebabe)

    // Confirmation names the group, the file count and the total size.
    // (The text spans nested elements, so match on the paragraph's textContent.)
    await waitFor(() => {
      const confirmText = screen.getByText(
        (_, el) => el?.classList.contains("staging-confirm-text") ?? false,
      );
      expect(confirmText.textContent).toBe(
        "Discard 2 files (40 KB) from taildrop-accept-cafebabe?",
      );
    });
    expect(
      screen.getByText(/These files will be deleted\. They may be the only copies/),
    ).toBeTruthy();
    // Files with sizes stay disclosed right above the confirmation.
    expect(screen.getAllByText("vacation-photo.jpg").length).toBe(2);

    // Cancel is the default focus of the confirmation.
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(screen.queryByText(/They may be the only copies/)).toBeNull(),
    );
    expect(invokeCalls("discard_staging_dir").length).toBe(0);
    expect(screen.getByText("/tmp/taildrop-accept-cafebabe")).toBeTruthy();

    // Confirming discards exactly that group; the other survives.
    fireEvent.click(screen.getAllByRole("button", { name: "Discard…" })[1]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Discard 2 files" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Discard 2 files" }));
    await waitFor(() => expect(invokeCalls("discard_staging_dir").length).toBe(1));
    expect(backend.discarded).toEqual(["/tmp/taildrop-accept-cafebabe"]);
    await waitFor(() => expect(screen.queryByText("/tmp/taildrop-accept-cafebabe")).toBeNull());
    expect(screen.getByText("/tmp/taildrop-accept-deadbeef")).toBeTruthy();
  });

  it("Escape during a confirmation cancels it (not the dialog); Escape otherwise closes the dialog and restores Review focus; background is inert while open", async () => {
    backend.staging = RUST_STAGING;
    render(<AppWithToast />);
    const review = await openReview();

    // Background (sidebar) is inert while the dialog is open.
    const sidebarBtn = document.querySelector(".sidebar .icon-btn") as HTMLElement;
    expect(sidebarBtn.closest(".sidebar")!.hasAttribute("inert")).toBe(true);

    // Open the discard confirmation, then Escape: cancels only the confirmation.
    fireEvent.click(screen.getByRole("button", { name: "Discard…" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy());
    // Dispatch on the focused control (Cancel) so the event bubbles through
    // the React tree and the overlay's cancel-on-Escape handler sees it.
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull(),
    );
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(invokeCalls("discard_staging_dir").length).toBe(0);

    // Escape again: closes the whole dialog, never discards, restores Review.
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(review);
    expect(sidebarBtn.closest(".sidebar")!.hasAttribute("inert")).toBe(false);
    expect(invokeCalls("discard_staging_dir").length).toBe(0);
  });

  it("Done closes the dialog without side effects; zero-byte files are never auto-deleted", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    render(<AppWithToast />);
    await openReview();

    // Both groups — including the 0-byte file — are still pending review.
    expect(screen.getByText("0 B")).toBeTruthy();
    expect(screen.getByText("4 B")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(invokeCalls("discard_staging_dir").length).toBe(0);
    expect(invokeCalls("recover_staging_files").length).toBe(0);
    expect(
      screen.getByText("2 recovery groups contain files that may be incomplete."),
    ).toBeTruthy();
  });
});

// ---- TD-UI-06: per-path in-flight op state must outlive the Review dialog ----
// Closing Review (Done/Escape/backdrop) unmounts the dialog; the backend op
// keeps running. The busy/error state is owned by useReceipts, so reopening
// shows the retained pending/error state and a second request for the same
// path can never be dispatched until the first settles.
describe("TD-UI-06: in-flight ops survive Review close/reopen (no double dispatch)", () => {
  const GROUP_A = "/tmp/taildrop-accept-deadbeef";
  const GROUP_B = "/tmp/taildrop-accept-cafebabe";

  function groupEl(path: string): HTMLElement {
    const el = document.querySelector(`article[data-staging-path="${path}"]`);
    expect(el, `group row for ${path}`).toBeTruthy();
    return el as HTMLElement;
  }

  async function reopenReview() {
    const review = screen.getByRole("button", { name: "Review" });
    review.focus();
    fireEvent.click(review);
    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
  }

  /** The group shows its pending label and every control in it is disabled. */
  function expectGroupLocked(path: string, pendingLabel: "Recovering…" | "Discarding…") {
    const group = within(groupEl(path));
    expect(group.getByText(pendingLabel)).toBeTruthy();
    for (const btn of group.getAllByRole("button")) {
      expect(btn.hasAttribute("disabled"), btn.textContent ?? "").toBe(true);
    }
  }

  it("recover in flight: Done, Escape and backdrop close/reopen keep pending state; exactly one request until settlement", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdRecover = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-deadbeef" }),
    );
    await waitFor(() => expect(screen.getByText("Recovering…")).toBeTruthy());

    // While group A is in flight, the unrelated group stays fully operable.
    expect(
      within(groupEl(GROUP_B)).getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" })
        .hasAttribute("disabled"),
    ).toBe(false);

    // --- Close via Done; the backend operation keeps running. ---
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    expectGroupLocked(GROUP_A, "Recovering…");
    // Hammer the busy Recover — the dispatch-boundary guard holds at one request.
    fireEvent.click(within(groupEl(GROUP_A)).getByText("Recovering…").closest("button")!);
    expect(invokeCalls("recover_staging_files").length).toBe(1);

    // --- Close via Escape; pending state must survive. ---
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    expectGroupLocked(GROUP_A, "Recovering…");
    fireEvent.click(within(groupEl(GROUP_A)).getByText("Recovering…").closest("button")!);
    expect(invokeCalls("recover_staging_files").length).toBe(1);

    // --- Close via backdrop click; pending state must survive. ---
    fireEvent.click(screen.getByRole("dialog")); // target === overlay → backdrop close
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    expectGroupLocked(GROUP_A, "Recovering…");
    // Both conflicting actions are hammered: neither may dispatch for this path.
    fireEvent.click(within(groupEl(GROUP_A)).getByText("Recovering…").closest("button")!);
    fireEvent.click(within(groupEl(GROUP_A)).getByRole("button", { name: "Discard…" }));
    expect(invokeCalls("recover_staging_files").length).toBe(1);
    expect(invokeCalls("discard_staging_dir").length).toBe(0);

    // --- Settlement: still exactly one request; only group A was handled. ---
    await act(async () => {
      backend.releaseRecover?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("recover_staging_files").length).toBe(1);
    await waitFor(() => expect(screen.queryByText(GROUP_A)).toBeNull());
    expect(screen.getByText(GROUP_B)).toBeTruthy();
  });

  it("discard in flight: Done, Escape and backdrop close/reopen keep pending state; exactly one request until settlement", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdDiscard = true;
    render(<AppWithToast />);
    await openReview();

    // Confirm a discard for group B; group A stays operable.
    fireEvent.click(within(groupEl(GROUP_B)).getByRole("button", { name: "Discard…" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Discard 2 files" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Discard 2 files" }));
    await waitFor(() => expect(screen.getByText("Discarding…")).toBeTruthy());
    // The confirmation itself locks while the request is unresolved.
    expect(screen.getByRole("button", { name: "Cancel" }).hasAttribute("disabled")).toBe(true);

    // --- Close via Done. ---
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    // The confirmation UI is gone (dialog-local), but the op is still in
    // flight: the actions block discloses it and stays locked.
    expectGroupLocked(GROUP_B, "Discarding…");
    fireEvent.click(within(groupEl(GROUP_B)).getByText("Discarding…").closest("button")!);
    expect(invokeCalls("discard_staging_dir").length).toBe(1);

    // --- Close via Escape. ---
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    expectGroupLocked(GROUP_B, "Discarding…");
    expect(invokeCalls("discard_staging_dir").length).toBe(1);

    // --- Close via backdrop. ---
    fireEvent.click(screen.getByRole("dialog"));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await reopenReview();
    expectGroupLocked(GROUP_B, "Discarding…");
    // Recover must not race the in-flight discard for the same path.
    fireEvent.click(
      within(groupEl(GROUP_B)).getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" }),
    );
    expect(invokeCalls("recover_staging_files").length).toBe(0);
    expect(invokeCalls("discard_staging_dir").length).toBe(1);

    // --- Settlement: exactly one discard, only group B removed. ---
    await act(async () => {
      backend.releaseDiscard?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("discard_staging_dir").length).toBe(1);
    expect(backend.discarded).toEqual([GROUP_B]);
    await waitFor(() => expect(screen.queryByText(GROUP_B)).toBeNull());
    expect(screen.getByText(GROUP_A)).toBeTruthy();
  });

  it("recover settles successfully while the dialog is closed: group handled, no busy residue, sibling operable", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdRecover = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-deadbeef" }),
    );
    await waitFor(() => expect(screen.getByText("Recovering…")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // The backend op completes while the dialog is closed (promise-scoped
    // cleanup must run without the dialog mounted).
    await act(async () => {
      backend.releaseRecover?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("recover_staging_files").length).toBe(1);
    // The notice count updated even with the dialog closed.
    expect(
      screen.getByText("1 recovery group contains files that may be incomplete."),
    ).toBeTruthy();

    await reopenReview();
    // Group A is gone, no error was recorded, and group B carries no residue.
    expect(screen.queryByText(GROUP_A)).toBeNull();
    expect(screen.queryByText(/Couldn’t handle this group/)).toBeNull();
    expect(
      within(groupEl(GROUP_B)).getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });

  it("recover fails while the dialog is closed: error retained on the correct group after reopen; deliberate retry succeeds", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdRecover = true;
    backend.failRecover = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" }),
    );
    await waitFor(() => expect(screen.getByText("Recovering…")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // The backend op FAILS while the dialog is closed.
    await act(async () => {
      backend.releaseRecover?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("recover_staging_files").length).toBe(1);

    await reopenReview();
    // Error retained for the failing group only; busy cleared → retry enabled.
    expect(
      within(groupEl(GROUP_B)).getByText(/Couldn’t handle this group: destination disk full/),
    ).toBeTruthy();
    expect(within(groupEl(GROUP_A)).queryByText(/Couldn’t handle this group/)).toBeNull();
    const retryBtn = within(groupEl(GROUP_B)).getByRole("button", {
      name: "Recover 2 files from taildrop-accept-cafebabe",
    });
    expect(retryBtn.hasAttribute("disabled")).toBe(false);

    // Positive control: a retry after settlement dispatches again and succeeds.
    backend.failRecover = false;
    backend.holdRecover = false;
    fireEvent.click(retryBtn);
    await waitFor(() => expect(invokeCalls("recover_staging_files").length).toBe(2));
    await waitFor(() => expect(screen.queryByText(GROUP_B)).toBeNull());
    expect(screen.getByText(GROUP_A)).toBeTruthy();
  });

  it("discard fails while the dialog is closed: error retained after reopen; unrelated group remains operable", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdDiscard = true;
    backend.failDiscard = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(within(groupEl(GROUP_A)).getByRole("button", { name: "Discard…" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Discard 2 files" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Discard 2 files" }));
    await waitFor(() => expect(screen.getByText("Discarding…")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await act(async () => {
      backend.releaseDiscard?.();
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(invokeCalls("discard_staging_dir").length).toBe(1);

    await reopenReview();
    expect(
      within(groupEl(GROUP_A)).getByText(/Couldn’t handle this group: cannot remove directory/),
    ).toBeTruthy();
    expect(within(groupEl(GROUP_B)).queryByText(/Couldn’t handle this group/)).toBeNull();

    // The failed group's controls re-enable, and the sibling stays operable.
    expect(
      within(groupEl(GROUP_A)).getByRole("button", { name: "Discard…" }).hasAttribute("disabled"),
    ).toBe(false);
    expect(
      within(groupEl(GROUP_B)).getByRole("button", { name: "Recover 2 files from taildrop-accept-cafebabe" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });

  // Codie's stale-event concern, preserved at the new op-state owner: a
  // mid-flight staging-recovery-found event that wholesale-replaces dirs and
  // re-inserts the in-flight path must never leave the row stuck disabled.
  it("mid-flight staging-recovery-found re-inserting the path never leaves the row permanently disabled", async () => {
    backend.staging = RUST_STAGING_TWO_DIRS;
    backend.holdRecover = true;
    backend.failRecover = true;
    render(<AppWithToast />);
    await openReview();

    fireEvent.click(
      screen.getByRole("button", { name: "Recover 2 files from taildrop-accept-deadbeef" }),
    );
    await waitFor(() => expect(screen.getByText("Recovering…")).toBeTruthy());

    // Stale startup-style event re-inserts BOTH groups (incl. the in-flight A).
    await act(async () => {
      stagingListener?.({ payload: RUST_STAGING_TWO_DIRS });
      await new Promise((r) => setTimeout(r, 30));
    });

    // The op settles (fails) — the promise-scoped finally must clear busy.
    await act(async () => {
      backend.releaseRecover?.();
      await new Promise((r) => setTimeout(r, 30));
    });

    const groupA = within(groupEl(GROUP_A));
    expect(groupA.getByText(/Couldn’t handle this group: destination disk full/)).toBeTruthy();
    expect(
      groupA.getByRole("button", { name: "Recover 2 files from taildrop-accept-deadbeef" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });
});
