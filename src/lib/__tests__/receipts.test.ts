// @vitest-environment node
import { describe, it, expect } from "vitest";
import {
  mergeReceipts,
  savedReceipts,
  failedReceipts,
  highestSeq,
  isTransferReceipt,
  isStagingRecoveryEvent,
} from "../../lib/receipts";
import type { TransferReceipt, ReceiptPage } from "../../types";

const mk = (
  overrides: Partial<TransferReceipt> & { seq: number; id: string },
): TransferReceipt => ({
  filename: "a.pdf",
  savedName: "a.pdf",
  savedPath: "/tmp/a.pdf",
  size: 100,
  peerName: "x",
  direction: "received",
  status: "saved",
  timestamp: 0,
  ...overrides,
});

describe("mergeReceipts", () => {
  it("appends new receipts in ascending seq", () => {
    const prev: TransferReceipt[] = [mk({ seq: 1, id: "a" })];
    const page: ReceiptPage = {
      receipts: [mk({ seq: 2, id: "b" }), mk({ seq: 3, id: "c" })],
      nextSinceSeq: 3,
      hasMore: false,
      reset: false,
    };
    const result = mergeReceipts(prev, page);
    expect(result.map((r) => r.seq)).toEqual([1, 2, 3]);
  });

  it("dedupes by id, keeping the higher seq", () => {
    const prev: TransferReceipt[] = [
      mk({ seq: 1, id: "a", status: "failed", error: "oops" }),
    ];
    const page: ReceiptPage = {
      // Same id but recovered via staging → new "saved" receipt at higher seq.
      receipts: [mk({ seq: 5, id: "a", status: "saved" })],
      nextSinceSeq: 5,
      hasMore: false,
      reset: false,
    };
    const result = mergeReceipts(prev, page);
    expect(result).toHaveLength(1);
    expect(result[0].status).toBe("saved");
    expect(result[0].seq).toBe(5);
  });

  it("ignores older duplicate seq", () => {
    const prev: TransferReceipt[] = [mk({ seq: 10, id: "a", status: "saved" })];
    const page: ReceiptPage = {
      receipts: [mk({ seq: 5, id: "a", status: "failed", error: "x" })],
      nextSinceSeq: 5,
      hasMore: false,
      reset: false,
    };
    const result = mergeReceipts(prev, page);
    expect(result).toHaveLength(1);
    expect(result[0].status).toBe("saved");
  });

  it("handles empty prev and empty page", () => {
    const page: ReceiptPage = {
      receipts: [],
      nextSinceSeq: 0,
      hasMore: false,
      reset: false,
    };
    expect(mergeReceipts([], page)).toEqual([]);
  });
});

describe("receipt filters", () => {
  const list: TransferReceipt[] = [
    mk({ seq: 1, id: "a", status: "saved" }),
    mk({ seq: 2, id: "b", status: "failed", error: "x" }),
    mk({ seq: 3, id: "c", status: "saved" }),
  ];

  it("savedReceipts returns only status=saved", () => {
    expect(savedReceipts(list).map((r) => r.id)).toEqual(["a", "c"]);
  });

  it("failedReceipts returns only status=failed", () => {
    expect(failedReceipts(list).map((r) => r.id)).toEqual(["b"]);
  });
});

describe("highestSeq", () => {
  it("returns 0 for empty list", () => {
    expect(highestSeq([])).toBe(0);
  });
  it("returns max seq", () => {
    expect(highestSeq([mk({ seq: 3, id: "a" }), mk({ seq: 7, id: "b" })])).toBe(7);
  });
});

describe("type guards", () => {
  it("isTransferReceipt accepts valid", () => {
    expect(isTransferReceipt(mk({ seq: 1, id: "a" }))).toBe(true);
  });
  it("isTransferReceipt rejects invalid", () => {
    expect(isTransferReceipt(null)).toBe(false);
    expect(isTransferReceipt({})).toBe(false);
    expect(isTransferReceipt({ seq: "1", id: "a", status: "saved" })).toBe(false);
    expect(isTransferReceipt({ seq: 1, id: 42, status: "saved" })).toBe(false);
    expect(isTransferReceipt({ seq: 1, id: "a", status: "pending" })).toBe(false);
  });

  it("isStagingRecoveryEvent accepts valid payload", () => {
    expect(
      isStagingRecoveryEvent({ dirs: [{ path: "/tmp/x", files: [] }] }),
    ).toBe(true);
  });
  it("isStagingRecoveryEvent rejects non-array dirs", () => {
    expect(isStagingRecoveryEvent({ dirs: "nope" })).toBe(false);
    expect(isStagingRecoveryEvent({})).toBe(false);
    expect(isStagingRecoveryEvent(null)).toBe(false);
  });
});
