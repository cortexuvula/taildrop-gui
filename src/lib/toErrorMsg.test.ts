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

  it("never renders [object Object] for objects without .message", () => {
    expect(toErrorMsg({ code: 403 })).toBe('{"code":403}');
    expect(toErrorMsg({ nested: { a: 1 } })).toBe('{"nested":{"a":1}}');
  });

  it("handles null and undefined without leaking bare 'null'/'undefined'", () => {
    expect(toErrorMsg(null)).toBe("Unknown error (null)");
    expect(toErrorMsg(undefined)).toBe("Unknown error (undefined)");
  });

  it("handles numbers", () => {
    expect(toErrorMsg(42)).toBe("42");
  });

  it("never returns an empty message", () => {
    expect(toErrorMsg("")).toBe("Unknown error");
    expect(toErrorMsg("   ")).toBe("Unknown error");
    expect(toErrorMsg(new Error(""))).toBe("Error");
    expect(toErrorMsg({ message: "" })).toBe("Unknown error");
  });

  it("serializes arrays usefully", () => {
    expect(toErrorMsg([1, 2])).toBe("[1,2]");
  });
});
