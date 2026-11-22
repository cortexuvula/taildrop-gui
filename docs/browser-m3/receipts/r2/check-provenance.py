#!/usr/bin/env python3
"""R2 provenance and narrow added-line hygiene checks, not a security audit."""
import ast
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
UPSTREAM = Path('/Users/cortexuvula/.hermes/projects/taildrop-browser/omarchy-tailnet-files')
EXPECTED_COMMIT = 'c95f1b7bf446304b6c8826b149f6d94ca719ac69'
EXPECTED_HASH = 'cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133'


def git(*args, cwd=ROOT):
    return subprocess.check_output(['git', *args], cwd=cwd)


head = git('rev-parse', 'HEAD').decode().strip()
assert git('rev-parse', 'HEAD', cwd=UPSTREAM).decode().strip() == EXPECTED_COMMIT
source = ast.parse((UPSTREAM / 'bin/tailfiles').read_text())
values = [ast.literal_eval(n.value).encode() for n in ast.walk(source)
          if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'WIN_AGENT_CS' for t in n.targets)]
assert len(values) == 1
vendor = (ROOT / 'src-tauri/agents/agent.cs').read_bytes()
assert vendor == values[0]
assert hashlib.sha256(vendor).hexdigest() == EXPECTED_HASH
assert b'\r' not in vendor
protected = ['src-tauri/src/tailscale.rs', 'src-tauri/src/browser/agent.rs',
             'src-tauri/agents/agent.pl', 'src-tauri/agents/posix_delete.sh']
for path in protected:
    assert (ROOT / path).read_bytes() == git('show', 'd111578:' + path), path
assert vendor == git('show', '8d555e9:src-tauri/agents/agent.cs')
patterns = {
    'unlinked_marker': r'\b(?:TODO|FIXME|stub)\b',
    'private_key_literal': r'BEGIN (?:RSA |OPENSSH |EC )?PRIVATE KEY',
    'token_literal': r'gh[pousr]_[A-Za-z0-9]{20,}',
    'shell_true': r'shell\s*=\s*True',
    'unsafe_deserialization': r'(?:pickle\.loads|yaml\.load)\s*\(',
}
controls = {'unlinked_marker': 'TODO', 'private_key_literal': 'BEGIN PRIVATE KEY',
            'token_literal': 'ghp_' + 'x' * 30, 'shell_true': 'shell=True',
            'unsafe_deserialization': 'pickle.loads(data)'}
for key, pattern in patterns.items():
    assert re.search(pattern, controls[key]), key
changes = git('diff', '--name-only', '8d555e9', 'HEAD', '--', 'src', 'src-tauri/src').decode().splitlines()
hits = []
for path in changes:
    diff = git('diff', '--unified=0', '8d555e9', 'HEAD', '--', path).decode()
    for line in diff.splitlines():
        if line.startswith('+') and not line.startswith('+++'):
            for name, pattern in patterns.items():
                if re.search(pattern, line[1:]):
                    hits.append({'path': path, 'pattern': name})
assert not hits, hits
native = (ROOT / 'src-tauri/src/browser/win_agent/tests/windows.rs').read_text()
native_tests = re.findall(r'#\[tokio::test\]\s*async fn (\w+)', native)
assert len(native_tests) == 9
report = dict(tested_sha=head, vendor_bytes=len(vendor), vendor_sha256=EXPECTED_HASH,
              upstream_commit=EXPECTED_COMMIT, vendor_equal_to_upstream=True,
              vendor_unchanged_since_r1=True, protected_unchanged_since_base=protected,
              source_files_scanned=changes, static_hits=hits, positive_controls=list(controls),
              native_tests=native_tests, native_tests_executed=False,
              limitation='Narrow added-line hygiene and static provenance, not an external security audit or native Windows execution')
(OUT / 'provenance.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
