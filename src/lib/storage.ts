/**
 * Versioned localStorage wrapper.
 *
 * Stores data in an envelope `{ version, data }` so schema changes can be
 * detected and migrated. Old bare-format data (pre-versioning) is loaded
 * transparently — existing users upgrade without data loss.
 */

const SCHEMA_VERSION = 1;

interface StorageEnvelope<T> {
  version: number;
  data: T;
}

/**
 * Load a value from localStorage with version checking.
 *
 * - Versioned envelope `{ version: 1, data: ... }` → returns `data`.
 * - Old bare format (object without `version` key) → returns as-is
 *   (backward compat for pre-versioning users).
 * - Unknown version → returns `null` (caller should use defaults).
 * - Parse failure or missing key → returns `null`.
 */
export function loadStored<T>(key: string): T | null {
  const raw = localStorage.getItem(key);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    // Versioned envelope matching current schema.
    if (
      parsed &&
      typeof parsed === "object" &&
      "version" in parsed &&
      "data" in parsed
    ) {
      if (parsed.version === SCHEMA_VERSION) {
        return parsed.data as T;
      }
      // Unknown future version — discard to avoid shape mismatches.
      console.warn(
        `[storage] ${key}: unknown schema version ${parsed.version}, discarding`,
      );
      localStorage.removeItem(key);
      return null;
    }
    // Old bare format (pre-versioning) — use as-is.
    return parsed as T;
  } catch {
    console.warn(`[storage] ${key}: failed to parse, discarding`);
    localStorage.removeItem(key);
    return null;
  }
}

/**
 * Save a value to localStorage with the current schema version envelope.
 */
export function saveStored<T>(key: string, data: T): void {
  const envelope: StorageEnvelope<T> = { version: SCHEMA_VERSION, data };
  try {
    localStorage.setItem(key, JSON.stringify(envelope));
  } catch (e) {
    // Quota exceeded or other write failure — caller handles.
    throw e;
  }
}
