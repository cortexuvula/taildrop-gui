#!/usr/bin/env python3
"""One-time seal of NEW ci-r1 raw logs, preserving all original bytes."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile

OUT = Path(__file__).resolve().parent
ROOT = OUT.parents[3]
assert not (OUT / "results.json").exists(), "ci-r1 is already sealed"
records = []
for path in sorted(OUT.glob("*.json")):
    data = json.loads(path.read_text())
    if "command" in data:
        log = OUT / data["log"]
        assert hashlib.sha256(log.read_bytes()).hexdigest() == data["sha256"], path
        assert data["exit"] == (101 if path.name == "ci-a-red.json" else 0), path
        records.append(data)
for log, expected in [("cargo-full.log", 318), ("cargo-lib-isolated.log", 318), ("targeted-win-agent.log", 25)]:
    assert re.search(rf"test result: ok\. {expected} passed; 0 failed; 0 ignored", (OUT / log).read_text()), log
assert re.search(r"Tests\s+211 passed", (OUT / "vitest.log").read_text())
assert "66 passed" in (OUT / "playwright.log").read_text()
logs = sorted(OUT.glob("*.log"))
raw = {p.name: p.read_bytes() for p in logs}
buf = io.BytesIO()
with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tf:
    for name, data in raw.items():
        info = tarfile.TarInfo(name)
        info.size = len(data)
        info.mode = 0o644
        info.mtime = 0
        tf.addfile(info, io.BytesIO(data))
archive = OUT / "raw-logs.tar.gz"
with archive.open("xb") as f:
    f.write(gzip.compress(buf.getvalue(), mtime=0))
with tarfile.open(archive, "r:gz") as tf:
    assert set(tf.getnames()) == set(raw)
    for name, data in raw.items():
        member = tf.extractfile(name)
        assert member is not None, name
        assert member.read() == data, name
manifest = {
    "status": "partial fixture fixes; CI-C unresolved scope blocker; NO Windows GREEN or M3 acceptance",
    "source_final": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
    "base_and_hosted_red_sha": "baa019b26ee9a11d259cfc511302e9a1ffd88423",
    "hosted_red_run": "https://github.com/cortexuvula/taildrop-gui/actions/runs/45031529401",
    "records": records,
    "archive": archive.name,
    "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
    "raw_logs": {name: {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()} for name, data in raw.items()},
    "native_windows_execution_here": False,
    "note": "Logical .log paths in command records are byte-exact archive members, not loose files. Tar bytes verified before removing only new loose log copies. No executable/target-content digests taken. Initial compiler-artifact selector was corrected from kind=lib to profile.test after setup AssertionError; compile and RED logs are real separate runs.",
}
with (OUT / "results.json").open("x") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
for p in logs:
    p.unlink()
print(json.dumps({"records": len(records), "raw_logs": len(raw), "archive_bytes": archive.stat().st_size, "sha256": manifest["archive_sha256"], "source_final": manifest["source_final"]}, indent=2))
