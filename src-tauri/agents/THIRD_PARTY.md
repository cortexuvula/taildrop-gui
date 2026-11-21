# Third-party code vendored into taildrop-gui

## omarchy-tailnet-files — Perl remote agent + POSIX delete script

- **Upstream:** https://github.com/dmajerus6/omarchy-tailnet-files
- **Clone of record:** `/Users/cortexuvula/.hermes/projects/taildrop-browser/omarchy-tailnet-files`
  at commit `c95f1b7bf446304b6c8826b149f6d94ca719ac69` ("Remote cleanup never
  reopens by name; Windows copy and rename via a C# helper")
- **License:** MIT — full text below. © 2026 Drew Majerus.

### What is vendored

| File here | Source in `bin/tailfiles` | Fidelity |
|---|---|---|
| `agent.pl` | `AGENT_PL` raw string, lines 1018–1263 | Body byte-identical below the attribution header (the ONLY deviation; documented in the header). sha256 of the unmodified body: `10f732185203fca259a75b06fcaadf00f6a00ecea786cbc45a03f1840f1e1bbd` (9820 bytes) |
| `posix_delete.sh` | runtime value of `POSIX_DELETE_SCRIPT`, lines ~2558–2573 (`POSIX_DELETE_CMD = "xargs -0 sh -c '%s' sh"`) | Byte-identical (633 bytes, single line, no single quotes); sha256 `b00c22f6f55502cac13983a040bfb6bd82b5851e63cf500b397ab9b5f50798ff`. A trailing newline is appended for file hygiene and trimmed at Rust embed time. |

The fixed bootstrap command (`AGENT_CMD`, bin/tailfiles:1264–1265) is
reproduced byte-identically as a Rust constant in
`src-tauri/src/browser/agent.rs`:

```
perl -e 'my $n = <STDIN>; defined $n or exit 2; read(STDIN, my $c, $n) == $n or exit 2; eval $c; die $@ if $@; exit 0'
```

### How taildrop-gui uses it (M2b)

- `recv` verb only: atomic no-replace placement (renameat2 RENAME_NOREPLACE /
  macOS renameatx_np RENAME_EXCL), handle-bound stage identity checks
  (owner/0700/empty), and creator-only stage cleanup on every exit path
  (finished/failed/stdin EOF/SIGPIPE/HUP/TERM). `send` and `rename` are
  vendored unmodified for protocol completeness but are NOT invoked by this
  app (source walks stay on SFTP; rename stays on its M1 tier).
- `posix_delete.sh` via the fixed `xargs -0 sh -c '…' sh` command with paths
  traveling NUL-separated on stdin: physical `cd -P` re-check of every
  delete target (and the home guard against the PHYSICALLY resolved
  spelling) immediately before `rm -rf` — closing the
  "path changed between confirm and delete" class on POSIX remotes that can
  exec.
- Paths are data, never shell text: both commands are fixed strings; all
  user-controlled values travel on stdin (plugin invariant §4.9).
- Hosts without exec/perl keep the M1/M2a SFTP tiers UNCHANGED (hardlink
  no-replace where advertised, else consented check-then-rename with the
  documented TOCTOU ceiling) — no silent downgrade.

Docs line (browser README/handoff): see `docs/browser-m2/m2b-handoff.md` and
`m1-handoff.md`.

## MIT License (omarchy-tailnet-files)

```
MIT License

Copyright (c) 2026 Drew Majerus

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
