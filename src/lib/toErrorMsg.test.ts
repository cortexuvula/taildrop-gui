import { describe, it, expect } from "vitest";
import { toErrorMsg } from "./toErrorMsg";

describe("toErrorMsg", () => {
  it("extracts .message from Error instances", () => {
    expect(toErrorMsg(new Error("disk full"))).toBe("disk full");
  });

  it("returns string values directly", () => {
    expect(toErrorMsg("network timeout")).toBe("network timeout");
  });

  it("extracts .message from Tauri IPC error objects", () => {
    expect(toErrorMsg({ message: "daemon not running" })).toBe("daemon not running");
    expect(toErrorMsg({ code: 403, message: "access denied" })).toBe("access denied");
  });

  it("falls back to [object Object] for objects without .message", () => {
    expect(toErrorMsg({ code: 403 })).toBe("[object Object]");
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
