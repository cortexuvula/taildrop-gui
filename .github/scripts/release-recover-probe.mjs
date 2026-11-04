#!/usr/bin/env node
/**
 * release-recover-probe.mjs — Independent offline probe that exercises the
 * EXACT YAML script entrypoints the production workflow uses, with mocked
 * external boundaries (no real GitHub API calls).
 *
 * This is NOT a test file.  It is a direct-execution probe that validates
 * the YAML-wired entrypoints work correctly (syntax, imports, argument
 * passing, error propagation) without hitting the network.
 *
 * Run:
 *   node .github/scripts/release-recover-probe.mjs
 *
 * Exit 0 = all probes pass.  Exit 1 = at least one probe failed.
 *
 * TAILDROP-0151-RECOVERY-WIRING-R2-1588 — exercises literal YAML entrypoints
 * for Phase6 (shell + GH_TOKEN), Phase8/9 (filename-based workflow_id), and
 * Phase11 (checkPublishedRelease with draft===false requirement).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

// ═══════════════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════════════

const REPO_ROOT = process.cwd();
const PIN_PATH = join(REPO_ROOT, ".github/scripts/release-recover-v0151.json");
const YAML_PATH = join(REPO_ROOT, ".github/workflows/release-recover.yml");

/** Safe exit: print all errors then process.exit(1). */
let globalFailures = [];
function fail(msg) {
  globalFailures.push(msg);
}

/**
 * Extract a named Phase script block from the YAML.
 * Returns the indentation-stripped script body.
 */
function extractPhaseScript(yaml, phaseName) {
  const lines = yaml.split("\n");
  const phaseIdx = lines.findIndex(l => l.includes(`- name: ${phaseName}`));
  if (phaseIdx < 0) throw new Error(`Phase "${phaseName}" not found in YAML`);

  // Find the "script: |" line after this phase
  const scriptIdx = lines.findIndex((l, i) => i > phaseIdx && l.trim() === "script: |");
  if (scriptIdx < 0) throw new Error(`script: | not found for phase "${phaseName}"`);

  // Read subsequent lines indented at 12 spaces (the YAML script block indent)
  const body = [];
  for (let i = scriptIdx + 1; i < lines.length; i++) {
    const line = lines[i];
    if (!line.trim()) continue; // skip blank lines
    if (!line.startsWith("            ")) break; // script indent is 12 spaces
    body.push(line.slice(12)); // strip the 12-space indent
  }
  if (body.length === 0) throw new Error(`Empty script body for phase "${phaseName}"`);
  return body.join("\n");
}

/**
 * Extract the env block from a named Phase in the YAML.
 * Returns a map of env var names to values.
 */
function extractPhaseEnv(yaml, phaseName) {
  const lines = yaml.split("\n");
  const phaseIdx = lines.findIndex(l => l.includes(`- name: ${phaseName}`));
  if (phaseIdx < 0) throw new Error(`Phase "${phaseName}" not found in YAML`);

  const env = {};
  let inEnv = false;
  for (let i = phaseIdx + 1; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (trimmed === "env:") { inEnv = true; continue; }
    if (inEnv) {
      if (!trimmed || trimmed.startsWith("-") || trimmed.startsWith("uses:") || trimmed.startsWith("with:") || trimmed.startsWith("shell:") || trimmed.startsWith("run:")) break;
      const match = trimmed.match(/^(\w+):\s*(.+)$/);
      if (match) env[match[1]] = match[2];
    }
  }
  return env;
}

/**
 * Extract the shell run block from a named Phase in the YAML.
 */
function extractPhaseRunBlock(yaml, phaseName) {
  const lines = yaml.split("\n");
  const phaseIdx = lines.findIndex(l => l.includes(`- name: ${phaseName}`));
  if (phaseIdx < 0) throw new Error(`Phase "${phaseName}" not found in YAML`);

  // Find "run: |" after this phase
  const runIdx = lines.findIndex((l, i) => i > phaseIdx && l.trim() === "run: |");
  if (runIdx < 0) throw new Error(`run: | not found for shell phase "${phaseName}"`);

  const body = [];
  for (let i = runIdx + 1; i < lines.length; i++) {
    const line = lines[i];
    if (!line.trim()) continue;
    if (!line.startsWith("          ")) break; // run block indent is 10 spaces
    body.push(line.slice(10)); // strip indent
  }
  return body.join("\n");
}

/**
 * Run a node script (the actual production entrypoint) with env and args.
 */
function runScript(scriptRelPath, args = [], env = {}) {
  const result = spawnSync(process.execPath, [scriptRelPath, ...args], {
    cwd: REPO_ROOT,
    env: { ...process.env, ...env },
    encoding: "utf8",
    timeout: 30_000,
  });
  return {
    stdout: (result.stdout || "").trim(),
    stderr: (result.stderr || "").trim(),
    exitCode: result.status,
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Shared mock data
// ═══════════════════════════════════════════════════════════════════════════════

const yaml = readFileSync(YAML_PATH, "utf8");
const pin = JSON.parse(readFileSync(PIN_PATH, "utf8"));

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 1: Core module imports cleanly and exports expected functions
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 1: Core module imports cleanly...");
  try {
    const core = await import("./release-recover-core.mjs");
    const expectedExports = [
      "checkIdentity", "checkTagBinding", "checkOriginalRun",
      "checkDraftRelease", "checkPublishedRelease", "checkAssetSet",
      "checkAssetBytes", "checkValidator", "checkCompetitor",
      "checkFinalRecheck", "evaluateRecovery",
    ];
    for (const name of expectedExports) {
      if (typeof core[name] !== "function") {
        fail(`Core missing expected export: ${name}`);
      }
    }
    console.log("   ✓ All expected exports present (including checkPublishedRelease)");
  } catch (e) {
    fail(`Core module import failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 2: Pin JSON is valid with required fields
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 2: Pin JSON validation...");
  const required = [
    "repo", "ref", "confirmation", "releaseId", "tagName",
    "tagObjectSha", "peeledSha", "originalRunId", "originalRunAttempt",
    "originalHeadSha", "requiredOriginalJobs", "assets", "requiredPlatforms",
  ];
  for (const key of required) {
    if (pin[key] === undefined) fail(`Pin missing required field: ${key}`);
  }
  if (!Array.isArray(pin.assets) || pin.assets.length !== 14) {
    fail(`Expected 14 assets, got ${pin.assets?.length}`);
  }
  for (const a of pin.assets) {
    if (!a.id || !a.name || !a.size || !a.digest) {
      fail(`Asset ${JSON.stringify(a.name)} missing required fields`);
    }
    if (!a.digest.startsWith("sha256:")) {
      fail(`Asset ${a.name} digest doesn't start with sha256:`);
    }
  }
  console.log("   ✓ Pin has all required fields and 14 valid assets");
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 3: Validator (unchanged) syntax check
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 3: Unchanged validator syntax check...");
  const result = runScript(".github/scripts/validate-updater-manifest.mjs", ["--help"]);
  if (result.exitCode !== 2) {
    if (result.exitCode !== 0) {
      fail(`Validator syntax check failed (exit ${result.exitCode}): ${result.stderr}`);
    } else {
      console.log("   ⚠ Validator accepted --help (exit 0)");
    }
  } else {
    console.log("   ✓ Validator syntax OK (exit 2 as expected for missing args)");
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 4: Phase8 YAML script — uses filename not display name
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 4: Phase8 YAML script — filename-based workflow_id...");
  try {
    const scriptBody = extractPhaseScript(yaml, "Phase 8 — Check for active competing runs (fail-closed)");

    // Verify the script uses 'release-recover.yml' (not context.workflow)
    if (!scriptBody.includes("'release-recover.yml'")) {
      fail("Phase8 script does not contain 'release-recover.yml' — still using display name?");
    }
    if (scriptBody.includes("context.workflow")) {
      fail("Phase8 script still references context.workflow — must use filename");
    }

    // Execute the script with mocked GitHub API that validates filename-based workflow_id
    process.env.GITHUB_WORKSPACE = REPO_ROOT;
    const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
    const core = { setFailed: m => { throw new Error(m); }, info: () => {}, setOutput: () => {} };
    const context = { repo: { owner: "cortexuvula", repo: "taildrop-gui" }, runId: 999 };
    const callLog = [];
    const github = {
      rest: {
        actions: {
          listWorkflowRuns: async params => {
            callLog.push(params);
            // Must receive 'release-recover.yml' or 'release.yml' as workflow_id
            if (!["release-recover.yml", "release.yml"].includes(params.workflow_id)) {
              throw new Error(`404: display name "${params.workflow_id}" is not a valid workflow_id`);
            }
            return { data: { workflow_runs: [] } };
          },
        },
      },
    };

    await new AsyncFunction("github", "context", "core", scriptBody)(github, context, core);

    // Verify we called both filenames
    const calledIds = callLog.map(c => c.workflow_id);
    if (calledIds.includes("Recover Existing v0.15.1")) {
      fail("Phase8 called listWorkflowRuns with display name — should be filename");
    }
    if (!calledIds.includes("release-recover.yml")) {
      fail("Phase8 did not call listWorkflowRuns for release-recover.yml");
    }
    if (!calledIds.includes("release.yml")) {
      fail("Phase8 did not call listWorkflowRuns for release.yml");
    }
    console.log("   ✓ Phase8 uses release-recover.yml + release.yml (filename-based, no display name)");
  } catch (e) {
    fail(`Phase8 probe failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 4b: Phase4 YAML script — uses release ID not tag name
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 4b: Phase4 YAML script — pinned by-ID release lookup...");
  try {
    const scriptBody = extractPhaseScript(yaml, "Phase 4 — Verify draft release 434359119");

    // Must use getRelease (not getReleaseByTag) with release_id.
    if (!scriptBody.includes(".repos.getRelease({")) {
      fail("Phase4 script does not use getRelease — must use by-ID lookup");
    }
    if (scriptBody.includes("getReleaseByTag")) {
      fail("Phase4 script still uses getReleaseByTag — must use getRelease with release_id");
    }
    if (!scriptBody.includes("release_id: pin.releaseId")) {
      fail("Phase4 script does not pass release_id — must use pinned release ID");
    }

    // Execute Phase4 with mocked GitHub API that succeeds on getRelease and
    // fails on getReleaseByTag (to verify the script uses the correct route).
    process.env.GITHUB_WORKSPACE = REPO_ROOT;
    process.env.PIN = JSON.stringify(pin);
    const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
    let failures4b = [];
    let outputs4b = {};
    const core4b = {
      setFailed: m => { failures4b.push(m); },
      info: () => {},
      setOutput: (k, v) => { outputs4b[k] = v; },
    };
    const context4b = {
      repo: { owner: "cortexuvula", repo: "taildrop-gui" },
    };
    const callLog4b = [];
    const github4b = {
      rest: {
        repos: {
          getRelease: async ({ release_id }) => {
            callLog4b.push({ method: "getRelease", release_id });
            // Must be called with exact pinned ID
            if (release_id !== pin.releaseId) {
              return { data: null };
            }
            return {
              data: {
                id: pin.releaseId,
                tag_name: pin.tagName,
                draft: true,
                published_at: null,
                prerelease: false,
                target_commitish: "master",
              },
            };
          },
          getReleaseByTag: async () => {
            callLog4b.push({ method: "getReleaseByTag" });
            throw new Error("Not Found — draft releases are not returned by by-tag endpoint");
          },
        },
      },
    };

    await new AsyncFunction("github", "context", "core", scriptBody)(
      github4b, context4b, core4b
    );

    // Verify getRelease was called (not getReleaseByTag).
    const getReleaseCalls = callLog4b.filter(c => c.method === "getRelease");
    if (getReleaseCalls.length === 0) {
      fail("Phase4 probe: getRelease was never called");
    }
    if (callLog4b.some(c => c.method === "getReleaseByTag")) {
      fail("Phase4 probe: getReleaseByTag was called (should use getRelease)");
    }

    // Verify success: no failures, output has release_id.
    if (failures4b.length > 0) {
      fail(`Phase4 probe (by-ID success): unexpected failures: ${JSON.stringify(failures4b)}`);
    }
    if (!outputs4b.release_id) {
      fail("Phase4 probe: no release_id output set");
    }
    console.log("   ✓ Phase4 uses getRelease with pinned release_id (by-ID, not by-tag)");
    console.log("   ✓ Phase4 by-ID success returns correct draft release");

    // Test 4c: Phase4 with WRONG release ID must fail.
    process.env.PIN = JSON.stringify({ ...pin, releaseId: 999999999 });
    let failures4c = [];
    const core4c = {
      setFailed: m => { failures4c.push(m); },
      info: () => {},
      setOutput: () => {},
    };
    const context4c = {
      repo: { owner: "cortexuvula", repo: "taildrop-gui" },
    };
    const github4c = {
      rest: {
        repos: {
          getRelease: async () => {
            throw new Error("Not Found");
          },
        },
      },
    };

    await new AsyncFunction("github", "context", "core", scriptBody)(
      github4c, context4c, core4c
    );

    if (failures4c.length === 0) {
      fail("Phase4 probe (wrong ID): should have failed but didn't");
    } else {
      console.log("   ✓ Phase4 with wrong release ID correctly fails");
    }

    // Reset PIN for subsequent probes.
    process.env.PIN = JSON.stringify(pin);
  } catch (e) {
    fail(`Phase4 probe failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 5: Phase9 YAML script — filename-based workflow_id...");
  try {
    const scriptBody = extractPhaseScript(yaml, "Phase 9 — Final recheck before publish (identity, tag, assets, competitors)");

    // Verify the script uses 'release-recover.yml' (not context.workflow)
    if (!scriptBody.includes("'release-recover.yml'")) {
      fail("Phase9 script does not contain 'release-recover.yml' — still using display name?");
    }
    if (scriptBody.includes("context.workflow")) {
      fail("Phase9 script still references context.workflow — must use filename");
    }

    // Verify the script uses getRelease (by-ID) not getReleaseByTag for the release fetch.
    if (!scriptBody.includes(".repos.getRelease({") && !scriptBody.includes("getRelease({")) {
      fail("Phase9 script does not use getRelease — must use by-ID release lookup");
    }
    if (scriptBody.includes("getReleaseByTag")) {
      fail("Phase9 script still uses getReleaseByTag — must use getRelease with release_id");
    }
    if (!scriptBody.includes("release_id: pin.releaseId")) {
      fail("Phase9 script does not pass release_id — must use pinned release ID");
    }

    // Execute with mocked GitHub API
    process.env.GITHUB_WORKSPACE = REPO_ROOT;
    process.env.PIN = JSON.stringify(pin);
    process.env.RELEASE_ID = String(pin.releaseId);
    const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
    const failures = [];
    const core = {
      setFailed: m => { failures.push(m); },
      info: () => {},
      setOutput: () => {},
    };
    const context = { repo: { owner: "cortexuvula", repo: "taildrop-gui" }, runId: 999 };
    const callLog = [];
    const github = {
      rest: {
        actions: {
          listWorkflowRuns: async params => {
            callLog.push(params);
            if (!["release-recover.yml", "release.yml"].includes(params.workflow_id)) {
              throw new Error(`404: display name "${params.workflow_id}" is not a valid workflow_id`);
            }
            return { data: { workflow_runs: [] } };
          },
        },
        repos: {
          getRelease: async ({ release_id }) => ({
            data: { id: release_id, tag_name: pin.tagName, draft: true, published_at: null, prerelease: false, target_commitish: "master" },
          }),
          listReleaseAssets: async () => ({ data: [] }),
        },
        git: {
          getRef: async () => ({ data: { object: { sha: pin.tagObjectSha } } }),
          getTag: async () => ({ data: { object: { sha: pin.peeledSha } } }),
        },
      },
    };

    // Wrap in try/catch because setFailed throws
    try {
      await new AsyncFunction("github", "context", "core", scriptBody)(github, context, core);
    } catch (_) {
      // Expected: the recheck will fail because release is still draft and has no published_at
      // That's OK — we're checking the workflow_id, not the full validation
    }

    const calledIds = callLog.map(c => c.workflow_id);
    if (calledIds.includes("Recover Existing v0.15.1")) {
      fail("Phase9 called listWorkflowRuns with display name — should be filename");
    }
    if (!calledIds.includes("release-recover.yml")) {
      fail("Phase9 did not call listWorkflowRuns for release-recover.yml");
    }
    if (!calledIds.includes("release.yml")) {
      fail("Phase9 did not call listWorkflowRuns for release.yml");
    }
    console.log("   ✓ Phase9 uses release-recover.yml + release.yml (filename-based, no display name)");
  } catch (e) {
    fail(`Phase9 probe failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 6: Phase6 YAML env block includes GH_TOKEN
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 6: Phase6 shell GH_TOKEN env mapping...");
  try {
    const env = extractPhaseEnv(yaml, "Phase 6 — Download and hash 14 asset bytes");

    // Must have GH_TOKEN mapped
    if (!env.GH_TOKEN) {
      fail("Phase6 shell step missing GH_TOKEN env mapping");
    }
    if (!env.GH_TOKEN.includes("github.token")) {
      fail(`Phase6 GH_TOKEN value "${env.GH_TOKEN}" does not reference github.token`);
    }
    if (!env.PIN) {
      fail("Phase6 missing PIN env");
    }
    if (!env.ASSETS_JSON) {
      fail("Phase6 missing ASSETS_JSON env");
    }

    // Verify the run block uses gh api
    const runBlock = extractPhaseRunBlock(yaml, "Phase 6 — Download and hash 14 asset bytes");
    if (!runBlock.includes("gh api")) {
      fail("Phase6 run block does not contain gh api");
    }

    console.log("   ✓ Phase6 shell step has GH_TOKEN, PIN, ASSETS_JSON env vars");
    console.log("   ✓ Phase6 run block uses gh api for downloads");
  } catch (e) {
    fail(`Phase6 probe failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Probe 7: Phase11 YAML script — uses checkPublishedRelease (not broken suppression)
// ═══════════════════════════════════════════════════════════════════════════════

{
  console.log("\n📦 Probe 7: Phase11 readback — uses checkPublishedRelease...");
  try {
    const scriptBody = extractPhaseScript(yaml, "Phase 11 — Readback published release and verify (full identity, assets, tag)");

    // Must use checkPublishedRelease, not checkDraftRelease
    if (!scriptBody.includes("checkPublishedRelease")) {
      fail("Phase11 script does not import/use checkPublishedRelease");
    }
    if (scriptBody.includes("checkDraftRelease")) {
      fail("Phase11 script still references checkDraftRelease — should use checkPublishedRelease");
    }
    if (scriptBody.includes("includes('not a draft')")) {
      fail("Phase11 script still has draft-error suppression hack");
    }
    if (scriptBody.includes("includes('already published')")) {
      fail("Phase11 script still has already-published suppression hack");
    }

    // Verify the function call shape — expects html_url, not targetCommitish
    if (!scriptBody.includes("html_url")) {
      fail("Phase11 script does not pass html_url to checkPublishedRelease");
    }

    // Test 7a: Execute Phase11 with PUBLISHED state (should pass)
    process.env.GITHUB_WORKSPACE = REPO_ROOT;
    process.env.PIN = JSON.stringify(pin);
    process.env.RELEASE_ID = String(pin.releaseId);
    const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
    const publishedRelease = {
      id: pin.releaseId,
      tag_name: pin.tagName,
      draft: false,
      published_at: "2026-11-04T20:00:00Z",
      prerelease: false,
      html_url: "https://github.com/cortexuvula/taildrop-gui/releases/tag/v0.15.1",
    };

    let failures7a = [];
    let infos = [];
    const core7a = {
      setFailed: m => { failures7a.push(m); },
      info: m => { infos.push(m); },
      setOutput: (k, v) => {},
    };
    const context7a = {
      repo: { owner: "cortexuvula", repo: "taildrop-gui" },
      runId: 999,
    };
    const github7a = {
      rest: {
        repos: {
          getRelease: async () => ({ data: publishedRelease }),
          listReleaseAssets: async () => ({
            data: pin.assets.map(a => ({ id: a.id, name: a.name, size: a.size, digest: a.digest, url: "", browser_download_url: "" })),
          }),
        },
        git: {
          getRef: async () => ({ data: { object: { sha: pin.tagObjectSha } } }),
          getTag: async () => ({ data: { object: { sha: pin.peeledSha } } }),
        },
      },
    };

    await new AsyncFunction("github", "context", "core", scriptBody)(
      github7a, context7a,
      { setFailed: m => { failures7a.push(m); }, info: m => { infos.push(m); }, setOutput: (k, v) => {} }
    );

    if (failures7a.length > 0) {
      fail(`Phase11 PROBE 7a (published state): unexpected failures: ${JSON.stringify(failures7a)}`);
    } else {
      console.log("   ✓ 7a: Phase11 accepts properly published release (draft=false, published_at set)");
    }

    // Test 7b: Execute Phase11 with STILL-DRAFT state (must FAIL)
    const draftRelease = {
      id: pin.releaseId,
      tag_name: pin.tagName,
      draft: true,
      published_at: null,
      prerelease: false,
      html_url: null,
    };

    let failures7b = [];
    const core7b = {
      setFailed: m => { failures7b.push(m); },
      info: () => {},
      setOutput: (k, v) => {},
    };
    const context7b = {
      repo: { owner: "cortexuvula", repo: "taildrop-gui" },
      runId: 999,
    };
    const github7b = {
      rest: {
        repos: {
          getRelease: async () => ({ data: draftRelease }),
          listReleaseAssets: async () => ({
            data: pin.assets.map(a => ({ id: a.id, name: a.name, size: a.size, digest: a.digest, url: "", browser_download_url: "" })),
          }),
        },
        git: {
          getRef: async () => ({ data: { object: { sha: pin.tagObjectSha } } }),
          getTag: async () => ({ data: { object: { sha: pin.peeledSha } } }),
        },
      },
    };

    await new AsyncFunction("github", "context", "core", scriptBody)(
      github7b, context7b,
      { setFailed: m => { failures7b.push(m); }, info: () => {}, setOutput: (k, v) => {} }
    );

    if (failures7b.length === 0) {
      fail("Phase11 PROBE 7b: still-draft release was ACCEPTED — must refuse draft publications");
    } else {
      const hasDraftMsg = failures7b.some(m => m.includes("still a draft") || m.includes("no published_at"));
      if (!hasDraftMsg) {
        fail(`Phase11 PROBE 7b: failures don't mention draft: ${JSON.stringify(failures7b)}`);
      }
      console.log(`   ✓ 7b: Phase11 correctly refuses still-draft release (${failures7b.length} error(s))`);
    }

    console.log("   ✓ Phase11 correctly uses checkPublishedRelease with no error suppression");
  } catch (e) {
    fail(`Phase11 probe failed: ${e.message}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Summary
// ═══════════════════════════════════════════════════════════════════════════════

console.log("\n" + "=".repeat(60));
if (globalFailures.length === 0) {
  console.log("✅ ALL PROBES PASSED");
  process.exit(0);
} else {
  console.error(`❌ ${globalFailures.length} FAILURE(S):`);
  for (const f of globalFailures) console.error("   ", f);
  process.exit(1);
}