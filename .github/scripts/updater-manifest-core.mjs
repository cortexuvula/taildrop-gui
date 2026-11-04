/**
 * Updater manifest validation core (TAILDROP-FIXES-20261104 improvement 2).
 *
 * Fail-closed post-build / pre-publish check that `latest.json` (the Tauri
 * updater manifest composed by tauri-action's parallel build jobs) actually
 * covers every platform this repo ships, points at the right version, and
 * references artifacts that exist on the release.
 *
 * Contract source (public): tauri-action's pinned composition logic
 * (src/upload-version-json.ts at the SHA pinned in release.yml) plus the
 * official Tauri v2 updater docs (required keys `version`,
 * `platforms.<target>.url`, `platforms.<target>.signature`; platform keys
 * are `OS-ARCH` with OS ∈ {darwin, linux, windows}).
 *
 * For this repo's release matrix:
 *  - macOS universal build → tauri-action writes BOTH `darwin-aarch64` and
 *    `darwin-x86_64` pointing at the same universal artifact (plus optional
 *    new-style `darwin-*-app` keys, which we tolerate but do not require),
 *  - Windows x64 → `windows-x86_64`,
 *  - Linux x64 → `linux-x86_64`.
 *
 * We only REQUIRE the classic `OS-ARCH` keys the tauri-plugin-updater v2
 * resolves for standard targets — we never invent platform keys.
 */

const EXPECTED_PLATFORMS = [
  "darwin-aarch64",
  "darwin-x86_64",
  "linux-x86_64",
  "windows-x86_64",
];

/** Normalize `v0.15.0` and `0.15.0` to the same comparable form. */
export function normalizeVersion(version) {
  if (typeof version !== "string") return "";
  return version.trim().replace(/^v/i, "");
}

/** Read the referenced asset name out of an updater URL (last path segment). */
export function assetNameFromUrl(url) {
  if (typeof url !== "string") return null;
  try {
    // URLs are always https://…/download/…/<asset> in practice; use the
    // pathname so query strings can't smuggle a fake name.
    const parsed = new URL(url);
    const segments = parsed.pathname.split("/").filter(Boolean);
    const last = segments[segments.length - 1];
    return last ? decodeURIComponent(last) : null;
  } catch {
    return null;
  }
}

/**
 * Fail-closed validation of one updater manifest.
 *
 * @param {object} opts
 * @param {unknown} opts.manifest Parsed latest.json content.
 * @param {string} opts.expectedVersion Release tag/version (e.g. "v0.15.0").
 * @param {string[]} [opts.assetNames] Asset names present on the release.
 * @returns {{ ok: boolean, errors: string[] }}
 */
export function validateUpdaterManifest({ manifest, expectedVersion, assetNames }) {
  const errors = [];

  if (typeof manifest !== "object" || manifest === null || Array.isArray(manifest)) {
    return { ok: false, errors: ["manifest must be a JSON object"] };
  }
  if (typeof manifest.version !== "string" || manifest.version.trim() === "") {
    errors.push("manifest.version is missing or not a string");
  } else {
    const actual = normalizeVersion(manifest.version);
    const expected = normalizeVersion(expectedVersion);
    if (actual !== expected) {
      errors.push(
        `manifest.version "${manifest.version}" does not match release version "${expectedVersion}"`,
      );
    }
  }

  const platforms = manifest.platforms;
  if (typeof platforms !== "object" || platforms === null || Array.isArray(platforms)) {
    errors.push("manifest.platforms is missing or not an object");
    return { ok: false, errors };
  }

  for (const platform of EXPECTED_PLATFORMS) {
    const entry = platforms[platform];
    if (entry === undefined) {
      errors.push(`missing platform "${platform}" in manifest.platforms`);
      continue;
    }
    if (typeof entry !== "object" || entry === null) {
      errors.push(`platform "${platform}" is not an object`);
      continue;
    }
    if (typeof entry.url !== "string" || entry.url.trim() === "") {
      errors.push(`platform "${platform}" is missing a url`);
    }
    if (typeof entry.signature !== "string" || entry.signature.trim() === "") {
      errors.push(`platform "${platform}" is missing a signature`);
    }
    if (Array.isArray(assetNames)) {
      const name = assetNameFromUrl(entry.url);
      if (!name) {
        errors.push(`platform "${platform}" has an unparseable url "${String(entry.url)}"`);
      } else if (!assetNames.includes(name)) {
        errors.push(
          `platform "${platform}" references artifact "${name}" which is NOT among the release assets`,
        );
      }
    }
  }

  // Tolerate extra keys (new-style `darwin-aarch64-app`, NSIS/MSI variants,
  // notes, pub_date…) — we only fail on the required contract, never on
  // unknown additions, so legitimate tauri-action output passes.
  return { ok: errors.length === 0, errors };
}