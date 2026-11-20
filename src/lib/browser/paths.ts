/**
 * Path rules for the two-pane browser — a faithful port of the plugin's
 * FilePane.qml:157-183. Remote POSIX paths look like "/home/user/...";
 * remote Windows peers (browse+pull only in M1) present SFTP paths as
 * "/C:/Users/..." — the drive-root rule keeps that shape stable under
 * parent/join so "Up" from "/C:/Users" lands on "/C:/" and then on "/".
 *
 * Pure string functions: no I/O, fully unit-testable, never throw.
 */

/** "" and "/" are both treated as root (the plugin's isRoot). */
export function isRoot(p: string): boolean {
  return p === "" || p === "/";
}

/**
 * Parent folder, plugin-exact:
 *  - trailing slashes are stripped first
 *  - anything at depth ≤ 1 goes to "/"
 *  - a bare drive root "/C:" is normalized back to "/C:/"
 */
export function parentOf(p: string): string {
  const s = (p || "").replace(/\/+$/, "");
  const i = s.lastIndexOf("/");
  if (i <= 0) return "/";
  const parent = s.substring(0, i);
  return /^\/[A-Za-z]:$/.test(parent) ? parent + "/" : parent;
}

/** Last path component (trailing slashes stripped first). */
export function baseName(p: string): string {
  const s = (p || "").replace(/\/+$/, "");
  return s.substring(s.lastIndexOf("/") + 1);
}

/** Join a folder and a name with exactly one "/" between them. */
export function join(base: string, name: string): string {
  if (!base || base === "/") return "/" + name;
  return base.replace(/\/+$/, "") + "/" + name;
}

/**
 * "~" expansion for the path bar (Ctrl+L). "~" alone → home; "~/x" →
 * home-relative join; anything else passes through trimmed. An empty home
 * (listing failed) falls back to "/".
 */
export function expandHome(input: string, home: string): string {
  const t = (input || "").trim();
  if (t === "~") return home || "/";
  if (t.startsWith("~/")) return join(home || "/", t.substring(2));
  return t;
}

/** True when "Up" would be a no-op (already at the top). */
export function atRoot(p: string): boolean {
  return isRoot(p) || parentOf(p) === p;
}
