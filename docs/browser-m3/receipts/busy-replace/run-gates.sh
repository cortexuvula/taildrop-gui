#!/bin/bash
# M3 busy-file replacement — local gate runner (macOS host).
# Re-runs every available local gate against the exact working tree and
# records command, exit code and full output under logs/.
# Native Windows C#/PowerShell runtime is NOT runnable here; windows-gnu
# check/clippy are compile gates only. RED/GREEN native proof is pending CI.
set -u
cd "$(dirname "$0")/../../../../" || exit 99
R=docs/browser-m3/receipts/busy-replace/logs
mkdir -p "$R"
: > "$R/exit-codes.txt"

gate() { # name cwd cmd...
  local name=$1 cwd=$2; shift 2
  {
    echo "### $name"
    echo "cwd: $cwd"
    printf 'command:'; printf ' %q' "$@"; echo
    echo "started: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$R/$name.log"
  ( cd "$cwd" && "$@" ) >> "$R/$name.log" 2>&1
  local rc=$?
  echo "$name exit=$rc" | tee -a "$R/exit-codes.txt"
}

gate fmt          src-tauri env CARGO_INCREMENTAL=0 cargo fmt --check
gate check-host   src-tauri env CARGO_INCREMENTAL=0 cargo check --lib --tests
gate clippy-host  src-tauri env CARGO_INCREMENTAL=0 cargo clippy --all-targets -- --deny warnings
gate test-lib     src-tauri env CARGO_INCREMENTAL=0 cargo test --lib
gate check-wingnu src-tauri env CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu --lib --tests
gate clippy-wingnu src-tauri env CARGO_INCREMENTAL=0 cargo clippy --target x86_64-pc-windows-gnu --lib --tests -- --deny warnings
gate tsc          . npx tsc --noEmit
gate fe-build     . npm run build
gate vitest       . npx vitest run
gate playwright   . npx playwright test

# Pure pin RED/GREEN witness (source-bound, runs on the Mac):
# upstream bytes restored into agent.cs must FAIL the active-successor pin;
# successor bytes restored must PASS it again.
name=pin-red-green
{
  echo "### $name (RED: upstream bytes as agent.cs -> active pin must fail)"
  date -u +%Y-%m-%dT%H:%M:%SZ
} > "$R/$name.log"
cp src-tauri/agents/agent.cs "$R/agent-successor.cs"
cp src-tauri/agents/agent.upstream-c95f1b7.cs src-tauri/agents/agent.cs
( cd src-tauri && CARGO_INCREMENTAL=0 cargo test --lib vendored_csharp ) >> "$R/$name.log" 2>&1
red_rc=$?
cp "$R/agent-successor.cs" src-tauri/agents/agent.cs
shasum -a 256 src-tauri/agents/agent.cs >> "$R/$name.log" 2>&1
( cd src-tauri && CARGO_INCREMENTAL=0 cargo test --lib vendored_csharp ) >> "$R/$name.log" 2>&1
green_rc=$?
rm "$R/agent-successor.cs"
echo "pin-red exit=$red_rc (expected nonzero)" | tee -a "$R/exit-codes.txt"
echo "pin-green exit=$green_rc (expected 0)" | tee -a "$R/exit-codes.txt"

echo '--- payload digests ---' | tee -a "$R/exit-codes.txt"
shasum -a 256 src-tauri/agents/agent.cs src-tauri/agents/agent.upstream-c95f1b7.cs | tee -a "$R/exit-codes.txt"
wc -c src-tauri/agents/agent.cs src-tauri/agents/agent.upstream-c95f1b7.cs | tee -a "$R/exit-codes.txt"
echo "git HEAD: $(git rev-parse HEAD)" | tee -a "$R/exit-codes.txt"
git diff --stat HEAD -- src-tauri | tee -a "$R/exit-codes.txt"
