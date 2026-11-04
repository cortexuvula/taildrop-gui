#!/usr/bin/env node
/**
 * release-recover.test.mjs — Offline safety regression tests for the
 * TailDrop v0.15.1 preservation-only recovery workflow.
 *
 * Every test constructs mock reality data and asserts that the core
 * validation functions produce the correct refusal (errors present) or
 * pass (no errors) outcome.  The composed {@link evaluateRecovery} test
 * asserts that the publish callback is called exactly once on the happy
 * path and zero times on every refusal path.
 *
 * Run:
 *   node --test .github/scripts/release-recover.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  checkIdentity,
  checkTagBinding,
  checkOriginalRun,
  checkDraftRelease,
  checkAssetSet,
  checkAssetBytes,
  checkCompetitor,
  checkFinalRecheck,
  evaluateRecovery,
} from "./release-recover-core.mjs";

// ═══════════════════════════════════════════════════════════════════════════════
//  Shared test data
// ═══════════════════════════════════════════════════════════════════════════════

const PIN = {
  repo: "cortexuvula/taildrop-gui",
  ref: "refs/heads/master",
  confirmation: "publish-existing-v0.15.1-434359119",
  releaseId: 434359119,
  tagName: "v0.15.1",
  tagObjectSha: "57b7150a06fc3f29b7e173b6d916a14cc929e833",
  peeledSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
  originalRunId: 41897833512,
  originalRunAttempt: 1,
  originalHeadSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
  requiredOriginalJobs: [
    "Quality Gate",
    "Create GitHub Release",
    "Build Release (macos-latest)",
    "Build Release (windows-latest)",
    "Build Release (ubuntu-24.04)",
  ],
  requiredPlatforms: ["darwin-aarch64", "darwin-x86_64", "windows-x86_64", "linux-x86_64"],
  assets: [
    { id: 710677092, name: "TailDrop_0.15.1_amd64.deb",           size: 5311804,  digest: "sha256:f3e015cdecda3f20a44a2ffa5f5341c541cbf9bf869c29b72e6295b717bad041" },
    { id: 710677146, name: "TailDrop_0.15.1_amd64.deb.sig",        size: 416,      digest: "sha256:cadcd5eff989b5458a3827ee60d04ffa7c52552504221790bb87110099576932" },
    { id: 710677163, name: "TailDrop-0.15.1-1.x86_64.rpm",         size: 5312083,  digest: "sha256:aa862ef6ca11586ceb9377000bde283f9d9469557a4de1c8b30dbc86a0fb77e7" },
    { id: 710677202, name: "TailDrop-0.15.1-1.x86_64.rpm.sig",     size: 420,      digest: "sha256:4ff31494b3482e5bcc537e6fb8434d8c136846353959dafbc19f8e8bb01f5bc4" },
    { id: 710677229, name: "TailDrop_0.15.1_amd64.AppImage",       size: 80796152, digest: "sha256:a3bf5ab14823e296c6bf419eb00fa22502ba0507f4672fa7ed0c3757ce396427" },
    { id: 710677423, name: "TailDrop_0.15.1_amd64.AppImage.sig",   size: 420,      digest: "sha256:29ca447ad656b624feb04e3e2b103e28176a7ee39b5b42f7b10fb6e39169748f" },
    { id: 710682983, name: "TailDrop_0.15.1_x64_en-US.msi",        size: 5201920,  digest: "sha256:c94d536b9fe9cf6dcb128261c8a9d275419c3960effb8f65d9812ea83ed57a17" },
    { id: 710683028, name: "TailDrop_0.15.1_x64_en-US.msi.sig",    size: 420,      digest: "sha256:de8ab6f7738aab041876452bdc0f9b5753c7b3e139f7c78de0f5fac20f818f87" },
    { id: 710683047, name: "TailDrop_0.15.1_x64-setup.exe",        size: 3589006,  digest: "sha256:77da011be9de789038a97b28443e5d24e5a230d9102be9d98552ca2e88f06ca9" },
    { id: 710683075, name: "TailDrop_0.15.1_x64-setup.exe.sig",    size: 420,      digest: "sha256:3ae0d89963421ccbab82a1698e6dbb19c7cdac9132e2ea41a572cd4ceda543b9" },
    { id: 710691402, name: "TailDrop_0.15.1_universal.dmg",        size: 8900281,  digest: "sha256:c6b6b481d2c6b68445e090f4c598e5b00082a34e9835457f8a3b0c9823738ffa" },
    { id: 710691461, name: "TailDrop_universal.app.tar.gz",         size: 9086811,  digest: "sha256:9d63adbc86744465467c80f27f36215b44551f0ae0bc9524d4896ae028edfa76" },
    { id: 710691530, name: "TailDrop_universal.app.tar.gz.sig",     size: 408,      digest: "sha256:6c9c4788b6430813e22c2d3bf024de52399576845d5460cb13d0a65bad8faecd" },
    { id: 710691628, name: "latest.json",                            size: 6540,    digest: "sha256:b8ee638dbf7cb77c8c0c4f904f2f78cbb76e8efdeb6b695661f6e56240a16f8c" },
  ],
};

const GOOD_IDENTITY = {
  repo: "cortexuvula/taildrop-gui",
  event: "workflow_dispatch",
  ref: "refs/heads/master",
  confirmation: "publish-existing-v0.15.1-434359119",
  execSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
  remoteSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
};

const GOOD_TAG = {
  tagObjectSha: "57b7150a06fc3f29b7e173b6d916a14cc929e833",
  peeledSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
};

const GOOD_ORIGINAL_RUN = {
  runId: 41897833512,
  attempt: 1,
  headSha: "de6bbcf1baaab9759ae46bb26d6b5e9f6cd2373f",
  jobs: [
    { name: "Quality Gate",                conclusion: "success" },
    { name: "Create GitHub Release",       conclusion: "success" },
    { name: "Build Release (macos-latest)",   conclusion: "success" },
    { name: "Build Release (windows-latest)", conclusion: "success" },
    { name: "Build Release (ubuntu-24.04)",   conclusion: "success" },
    // Validate Updater Manifest — failed (expected, not required)
    { name: "Validate Updater Manifest",      conclusion: "failure" },
    // Publish Release — skipped (expected)
    { name: "Publish Release",                conclusion: "skipped" },
  ],
};

const GOOD_RELEASE = {
  id: 434359119,
  tagName: "v0.15.1",
  draft: true,
  publishedAt: null,
  prerelease: false,
  targetCommitish: "master",
};

function makeGoodAssets() {
  return PIN.assets.map((a) => ({ id: a.id, name: a.name, size: a.size, digest: a.digest }));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Individual check function tests
// ═══════════════════════════════════════════════════════════════════════════════

test("checkIdentity: good identity passes", () => {
  const errors = checkIdentity(GOOD_IDENTITY, PIN);
  assert.deepEqual(errors, []);
});

test("checkIdentity: wrong repo fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, repo: "other/repo" }, PIN);
  assert.ok(e.some((m) => m.includes("Repository")));
  assert.ok(e.some((m) => m.includes("other/repo")));
});

test("checkIdentity: wrong event fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, event: "push" }, PIN);
  assert.ok(e.some((m) => m.includes("Event") || m.includes("push")));
});

test("checkIdentity: wrong ref fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, ref: "refs/heads/develop" }, PIN);
  assert.ok(e.some((m) => m.includes("Ref")));
});

test("checkIdentity: wrong confirmation fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, confirmation: "wrong-string" }, PIN);
  assert.ok(e.some((m) => m.includes("Confirmation")));
});

test("checkIdentity: empty executing SHA fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, execSha: "" }, PIN);
  assert.ok(e.some((m) => m.includes("SHA")));
});

// ─── Tag binding ──────────────────────────────────────────────────────────

test("checkTagBinding: good tag passes", () => {
  assert.deepEqual(checkTagBinding(GOOD_TAG, PIN), []);
});

test("checkTagBinding: wrong tag object SHA fails", () => {
  const e = checkTagBinding({ ...GOOD_TAG, tagObjectSha: "0000000000000000000000000000000000000000" }, PIN);
  assert.ok(e.some((m) => m.includes("Tag object SHA")));
});

test("checkTagBinding: wrong peeled SHA fails", () => {
  const e = checkTagBinding({ ...GOOD_TAG, peeledSha: "0000000000000000000000000000000000000000" }, PIN);
  assert.ok(e.some((m) => m.includes("Peeled commit SHA")));
});

// ─── Original run ─────────────────────────────────────────────────────────

test("checkOriginalRun: good run passes", () => {
  assert.deepEqual(checkOriginalRun(GOOD_ORIGINAL_RUN, PIN), []);
});

test("checkOriginalRun: wrong run id fails", () => {
  const e = checkOriginalRun({ ...GOOD_ORIGINAL_RUN, runId: 1 }, PIN);
  assert.ok(e.some((m) => m.includes("run id")));
});

test("checkOriginalRun: wrong attempt fails", () => {
  const e = checkOriginalRun({ ...GOOD_ORIGINAL_RUN, attempt: 2 }, PIN);
  assert.ok(e.some((m) => m.includes("attempt")));
});

test("checkOriginalRun: missing Quality Gate fails", () => {
  const e = checkOriginalRun({
    ...GOOD_ORIGINAL_RUN,
    jobs: GOOD_ORIGINAL_RUN.jobs.filter((j) => j.name !== "Quality Gate"),
  }, PIN);
  assert.ok(e.some((m) => m.includes('"Quality Gate"')));
  assert.ok(e.some((m) => m.includes("not found")));
});

test("checkOriginalRun: Quality Gate failed report fails", () => {
  const e = checkOriginalRun({
    ...GOOD_ORIGINAL_RUN,
    jobs: GOOD_ORIGINAL_RUN.jobs.map((j) =>
      j.name === "Quality Gate" ? { ...j, conclusion: "failure" } : j),
  }, PIN);
  assert.ok(e.some((m) => m.includes("Quality Gate")));
  assert.ok(e.some((m) => m.includes("failure")));
});

test("checkOriginalRun: missing build job fails", () => {
  const e = checkOriginalRun({
    ...GOOD_ORIGINAL_RUN,
    jobs: GOOD_ORIGINAL_RUN.jobs.filter((j) => j.name !== "Build Release (macos-latest)"),
  }, PIN);
  assert.ok(e.some((m) => m.includes("Build Release (macos-latest)")));
});

test("checkOriginalRun: wrong head SHA fails", () => {
  const e = checkOriginalRun({ ...GOOD_ORIGINAL_RUN, headSha: "0000000000000000000000000000000000000000" }, PIN);
  assert.ok(e.some((m) => m.includes("head SHA")));
});

// ─── Draft release ────────────────────────────────────────────────────────

test("checkDraftRelease: good draft passes", () => {
  assert.deepEqual(checkDraftRelease(GOOD_RELEASE, PIN), []);
});

test("checkDraftRelease: wrong id fails", () => {
  const e = checkDraftRelease({ ...GOOD_RELEASE, id: 1 }, PIN);
  assert.ok(e.some((m) => m.includes("Release id")));
});

test("checkDraftRelease: wrong tag name fails", () => {
  const e = checkDraftRelease({ ...GOOD_RELEASE, tagName: "v0.15.0" }, PIN);
  assert.ok(e.some((m) => m.includes("Release tag")));
});

test("checkDraftRelease: already published fails", () => {
  const e = checkDraftRelease({ ...GOOD_RELEASE, draft: false, publishedAt: "2026-11-04T20:00:00Z" }, PIN);
  assert.ok(e.some((m) => m.includes("already published") || m.includes("not a draft")));
});

test("checkDraftRelease: non-draft fails", () => {
  const e = checkDraftRelease({ ...GOOD_RELEASE, draft: false }, PIN);
  assert.ok(e.some((m) => m.includes("not a draft")));
});

test("checkDraftRelease: prerelease fails", () => {
  const e = checkDraftRelease({ ...GOOD_RELEASE, prerelease: true }, PIN);
  assert.ok(e.some((m) => m.includes("prerelease")));
});

// ─── Asset set ────────────────────────────────────────────────────────────

test("checkAssetSet: good assets pass", () => {
  assert.deepEqual(checkAssetSet(makeGoodAssets(), PIN), []);
});

test("checkAssetSet: missing asset fails", () => {
  const assets = makeGoodAssets().filter((a) => a.id !== 710677092);
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("Missing")));
  assert.ok(e.some((m) => m.includes("amd64.deb") || m.includes("710677092")));
});

test("checkAssetSet: extra asset fails", () => {
  const assets = [...makeGoodAssets(), { id: 999999999, name: "extra.file", size: 100, digest: "sha256:00" }];
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("Unexpected")));
  assert.ok(e.some((m) => m.includes("extra.file") || m.includes("999999999")));
});

test("checkAssetSet: wrong size fails", () => {
  const assets = makeGoodAssets().map((a) =>
    a.id === 710677092 ? { ...a, size: a.size + 1 } : a);
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("size")));
});

test("checkAssetSet: wrong digest fails", () => {
  const assets = makeGoodAssets().map((a) =>
    a.id === 710677092 ? { ...a, digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000" } : a);
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("digest") || m.includes("sha256:0000")));
});

test("checkAssetSet: duplicate asset id fails", () => {
  const assets = [...makeGoodAssets(), { id: 710677092, name: "TailDrop_0.15.1_amd64.deb", size: 5311804, digest: "sha256:f3e015cdecda3f20a44a2ffa5f5341c541cbf9bf869c29b72e6295b717bad041" }];
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("Duplicate")));
});

test("checkAssetSet: wrong count fails", () => {
  const assets = makeGoodAssets().slice(0, 10);
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("count") || m.includes("10")));
});

test("checkAssetSet: zero-size asset fails", () => {
  const assets = makeGoodAssets().map((a) =>
    a.id === 710677092 ? { ...a, size: 0 } : a);
  const e = checkAssetSet(assets, PIN);
  assert.ok(e.some((m) => m.includes("zero size")));
});

// ─── Competitor ───────────────────────────────────────────────────────────

test("checkCompetitor: no active runs passes", () => {
  assert.deepEqual(checkCompetitor([], PIN), []);
  assert.deepEqual(checkCompetitor([{ id: 100, name: "past-run", status: "completed" }], PIN), []);
});

test("checkCompetitor: active run fails", () => {
  const e = checkCompetitor([{ id: 100, name: "some-workflow", status: "in_progress" }], PIN);
  assert.ok(e.some((m) => m.includes("Active competing")));
});

test("checkCompetitor: queued run fails", () => {
  const e = checkCompetitor([{ id: 100, name: "queued-workflow", status: "queued" }], PIN);
  assert.ok(e.some((m) => m.includes("Active competing")));
});

// ─── Final recheck ───────────────────────────────────────────────────────

test("checkFinalRecheck: good state passes", () => {
  const errors = checkFinalRecheck({
    release: GOOD_RELEASE,
    assets: makeGoodAssets(),
    tag: GOOD_TAG,
  }, PIN);
  assert.deepEqual(errors, []);
});

test("checkFinalRecheck: detected drift fails", () => {
  const errors = checkFinalRecheck({
    release: { ...GOOD_RELEASE, draft: false, publishedAt: "2026-11-04T20:00:00Z" },
    assets: makeGoodAssets(),
    tag: GOOD_TAG,
  }, PIN);
  assert.ok(errors.some((m) => m.includes("Final recheck:") && m.includes("not a draft")));
});

test("checkFinalRecheck: asset drift fails", () => {
  const drifted = makeGoodAssets().slice(0, 13);
  const errors = checkFinalRecheck({
    release: GOOD_RELEASE,
    assets: drifted,
    tag: GOOD_TAG,
  }, PIN);
  assert.ok(errors.some((m) => m.includes("Final recheck:") && m.includes("Missing")));
});

// ═══════════════════════════════════════════════════════════════════════════════
//  Composed evaluateRecovery tests — publish callback counting
// ═══════════════════════════════════════════════════════════════════════════════

function goodReality() {
  return {
    identity: { ...GOOD_IDENTITY },
    tag: GOOD_TAG,
    originalRun: GOOD_ORIGINAL_RUN,
    release: GOOD_RELEASE,
    assets: makeGoodAssets(),
    downloaded: [],  // Empty by default — failure tests replace this when needed
    manifest: {
      version: "0.15.1",
      pub_date: "2026-11-04T19:43:00Z",
      platforms: {
        "darwin-aarch64": {
          signature: "sig-darwin-aarch64",
          url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.1/TailDrop_universal.app.tar.gz",
        },
        "darwin-x86_64": {
          signature: "sig-darwin-x86_64",
          url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.1/TailDrop_universal.app.tar.gz",
        },
        "windows-x86_64": {
          signature: "sig-win",
          url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.1/TailDrop_0.15.1_x64-setup.exe",
        },
        "linux-x86_64": {
          signature: "sig-linux",
          url: "https://github.com/cortexuvula/taildrop-gui/releases/download/v0.15.1/TailDrop_0.15.1_amd64.AppImage",
        },
      },
    },
    assetNames: makeGoodAssets().map((a) => a.name),
    activeRuns: [],
    finalRecheck: {
      release: GOOD_RELEASE,
      assets: makeGoodAssets(),
      tag: GOOD_TAG,
    },
  };
}

test("evaluateRecovery: happy path calls publish exactly once", async () => {
  let calls = 0;
  const reality = goodReality();
  // Provide mock downloaded bytes.  Use small buffers (not full-size) and
  // override the pin's expected sizes/digests to match.
  const { createHash } = await import("node:crypto");
  const mockPin = JSON.parse(JSON.stringify(PIN));
  const mockDownloaded = mockPin.assets.map((exp) => {
    const bytes = Buffer.from(exp.name); // small, deterministic content
    return { id: exp.id, bytes };
  });
  // Override pin's expected sizes and digests to match our small mock bytes.
  for (let i = 0; i < mockPin.assets.length; i++) {
    mockPin.assets[i].size = mockDownloaded[i].bytes.length;
    const hash = createHash("sha256");
    hash.update(mockDownloaded[i].bytes);
    mockPin.assets[i].digest = "sha256:" + hash.digest("hex");
  }
  reality.downloaded = mockDownloaded;
  // Also override reality.assets to match mockPin (goodReality() copies PIN
  // sizes which differ from mockPin).
  reality.assets = mockPin.assets.map(a => ({ id: a.id, name: a.name, size: a.size, digest: a.digest }));
  // Override finalRecheck assets too — goodReality() uses makeGoodAssets()
  // which copies PIN sizes, not mockPin sizes.
  if (reality.finalRecheck) {
    reality.finalRecheck.assets = mockPin.assets.map(a => ({ id: a.id, name: a.name, size: a.size, digest: a.digest }));
  }
  const r = await evaluateRecovery(reality, mockPin, () => { calls++; });
  assert.equal(r.ok, true, "expected ok=true");
  assert.equal(r.published, true, "expected published=true");
  assert.equal(r.errors.length, 0, `expected no errors, got: ${JSON.stringify(r.errors)}`);
  assert.equal(calls, 1, "expected exactly 1 publish call");
});

test("evaluateRecovery: wrong repo — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.identity = { ...reality.identity, repo: "other/repo" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: wrong event — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.identity = { ...reality.identity, event: "push" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: wrong ref — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.identity = { ...reality.identity, ref: "refs/heads/develop" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: wrong confirmation — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.identity = { ...reality.identity, confirmation: "wrong" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: wrong tag object SHA — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.tag = { ...reality.tag, tagObjectSha: "0000000000000000000000000000000000000000" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: wrong peeled SHA — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.tag = { ...reality.tag, peeledSha: "0000000000000000000000000000000000000000" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: original run missing Quality Gate — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.originalRun = { ...reality.originalRun, jobs: reality.originalRun.jobs.filter((j) => j.name !== "Quality Gate") };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: release not a draft — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.release = { ...reality.release, draft: false, publishedAt: "2026-11-04T20:00:00Z" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: missing asset — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.assets = reality.assets.filter((a) => a.id !== 710677092);
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: extra asset — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.assets = [...reality.assets, { id: 999, name: "extra.file", size: 100, digest: "sha256:00" }];
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: drifted asset size — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.assets = reality.assets.map((a) =>
    a.id === 710677092 ? { ...a, size: a.size + 1 } : a);
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: asset wrong digest — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.assets = reality.assets.map((a) =>
    a.id === 710677092 ? { ...a, digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000" } : a);
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: active competitor — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.activeRuns = [{ id: 100, name: "release", status: "in_progress" }];
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

test("evaluateRecovery: duplicate asset id — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.assets = [...reality.assets, { id: 710677092, name: "TailDrop_0.15.1_amd64.deb", size: 5311804, digest: "sha256:f3e015cdecda3f20a44a2ffa5f5341c541cbf9bf869c29b72e6295b717bad041" }];
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
});

// ═══════════════════════════════════════════════════════════════════════════════
//  R1 correction: expanded coverage for blockers identified by reviewer
// ═══════════════════════════════════════════════════════════════════════════════

// ─── checkIdentity: remoteSha binding ───────────────────────────────────────

test("checkIdentity: missing remote SHA fails", () => {
  const e = checkIdentity({ ...GOOD_IDENTITY, remoteSha: "" }, PIN);
  assert.ok(e.some((m) => m.includes("Remote master SHA")));
});

test("checkIdentity: mismatched remote SHA fails", () => {
  const e = checkIdentity({
    ...GOOD_IDENTITY,
    remoteSha: "0000000000000000000000000000000000000000",
  }, PIN);
  assert.ok(e.some((m) => m.includes("Remote master SHA")));
});

test("checkIdentity: matching remote SHA passes", () => {
  const e = checkIdentity({
    ...GOOD_IDENTITY,
    remoteSha: GOOD_IDENTITY.execSha,
  }, PIN);
  assert.deepEqual(e, []);
});

// ─── checkAssetBytes: empty/partial downloads ──────────────────────────────

test("checkAssetBytes: no downloaded assets fails", () => {
  const e = checkAssetBytes([], PIN);
  assert.ok(e.some((m) => m.includes("No assets downloaded")));
});

test("checkAssetBytes: missing some assets fails", () => {
  const d = PIN.assets.slice(0, 10).map((a) => ({
    id: a.id,
    bytes: Buffer.alloc(a.size, 0),
  }));
  const e = checkAssetBytes(d, PIN);
  // Should report which assets were NOT downloaded.
  assert.ok(e.length > 0, "expected errors for missing assets");
  assert.ok(e.some((m) => m.includes("was not downloaded")), "expected 'not downloaded' error");
});

test("checkAssetBytes: wrong size fails", () => {
  const pinAssets = PIN.assets.slice(0, 1);
  const d = pinAssets.map((a) => ({
    id: a.id,
    bytes: Buffer.alloc(a.size + 1, 0),
  }));
  const e = checkAssetBytes(d, { ...PIN, assets: pinAssets });
  assert.ok(e.some((m) => m.includes("size")), "expected size mismatch error");
});

// ─── checkCompetitor: expanded status coverage ─────────────────────────────

test("checkCompetitor: queued run fails", () => {
  const e = checkCompetitor([{ id: 100, name: "queued-wf", status: "queued" }], {});
  assert.ok(e.some((m) => m.includes("Active competing") && m.includes("queued")));
});

test("checkCompetitor: pending run fails", () => {
  const e = checkCompetitor([{ id: 100, name: "pending-wf", status: "pending" }], {});
  assert.ok(e.some((m) => m.includes("Active competing") && m.includes("pending")));
});

test("checkCompetitor: waiting run fails", () => {
  const e = checkCompetitor([{ id: 100, name: "waiting-wf", status: "waiting" }], {});
  assert.ok(e.some((m) => m.includes("Active competing") && m.includes("waiting")));
});

test("checkCompetitor: completed run passes", () => {
  const e = checkCompetitor([{ id: 100, name: "done-wf", status: "completed" }], {});
  assert.deepEqual(e, []);
});

test("checkCompetitor: multiple non-completed statuses all reported", () => {
  const e = checkCompetitor([
    { id: 1, name: "wf1", status: "queued" },
    { id: 2, name: "wf2", status: "in_progress" },
    { id: 3, name: "wf3", status: "pending" },
  ], {});
  assert.equal(e.length, 3, "expected all 3 non-completed runs to be reported");
});

// ─── checkFinalRecheck: tag drift ──────────────────────────────────────────

test("checkFinalRecheck: tag object drift fails", () => {
  const errors = checkFinalRecheck({
    release: GOOD_RELEASE,
    assets: makeGoodAssets(),
    tag: { ...GOOD_TAG, tagObjectSha: "0000000000000000000000000000000000000000" },
  }, PIN);
  assert.ok(
    errors.some((m) => m.includes("Final recheck:") && m.includes("Tag object SHA")),
    "expected final recheck to catch tag object drift"
  );
});

test("checkFinalRecheck: peeled commit drift fails", () => {
  const errors = checkFinalRecheck({
    release: GOOD_RELEASE,
    assets: makeGoodAssets(),
    tag: { ...GOOD_TAG, peeledSha: "0000000000000000000000000000000000000000" },
  }, PIN);
  assert.ok(
    errors.some((m) => m.includes("Final recheck:") && m.includes("Peeled commit SHA")),
    "expected final recheck to catch peeled SHA drift"
  );
});

// ─── evaluateRecovery: empty downloads ─────────────────────────────────────

test("evaluateRecovery: empty downloaded assets — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  // Already has downloaded: [] — must fail with new check
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false, "expected ok=false for empty downloads");
  assert.equal(r.published, false, "expected published=false for empty downloads");
  assert.equal(calls, 0, "expected 0 publish calls for empty downloads");
  assert.ok(r.errors.some((m) => m.includes("No assets downloaded")), "expected 'No assets downloaded' error");
});

test("evaluateRecovery: missing remote SHA — publish never called", async () => {
  let calls = 0;
  const reality = goodReality();
  reality.identity = { ...reality.identity, remoteSha: "" };
  const r = await evaluateRecovery(reality, PIN, () => { calls++; });
  assert.equal(r.ok, false);
  assert.equal(r.published, false);
  assert.equal(calls, 0);
  assert.ok(r.errors.some((m) => m.includes("Remote master SHA")), "expected remote SHA error");
});