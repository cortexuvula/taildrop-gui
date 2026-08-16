/**
 * Safely extract a human-readable error message from a caught value.
 *
 * `catch` bindings are typed `unknown`. Naive `String(e)` produces
 * `"[object Object]"` for thrown plain objects — useless to the user.
 * This helper handles the common shapes: Error instances (use `.message`),
 * strings (use directly, but never empty), `{ message }` objects (Tauri IPC),
 * and everything else (JSON-serialized when possible).
 */
export function toErrorMsg(e: unknown): string {
  if (e instanceof Error) return e.message || e.name || "Unknown error";
  if (typeof e === "string") {
    return e.trim() || "Unknown error";
  }
  // Tauri IPC errors often arrive as { message: "..." } objects.
  if (e && typeof e === "object" && typeof (e as { message?: unknown }).message === "string") {
    const msg = (e as { message: string }).message;
    return msg.trim() || "Unknown error";
  }
  if (typeof e === "number" || typeof e === "boolean" || typeof e === "bigint") {
    return String(e);
  }
  // Plain objects (and null): serialize when possible so the user sees the
  // payload instead of "[object Object]".
  if (e && typeof e === "object") {
    try {
      const json = JSON.stringify(e);
      if (json) return json;
    } catch {
      // non-serializable (circular) — fall through
    }
  }
  if (e === null) return "Unknown error (null)";
  if (e === undefined) return "Unknown error (undefined)";
  return "Unknown error";
}
