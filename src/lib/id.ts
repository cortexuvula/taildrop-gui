/**
 * Collision-resistant ID for transfer records.
 *
 * Prefers crypto.randomUUID() (available in Tauri webviews, which are secure
 * contexts) over the old Date.now()+Math.random() pairs that could collide
 * for same-millisecond operations. Non-crypto fallback for exotic hosts.
 */
export function newId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    try {
      return crypto.randomUUID();
    } catch {
      // fall through to the fallback below
    }
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}
