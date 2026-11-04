#!/usr/bin/env node
/**
 * CLI for validateUpdaterManifest: fail-closed pre-publish check of the
 * Tauri updater `latest.json` against the release's expected platform matrix.
 *
 * Usage:
 *   node validate-updater-manifest.mjs \
 *     --manifest latest.json \
 *     --version v0.15.0 \
 *     [--assets release-assets.json]   # JSON array of asset names
 *
 * Exit 0 = manifest is complete and internally consistent; exit 1 = at
 * least one required property is missing/wrong (publish must be blocked).
 */
import { readFileSync } from "node:fs";
import { validateUpdaterManifest } from "./updater-manifest-core.mjs";

function usage() {
  process.stderr.write(
    "usage: validate-updater-manifest.mjs --manifest <latest.json> --version <vX.Y.Z> [--assets <names.json>]\n",
  );
  process.exit(2);
}

function arg(name) {
  const idx = process.argv.indexOf(name);
  return idx >= 0 ? process.argv[idx + 1] : undefined;
}

const manifestPath = arg("--manifest");
const version = arg("--version");
const assetsPath = arg("--assets");
if (!manifestPath || !version) usage();

let manifest;
try {
  manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
} catch (e) {
  process.stderr.write(`FAIL: cannot read/parse manifest "${manifestPath}": ${e.message}\n`);
  process.exit(1);
}

let assetNames;
if (assetsPath) {
  try {
    const parsed = JSON.parse(readFileSync(assetsPath, "utf8"));
    if (!Array.isArray(parsed)) {
      process.stderr.write(`FAIL: "${assetsPath}" must contain a JSON array of asset names\n`);
      process.exit(1);
    }
    assetNames = parsed.map(String);
  } catch (e) {
    process.stderr.write(`FAIL: cannot read assets list "${assetsPath}": ${e.message}\n`);
    process.exit(1);
  }
}

const { ok, errors } = validateUpdaterManifest({ manifest, expectedVersion: version, assetNames });

for (const error of errors) process.stderr.write(`FAIL: ${error}\n`);
if (!ok) {
  process.stderr.write(
    `Updater manifest validation failed for ${version} — publish is blocked.\n`,
  );
  process.exit(1);
}
process.stdout.write(`Updater manifest validation passed for ${version}.\n`);
process.exit(0);