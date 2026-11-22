# M3 operator-owned smoke before v0.18.0

**Prepared instructions, NOT execution evidence. Ada did not connect to either peer.**
Frikkie owns this gate on **both `room-6` and `accuro-server`**, after Codie review
and a green exact-candidate `windows-latest` job. Do not change sshd, firewall,
DefaultShell registry settings, services, permissions, or clinical directories.
Use the intended account (`corte` in the prerequisite checklist). If that account
or the approved scratch location differs, stop and resolve it first.

## 1. Create only owned fixtures (Windows PowerShell 5.1)

Run locally on the selected Windows peer, as the account the app will use.
This creates a unique tree only beneath that account's LocalApplicationData.
Keep `$root` for subsequent assertions; do not reuse an existing directory.

```powershell
$ErrorActionPreference = 'Stop'
$root = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('TailDrop-M3-smoke-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$source = Join-Path $root 'source'
$dest = Join-Path $root 'destination'
New-Item -ItemType Directory -Path $source,$dest | Out-Null
New-Item -ItemType Directory -Path (Join-Path $source 'tree\empty') | Out-Null
[IO.File]::WriteAllBytes((Join-Path $source 'tree\payload.bin'), [Text.Encoding]::UTF8.GetBytes("M3 scratch payload`0with binary NUL"))
[IO.File]::SetLastWriteTimeUtc((Join-Path $source 'tree\payload.bin'), [DateTime]::Parse('2023-11-14T22:13:21Z').ToUniversalTime())
[IO.Directory]::SetLastWriteTimeUtc((Join-Path $source 'tree'), [DateTime]::Parse('2023-11-14T22:13:20Z').ToUniversalTime())
[IO.File]::WriteAllText((Join-Path $dest 'Report.TXT'), 'old scratch report')
$ro = Join-Path $dest 'readonly.txt'
[IO.File]::WriteAllText($ro, 'scratch readonly deletion')
(Get-Item -LiteralPath $ro).IsReadOnly = $true
$sentinel = Join-Path $root 'sentinel-outside-delete-selection'
New-Item -ItemType Directory -Path $sentinel | Out-Null
[IO.File]::WriteAllText((Join-Path $sentinel 'KEEP.txt'), 'must survive')
$junction = Join-Path $dest 'junction-to-sentinel'
cmd /c "mklink /J `"$junction`" `"$sentinel`""
if ($LASTEXITCODE -ne 0) { throw 'Scratch junction creation failed' }
$root
Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $source 'tree\payload.bin')
```

Record the printed root, account, candidate SHA and Windows CI job URL. In the
app use its `/C:/...` spelling; native `C:\...` is also accepted by the helper.
Do not select the whole `$root` for deletion while any assertion remains.

## 2. First install, cached install, and copy lanes

1. Open Files, choose this peer, and verify the loaded account is the intended
   one. Browse `source/tree`; pull it into a **new empty scratch folder on the
   Mac**, not Downloads or a clinical folder. Compare the pulled payload's
   SHA-256 to the setup output and check the empty directory and UTC mtimes.
2. Wait for this pane's probe. `winAgent` must become literal `true` only for
   the loaded account. A failed probe must leave browse/pull usable and all
   destination writes disabled/typed Unsupported. Check app debug output and
   the installed path below; do not infer proof from the filename alone.
3. Copy the pulled Mac `tree` into Windows `destination`. Check SHA-256, empty
   directory and mtimes. Repeat via remote-to-remote if a separate approved
   **scratch-only source peer** is available; otherwise record that fleet lane
   as not run (the fake-source Rust test covers its engine routing).
4. Refresh/reopen the Windows pane and repeat a harmless exists/copy check to
   exercise the existing-executable/cache path. Expected executable:
   `%LOCALAPPDATA%\TailFiles\agent-cfa1c1af6b161718.exe`.

Read-only executable inventory (not capability proof):

```powershell
$exe = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'TailFiles\agent-cfa1c1af6b161718.exe'
Get-Item -LiteralPath $exe | Select-Object FullName,Length,LastWriteTimeUtc
Get-FileHash -Algorithm SHA256 -LiteralPath $exe
```

Executable digests vary with the compiler; record the observed digest, do not
compare it to the C# source digest. POSIX modes are **not** preserved by this
unmodified C# receiver; Windows inherits destination ACLs. Mtime is upstream
best effort, not a guarantee on every filesystem.

## 3. Consent, spelling, names and cancellation

- Copy `tree` again without Replace: expect conflict and unchanged destination.
  Explicit Replace may merge the real folder; no hidden SFTP write fallback.
- In the Mac scratch source create `report.txt` containing `new scratch report`.
  Copy it into Windows `destination`: first Keep must refuse; explicit Replace
  must succeed and the on-disk spelling must remain **`Report.TXT`**.
- On the case-sensitive Mac source create a new scratch folder containing
  sibling `A` and `a` files. Copy it to Windows (also try Replace/merge): expect
  a case-clash refusal, never silent overwriting by the second record. On a
  case-insensitive source volume, mark this manual case not run; CI covers it.
- Create a nested Mac scratch `CON.txt` and try copying it: expect client-side
  refusal before that record is sent. No `CON.txt` may be created on Windows.
  Symlinks must be skipped/count-reported, never followed or sent as L.
- Cancel a scratch transfer while it is actually active. The unfinished temp
  must disappear after EOF cleanup; previously committed siblings and unrelated
  dot-prefixed files must remain. If it finished before Cancel, record
  **inconclusive**, not pass. Do not use large fixtures on `accuro-server` just
  to create a timing window: keep that peer's fixtures at or below the prior
  approved 2 MiB probe size. Use the Windows CI cancel barrier for determinism.

Assertions after report Replace and tree copy:

```powershell
$names = @(Get-ChildItem -LiteralPath $dest | ForEach-Object Name)
if ($names -cnotcontains 'Report.TXT' -or $names -ccontains 'report.txt') { throw 'RealName spelling changed' }
if ([IO.File]::ReadAllText((Join-Path $dest 'Report.TXT')) -ne 'new scratch report') { throw 'Replace payload mismatch' }
$original = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $source 'tree\payload.bin')).Hash
$received = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dest 'tree\payload.bin')).Hash
if ($original -ne $received) { throw 'Tree payload mismatch' }
if (!(Test-Path -LiteralPath (Join-Path $dest 'tree\empty') -PathType Container)) { throw 'Empty folder missing' }
Get-Item -LiteralPath (Join-Path $dest 'tree'),(Join-Path $dest 'tree\payload.bin') | Select-Object FullName,LastWriteTimeUtc
```

## 4. Native mutations and scope safety

1. In app `destination`, New folder `made-by-app`, then Rename it to
   `renamed-by-app`. Rename to another existing scratch item's name must refuse.
2. Delete **only `readonly.txt` and `junction-to-sentinel`** using the app.
   Confirm the permanent-delete dialog still names the pinned peer/account/path.
3. Assert the external-to-selection sentinel survived:

```powershell
if (Test-Path -LiteralPath $ro) { throw 'Readonly file was not deleted' }
if (Test-Path -LiteralPath $junction) { throw 'Junction was not deleted' }
if ([IO.File]::ReadAllText((Join-Path $sentinel 'KEEP.txt')) -ne 'must survive') { throw 'Junction target was traversed' }
```

4. Open a scratch mutation dialog, then navigate/change pane or account before
   confirming. Expect scope-lock refusal, not a write to the new context. Never
   borrow another account's proof. Do not change a host-key pin unless you can
   independently verify the fingerprint.
5. **Do not test home/root/ancestor deletion against a real user profile.** The
   fail-closed all-target guard is exercised against owned fixtures in Windows
   CI; selecting clinical home data to prove a refusal is not a safe smoke test.
6. Missing-helper invalidation/reinstall is covered by Rust fault tests. A live
   executable rename or exec-policy change is a separate operator-maintenance
   action, not part of this default fleet smoke. Do not alter a shared helper,
   service or registry merely to simulate failure.

## 5. Closeout

Record every check as pass/fail/not-run/inconclusive, separately for each peer.
Close app jobs and remove only the exact unique `$root` after confirming it is
owned scratch. If an earlier junction assertion failed, stop and inspect before
recursive cleanup. Remove the Mac scratch tree separately. Do not remove the
account's TailFiles installation or any other application's files.

A green local Mac test suite is not Windows execution evidence. Require the
exact-head Windows job plus both per-peer records before release approval.
