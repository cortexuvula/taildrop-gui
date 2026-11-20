/**
 * paths.ts — the FilePane.qml:157-183 port. Every rule the plugin's QML
 * implemented must behave identically here, including the Windows
 * drive-root shape ("/C:/") remote SFTP paths use.
 */
import { describe, it, expect } from "vitest";
import {
  isRoot,
  parentOf,
  baseName,
  join,
  expandHome,
  atRoot,
} from "../paths";

describe("isRoot", () => {
  it("treats empty and / as root", () => {
    expect(isRoot("")).toBe(true);
    expect(isRoot("/")).toBe(true);
    expect(isRoot("/a")).toBe(false);
  });
});

describe("parentOf", () => {
  it("strips trailing slashes before resolving", () => {
    expect(parentOf("/a/b/")).toBe("/a");
    expect(parentOf("/a/b///")).toBe("/a");
  });
  it("depth-1 folders go to /", () => {
    expect(parentOf("/a")).toBe("/");
    expect(parentOf("/")).toBe("/");
    expect(parentOf("")).toBe("/");
  });
  it("normalizes a bare Windows drive root back to /C:/", () => {
    // Up from /C:/Users lands on /C:/ (not /C:) per the QML rule.
    expect(parentOf("/C:/Users")).toBe("/C:/");
    expect(parentOf("/c:/x/y")).toBe("/c:/x");
  });
  it("Windows drive root then goes to /", () => {
    expect(parentOf("/C:/")).toBe("/");
  });
  it("deep paths", () => {
    expect(parentOf("/home/u/docs/a")).toBe("/home/u/docs");
  });
});

describe("baseName", () => {
  it("last component, trailing slashes stripped", () => {
    expect(baseName("/a/b.txt")).toBe("b.txt");
    expect(baseName("/a/b/")).toBe("b");
    expect(baseName("/")).toBe("");
    expect(baseName("")).toBe("");
    expect(baseName("/C:/Users")).toBe("Users");
  });
});

describe("join", () => {
  it("exactly one slash between parts", () => {
    expect(join("/a", "b")).toBe("/a/b");
    expect(join("/a/", "b")).toBe("/a/b");
    expect(join("/a///", "b")).toBe("/a/b");
  });
  it("root and empty base produce /name", () => {
    expect(join("/", "b")).toBe("/b");
    expect(join("", "b")).toBe("/b");
  });
  it("Windows drive base", () => {
    expect(join("/C:/", "Users")).toBe("/C:/Users");
    expect(join("/C:", "Users")).toBe("/C:/Users");
  });
});

describe("expandHome", () => {
  it("~ alone becomes home", () => {
    expect(expandHome("~", "/home/u")).toBe("/home/u");
    expect(expandHome("  ~  ", "/home/u")).toBe("/home/u");
  });
  it("~/x joins under home", () => {
    expect(expandHome("~/docs", "/home/u")).toBe("/home/u/docs");
    expect(expandHome("~/a/b", "/home/u/")).toBe("/home/u/a/b");
  });
  it("empty home falls back to /", () => {
    expect(expandHome("~", "")).toBe("/");
    expect(expandHome("~/x", "")).toBe("/x");
  });
  it("other input passes through trimmed", () => {
    expect(expandHome("  /tmp/x ", "/home/u")).toBe("/tmp/x");
    expect(expandHome("relative", "/home/u")).toBe("relative");
    expect(expandHome("~user/x", "/home/u")).toBe("~user/x");
  });
});

describe("atRoot", () => {
  it("true for root shapes, false when Up would move", () => {
    expect(atRoot("")).toBe(true);
    expect(atRoot("/")).toBe(true);
    expect(atRoot("/a")).toBe(false);
    expect(atRoot("/C:/")).toBe(false); // Up goes to "/" (drive list)
  });
});
