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

## Windows C# helper (M3)

Upstream: the exact UTF-8 runtime body of `WIN_AGENT_CS` at
omarchy-tailnet-files commit `c95f1b7bf446304b6c8826b149f6d94ca719ac69`,
`bin/tailfiles:1495–2216`, including `Pipe` (overlapped stdio).
Copyright © 2026 Drew Majerus; MIT license below applies in full.

Two payloads now exist:

| File | Role | Bytes | SHA-256 |
|---|---|---|---|
| `agent.upstream-c95f1b7.cs` | Immutable upstream reference; never uploaded, compiled only inside the native Windows test fixture as the RED witness | 35158 | `cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133` |
| `agent.cs` | ACTIVE payload (uploaded, installed, executed) — reviewed successor | 35927 | `adefa12c41424c983b78cf1ba5956ba23d1dd0e1a03be11784167cdf9e563f10` |

The active executable name uses the successor hash's first 16 hex
characters: `agent-adefa12c41424c98.exe`. The Rust vendoring tests pin both
files (upstream immutability + active hash/divergence), and `win_agent.rs`
pins `SOURCE_HASH` to the active payload's first 16 hex characters. A host
that already installed the upstream-named `agent-cfa1c1af6b161718.exe` keeps
it; old helper versions are not garbage-collected.

### Reviewed successor delta (owner ruling 2026-11-22)

Owner choice: "Preserve busy-file success — authorize a narrow vendor change,
Codie review and CI". Measured control behind it: hosted diagnostic run
45084283852 (NTFS, PowerShell 5.1) — legacy class-10 replacement succeeded on
a closed destination but returned `0xC0000022` with the destination held open
under READ|WRITE|DELETE sharing. The successor is the smallest
handle-relative change that requests the documented newer semantic:

- `RenameTo` only, two code lines: a replacing rename
  (`replace == true`) now calls `NtSetInformationFile` with information
  class 65 (`FileRenameInformationEx`) and a DWORD `Flags` value of
  `0x3` = `FILE_RENAME_FLAG_REPLACE_IF_EXISTS (0x1) |
  FILE_RENAME_FLAG_POSIX_SEMANTICS (0x2)` (MS-FSCC FileRenameInformationEx)
  written at offset 0 of the same native layout (union Flags →
  RootDirectory at `IntPtr.Size` → counted `FileNameLength` in bytes →
  UTF-16 name); `AllocHGlobal`/`FreeHGlobal` cleanup unchanged. A
  non-replacing rename keeps class 10 with flags 0, byte-equivalent to
  upstream, preserving its collision refusal.
- Everything else — `Pipe`, TOCTOU keep, RealName casing (the destination's
  real spelling is passed explicitly, so exact-name POSIX renames land on
  it), consent timing, per-item temp ownership, mtime/bytes, directory
  merge, link safety, cancellation/EOF cleanup, rename/exists/delete verbs
  — is upstream behavior preserved verbatim.

Behavior and support limits (documented, not all yet runtime-proven):

- Busy-file success: with POSIX semantics the replace succeeds while other
  handles hold the destination open IF they share deletion (the normal
  reader share READ|WRITE|DELETE); existing handles keep reading the old
  bytes, new opens see the replacement (MS-FSCC POSIX_SEMANTICS wording).
- Fail-closed: a holder that denies delete sharing, an OS/filesystem
  without class 65 (`STATUS_INVALID_INFO_CLASS`, pre-Win10-RS1-era
  systems), or an unsupported filesystem still fails the rename; the
  helper then reports its existing conflict reply (`C` record → typed
  Exists at the Rust layer), removes only its own staged temp, and never
  deletes or replaces the old target. No delete-before-rename, no retry
  loop, no closed-reader trick, no SFTP fallback.
- Minimum supported environment for busy-file replace: Windows/FileSystem
  accepting `FileRenameInformationEx` with POSIX semantics (NTFS on
  Windows 10 RS1+/Windows Server 2016+ era; the hosted diagnostic
  environment NT 10.0.26100 qualifies). On such systems the no-replace
  lane is unaffected. This OS floor is a documented qualification, not a
  runtime probe implemented in the product.
- Unsupported-filesystem runtime proof is pending Windows CI; it cannot be
  produced on macOS.

`browser/win_agent.rs` ports `win_ps` / `win_agent` (lines 2222–2287):
UTF-16LE EncodedCommand works under cmd and PowerShell login shells;
PowerShell 5.1 Add-Type compiles a PID-specific temporary exe and Move-Item
publishes it. Unlike upstream, query/compile/probe entire I/O is bounded and
stdout is capped while reading; old helper versions are not garbage-collected.
Installer sources use exclusive `agent-<hash>.<random-16hex>.cs` names so a
failed/interrupted upload cannot poison the next attempt. The final executable
keeps the plugin's exact `agent-<hash>.exe` basename. The compile script removes
its own source/PID-temp in `finally` and accepts a Move-Item race only when a
final executable exists (which must still pass the known-answer proof).
Existing and newly installed binaries must pass `exists` with the actual
TailFiles directory, count `1`, and their own basename: exactly `1`, exit 0.
Installer executable paths additionally reject `&;!()` to narrow shell parsing.
All other paths travel only as NUL-delimited stdin data.

Receive supports D/F/Z only: symlinks are skipped and counted, never sent as L.
It places completed sibling files incrementally: cancellation/EOF removes
unfinished items through held handles, **not previously committed siblings**.
Dot-prefixed temporary names are not Windows Hidden attributes. The explicit
destination's Top follows that path; descendant opens are no-follow.
Receive ignores POSIX modes (inherits destination ACLs); timestamps are best
effort (SetFileTime's return is not checked upstream). No mode-fidelity claim
is made. Client validation checks every path component, reserved names, and
case-variant sibling paths even during Replace/merge; RealName retains the
existing destination spelling. Mkdir is a single D/Z receive, since upstream
has no mkdir verb. Rename and delete use the native helper verbs; destructive
calls retain fresh-home/parent guards plus C#'s all-target handle guards.

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
