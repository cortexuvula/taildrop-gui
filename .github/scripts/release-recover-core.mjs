#!/usr/bin/env node
/**
 * release-recover-core.mjs — Pure validation logic for TailDrop v0.15.1
 * preservation-only recovery.  Every function returns an ARRAY of error
 * strings (empty === pass).  The composed {@link evaluateRecovery} drives
 * the full fail-closed sequence and calls a publish callback only when all
 * guards pass.
 *
 * All functions are deterministic: they act on provided data, never on
 * ambient state (no env, no network, no filesystem).
 *
 * @module release-recover-core
 */

import { createHash } from "node:crypto";

// ─── Identity bindings ───────────────────────────────────────────────────────

/**
 * Check repo, event, ref, confirmation and SHA bindings.
 * @param {object} r
 * @param {string} r.repo         github.repository ("owner/repo")
 * @param {string} r.event        github.event_name
 * @param {string} r.ref          github.ref
 * @param {string} r.confirmation workflow_dispatch input
 * @param {string} r.execSha      SHA of the commit the workflow runs on
 * @param {string} r.remoteSha    SHA of origin/master HEAD (checked separately)
 * @param {object} pin            parsed pin JSON
 * @returns {string[]}
 */
export function checkIdentity(r, pin) {
  const e = [];
  if (r.repo !== pin.repo)
    e.push(`Repository "${r.repo}" !== "${pin.repo}"`);
  if (r.event !== "workflow_dispatch")
    e.push(`Event "${r.event}" !== "workflow_dispatch"`);
  if (r.ref !== pin.ref)
    e.push(`Ref "${r.ref}" !== "${pin.ref}"`);
  if (r.confirmation !== pin.confirmation)
    e.push("Confirmation input does not match the fixed string");
  if (r.execSha === undefined || r.execSha === "")
    e.push("Executing SHA is empty or undefined");
  // Verify remote master HEAD matches executing SHA
  if (r.remoteSha === undefined || r.remoteSha === "")
    e.push("Remote master SHA is empty or undefined");
  else if (r.remoteSha !== r.execSha)
    e.push(`Remote master SHA "${r.remoteSha}" !== executing SHA "${r.execSha}"`);
  return e;
}

// ─── Tag / commit binding ────────────────────────────────────────────────────

/**
 * Verify the annotated tag object SHA and the commit it peels to.
 * @param {object} t
 * @param {string} t.tagObjectSha annotated tag object
 * @param {string} t.peeledSha    commit the tag points to
 * @param {object} pin
 * @returns {string[]}
 */
export function checkTagBinding(t, pin) {
  const e = [];
  if (t.tagObjectSha !== pin.tagObjectSha)
    e.push(`Tag object SHA mismatch: got "${t.tagObjectSha}", expected "${pin.tagObjectSha}"`);
  if (t.peeledSha !== pin.peeledSha)
    e.push(`Peeled commit SHA mismatch: got "${t.peeledSha}", expected "${pin.peeledSha}"`);
  return e;
}

// ─── Original run verification ───────────────────────────────────────────────

/**
 * Verify that original run 41897833512 attempt 1 exists and its required jobs
 * completed successfully.  The Validate Updater Manifest and Publish Release
 * jobs are expected to have failed/skipped — those are NOT required.
 * @param {object} o
 * @param {number}              o.runId
 * @param {number}              o.attempt
 * @param {string}              o.headSha
 * @param {Array<{name:string, conclusion:string}>} o.jobs
 * @param {object} pin
 * @returns {string[]}
 */
export function checkOriginalRun(o, pin) {
  const e = [];
  if (o.runId !== pin.originalRunId)
    e.push(`Original run id ${o.runId} !== ${pin.originalRunId}`);
  if (o.attempt !== pin.originalRunAttempt)
    e.push(`Original run attempt ${o.attempt} !== ${pin.originalRunAttempt}`);
  if (o.headSha !== pin.originalHeadSha)
    e.push(`Original run head SHA "${o.headSha}" !== "${pin.originalHeadSha}"`);

  for (const requiredName of pin.requiredOriginalJobs) {
    const job = o.jobs.find((j) => j.name === requiredName);
    if (!job) {
      e.push(`Required job "${requiredName}" not found in original run`);
      continue;
    }
    if (job.conclusion !== "success")
      e.push(`Required job "${requiredName}" conclusion is "${job.conclusion}" !== "success"`);
  }
  return e;
}

// ─── Draft release state ─────────────────────────────────────────────────────

/**
 * Verify release identity and draft state.
 * @param {object} rel
 * @param {number}              rel.id
 * @param {string}              rel.tagName
 * @param {boolean}             rel.draft
 * @param {string|null}         rel.publishedAt
 * @param {boolean}             rel.prerelease
 * @param {string}              rel.targetCommitish
 * @param {object} pin
 * @returns {string[]}
 */
export function checkDraftRelease(rel, pin) {
  const e = [];
  if (rel.id !== pin.releaseId)
    e.push(`Release id ${rel.id} !== ${pin.releaseId}`);
  if (rel.tagName !== pin.tagName)
    e.push(`Release tag "${rel.tagName}" !== "${pin.tagName}"`);
  if (rel.draft !== true)
    e.push(`Release is not a draft (draft=${JSON.stringify(rel.draft)})`);
  if (rel.publishedAt !== null)
    e.push(`Release already published (published_at="${rel.publishedAt}")`);
  if (rel.prerelease !== false)
    e.push(`Release prerelease=${JSON.stringify(rel.prerelease)}, expected false`);
  return e;
}

/**
 * Verify that a release is in published (non-draft) state with all
 * expected identity bindings intact.  This is the POST-publication
 * readback check — the opposite of checkDraftRelease.
 *
 * @param {object} rel
 * @param {number}              rel.id
 * @param {string}              rel.tagName
 * @param {boolean}             rel.draft       MUST be false
 * @param {string|null}         rel.publishedAt MUST be a non-empty string
 * @param {boolean}             rel.prerelease  MUST be false
 * @param {string}              rel.html_url    MUST be a non-empty string
 * @param {object} pin
 * @returns {string[]}
 */
export function checkPublishedRelease(rel, pin) {
  const e = [];
  if (rel.id !== pin.releaseId)
    e.push(`Readback: Release id ${rel.id} !== ${pin.releaseId}`);
  if (rel.tagName !== pin.tagName)
    e.push(`Readback: Release tag "${rel.tagName}" !== "${pin.tagName}"`);
  if (rel.draft !== false)
    e.push(`Readback: Release is still a draft (draft=${JSON.stringify(rel.draft)}) — expected published`);
  if (!rel.publishedAt || typeof rel.publishedAt !== 'string' || rel.publishedAt.trim() === '')
    e.push(`Readback: Release has no published_at (got "${rel.publishedAt}")`);
  if (rel.prerelease !== false)
    e.push(`Readback: Release prerelease=${JSON.stringify(rel.prerelease)}, expected false`);
  if (!rel.html_url || typeof rel.html_url !== 'string' || rel.html_url.trim() === '')
    e.push(`Readback: Release has no html_url`);
  return e;
}

// ─── Asset set integrity ─────────────────────────────────────────────────────

/**
 * Verify exact set of 14 assets: no missing, extra, duplicate, empty, or
 * mismatched id/name/size/digest.
 *
 * @param {Array<{id:number, name:string, size:number, digest:string}>} assets
 *   Current assets from the release API.
 * @param {object} pin
 * @returns {string[]}
 */
export function checkAssetSet(assets, pin) {
  const e = [];
  const expected = pin.assets;

  if (assets.length !== expected.length) {
    e.push(`Asset count ${assets.length} !== expected ${expected.length}`);
    // Continue checking so the full delta is reported.
  }

  // Build a lookup-keyed map for the current assets.
  const byId = new Map();
  for (const a of assets) {
    if (byId.has(a.id)) {
      e.push(`Duplicate asset id ${a.id} ("${a.name}")`);
    }
    byId.set(a.id, a);
  }

  for (const exp of expected) {
    const cur = byId.get(exp.id);
    if (!cur) {
      e.push(`Missing asset id ${exp.id} ("${exp.name}")`);
      continue;
    }
    if (cur.name !== exp.name)
      e.push(`Asset ${exp.id} name "${cur.name}" !== "${exp.name}"`);
    if (cur.size !== exp.size)
      e.push(`Asset ${exp.id} ("${exp.name}") size ${cur.size} !== expected ${exp.size}`);
    if (typeof cur.digest !== "string" || cur.digest === "")
      e.push(`Asset ${exp.id} ("${exp.name}") has empty or missing digest`);
    else if (cur.digest !== exp.digest)
      e.push(`Asset ${exp.id} ("${exp.name}") digest "${cur.digest}" !== "${exp.digest}"`);
    if (typeof cur.size === "number" && cur.size === 0)
      e.push(`Asset ${exp.id} ("${exp.name}") has zero size`);
  }

  // Extra assets (present in assets but not in pin)
  for (const cur of assets) {
    const match = expected.find((e) => e.id === cur.id);
    if (!match) {
      e.push(`Unexpected extra asset id ${cur.id} ("${cur.name}")`);
    }
  }

  return e;
}

// ─── Asset byte verification ─────────────────────────────────────────────────

/**
 * Verify downloaded bytes: actual size and SHA-256 digest against pins.
 *
 * @param {Array<{id:number, bytes:Buffer}>} downloaded
 * @param {object} pin
 * @returns {string[]}
 */
export function checkAssetBytes(downloaded, pin) {
  const e = [];
  if (!Array.isArray(downloaded) || downloaded.length === 0) {
    e.push("No assets downloaded — all " + pin.assets.length + " assets required");
    return e;
  }
  // Build a set of asset IDs that were downloaded
  const downloadedIds = new Set(downloaded.map(d => d.id));
  for (const exp of pin.assets) {
    if (!downloadedIds.has(exp.id)) {
      e.push(`Asset id ${exp.id} ("${exp.name}") was not downloaded`);
    }
  }
  for (const d of downloaded) {
    const exp = pin.assets.find((a) => a.id === d.id);
    if (!exp) {
      e.push(`Downloaded asset id ${d.id} is not in the expected set`);
      continue;
    }
    if (d.bytes.length !== exp.size) {
      e.push(`Asset ${exp.id} ("${exp.name}"): downloaded size ${d.bytes.length} !== expected ${exp.size}`);
    }
    // Compute SHA-256 of the downloaded bytes.
    const hash = createHash("sha256");
    hash.update(d.bytes);
    const actual = "sha256:" + hash.digest("hex");
    if (actual !== exp.digest) {
      e.push(`Asset ${exp.id} ("${exp.name}"): digest "${actual}" !== "${exp.digest}"`);
    }
  }
  return e;
}

// ─── Validator integration ────────────────────────────────────────────────────

/**
 * Validate the downloaded latest.json against the unchanged validator.
 * This function calls the validator core directly (not a subprocess) so it
 * works in both tests and workflow.
 *
 * @param {object}   manifest      parsed latest.json
 * @param {string}   version       release version tag (e.g. "v0.15.1")
 * @param {string[]} assetNames    names of all 14 release assets
 * @param {object}   pin
 * @returns {Promise<string[]>}
 */
export async function checkValidator(manifest, version, assetNames, pin) {
  const e = [];
  // Import the existing validator.
  // We use a dynamic path that works whether called from the same dir or
  // from the repo root.
  const { validateUpdaterManifest } = await import("./updater-manifest-core.mjs");
  const { ok, errors } = validateUpdaterManifest({
    manifest,
    expectedVersion: version,
    assetNames,
  });
  if (!ok) {
    for (const err of errors) e.push(`Validator: ${err}`);
  }
  // Check platform coverage matches what this release needs.
  const platforms = manifest.platforms ?? {};
  for (const plat of pin.requiredPlatforms) {
    if (!platforms[plat])
      e.push(`Validator: missing required platform "${plat}" in manifest`);
  }
  return e;
}

// ─── Competitor check ────────────────────────────────────────────────────────

/**
 * Refuse an active competing publication or recovery run.
 * Checks queued, pending, in_progress, waiting, requested — any non-completed
 * status that could conflict with this recovery.
 * @param {Array<{id:number, status:string, name:string}>} activeRuns
 * @param {object} pin
 * @returns {string[]}
 */
export function checkCompetitor(activeRuns, pin) {
  const e = [];
  if (!Array.isArray(activeRuns) || activeRuns.length === 0) return e;
  for (const run of activeRuns) {
    if (run.status === "in_progress" || run.status === "queued" || run.status === "pending" || run.status === "waiting" || run.status === "requested") {
      e.push(`Active competing run #${run.id} ("${run.name}") is ${run.status}`);
    }
  }
  return e;
}

// ─── Final TOCTOU recheck ────────────────────────────────────────────────────

/**
 * Immediately-before-publish recheck: re-read release and asset state and
 * repeat draft identity guards.  Must catch drift from the earlier snapshot.
 *
 * @param {object} re
 * @param {object} re.release   Re-read release object
 * @param {Array}  re.assets    Re-read assets
 * @param {object} re.tag       Re-read tag info
 * @param {object} pin
 * @returns {string[]}
 */
export function checkFinalRecheck(re, pin) {
  const e = [];
  // Repeat draft + identity checks on the fresh snapshot.
  const relErrors = checkDraftRelease(re.release, pin);
  e.push(...relErrors.map((msg) => `Final recheck: ${msg}`));
  const assetErrors = checkAssetSet(re.assets, pin);
  e.push(...assetErrors.map((msg) => `Final recheck: ${msg}`));
  const tagErrors = checkTagBinding(re.tag, pin);
  e.push(...tagErrors.map((msg) => `Final recheck: ${msg}`));
  return e;
}

// ─── Composed evaluation ─────────────────────────────────────────────────────

/**
 * Run the full fail-closed recovery sequence.
 *
 * @param {object}   reality           All observed state.
 * @param {object}   reality.identity  { repo, event, ref, confirmation, execSha }
 * @param {object}   reality.tag       { tagObjectSha, peeledSha }
 * @param {object}   reality.originalRun  { runId, attempt, headSha, jobs }
 * @param {object}   reality.release   Release object (from API).
 * @param {Array}    reality.assets    Release assets (from API).
 * @param {Array}    reality.downloaded  [{ id, bytes }] from download step.
 * @param {object|null} reality.manifest  Parsed latest.json.
 * @param {string[]} reality.assetNames   All asset name strings.
 * @param {Array}    reality.activeRuns   Active workflow runs.
 * @param {object}   reality.finalRecheck { release, assets, tag }
 * @param {object}   pin               Parsed pin JSON.
 * @param {Function} publish           Callback — only called when all guards pass.
 * @returns {{ ok: boolean, errors: string[], published: boolean }}
 */
export async function evaluateRecovery(reality, pin, publish) {
  const E = [];  // accumulated errors — NOT returned early.

  // Phase 1 — Identity bindings.
  E.push(...checkIdentity(reality.identity, pin));

  // Phase 2 — Tag binding.
  E.push(...checkTagBinding(reality.tag, pin));

  // Phase 3 — Original run verification.
  E.push(...checkOriginalRun(reality.originalRun, pin));

  // Phase 4 — Draft release identity.
  E.push(...checkDraftRelease(reality.release, pin));

  // Phase 5 — Asset set integrity.
  E.push(...checkAssetSet(reality.assets, pin));

  // Phase 6 — Competitor check.
  E.push(...checkCompetitor(reality.activeRuns, pin));

  // Stop here if any check failed — don't download, hash or validate
  // bytes against a broken baseline.
  if (E.length > 0) return { ok: false, errors: E, published: false };

  // Phase 7 — Asset byte verification (requires crypto).
  E.push(...await checkAssetBytes(reality.downloaded, pin));

  // Phase 8 — Run unchanged validator.
  if (reality.manifest && reality.assetNames) {
    E.push(...await checkValidator(
      reality.manifest,
      reality.identity.version ?? pin.tagName,
      reality.assetNames,
      pin,
    ));
  } else {
    E.push("Validator data not provided (manifest or assetNames missing)");
  }

  // Phase 9 — Final TOCTOU recheck.
  if (reality.finalRecheck) {
    E.push(...checkFinalRecheck(reality.finalRecheck, pin));
  }

  if (E.length > 0) return { ok: false, errors: E, published: false };

  // ALL GATES PASSED — call publish exactly once.
  if (typeof publish === "function") await publish();
  return { ok: true, errors: [], published: true };
}