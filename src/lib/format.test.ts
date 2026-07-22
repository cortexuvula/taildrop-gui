import { describe, it, expect } from "vitest";
import { formatSize, shortenError, statusIcon } from "./format";

describe("formatSize", () => {
  it("returns 0 B for zero", () => {
    expect(formatSize(0)).toBe("0 B");
  });

  it("returns 0 B for negative", () => {
    expect(formatSize(-100)).toBe("0 B");
  });

  it("formats bytes", () => {
    expect(formatSize(500)).toBe("500 B");
  });

  it("formats kilobytes", () => {
    expect(formatSize(1024)).toBe("1 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
  });

  it("formats megabytes", () => {
    expect(formatSize(1048576)).toBe("1 MB");
    expect(formatSize(5242880)).toBe("5 MB");
  });

  it("formats gigabytes", () => {
    expect(formatSize(1073741824)).toBe("1 GB");
  });

  it("caps at petabytes", () => {
    const pb = 1024 ** 5;
    expect(formatSize(pb)).toBe("1 PB");
    // Beyond PB shouldn't crash
    expect(formatSize(pb * 1024)).toBe("1024 PB");
  });
});

describe("shortenError", () => {
  it("extracts from Tailscale API error prefix", () => {
    const result = shortenError("Tailscale API error (403): file access denied");
    expect(result).toBe("file access denied");
  });

  it("extracts from tailscale file cp failed prefix", () => {
    const result = shortenError("tailscale file cp failed: exit status 1");
    expect(result).toBe("exit status 1");
  });

  it("returns the full string when no prefix matches", () => {
    expect(shortenError("some random error")).toBe("some random error");
  });

  it("returns the full string for empty input", () => {
    expect(shortenError("")).toBe("");
  });
});

describe("statusIcon", () => {
  it("returns hourglass for pending", () => {
    expect(statusIcon("pending")).toBe("⏳");
  });

  it("returns hourglass for sending", () => {
    expect(statusIcon("sending")).toBe("⏳");
  });

  it("returns checkmark for success", () => {
    expect(statusIcon("success")).toBe("✓");
  });

  it("returns X for error", () => {
    expect(statusIcon("error")).toBe("✗");
  });
});
