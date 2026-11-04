/**
 * Synthetic tests for the updater-manifest validator
 * (TAILDROP-FIXES-20261104 improvement 2). Run with:
 *   node --test .github/scripts/updater-manifest.test.mjs
 *
 * Coverage required by the brief: a complete manifest PASSES; a missing
 * platform, wrong version, and a missing referenced artifact each FAIL.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  validateUpdaterManifest,
  normalizeVersion,
  assetNameFromUrl,
} from "./updater-manifest-core.mjs";

const VERSION = "v0.15.0";

/** A complete, internally-consistent manifest for this repo's matrix. */
function completeManifest() {
  return {
    version: "0.15.0",
    notes: "release",
    pub_date: "2026-11-04T00:00:00Z",
    platforms: {
      "darwin-aarch64": {
        signature: "sig-darwin-aarch64",
        url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.0/TailDrop_0.15.0_universal.app.tar.gz",
      },
      "darwin-x86_64": {
        signature: "sig-darwin-x86_64",
        url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.0/TailDrop_0.15.0_universal.app.tar.gz",
      },
      "linux-x86_64": {
        signature: "sig-linux",
        url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.0/TailDrop_0.15.0_amd64.AppImage.tar.gz",
      },
      "windows-x86_64": {
        signature: "sig-win",
        url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.0/TailDrop_0.15.0_x64-setup.exe.nsis.zip",
      },
      // New-style keys tauri-action may add — tolerated, never required.
      "darwin-aarch64-app": {
        signature: "sig",
        url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.0/TailDrop_0.15.0_universal.app.tar.gz",
      },
    },
  };
}

function fullAssetNames() {
  return [
    "TailDrop_0.15.0_universal.app.tar.gz",
    "TailDrop_0.15.0_amd64.AppImage.tar.gz",
    "TailDrop_0.15.0_x64-setup.exe.nsis.zip",
  ];
}

test("complete manifest with matching version and present artifacts passes", () => {
  const { ok, errors } = validateUpdaterManifest({
    manifest: completeManifest(),
    expectedVersion: VERSION,
    assetNames: fullAssetNames(),
  });
  assert.equal(ok, true, `expected pass, got: ${JSON.stringify(errors)}`);
  assert.deepEqual(errors, []);
});

test("version matches with or without the leading v", () => {
  assert.equal(validateUpdaterManifest({
    manifest: completeManifest(),
    expectedVersion: "v0.15.0",
  }).ok, true);
  assert.equal(validateUpdaterManifest({
    manifest: completeManifest(),
    expectedVersion: "0.15.0",
  }).ok, true);
});

test("missing platform fails closed", () => {
  const manifest = completeManifest();
  delete manifest.platforms["linux-x86_64"];
  const { ok, errors } = validateUpdaterManifest({ manifest, expectedVersion: VERSION });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes('missing platform "linux-x86_64"')));
});

test("wrong version fails closed", () => {
  const manifest = completeManifest();
  manifest.version = "0.14.9";
  const { ok, errors } = validateUpdaterManifest({ manifest, expectedVersion: VERSION });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes('does not match release version "v0.15.0"')));
});

test("missing referenced artifact fails closed", () => {
  const manifest = completeManifest();
  // Keep linux in the manifest but remove its artifact from the release.
  const { ok, errors } = validateUpdaterManifest({
    manifest,
    expectedVersion: VERSION,
    assetNames: fullAssetNames().filter((n) => !n.includes("AppImage")),
  });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes('"linux-x86_64" references artifact')));
  assert.ok(errors.some((e) => e.includes("AppImage.tar.gz")));
});

test("missing signature fails closed", () => {
  const manifest = completeManifest();
  delete manifest.platforms["windows-x86_64"].signature;
  const { ok, errors } = validateUpdaterManifest({ manifest, expectedVersion: VERSION });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes('"windows-x86_64" is missing a signature')));
});

test("missing url fails closed", () => {
  const manifest = completeManifest();
  delete manifest.platforms["darwin-aarch64"].url;
  const { ok, errors } = validateUpdaterManifest({ manifest, expectedVersion: VERSION });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes('"darwin-aarch64" is missing a url')));
});

test("non-object manifest fails closed", () => {
  for (const bad of [null, "nope", 42, []]) {
    const { ok, errors } = validateUpdaterManifest({ manifest: bad, expectedVersion: VERSION });
    assert.equal(ok, false);
    assert.equal(errors.length, 1);
  }
});

test("missing platforms object fails closed", () => {
  const { ok, errors } = validateUpdaterManifest({
    manifest: { version: "0.15.0" },
    expectedVersion: VERSION,
  });
  assert.equal(ok, false);
  assert.ok(errors.some((e) => e.includes("platforms is missing")));
});

test("asset name extraction tolerates query strings and encodes", () => {
  assert.equal(
    assetNameFromUrl("https://example.com/download/v0.1.0/My%20App_0.1.0_universal.app.tar.gz"),
    "My App_0.1.0_universal.app.tar.gz",
  );
  assert.equal(
    assetNameFromUrl("https://example.com/download/v0.1.0/file.tar.gz?x=1"),
    "file.tar.gz",
  );
  assert.equal(assetNameFromUrl(undefined), null);
  assert.equal(assetNameFromUrl("not a url"), null);
});