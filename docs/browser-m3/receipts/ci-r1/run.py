#!/usr/bin/env python3
"""Fresh CI-repair receipts; refuses to overwrite any existing run."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
name, *argv = sys.argv[1:]
assert not (OUT / f"{name}.json").exists(), "receipt name is already frozen"
isolated = bool(argv and argv[0] == "--isolated-home")
if isolated:
    argv.pop(0)
env = os.environ.copy()
env.update(CARGO_TARGET_DIR=str(ROOT / "src-tauri/target"), CARGO_INCREMENTAL="0", CI="1", CARGO_TERM_COLOR="never")
if isolated:
    home = OUT / "empty-home"
    home.mkdir(exist_ok=True)
    assert not list(home.iterdir()), "isolated home must remain empty"
    env.update(HOME=str(home), USERPROFILE=str(home), SSH_AUTH_SOCK=str(home / "no-agent.sock"))
assert shutil.disk_usage(ROOT).free >= 60_000_000_000, "disk below 60 GB"
start = datetime.datetime.now(datetime.timezone.utc).isoformat()
with (OUT / f"{name}.log").open("xb") as log:
    result = subprocess.run(argv, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
record = {
    "command": shlex.join(argv), "cwd": str(ROOT), "isolated_home_and_agent": isolated,
    "CARGO_TARGET_DIR": env["CARGO_TARGET_DIR"], "CARGO_INCREMENTAL": "0", "CI": "1",
    "started": start, "ended": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "exit": result.returncode, "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
    "tracked_diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"], cwd=ROOT)).hexdigest(),
    "log": f"{name}.log", "sha256": hashlib.sha256((OUT / f"{name}.log").read_bytes()).hexdigest(),
}
with (OUT / f"{name}.json").open("x") as f:
    json.dump(record, f, indent=2)
    f.write("\n")
print(json.dumps(record, indent=2))
print((OUT / f"{name}.log").read_text(errors="replace")[-6500:])
sys.exit(result.returncode)
