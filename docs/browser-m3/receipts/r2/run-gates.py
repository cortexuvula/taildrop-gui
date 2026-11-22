#!/usr/bin/env python3
"""R2 local verification only. Never compiles/runs C# or contacts fleet hosts."""
import concurrent.futures
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
ENV = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'src-tauri/target'), CARGO_INCREMENTAL='0', CI='1')


def git(*args):
    return subprocess.check_output(['git', *args], cwd=ROOT).decode().strip()


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def run(item):
    name, command, scope = item
    started = now()
    start = time.monotonic()
    log = OUT / (name + '.log')
    with log.open('wb') as f:
        result = subprocess.run(command, cwd=ROOT, env=ENV, stdout=f, stderr=subprocess.STDOUT, check=False)
        f.write(b'\n-- End raw output --\n')
    row = dict(command=command, source=str(ROOT), tested_sha=HEAD, exit_status=result.returncode,
               scope=scope, started=started, finished=now(), seconds=round(time.monotonic() - start, 3),
               freshness='source committed at tested_sha; runner/receipts only added while running',
               log=str(log.relative_to(ROOT)), sha256=hashlib.sha256(log.read_bytes()).hexdigest())
    output_lines = log.read_text(errors='replace').splitlines()
    row['summary'] = '\n'.join(output_lines[-12:])
    if result.returncode:
        row['origin'] = 'unknown'
    print(json.dumps(row), flush=True)
    return row


def sequence(items):
    return [run(item) for item in items]


if __name__ == '__main__':
    HEAD = git('rev-parse', 'HEAD')
    free = shutil.disk_usage('/').free
    if free < 60_000_000_000:
        raise SystemExit('STOP: startup disk has less than 60 GB free')
    rust = [
        ('cargo-lib', ['cargo', 'test', '--manifest-path', 'src-tauri/Cargo.toml', '--lib'], 'all host Rust lib tests; no C# execution'),
        ('fmt', ['cargo', 'fmt', '--manifest-path', 'src-tauri/Cargo.toml', '--all', '--', '--check'], 'Rust formatting'),
        ('clippy-host', ['cargo', 'clippy', '--manifest-path', 'src-tauri/Cargo.toml', '--all-targets', '--', '-D', 'warnings'], 'host Rust code and tests'),
        ('check-windows', ['cargo', 'check', '--manifest-path', 'src-tauri/Cargo.toml', '--target', 'x86_64-pc-windows-gnu', '--tests'], 'Windows Rust/tests compilation only; no C# compilation or execution'),
        ('clippy-windows', ['cargo', 'clippy', '--manifest-path', 'src-tauri/Cargo.toml', '--target', 'x86_64-pc-windows-gnu', '--all-targets', '--', '-D', 'warnings'], 'Windows Rust/tests lint and compilation only'),
    ]
    frontend = [
        ('tsc', ['./node_modules/.bin/tsc', '--noEmit'], 'TypeScript strict typecheck'),
        ('vitest', ['npm', 'test'], 'all frontend unit tests'),
        ('playwright', ['./node_modules/.bin/playwright', 'test', '--workers=4', '--retries=0'], 'production React app with IPC fakes, fresh Vite, no fleet or native WebView'),
        ('frontend-build', ['npm', 'run', 'build'], 'production frontend build'),
        ('diff-hygiene', ['git', 'diff', '--check', 'd111578', 'HEAD'], 'full committed M3 delta whitespace'),
    ]
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        jobs = [pool.submit(sequence, rust), pool.submit(sequence, frontend)]
        rows = [row for job in jobs for row in job.result()]
    result = dict(schema='observed_check_results/v1', tested_sha=HEAD, finished=now(), free_bytes_at_start=free,
                  env={key: ENV[key] for key in ('CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CI')},
                  generated_artifact_provenance='map_not_declared', checks=rows,
                  missing=['Codie R2 approval', 'exact-head windows-latest CI and all nine C# tests', 'Frikkie room-6 smoke', 'Frikkie accuro-server smoke'],
                  release_verdict='HOLD', local_checks_passed=all(r['exit_status'] == 0 for r in rows))
    (OUT / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    sys.exit(0 if result['local_checks_passed'] else 1)
