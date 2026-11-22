/**
 * guards.ts (browser) — IPC sanitizers must never throw and must drop bad
 * rows rather than whole payloads; the client mirrors of the Rust name
 * guards must agree with the server rules (server stays authoritative).
 */
import { describe, it, expect } from "vitest";
import {
  sanitizeListing,
  sanitizePeers,
  sanitizeJobs,
  sanitizeCopyEvent,
  sanitizeBrowserConfig,
  sanitizeProbe,
  sanitizeKnownHosts,
  toBrowserError,
  newNameProblem,
  controlCharProblem,
  isWindowsPeer,
  canWriteTo,
  isBrowserEntry,
} from "../guards";

const goodEntry = {
  name: "a.txt",
  type: "file",
  link: false,
  size: 3,
  mtime: 1700000000,
  perm: 0o644,
};

const goodPeer = {
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

describe("sanitizeListing", () => {
  it("passes a well-shaped listing through", () => {
    const l = sanitizeListing({
      path: "/",
      entries: [goodEntry],
      skipped: 2,
      user: "u",
      home: "/home/u",
    });
    expect(l).not.toBeNull();
    expect(l!.entries).toHaveLength(1);
    expect(l!.skipped).toBe(2);
  });
  it("drops malformed rows but keeps the listing", () => {
    const l = sanitizeListing({
      path: "/",
      entries: [goodEntry, { name: "" }, null, { ...goodEntry, name: "ok", size: "huge" }],
      skipped: 0,
      user: "",
      home: "/",
    });
    expect(l!.entries.map((e) => e.name)).toEqual(["a.txt"]);
  });
  it("rejects a wholly malformed payload", () => {
    expect(sanitizeListing(null)).toBeNull();
    expect(sanitizeListing({ path: 5 })).toBeNull();
    expect(sanitizeListing("nope")).toBeNull();
  });
  it("clamps negative skipped", () => {
    const l = sanitizeListing({ path: "/", entries: [], skipped: -4, user: "", home: "" });
    expect(l!.skipped).toBe(0);
  });
});

describe("sanitizePeers", () => {
  it("keeps good rows, drops bad", () => {
    expect(sanitizePeers([goodPeer, { host: "" }, 42])).toHaveLength(1);
    expect(sanitizePeers("junk")).toEqual([]);
  });
  it("requires every documented field with the right type", () => {
    expect(isBrowserEntry({ ...goodPeer })).toBe(false);
    expect(isBrowserEntry(goodEntry)).toBe(true);
  });
});

describe("sanitizeJobs", () => {
  const job = {
    id: 1,
    state: "running",
    label: "a.txt",
    pct: 40,
    rate: "1 MB/s",
    file: "a.txt",
    skipped: 0,
    request: { srcTarget: "local", srcUser: null, srcDir: "/", dstTarget: "p", dstUser: null, dstDir: "/", names: ["a.txt"], replace: false },
    created: 1700000000000,
  };
  it("passes a snapshot through", () => {
    expect(sanitizeJobs([job])).toHaveLength(1);
  });
  it("drops rows with a bad state or missing request", () => {
    expect(sanitizeJobs([{ ...job, state: "exploded" }])).toHaveLength(0);
    expect(sanitizeJobs([{ ...job, request: null }])).toHaveLength(0);
    expect(sanitizeJobs("x")).toEqual([]);
  });
  it("keeps exists-kind jobs with their names", () => {
    const j = { ...job, state: "failed", kind: "exists", names: ["a.txt"], error: "exists" };
    const out = sanitizeJobs([j]);
    expect(out[0].kind).toBe("exists");
    expect(out[0].names).toEqual(["a.txt"]);
  });
});

describe("sanitizeCopyEvent", () => {
  it("accepts progress events", () => {
    expect(sanitizeCopyEvent({ jobId: 1, event: "progress", pct: 5, rate: "r", file: "f" })).not.toBeNull();
  });
  it("rejects unknown event names and bad jobId", () => {
    expect(sanitizeCopyEvent({ jobId: 1, event: "explode" })).toBeNull();
    expect(sanitizeCopyEvent({ jobId: "1", event: "start" })).toBeNull();
    expect(sanitizeCopyEvent(null)).toBeNull();
  });
});

describe("sanitizeBrowserConfig", () => {
  it("falls back on junk", () => {
    expect(sanitizeBrowserConfig(null)).toEqual({ hosts: {}, last: "" });
  });
  it("keeps valid host entries only", () => {
    const c = sanitizeBrowserConfig({
      hosts: { a: { user: "u" }, b: { user: 5 }, c: "junk" },
      last: "a",
    });
    expect(c.hosts).toEqual({ a: { user: "u" } });
    expect(c.last).toBe("a");
  });
});

describe("sanitizeProbe", () => {
  it.each([undefined, null, false, "true", 1, {}, []])("defaults malformed/missing winAgent %j to false", (winAgent) => {
    expect(sanitizeProbe({
      system: "Windows", posix: false, home: "C:/Users/u",
      flavor: "openssh-windows", user: "u", extensions: {}, winAgent,
    })).toHaveProperty("winAgent", false);
  });
  it("accepts only literal true as Windows agent proof", () => {
    expect(sanitizeProbe({
      system: "Windows", posix: false, home: "C:/Users/u",
      flavor: "openssh-windows", user: "u", extensions: {}, winAgent: true,
    })).toHaveProperty("winAgent", true);
  });
  it("accepts a full probe", () => {
    const p = sanitizeProbe({
      system: "Linux",
      posix: true,
      home: "/home/u",
      flavor: "tailscale-ssh",
      user: "u",
      extensions: { "hardlink@openssh.com": "1", bad: 5 },
    });
    expect(p).not.toBeNull();
    expect(p!.extensions).toEqual({ "hardlink@openssh.com": "1" });
  });
  it("rejects unknown flavors", () => {
    expect(
      sanitizeProbe({ system: "", posix: true, home: "", flavor: "telnet", user: "", extensions: {} }),
    ).toBeNull();
  });
});

describe("toBrowserError", () => {
  it("keeps the typed shape", () => {
    expect(toBrowserError({ message: "m", kind: "exists", names: ["a"] })).toEqual({
      message: "m",
      kind: "exists",
      names: ["a"],
    });
  });
  it("downgrades unknown kinds to other", () => {
    expect(toBrowserError({ message: "m", kind: "weird" }).kind).toBe("other");
  });
  it("handles strings, Errors, and junk", () => {
    expect(toBrowserError("boom")).toEqual({ message: "boom", kind: "other" });
    expect(toBrowserError(new Error("e")).message).toBe("e");
    expect(toBrowserError(42).message).toBe("42");
  });
});

describe("newNameProblem (mirror of Rust guards)", () => {
  it("accepts ordinary names incl. unicode and dots", () => {
    expect(newNameProblem("report.pdf")).toBeNull();
    expect(newNameProblem("café")).toBeNull();
    expect(newNameProblem("a.b.c")).toBeNull();
    expect(newNameProblem("literal*[\"'\\")).toBeNull();
  });
  it("refuses empty, slashes, dot names", () => {
    expect(newNameProblem("")).not.toBeNull();
    expect(newNameProblem("a/b")).not.toBeNull();
    expect(newNameProblem(".")).not.toBeNull();
    expect(newNameProblem("..")).not.toBeNull();
  });
  it("refuses control characters (C0 + DEL)", () => {
    expect(newNameProblem("a\nb")).not.toBeNull();
    expect(newNameProblem("a\tb")).not.toBeNull();
    expect(newNameProblem("a\u{0}b")).not.toBeNull();
    expect(newNameProblem("a\u{7f}b")).not.toBeNull();
    expect(controlCharProblem("plain")).toBeNull();
  });
});

// NOTE (M2a): the former `remoteToRemoteProblem` mirror of the B5 refusal is
// GONE — remote→remote copies are now allowed and stream through the app on
// two independent sessions (Rust preflight stays authoritative for the
// same-host-two-account and Windows-destination refusals). The lifted
// behavior is covered by the TwoPaneView r2r-enqueue vitest and the e2e
// remote→remote copy test.

describe("sanitizeKnownHosts (M2a pin-management boundary)", () => {
  const good = {
    host: "studio",
    flavor: "tailscale-ssh",
    fingerprint: "SHA256:WnFigd6vxW1unbyFJHKepROv0zUUHGzEQZiaKA2ez/4",
  };
  it("passes well-formed rows through", () => {
    expect(sanitizeKnownHosts([good])).toEqual([good]);
  });
  it("drops malformed rows instead of throwing", () => {
    const rows = [
      good,
      null,
      { host: "", flavor: "openssh", fingerprint: "x" }, // empty host
      { host: "h", flavor: "not-a-flavor", fingerprint: "x" }, // bad flavor
      { host: "h", flavor: "openssh" }, // fingerprint coerced to ""
      42,
    ];
    const out = sanitizeKnownHosts(rows);
    expect(out).toEqual([
      good,
      { host: "h", flavor: "openssh", fingerprint: "" },
    ]);
  });
  it("degrades to [] on non-arrays", () => {
    expect(sanitizeKnownHosts(undefined)).toEqual([]);
    expect(sanitizeKnownHosts({ host: "x" })).toEqual([]);
    expect(sanitizeKnownHosts("nope")).toEqual([]);
  });
});

describe("Windows write-disable heuristic (M1: browse + pull only)", () => {
  it("detects windows by os string, case-insensitively", () => {
    expect(isWindowsPeer({ os: "windows" })).toBe(true);
    expect(isWindowsPeer({ os: "Windows" })).toBe(true);
    expect(isWindowsPeer({ os: "linux" })).toBe(false);
    expect(isWindowsPeer(null)).toBe(false);
  });
  it("local is always writable; windows peers are not", () => {
    expect(canWriteTo("local", undefined)).toBe(true);
    expect(canWriteTo("peer", { os: "windows" })).toBe(false);
    expect(canWriteTo("peer", { os: "darwin" })).toBe(true);
    expect(canWriteTo("peer", undefined)).toBe(true);
  });
});
