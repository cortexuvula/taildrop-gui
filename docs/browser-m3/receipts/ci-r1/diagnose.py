#!/usr/bin/env python3
"""Read-only preservation/provenance and STATIC diagnostic, not Windows execution."""
import ast
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
BASE = "baa019b26ee9a11d259cfc511302e9a1ffd88423"

def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)

def digest(data):
    return hashlib.sha256(data).hexdigest()

preserved = json.loads((OUT / "preserved-before.json").read_text())
for name, sha in preserved.items():
    assert digest((ROOT / name).read_bytes()) == sha, name

vendor = (ROOT / "src-tauri/agents/agent.cs").read_bytes()
upstream = Path("/Users/cortexuvula/.hermes/projects/taildrop-browser/omarchy-tailnet-files")
pin = "c95f1b7bf446304b6c8826b149f6d94ca719ac69"
upstream_source = subprocess.check_output(["git", "show", f"{pin}:bin/tailfiles"], cwd=upstream)
assigns = [n for n in ast.walk(ast.parse(upstream_source)) if isinstance(n, ast.Assign)
           and any(isinstance(t, ast.Name) and t.id == "WIN_AGENT_CS" for t in n.targets)]
assert len(assigns) == 1
assert ast.literal_eval(assigns[0].value).encode() == vendor
protected = ["src-tauri/src/tailscale.rs", "src-tauri/src/browser/agent.rs",
             "src-tauri/agents/agent.pl", "src-tauri/agents/posix_delete.sh"]
for name in protected:
    assert (ROOT / name).read_bytes() == git("show", f"d111578:{name}"), name

native_name = "src-tauri/src/browser/win_agent/tests/windows.rs"
current = (ROOT / native_name).read_text()
previous = git("show", f"{BASE}:{native_name}").decode()
marker = "async fn csharp_toctou_keep_and_atomic_replace_preserve_realname()"
def original_reader_test(s):
    return s.split(marker, 1)[1].split("#[tokio::test]", 1)[0]
assert original_reader_test(current) == original_reader_test(previous)
assert git("diff", BASE, "HEAD", "--", "src-tauri/src/browser/win_agent.rs") == b""

# Pure model of Rust std's regular argument quoting (append_arg at b940084d7).
# This is a static encoding explanation; NOT an observed CreateProcess/cmd run.
def regular(arg):
    quote = not arg or any(c in arg for c in " \t")
    result = '"' if quote else ""
    slashes = 0
    for c in arg:
        if c == "\\":
            slashes += 1
        else:
            if c == '"':
                result += "\\" * (slashes + 1)
            slashes = 0
        result += c
    return result + ("\\" * slashes + '"' if quote else "")

link = r"C:\owned scratch\recursive\nested\junction"
target = r"C:\owned scratch\sentinel"
old = "cmd.exe /c " + regular(f'mklink /J "{link}" "{target}"')
new = "cmd.exe /d /c mklink /J " + regular(link) + " " + regular(target)
assert '\\"' in old and '\\"' not in new
assert '.args(["/d", "/c", "mklink", "/J"])' in current
assert 'tree.join("nested").join("junction")' in current
assert 'Fixture::new("m3-delete space")' in current

record = {
    "kind": "source/static diagnosis and preservation; NO Windows runtime",
    "base": BASE, "source_final": git("rev-parse", "HEAD").decode().strip(),
    "preserved_files_checked": len(preserved), "preserved_all_equal": True,
    "vendor": {"upstream_pin": pin, "bytes": len(vendor), "sha256": digest(vendor), "byte_exact": True},
    "protected_equal_to_d111578": protected,
    "ci_c_original_reader_test_byte_equal": True, "ci_c_production_sink_unchanged": True,
    "ci_b_static_cmd_encoding_model": {"old": old, "new": new,
        "source": "https://raw.githubusercontent.com/rust-lang/rust/b940084d7/library/std/src/sys/args/windows.rs",
        "native_runtime_observed": False},
    "targets_created": [], "targets_deleted": [],
    "existing_target_retained": str(ROOT / "src-tauri/target"),
    "existing_target_size_du_k": subprocess.check_output(["du", "-sk", str(ROOT / "src-tauri/target")], text=True).strip(),
    "free_bytes": shutil.disk_usage(ROOT).free,
    "publishing": "none", "fleet_contact": False,
}
with (OUT / "provenance.json").open("x") as f:
    json.dump(record, f, indent=2)
    f.write("\n")
print(json.dumps(record, indent=2))
