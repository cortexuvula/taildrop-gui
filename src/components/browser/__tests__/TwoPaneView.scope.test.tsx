// @vitest-environment jsdom
/**
 * TwoPaneView dialog SCOPE-LOCK — the blocker-level invariant (App.qml:371-383
 * port): a dialog captures {target, path, user} when it opens and the action
 * REVALIDATES at confirm time. A pane that moved while the dialog was open
 * makes the action a no-op with a status notice — never a mutation against
 * the pane's NEW location.
 *
 * The retarget-while-open path is driven through the real seedPeer effect
 * (the plugin's "a summon can retarget it" scenario).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { TwoPaneView } from "../TwoPaneView";

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const peer1 = {
  host: "peer1",
  name: "peer-one",
  dns: "peer-one.tail.ts.net.",
  ip: "100.1.2.3",
  os: "linux",
  online: true,
  user: "",
  tailscaleSsh: true,
  browsable: true,
  isSelf: false,
};

const seed = {
  id: "node1",
  hostname: "peer-one",
  dns_name: "peer-one.tail.ts.net",
  ips: ["100.1.2.3"],
};

function localListing() {
  return {
    path: "/",
    entries: [
      { name: "a.txt", type: "file", link: false, size: 3, mtime: 1700000000, perm: 420 },
      { name: "b.txt", type: "file", link: false, size: 4, mtime: 1700000000, perm: 420 },
    ],
    skipped: 0,
    user: "",
    home: "/",
  };
}

function peerListing() {
  return {
    path: "/home/u",
    entries: [
      { name: "r.txt", type: "file", link: false, size: 5, mtime: 1700000000, perm: 420 },
    ],
    skipped: 0,
    user: "u",
    home: "/home/u",
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue((() => {}) as unknown as Awaited<ReturnType<typeof listen>>);
  mockInvoke.mockImplementation((async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "browser_peers":
        return [peer1];
      case "browser_config":
        return { hosts: {}, last: "" };
      case "browser_ls":
        return args?.target === "peer1" ? peerListing() : localListing();
      case "browser_copy_jobs":
        return [];
      case "browser_delete":
        return { trashed: true, done: 1 };
      default:
        return null; // mkdir/rename/set_user/set_last resolve unit
    }
  }) as unknown as typeof invoke);
  // jsdom lacks scrollIntoView (FilePane cursor-visibility effect).
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(cleanup);

function calledWith(cmd: string): Array<Record<string, unknown> | undefined> {
  return mockInvoke.mock.calls
    .filter((c) => c[0] === cmd)
    .map((c) => c[1] as Record<string, unknown> | undefined);
}

async function expectPanesLoaded() {
  await screen.findAllByText("a.txt"); // both panes start on local "/"
  await waitFor(() => expect(calledWith("browser_ls").length).toBeGreaterThanOrEqual(2));
}

describe("dialog scope-lock (revalidation at confirm)", () => {
  it("mkdir is a NO-OP when the pane retargeted while the dialog was open", async () => {
    const view = render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    // Focus the RIGHT pane, open the mkdir dialog there.
    fireEvent.mouseDown(screen.getByTestId("pane-right"));
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "n" });
    await screen.findByRole("heading", { name: /New folder/i });

    // Retarget the right pane while the dialog is open (sidebar seed — the
    // plugin's summon scenario).
    view.rerender(<TwoPaneView seedPeer={seed} />);
    await screen.findByText("r.txt"); // right pane now shows peer1:/home/u

    // Confirm the dialog — it must NOT mkdir on the NEW location.
    fireEvent.change(screen.getByPlaceholderText("Folder name"), {
      target: { value: "docs" },
    });
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByText("Create"));

    await waitFor(() =>
      expect(screen.getByText(/no longer applies/i)).toBeTruthy(),
    );
    expect(calledWith("browser_mkdir")).toHaveLength(0);
  });

  it("mkdir fires with the CAPTURED scope when the pane did not move", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    fireEvent.mouseDown(screen.getByTestId("pane-left"));
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "n" });
    await screen.findByRole("heading", { name: /New folder/i });
    fireEvent.change(screen.getByPlaceholderText("Folder name"), {
      target: { value: "docs" },
    });
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByText("Create"));

    await waitFor(() => expect(calledWith("browser_mkdir").length).toBe(1));
    expect(calledWith("browser_mkdir")[0]).toEqual({
      target: "local",
      path: "/docs", // captured scope path joined with the new name
      asUser: null, // pinned account from the listing
    });
  });

  it("delete sends full paths under the captured scope, then reloads", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    // Select a.txt in the left pane (single click = cursor).
    fireEvent.mouseDown(screen.getByTestId("pane-left"));
    const row = screen.getAllByText("a.txt")[0];
    fireEvent.click(row);
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "D" });

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/Delete 1 item/i)).toBeTruthy();
    // Local delete promises Trash, never permanent (invariant §4.11).
    expect(within(dialog).getByText(/Trash/i)).toBeTruthy();
    fireEvent.click(within(dialog).getAllByText("Delete").pop()!);

    await waitFor(() => expect(calledWith("browser_delete").length).toBe(1));
    expect(calledWith("browser_delete")[0]).toEqual({
      target: "local",
      paths: ["/a.txt"],
      asUser: null,
    });
  });

  it("delete is a NO-OP when the pane moved while the dialog was open", async () => {
    const view = render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    fireEvent.mouseDown(screen.getByTestId("pane-right"));
    fireEvent.click(screen.getAllByText("a.txt")[1]); // right pane's row
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "D" });
    const dialog = await screen.findByRole("dialog");

    view.rerender(<TwoPaneView seedPeer={seed} />);
    await screen.findByText("r.txt");

    fireEvent.click(within(screen.getByRole("dialog")).getAllByText("Delete").pop()!);
    await waitFor(() => expect(screen.getByText(/no longer applies/i)).toBeTruthy());
    expect(calledWith("browser_delete")).toHaveLength(0);
    // The dialog element reference is stale after close — nothing crashed.
    expect(dialog).toBeTruthy();
  });

  it("invalid names keep the NameDialog open with an inline problem", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "n" });
    await screen.findByRole("heading", { name: /New folder/i });
    fireEvent.change(screen.getByPlaceholderText("Folder name"), {
      target: { value: "bad/name" },
    });
    fireEvent.click(within(screen.getByRole("dialog")).getByText("Create"));
    // Still open, problem shown, nothing invoked.
    expect(screen.getByRole("heading", { name: /New folder/i })).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toMatch(/slash/i);
    expect(calledWith("browser_mkdir")).toHaveLength(0);
  });
});
