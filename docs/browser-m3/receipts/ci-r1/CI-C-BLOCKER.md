# CI-C: scope blocker — open-target replacement in the byte-exact vendor

Status: **NOT FIXED; Windows runtime remains RED.** No vendor, production Rust, or reader-test changes were made for CI-C. This is a source/API-contract diagnosis, not a locally reproduced Windows pass or an observed native NTSTATUS.

## Observed symptom

Hosted run [45031529401](https://github.com/cortexuvula/taildrop-gui/actions/runs/45031529401), candidate `baa019b26ee9a11d259cfc511302e9a1ffd88423`, Windows Rust tests: `csharp_toctou_keep_and_atomic_replace_preserve_realname` failed at `windows.rs:205` with conflicts `["report.txt"]`. The preceding no-replace race assertions completed. The raw hosted log is retained byte-exact in the raw-log archive in this directory.

## Data-flow trace (unchanged source)

1. `windows.rs:370–394` opens a **Keep** session, waits for its actual native temp, then plants `race`. It requires `C(race)` and preserves the winning bytes. This is a separate session from Replace; there is no shared native conflict list between processes.
2. `windows.rs:395–437` writes a normal 1 MiB `Report.TXT`, starts a bounded concurrent `std::fs::read` loop, and awaits a complete old-content observation before opening the replacement session. The reader continues opening/reading/closing the destination while Replace runs. This is an intentional test of concurrent-reader behavior, not merely an unrelated setup handle.
3. `windows.rs:438–440` calls `Fixture::sink(tag, true)`. `WinAgentSink::open/start` (`win_agent.rs:497–541`) immediately serializes `recv\0<dir>\0<tag>\0replace\0`. The bool reaches the C# session before F data; it is not withheld until `place()`.
4. C# `CmdRecv` (`agent.cs:511–514`) reads that bool once. For a top-level F, `temp = !P.New` is true (`:570–575`); the full file is received into an exclusive temp and timestamped (`:578–579`). **The rename happens during the F handling, before the client's Z/place** (`:580–585`). `RealName` opens the case-insensitive target with READ_ATTRIBUTES, retrieves its actual basename, and closes its own handle (`:469–483`). That method does not hold its own handle across the rename.
5. `RenameTo` (`agent.cs:281–294`) sets a one-byte ReplaceIfExists and calls `NtSetInformationFile(..., 10)`: **legacy FileRenameInformation**, not FileRenameInformationEx with POSIX semantics.
6. C# `:583` reports **every negative rename status during Replace as a conflict**, not just NAME_COLLISION: `(st == NAME_COLLISION || replace ? conflicts : errors).Add(p)`. It unlinks the failed temp and removes it from the owned-temp list. Therefore `C(report.txt)` is consistent with an open-target failure; it does NOT prove that Replace consent was missing or that another name appeared.
7. `WinAgentSink::place` (`win_agent.rs:643–675`) sends Z, EOF, then parses C/E/Z and waits for exit. Its `_replace` parameter cannot change a session's already-executed placement. Changing only that argument or retrying `place()` cannot repair this failure: the failed temp has already been discarded by the vendor.

## Native contract and limits of attribution

Primary references inspected on 2026-11-22:

- [Microsoft FILE_RENAME_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information): the open-files restrictions say a target with open handles prevents the legacy rename. The FileRenameInformationEx-only `FILE_RENAME_POSIX_SEMANTICS` flag, together with `FILE_RENAME_REPLACE_IF_EXISTS`, permits replacing a file with existing handles; old handles remain valid and later opens resolve to the new file.
- [Microsoft MS-FSCC FileRenameInformation](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/1d2673a8-8fb9-4868-920a-775ccaa30cf8): `STATUS_ACCESS_DENIED (0xC0000022)` includes an open target with nonzero ReplaceIfExists.
- [Rust std Windows OpenOptions, pinned b940084d7](https://raw.githubusercontent.com/rust-lang/rust/b940084d7/library/std/src/sys/fs/windows.rs): default sharing is `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`. Merely adding delete-sharing to the Rust reader is not a demonstrated fix; it is already present. That sharing flag is not the vendor's missing FileRenameInformationEx/POSIX flag.

This is a strong source/API explanation for the actual symptom, but **the exact failed NTSTATUS has NOT been measured**. C# suppresses it in the C reply, this host is macOS, and no Windows/fleet contact is authorized. Other negative native statuses also collapse into C. Do not relabel this as a fully isolated native reproduction or assert that every CI run must fail: short intervals between reader opens can let the legacy rename succeed nondeterministically.

## Why no authorized narrow repair was applied

The original-case assertions, no-replace race assertion, bounded task shutdown, concurrent-reader activity, and required complete-old AND complete-new observations remain byte-for-byte unchanged. Pausing/closing the reader specifically for the rename, serializing away concurrency, adding sleeps until it happens to pass, accepting C as success, or relaxing the old/new checks would weaken the intended acceptance test. None was done.

A Rust-only `place` retry cannot retry a native temp that the vendor already removed. Replaying entire transfers with retained payloads/retry rules would be a product-policy redesign, not a fixture correction; no such retry was added. Deleting the target before a rename would break atomicity/no-missing-path safety and is not an option.

## Owner options (not implemented or authorized here)

1. **Preferred next diagnostic:** obtain a Windows-owned closed-target control and held-open-target control, using an independently instrumented scratch harness to capture the legacy call's NTSTATUS. Keep the accepted vendor bytes unchanged, bind the diagnostic source separately, and retain the original failing concurrent test. This can distinguish open-target failure from other failures hidden by C, without granting product acceptance.
2. If that confirms the contract mismatch and concurrent-open-reader replacement remains mandatory, explicitly approve a separately reviewed vendor successor/upstream change using FileRenameInformationEx with REPLACE_IF_EXISTS + POSIX_SEMANTICS, with supported-filesystem/OS failure behavior specified. This violates the current byte-exact/no-divergence constraint and requires new authority, new provenance, independent source review and hosted native RED→GREEN.
3. Alternatively explicitly decide that Windows Replace may fail closed when another application is reading the target. That is an acceptance/product-policy change, not a test-fixture repair, and conflicts with this brief's required reader-success assertion. Do not silently turn that policy into the current test's expected result.

**Required decision:** authorize the separately bound native diagnostic and then choose the replacement contract/vendor policy. M3 is not complete, and release/smokes remain gated. No source repair for CI-C is claimed.
