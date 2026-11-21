# M2b handoff — POSIX exec-agent parity + owner-decision carry-forwards

Work by Ada (task t_d7720305) on top of M2a (39c7f77). This file documents
the M2b delta: owner-decision S-2, the Codie M2a carry-forwards (W-1/S-3/S-1),
and the vendored Perl exec agent (attribution, protocol, deviations, test
harness, ceilings).

## S-2 (OWNER DECISION, supersedes the brief's "do not change")

Copy-finish notifications now HONOR the app's "Desktop notifications"
Settings toggle — fire only when it is ON, consistent with incoming-file
notifications (useIncomingFiles.ts gate).

Mechanics (the toggle lives in the webview's localStorage; the notifications
fire from the Rust pump, which must survive a suspended webview):

- `BrowserConfig.notify: bool` (persisted browser.json, serde default false —
  a missing/old/corrupt config NEVER enables notifications).
- `ConfigStore::notify_enabled()` / `set_notify(bool)` (browser/mod.rs).
- `notify_finish` (browser/commands.rs) returns early when the flag is off.
  Done / failed-not-cancelled semantics unchanged; cancels never notify.
- New IPC command `browser_set_finish_notifications(enabled)` (registered in
  lib.rs) stores the flag.
- Frontend: ONE effect in `useTailscale.ts` (the settings owner, which calls
  useSettings) mirrors `settings.notifications` into the browser config —
  after hydration (TD-03: persisted value first, never pre-hydration
  defaults) and on every change. `browserSetFinishNotifications` in
  lib/browser/api.ts.
- useIncomingFiles and the Settings UI are untouched (per owner instruction).

Tests: Rust `notify_flag_defaults_off_and_persists` (default-off, corrupt/old
config off, set-on fires → persistence across ConfigStore instances, set-off
suppresses, no clobber of per-host accounts); tripwire extension in
`run_job_notifies_on_settle_tripwire` (gate inside notify_finish + setter
writes the flag the gate reads); vitest
`src/hooks/__tests__/useTailscale.notify-sync.test.tsx` (startup push carries
the PERSISTED value; default-false startup; re-push on every change).

## Codie M2a carry-forwards

- **W-1 (done):** Codie's throwaway probe promoted permanently into fake.rs —
  `remote_to_remote_same_host_one_transport`: `/a` → `/b` on ONE FakeServer,
  both sides ONE transport (the commands.rs `dst_same_as_src` shape), file of
  3×CHUNK+9 bytes + nested folder; asserts byte fidelity, mtime travel,
  source untouched, stage cleanup on both sides. All platforms.
- **S-3 (done):** pre-enqueue mirror of the same-host-two-account refusal in
  TwoPaneView `copyFrom` — instant status-line feedback; engine Account
  refusal + queue row stay authoritative. Vitest pins status text +
  `browser_copy` never called (verified RED-first without the mirror).
- **S-1 (done):** third-spelling ancestor-alias residual documented next to
  gap 12 in m1-handoff.md. No guard change.
