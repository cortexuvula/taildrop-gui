import { describe, it, expect, beforeEach, vi } from "vitest";
import { loadStored, saveStored } from "./storage";

describe("storage", () => {
  let store: Record<string, string>;

  beforeEach(() => {
    store = {};
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, value: string) => {
        store[key] = value;
      },
      removeItem: (key: string) => {
        delete store[key];
      },
    });
  });

  describe("loadStored", () => {
    it("returns data from a versioned envelope", () => {
      store["test"] = JSON.stringify({ version: 1, data: { name: "hello" } });
      expect(loadStored<{ name: string }>("test")).toEqual({ name: "hello" });
    });

    it("returns old bare-format data as-is (backward compat)", () => {
      store["test"] = JSON.stringify({ name: "legacy", extra: true });
      expect(loadStored<{ name: string }>("test")).toEqual({
        name: "legacy",
        extra: true,
      });
    });

    it("returns null for unknown schema version", () => {
      store["test"] = JSON.stringify({ version: 999, data: "future" });
      expect(loadStored("test")).toBeNull();
    });

    it("returns null for malformed JSON", () => {
      store["test"] = "{ broken json";
      expect(loadStored("test")).toBeNull();
    });

    it("returns null for missing key", () => {
      expect(loadStored("nonexistent")).toBeNull();
    });

    it("handles arrays from old format (transfers backward compat)", () => {
      store["test"] = JSON.stringify([{ id: 1 }, { id: 2 }]);
      const result = loadStored<{ id: number }[]>("test");
      expect(result).toEqual([{ id: 1 }, { id: 2 }]);
    });
  });

  describe("saveStored", () => {
    it("writes a versioned envelope", () => {
      saveStored("test", { foo: "bar" });
      const parsed = JSON.parse(store["test"]);
      expect(parsed.version).toBe(1);
      expect(parsed.data).toEqual({ foo: "bar" });
    });

    it("can round-trip through loadStored", () => {
      const data = [1, 2, 3];
      saveStored("test", data);
      expect(loadStored<number[]>("test")).toEqual(data);
    });
  });
});
