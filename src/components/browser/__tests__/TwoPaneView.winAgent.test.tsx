// @vitest-environment jsdom
/** Production panes + API sanitizer, mocked only at the Tauri boundary. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { TwoPaneView } from "../TwoPaneView";

const mockInvoke = vi.mocked(invoke);
const peer = (host: string, os = "windows") => ({
  host, name: host, dns: `${host}.tail.test.`, ip: "100.1.1.1", os,
  online: true, user: "", tailscaleSsh: false, browsable: true, isSelf: false,
});
const seed = (host: string) => ({ id: host, hostname: host, dns_name: "", ips: [] });
const probe = (winAgent: unknown, user = "alice") => ({
  system: "Windows", posix: false, home: "/", flavor: "openssh-windows",
  user, extensions: {}, winAgent,
});
let probeFor: (host: string, user: unknown) => unknown;
let peerOs: string;
function calls(cmd: string) {
  return mockInvoke.mock.calls.filter(([c]) => c === cmd).map(([, args]) => args);
}
function right() { return within(screen.getByTestId("pane-right")); }
function key(key: string) { fireEvent.keyDown(screen.getByTestId("browser-view"), { key }); }
function deferred() {
  let resolve!: (v: unknown) => void;
  const promise = new Promise<unknown>((r) => { resolve = r; });
  return { promise, resolve };
}
beforeEach(() => {
  peerOs = "windows";
  probeFor = () => probe(false);
  mockInvoke.mockReset();
  vi.mocked(listen).mockResolvedValue(() => {});
  Element.prototype.scrollIntoView = vi.fn();
  mockInvoke.mockImplementation((async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "browser_peers") return [peer("win-a", peerOs), peer("win-b", peerOs)];
    if (cmd === "browser_config") return { hosts: {}, last: "" };
    if (cmd === "browser_ls") return {
      path: "/", home: "/", user: args?.target === "local" ? "" : "alice", skipped: 0,
      entries: [{ name: "a.txt", type: "file", link: false, size: 1, mtime: 1, perm: 420 }],
    };
    if (cmd === "browser_probe") return probeFor(String(args?.host), args?.asUser);
    if (cmd === "browser_copy_jobs") return [];
    if (cmd === "browser_copy") return 1;
    if (cmd === "browser_delete") return { trashed: false, done: 1 };
    return null;
  }) as typeof invoke);
});
afterEach(cleanup);

async function mountWindows() {
  const view = render(<TwoPaneView seedPeer={seed("win-a")} />);
  await waitFor(() => expect(right().getAllByText("alice@")).toHaveLength(1));
  fireEvent.mouseDown(screen.getByTestId("pane-right"));
  return view;
}
async function expectWriteButtons(enabled: boolean) {
  await waitFor(() => {
    for (const name of ["New folder", "Rename", "Delete"]) {
      expect((right().getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(!enabled);
    }
  });
}

describe("Windows host/account capability in real UI", () => {
  it("probes the pinned account and permits all writes only after proof", async () => {
    const pending = deferred();
    probeFor = () => pending.promise;
    await mountWindows();
    await waitFor(() => expect(calls("browser_probe")).toContainEqual({ host: "win-a", asUser: "alice" }));
    await expectWriteButtons(false);
    await act(async () => pending.resolve(probe(true)));
    await expectWriteButtons(true);
    expect(right().queryByText(/browse and pull only/)).toBeNull();

    fireEvent.click(right().getByRole("button", { name: "New folder" }));
    fireEvent.change(screen.getByPlaceholderText("Folder name"), { target: { value: "docs" } });
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(calls("browser_mkdir")).toEqual([{ target: "win-a", path: "/docs", asUser: "alice" }]));
    await expectWriteButtons(true);
    fireEvent.click(right().getByRole("button", { name: "Rename" }));
    fireEvent.change(within(screen.getByRole("dialog")).getByRole("textbox"), { target: { value: "b.txt" } });
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Rename" }));
    await waitFor(() => expect(calls("browser_rename")).toEqual([{ target: "win-a", old: "/a.txt", new: "/b.txt", asUser: "alice" }]));
    await expectWriteButtons(true);
    fireEvent.click(right().getByRole("button", { name: "Delete" }));
    expect(screen.getByRole("dialog").textContent).toContain("PERMANENTLY");
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(calls("browser_delete")).toEqual([{ target: "win-a", paths: ["/a.txt"], asUser: "alice" }]));
    await expectWriteButtons(true);
    fireEvent.click(screen.getByRole("button", { name: "Copy →" }));
    await waitFor(() => expect(calls("browser_copy")).toHaveLength(1));
    expect(calls("browser_copy")[0]).toMatchObject({ job: { dstTarget: "win-a", dstUser: "alice", replace: false } });
  });

  it.each([false, undefined, "true", "failure"])("refuses unproven %s writes, including keyboard shortcuts, but allows pull", async (proof) => {
    probeFor = () => {
      if (proof === "failure") throw { message: "compile failed", kind: "unsupported" };
      return probe(proof);
    };
    await mountWindows();
    await expectWriteButtons(false);
    for (const shortcut of ["n", "R", "D"]) {
      key(shortcut);
      expect(screen.queryByRole("dialog")).toBeNull();
    }
    expect(right().getByText(/browse and pull only/)).toBeTruthy();
    fireEvent.mouseDown(screen.getByTestId("pane-left"));
    key("c");
    expect(screen.getByText(/browse and pull only/, { selector: ".status-error" })).toBeTruthy();
    expect(calls("browser_copy")).toHaveLength(0);
    expect((screen.getByRole("button", { name: "Copy →" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "← Copy" }));
    await waitFor(() => expect(calls("browser_copy")).toHaveLength(1));
    expect(calls("browser_copy")[0]).toMatchObject({ job: { srcTarget: "win-a", srcUser: "alice", dstTarget: "local" } });
    for (const cmd of ["browser_mkdir", "browser_rename", "browser_delete"]) expect(calls(cmd)).toHaveLength(0);
  });

  it("uses Windows probe flavor when the peer OS hint is unknown", async () => {
    peerOs = "";
    await mountWindows();
    await waitFor(() => expect(calls("browser_probe")).toHaveLength(1));
    await expectWriteButtons(false);
    key("n");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("ignores late proof from another host and does not reuse proof after switching back", async () => {
    const pendingA = deferred();
    const pendingB = deferred();
    probeFor = (host) => host === "win-a" ? pendingA.promise : pendingB.promise;
    const view = await mountWindows();
    await waitFor(() => expect(calls("browser_probe")).toHaveLength(1));
    view.rerender(<TwoPaneView seedPeer={seed("win-b")} />);
    await waitFor(() => expect(calls("browser_probe")).toHaveLength(2));
    await act(async () => pendingA.resolve(probe(true)));
    await expectWriteButtons(false);
    await act(async () => pendingB.resolve(probe(true)));
    await expectWriteButtons(true);
    probeFor = () => probe(false);
    view.rerender(<TwoPaneView seedPeer={seed("win-a")} />);
    await waitFor(() => expect(calls("browser_probe")).toHaveLength(3));
    await expectWriteButtons(false);
  });

  it("does not accept another account's proof for the current listing", async () => {
    probeFor = () => probe(true, "bob");
    await mountWindows();
    await waitFor(() => expect(calls("browser_probe")).toHaveLength(1));
    await expectWriteButtons(false);
    key("n");
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
