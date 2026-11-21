// @vitest-environment jsdom
/**
 * M2a behaviors in TwoPaneView (task t_6197d588):
 *
 * 1. remote→remote copy ENQUEUES (the M1 B5 mirror refusal is gone) — the
 *    browser_copy request carries BOTH remote targets; the Rust preflight
 *    stays authoritative for the remaining refusals.
 * 2. "=" mirrors the ACTIVE pane's machine+path onto the other pane
 *    (plugin App.qml:487 behavior — see M2A-STATUS decision note).
 * 3. vim navigation g/G (top/bottom) and h/l (up/open) through the real
 *    router, without disturbing j/k.
 * 4. "?" opens the help overlay; the overlay's key table is PINNED to the
 *    router source: every `case` in TwoPaneView's switch must appear in the
 *    help table and every help-table key must exist in the router (or in
 *    the pre-switch Tab/Esc/Ctrl handling). Adding a binding without a
 *    help row — or a help row without a binding — fails this test.
 * 5. Known-hosts dialog lists pins and Forget wires to
 *    browser_forget_host_key.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { TwoPaneView } from "../TwoPaneView";
// Vite ?raw import (typed via vite/client): the router source text, used by
// the help-overlay truthfulness pin below.
import routerSource from "../TwoPaneView.tsx?raw";

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function peer(host: string, name: string) {
  return {
    host,
    name,
    dns: `${name}.tail.ts.net.`,
    ip: "100.1.2.3",
    os: "linux",
    online: true,
    user: "",
    tailscaleSsh: true,
    browsable: true,
    isSelf: false,
  };
}

const peer1 = peer("peer1", "peer-one");
const peer2 = peer("peer2", "peer-two");

const PINS = [
  {
    host: "studio",
    flavor: "tailscale-ssh",
    fingerprint: "SHA256:WnFigd6vxW1unbyFJHKepROv0zUUHGzEQZiaKA2ez/4",
  },
  { host: "peer2", flavor: "openssh", fingerprint: "" },
];

function listing(path: string, names: Array<[string, "file" | "dir"]>, user = "", home = "/") {
  return {
    path,
    entries: names.map(([name, type], i) => ({
      name,
      type,
      link: false,
      size: 10 + i,
      mtime: 1700000000,
      perm: 420,
    })),
    skipped: 0,
    user,
    home,
  };
}

function lsFor(target: unknown, path: unknown) {
  const p = typeof path === "string" ? path : "";
  if (target === "peer1") {
    if (p === "/home") return listing("/home", [], "u", "/home/u");
    return listing("/home/u", [["r.txt", "file"]], "u", "/home/u");
  }
  if (target === "peer2") return listing("/srv", [["dest.txt", "file"]], "", "/");
  if (p === "/sub") return listing("/sub", [], "", "/");
  return listing(
    "/",
    [
      ["sub", "dir"],
      ["a.txt", "file"],
      ["b.txt", "file"],
      ["c.txt", "file"],
    ],
    "",
    "/",
  );
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue((() => {}) as unknown as Awaited<ReturnType<typeof listen>>);
  mockInvoke.mockImplementation((async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "browser_peers":
        return [peer1, peer2];
      case "browser_config":
        return { hosts: {}, last: "" };
      case "browser_ls":
        return lsFor(args?.target, args?.path);
      case "browser_copy_jobs":
        return [];
      case "browser_copy":
        return 1;
      case "browser_known_hosts":
        return PINS;
      default:
        return null; // forget/set_last/... resolve unit
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

/** Focus a pane, open its machine picker, choose a machine by display name. */
async function pickMachine(paneId: "left" | "right", machineName: string) {
  const pane = screen.getByTestId(`pane-${paneId}`);
  fireEvent.mouseDown(pane);
  fireEvent.click(within(pane).getByRole("button", { name: /This computer|peer-/i }));
  const menu = await within(pane).findByRole("listbox", { name: /Choose machine/i });
  fireEvent.click(within(menu).getByText(machineName));
}

function focusPane(paneId: "left" | "right") {
  fireEvent.mouseDown(screen.getByTestId(`pane-${paneId}`));
}

function optionsIn(paneId: "left" | "right"): HTMLElement[] {
  return within(screen.getByTestId(`pane-${paneId}`)).getAllByRole("option");
}

// ---------------------------------------------------------------------------
// 1. remote→remote copy enqueues (B5 mirror refusal lifted)
// ---------------------------------------------------------------------------

describe("M2a: remote→remote copy", () => {
  it("enqueues browser_copy with BOTH remote targets (no local hop, no UI refusal)", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    await pickMachine("left", "peer-one");
    await within(screen.getByTestId("pane-left")).findByText("r.txt");
    await pickMachine("right", "peer-two");
    await within(screen.getByTestId("pane-right")).findByText("dest.txt");

    // Select r.txt in the source pane and copy.
    focusPane("left");
    fireEvent.click(within(screen.getByTestId("pane-left")).getByText("r.txt"));
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "c" });

    await waitFor(() => expect(calledWith("browser_copy").length).toBe(1));
    expect(calledWith("browser_copy")[0]).toEqual({
      job: {
        srcTarget: "peer1",
        srcUser: "u",
        srcDir: "/home/u",
        dstTarget: "peer2",
        dstUser: null,
        dstDir: "/srv",
        names: ["r.txt"],
        replace: false,
      },
    });
    // The M1 refusal text must not appear anywhere.
    expect(screen.queryByText(/arrives with M2/i)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// 2. "=" mirror key
// ---------------------------------------------------------------------------

describe("M2a: = mirror key", () => {
  it("mirrors the ACTIVE pane's machine+path onto the other pane", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    await pickMachine("left", "peer-one");
    await within(screen.getByTestId("pane-left")).findByText("r.txt");

    focusPane("left"); // active = left (peer1:/home/u); right is still local
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "=" });

    // The right pane now loads peer1:/home/u — r.txt appears in BOTH panes.
    await waitFor(() => expect(screen.getAllByText("r.txt").length).toBe(2));
    expect(
      calledWith("browser_ls").filter((c) => c?.target === "peer1" && c?.path === "").length,
    ).toBeGreaterThanOrEqual(1);
    // Mirror direction is active→other: the left pane did NOT move to local.
    expect(within(screen.getByTestId("pane-left")).getByText("r.txt")).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// 3. vim navigation additions g/G/h/l (router-level, real listings)
// ---------------------------------------------------------------------------

describe("M2a: vim navigation g/G/h/l", () => {
  it("G jumps to the bottom row, g back to the top; j/k still step", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    focusPane("left");
    const view = screen.getByTestId("browser-view");

    // Rows sort folders-first: sub, a.txt, b.txt, c.txt.
    fireEvent.keyDown(view, { key: "G" });
    await waitFor(() => {
      const opts = optionsIn("left");
      expect(opts[opts.length - 1].getAttribute("aria-selected")).toBe("true");
      expect(opts[opts.length - 1].textContent).toContain("c.txt");
    });

    fireEvent.keyDown(view, { key: "k" }); // one up from the bottom
    await waitFor(() =>
      expect(optionsIn("left")[2].getAttribute("aria-selected")).toBe("true"),
    );

    fireEvent.keyDown(view, { key: "g" });
    await waitFor(() => {
      const opts = optionsIn("left");
      expect(opts[0].getAttribute("aria-selected")).toBe("true");
      expect(opts[0].textContent).toContain("sub");
    });

    fireEvent.keyDown(view, { key: "j" }); // one down from the top
    await waitFor(() =>
      expect(optionsIn("left")[1].getAttribute("aria-selected")).toBe("true"),
    );
  });

  it("l opens the folder under the cursor; h goes up to the parent", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    focusPane("left");
    const view = screen.getByTestId("browser-view");

    // Cursor starts on row 0 = "sub" (folders first). l opens it.
    fireEvent.keyDown(view, { key: "l" });
    await waitFor(() =>
      expect(
        calledWith("browser_ls").some((c) => c?.target === "local" && c?.path === "/sub"),
      ).toBe(true),
    );

    // h on the right pane (peer1:/home/u) climbs to /home.
    await pickMachine("right", "peer-one");
    await within(screen.getByTestId("pane-right")).findByText("r.txt");
    focusPane("right");
    fireEvent.keyDown(view, { key: "h" });
    await waitFor(() =>
      expect(
        calledWith("browser_ls").some((c) => c?.target === "peer1" && c?.path === "/home"),
      ).toBe(true),
    );
  });
});

// ---------------------------------------------------------------------------
// 4. "?" help overlay + truthfulness pin against the router source
// ---------------------------------------------------------------------------

/** Key values in the router switch → the label the help table must show. */
const KEY_LABELS: Record<string, string> = {
  ArrowDown: "↓",
  ArrowUp: "↑",
  ArrowRight: "→",
  ArrowLeft: "←",
  PageDown: "PgDn",
  PageUp: "PgUp",
  " ": "Space",
  Insert: "Ins",
  Delete: "Del",
};

/** Keys handled BEFORE the switch (Tab / Escape cascade / Ctrl combos). */
const PRE_SWITCH_LABELS = ["Tab", "Esc", "Ctrl+A", "Ctrl+R", "Ctrl+H", "Ctrl+L"];

function routerCaseKeys(): string[] {
  const src = routerSource;
  const start = src.indexOf("switch (key) {");
  expect(start).toBeGreaterThan(0); // the router must keep this shape
  const body = src.slice(start, src.indexOf("default:", start));
  const keys: string[] = [];
  for (const m of body.matchAll(/case "((?:[^"\\]|\\.)*)":/g)) {
    keys.push(m[1] === " " ? " " : JSON.parse(`"${m[1]}"`));
  }
  expect(keys.length).toBeGreaterThan(20); // sanity: we found the real switch
  return keys;
}

describe("M2a: ? help overlay", () => {
  it("opens on ?, closes on Esc", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "?" });
    const dialog = await screen.findByTestId("help-dialog");
    expect(within(dialog).getByRole("heading", { name: /Keyboard shortcuts/i })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("help-dialog")).toBeNull());
  });

  it("also opens on F1", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "F1" });
    expect(await screen.findByTestId("help-dialog")).toBeTruthy();
  });

  it("lists EXACTLY the keys the router implements (truthfulness pin)", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    fireEvent.keyDown(screen.getByTestId("browser-view"), { key: "?" });
    const dialog = await screen.findByTestId("help-dialog");

    // Every <kbd> cell, split on " / " into individual key tokens (a bare
    // "/" kbd has no surrounding spaces and stays intact).
    const helpTokens = new Set<string>();
    for (const kbd of dialog.querySelectorAll("kbd")) {
      for (const part of (kbd.textContent ?? "").split(/\s+\/\s+/)) {
        const t = part.trim();
        if (t !== "") helpTokens.add(t);
      }
    }

    // Direction 1: every router case key must appear in the help table.
    const labels = new Set<string>(PRE_SWITCH_LABELS);
    for (const key of routerCaseKeys()) {
      const label = KEY_LABELS[key] ?? key;
      labels.add(label);
      expect(helpTokens, `router case "${key}" missing from the help table`).toContain(label);
    }

    // Direction 2: every help-table key must exist in the router (or in the
    // pre-switch Tab/Esc/Ctrl handling) — no aspirational plugin bindings.
    for (const token of helpTokens) {
      expect(labels, `help table lists "${token}" but the router never handles it`).toContain(
        token,
      );
    }
  });
});

// ---------------------------------------------------------------------------
// 5. Known-hosts dialog (M1 gap 4)
// ---------------------------------------------------------------------------

describe("M2a: known-hosts dialog", () => {
  it("lists pins with flavor + fingerprint and Forget wires to browser_forget_host_key", async () => {
    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();

    fireEvent.click(screen.getByTestId("known-hosts-button"));
    const dialog = await screen.findByTestId("known-hosts-dialog");
    await within(dialog).findAllByTestId("known-host-row");

    const rows = within(dialog).getAllByTestId("known-host-row");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("studio");
    expect(rows[0].textContent).toContain("Tailscale SSH");
    expect(rows[0].textContent).toContain("SHA256:WnFigd6vxW1unbyFJHKepROv0zUUHGzEQZiaKA2ez/4");
    // Unparseable pin shows the em-dash placeholder, never a fake fingerprint.
    expect(rows[1].textContent).toContain("—");

    fireEvent.click(
      within(dialog).getByRole("button", { name: /Forget the Tailscale SSH key for studio/i }),
    );
    await waitFor(() =>
      expect(calledWith("browser_forget_host_key")).toEqual([
        { host: "studio", flavor: "tailscale-ssh" },
      ]),
    );
    // The forgotten pin's row is gone; the other remains.
    await waitFor(() => expect(within(dialog).getAllByTestId("known-host-row")).toHaveLength(1));
    expect(within(dialog).getAllByTestId("known-host-row")[0].textContent).toContain("peer2");
  });

  it("shows the empty-state hint when nothing is pinned", async () => {
    mockInvoke.mockImplementation((async (cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case "browser_peers":
          return [peer1, peer2];
        case "browser_config":
          return { hosts: {}, last: "" };
        case "browser_ls":
          return lsFor(args?.target, args?.path);
        case "browser_copy_jobs":
          return [];
        case "browser_known_hosts":
          return [];
        default:
          return null;
      }
    }) as unknown as typeof invoke);

    render(<TwoPaneView seedPeer={null} />);
    await expectPanesLoaded();
    fireEvent.click(screen.getByTestId("known-hosts-button"));
    const dialog = await screen.findByTestId("known-hosts-dialog");
    expect(await within(dialog).findByTestId("known-hosts-empty")).toBeTruthy();
  });
});
