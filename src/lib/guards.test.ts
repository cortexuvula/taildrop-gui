import { describe, it, expect } from "vitest";
import {
  isTransferRecord,
  sanitizeTransfers,
  isPeer,
  sanitizePeers,
  isIncomingFile,
  sanitizeIncomingFiles,
  sanitizeAppSettings,
} from "./guards";
import { DEFAULT_SETTINGS } from "../hooks/useSettings";
import type { TransferRecord } from "../types";

const validTransfer: TransferRecord = {
  id: "t1",
  filename: "report.pdf",
  peerName: "My Laptop",
  direction: "received",
  timestamp: 1700000000000,
  status: "success",
};

const validPeer = {
  id: "node1",
  public_key: "key",
  hostname: "host",
  dns_name: "host.tail.ts.net.",
  display_name: "Host",
  machine_name: "host",
  os: "linux",
  ips: ["100.64.0.1"],
  online: true,
  is_self: false,
  is_exit_node: false,
};

describe("transfer guards", () => {
  it("accepts a valid record", () => {
    expect(isTransferRecord(validTransfer)).toBe(true);
  });

  it("accepts every status variant", () => {
    for (const status of ["pending", "sending", "receiving", "success", "error", "cancelled"]) {
      expect(isTransferRecord({ ...validTransfer, status })).toBe(true);
    }
  });

  it("rejects null, primitives and records with wrong field types", () => {
    expect(isTransferRecord(null)).toBe(false);
    expect(isTransferRecord("nope")).toBe(false);
    expect(isTransferRecord({ ...validTransfer, status: "bogus" })).toBe(false);
    expect(isTransferRecord({ ...validTransfer, timestamp: "soon" })).toBe(false);
    expect(isTransferRecord({ ...validTransfer, direction: "sideways" })).toBe(false);
  });

  it("drops corrupt elements instead of failing the whole list (white-screen regression)", () => {
    const out = sanitizeTransfers([validTransfer, null, "garbage", { id: 1 }]);
    expect(out).toEqual([validTransfer]);
  });

  it("returns [] for non-array payloads", () => {
    expect(sanitizeTransfers(null)).toEqual([]);
    expect(sanitizeTransfers({ version: 1 })).toEqual([]);
  });
});

describe("peer guards", () => {
  it("accepts a valid peer and drops invalid ones", () => {
    expect(isPeer(validPeer)).toBe(true);
    expect(isPeer({ ...validPeer, ips: "100.64.0.1" })).toBe(false);
    expect(sanitizePeers([validPeer, null, {}])).toEqual([validPeer]);
  });
});

describe("incoming file guards", () => {
  it("accepts valid files (peerName optional) and drops invalid ones", () => {
    expect(isIncomingFile({ name: "a.txt", size: 10 })).toBe(true);
    expect(isIncomingFile({ name: "a.txt", size: 10, peerName: "peer" })).toBe(true);
    expect(isIncomingFile({ name: "a.txt", size: -5 })).toBe(false);
    expect(isIncomingFile({ size: 10 })).toBe(false);
    expect(sanitizeIncomingFiles([{ name: "a", size: 1 }, null])).toEqual([
      { name: "a", size: 1 },
    ]);
  });
});

describe("settings sanitizer", () => {
  it("keeps well-typed fields and resets mistyped ones to defaults", () => {
    const out = sanitizeAppSettings(
      {
        hiddenNodes: "not-a-list",
        saveDirectory: 42,
        autoAccept: "yes",
        showOfflineNodes: true,
        notifications: false,
      },
      DEFAULT_SETTINGS,
    );
    expect(out.hiddenNodes).toEqual([]);
    expect(out.saveDirectory).toBe("");
    expect(out.autoAccept).toBe(false);
    expect(out.showOfflineNodes).toBe(true);
    expect(out.notifications).toBe(false);
  });

  it("passes through valid settings untouched", () => {
    const settings = {
      hiddenNodes: ["node1"],
      saveDirectory: "/tmp/downloads",
      autoAccept: true,
      showOfflineNodes: false,
      showExitNodes: true,
      notifications: true,
    };
    expect(sanitizeAppSettings(settings, DEFAULT_SETTINGS)).toEqual(settings);
  });

  it("falls back entirely for non-object payloads", () => {
    expect(sanitizeAppSettings("junk", DEFAULT_SETTINGS)).toEqual(DEFAULT_SETTINGS);
    expect(sanitizeAppSettings(null, DEFAULT_SETTINGS)).toEqual(DEFAULT_SETTINGS);
  });
});
