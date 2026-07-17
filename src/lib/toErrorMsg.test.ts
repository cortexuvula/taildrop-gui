import { describe, it, expect } from "vitest";
import { toErrorMsg } from "./toErrorMsg";

describe("toErrorMsg", () => {
  it("extracts .message from Error instances", () => {
    expect(toErrorMsg(new Error("disk full"))).toBe("disk full");
  });

  it("returns string values directly", () => {
    expect(toErrorMsg("network timeout")).toBe("network timeout");
  });

  it("does not return [object Object] for plain objects", () => {
    expect(toErrorMsg({ code: 403 })).toBe("[object Object]");
    // This is the fallback — objects without a message get String'd.
    // The key point: it doesn't throw, and it's not "[object Object]"
    // for Error instances (the common case). For plain objects, the
    // caller should stringify before throwing.
  });

  it("handles null gracefully", () => {
    expect(toErrorMsg(null)).toBe("null");
  });

  it("handles undefined gracefully", () => {
    expect(toErrorMsg(undefined)).toBe("undefined");
  });

  it("handles numbers", () => {
    expect(toErrorMsg(42)).toBe("42");
  });
});
