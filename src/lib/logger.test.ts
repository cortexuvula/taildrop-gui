import { describe, it, expect, beforeEach } from "vitest";
import {
  logger,
  getFrontendLogs,
  clearFrontendLogs,
  subscribe,
} from "./logger";

describe("logger", () => {
  beforeEach(() => {
    clearFrontendLogs();
  });

  describe("logger API", () => {
    it("debug pushes an entry to the buffer", () => {
      logger.debug("Test", "message");
      const logs = getFrontendLogs();
      expect(logs).toHaveLength(1);
      expect(logs[0].level).toBe("debug");
      expect(logs[0].target).toBe("Test");
      expect(logs[0].message).toBe("message");
      expect(logs[0].source).toBe("frontend");
    });

    it("info/warn/error levels work", () => {
      logger.info("App", "info msg");
      logger.warn("App", "warn msg");
      logger.error("App", "error msg");
      const logs = getFrontendLogs();
      expect(logs).toHaveLength(3);
      expect(logs[0].level).toBe("info");
      expect(logs[1].level).toBe("warn");
      expect(logs[2].level).toBe("error");
    });

    it("joins extra args into the message", () => {
      logger.debug("Test", "value:", 42, { key: "val" });
      const logs = getFrontendLogs();
      expect(logs[0].message).toContain("42");
      expect(logs[0].message).toContain('"key":"val"');
    });
  });

  describe("getFrontendLogs", () => {
    it("returns a copy (not the internal array)", () => {
      logger.debug("Test", "msg");
      const a = getFrontendLogs();
      const b = getFrontendLogs();
      expect(a).not.toBe(b);
      expect(a).toEqual(b);
    });
  });

  describe("clearFrontendLogs", () => {
    it("empties the buffer", () => {
      logger.debug("Test", "msg");
      expect(getFrontendLogs()).toHaveLength(1);
      clearFrontendLogs();
      expect(getFrontendLogs()).toHaveLength(0);
    });
  });

  describe("subscribe", () => {
    it("fires listener on each log push", () => {
      let calls = 0;
      const unsub = subscribe(() => {
        calls++;
      });
      logger.debug("Test", "one");
      logger.debug("Test", "two");
      expect(calls).toBe(2);
      unsub();
    });

    it("stops firing after unsubscribe", () => {
      let calls = 0;
      const unsub = subscribe(() => {
        calls++;
      });
      logger.debug("Test", "msg");
      unsub();
      logger.debug("Test", "msg2");
      expect(calls).toBe(1);
    });
  });

  describe("buffer cap", () => {
    it("caps at 500 entries", () => {
      for (let i = 0; i < 501; i++) {
        logger.debug("Test", `msg ${i}`);
      }
      expect(getFrontendLogs()).toHaveLength(500);
    });
  });
});
