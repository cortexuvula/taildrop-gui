/**
 * Safely extract a human-readable error message from a caught value.
 *
 * `catch` bindings are typed `unknown`. Naive `String(e)` produces
 * `"[object Object]"` for thrown plain objects — useless to the user.
 * This helper handles the common shapes: Error instances (use `.message`),
 * strings (use directly), and everything else (fall back to `String`).
 */
export function toErrorMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  return String(e);
}
