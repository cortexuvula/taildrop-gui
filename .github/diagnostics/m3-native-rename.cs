// Standalone hosted-Windows diagnostic: proof of the vendored TailFiles
// agent's legacy rename (NtSetInformationFile class 10, ReplaceIfExists=1).
// Diagnostic-only; never a product replacement and never a product CI
// acceptance gate. Not compiled or executed on macOS.
//
// Mirrors src-tauri/agents/agent.cs exactly where it matters:
//   Top()       CreateFileW(GENERIC_READ, share 7, OPEN_EXISTING,
//               FILE_FLAG_BACKUP_SEMANTICS)
//   Open()      NtCreateFile(access|SYNCHRONIZE, attributes 0x80, share 7,
//               options|SYNC_IO|OPEN_REPARSE_POINT, OBJ_CASE_INSENSITIVE);
//               status is logged, never gated on
//   RealName()  FileNormalizedNameInfo(24) spelling lookup with fallback to
//               the requested name
//   RenameTo()  FILE_RENAME_INFORMATION buffer: replace byte at 0, root
//               handle at IntPtr.Size, name byte length at 2*IntPtr.Size,
//               UTF-16 name at 2*IntPtr.Size+4, buffer zeroed, class 10
//               ONLY - no FileRenameInformationEx, no POSIX successor, no
//               retry, no sleep, no delete-before-rename
//
// Cases:
//   closed         target handle closed before the rename (positive control;
//                  failure here FAILS the diagnostic truthfully)
//   held-share-rwd target held with FILE_SHARE_READ|WRITE|DELETE (7), the
//                  equivalent of the share mask a Rust std::fs open holds
//                  by default (disclosed equivalent mask, not Rust code)
//   held-share-rw  target held without DELETE sharing (3), comparison case
//
// Every event goes to stdout AND is appended to the events file (argv[1])
// as it happens, flushed per line, so an interrupted run still leaves
// evidence on disk. The raw NTSTATUS is logged immediately after the rename
// returns, before any postcondition assertion; Win32 last-error is never
// used as a substitute for it (it only decorates setup/teardown failures).
// All log content is ASCII by construction (GUID sandbox, ASCII fixtures).
//
// Exit codes: 0 all cases internally consistent and sandbox removed;
// 1 a case failed its assertions or an argument guard failed;
// 2 cleanup could not remove every fixture or the sandbox (evidence kept).

using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

public static class M3NativeRenameProof
{
    [StructLayout(LayoutKind.Sequential)] struct UNICODE_STRING { public ushort Length; public ushort MaximumLength; public IntPtr Buffer; }
    [StructLayout(LayoutKind.Sequential)] struct OBJECT_ATTRIBUTES { public int Length; public IntPtr RootDirectory; public IntPtr ObjectName; public uint Attributes; public IntPtr SecurityDescriptor; public IntPtr SecurityQualityOfService; }
    [StructLayout(LayoutKind.Sequential)] struct IO_STATUS_BLOCK { public IntPtr Status; public IntPtr Information; }

    [DllImport("ntdll.dll")] static extern int NtCreateFile(out IntPtr handle, uint access, ref OBJECT_ATTRIBUTES oa, out IO_STATUS_BLOCK io, IntPtr allocation, uint attributes, uint share, uint disposition, uint options, IntPtr ea, uint eaLength);
    [DllImport("ntdll.dll")] static extern int NtSetInformationFile(IntPtr handle, out IO_STATUS_BLOCK io, IntPtr info, uint length, int cls);
    [DllImport("ntdll.dll")] static extern int NtClose(IntPtr handle);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool WriteFile(IntPtr h, byte[] data, int size, out int written, IntPtr overlapped);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool ReadFile(IntPtr h, byte[] data, int size, out int read, IntPtr overlapped);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetFilePointerEx(IntPtr h, long move, out long position, uint method);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetFileTime(IntPtr h, IntPtr creation, IntPtr access, ref long write);
    [DllImport("kernel32.dll", SetLastError = true, EntryPoint = "GetFileInformationByHandleEx")] static extern bool GetFileNameByHandle(IntPtr handle, int cls, byte[] info, uint size);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern uint GetFinalPathNameByHandleW(IntPtr h, StringBuilder path, uint length, uint flags);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool GetVolumeInformationW(string root, StringBuilder label, uint size, out uint serial, out uint maxComponent, out uint flags, StringBuilder fs, uint fsSize);

    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000, DELETE = 0x10000, SYNCHRONIZE = 0x100000;
    const uint READ_ATTRIBUTES = 0x80, SHARE_ALL = 7, OPEN = 1, CREATE = 2, NON_DIRECTORY_FILE = 0x40, SYNC_IO = 0x20, OPEN_REPARSE_POINT = 0x200000;
    const string SandboxPrefix = "m3-native-rename-";
    const string TargetName = "Report.TXT";
    const string SourceName = ".tailfiles-incoming-0000000000000003-1";
    const string SentinelName = "sentinel.txt";

    static string sandbox;
    static string eventsPath;
    static readonly object gate = new object();
    static readonly List<string> files = new List<string>(), dirs = new List<string>();
    static readonly List<string> caseResults = new List<string>();
    static readonly byte[] Old = Payload((byte)'o'), New = Payload((byte)'n'), Sentinel = Encoding.ASCII.GetBytes("M3-NATIVE-RENAME-SENTINEL-UNCHANGED\n");

    static byte[] Payload(byte value) { byte[] b = new byte[4096]; for (int i = 0; i < b.Length; i++) b[i] = value; return b; }
    static string Hex(int v) { return "0x" + unchecked((uint)v).ToString("X8"); }
    static string Hash(byte[] b) { using (SHA256 h = SHA256.Create()) return BitConverter.ToString(h.ComputeHash(b)).Replace("-", "").ToLowerInvariant(); }

    static void Log(string ev, params string[] fields)
    {
        string line = DateTime.UtcNow.ToString("o") + "\t" + ev + "\t" + string.Join("\t", fields);
        lock (gate)
        {
            Console.WriteLine(line);
            Console.Out.Flush();
            try { File.AppendAllText(eventsPath, line + "\n", Encoding.ASCII); }
            catch (Exception ex)
            {
                Console.WriteLine(DateTime.UtcNow.ToString("o") + "\tEVENTS_WRITE_ERROR\t" + ex.Message);
                Console.Out.Flush();
            }
        }
    }

    // Setup/teardown assertion. The rename result itself is the raw NTSTATUS
    // logged by RENAME_RETURN; GetLastWin32Error never substitutes for it.
    static void Need(bool yes, string what)
    {
        if (!yes) throw new IOException(what + "; win32=" + Marshal.GetLastWin32Error());
    }

    static string Norm(string p) { return Path.GetFullPath(p).TrimEnd('\\'); }

    // Path guard: name is a single component, and the result stays inside the
    // sandbox as an immediate child of `parent` (no traversal, no reparse hop).
    static string Child(string parent, string name)
    {
        if (name.Length == 0 || name == "." || name == ".." || name.IndexOfAny(new char[] { '\\', '/', ':', '\0' }) >= 0) throw new IOException("bad child name");
        string p = Norm(Path.Combine(parent, name));
        if (!p.StartsWith(sandbox + "\\", StringComparison.OrdinalIgnoreCase)) throw new IOException("outside sandbox: " + p);
        if (!string.Equals(Norm(Path.GetDirectoryName(p)), Norm(parent), StringComparison.OrdinalIgnoreCase)) throw new IOException("not immediate child: " + p);
        return p;
    }

    static string FilePlan(string dir, string name) { string p = Child(dir, name); files.Add(p); Log("PLAN_FILE", p); return p; }

    static void Close(ref IntPtr h, string who)
    {
        if (h != IntPtr.Zero && h != new IntPtr(-1))
        {
            bool ok = CloseHandle(h);
            Log("CLOSE_HANDLE", who, "ok=" + ok);
            h = IntPtr.Zero;
            Need(ok, "CloseHandle " + who);
        }
    }

    // Vendor Top(): exact access/share/disposition/flags, logged before the call.
    static IntPtr Top(string path)
    {
        Log("TOP_ARGS", path, "access=0x80000000", "share=0x00000007", "disposition=3", "flags=0x02000000");
        IntPtr h = CreateFileW(path, GENERIC_READ, SHARE_ALL, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
        Need(h != new IntPtr(-1), "Top " + path);
        Need((File.GetAttributes(path) & FileAttributes.Directory) != 0, "not a folder: " + path);
        return h;
    }

    // Vendor Open(): status logged, never gated. Share is always 7 here.
    static int Open(IntPtr root, string name, uint access, uint disposition, uint options, bool noFollow, out IntPtr h)
    {
        IntPtr buf = Marshal.StringToHGlobalUni(name), us = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(UNICODE_STRING)));
        try
        {
            UNICODE_STRING u; u.Length = (ushort)(name.Length * 2); u.MaximumLength = u.Length; u.Buffer = buf;
            Marshal.StructureToPtr(u, us, false);
            OBJECT_ATTRIBUTES oa = new OBJECT_ATTRIBUTES();
            oa.Length = Marshal.SizeOf(typeof(OBJECT_ATTRIBUTES));
            oa.RootDirectory = root; oa.ObjectName = us; oa.Attributes = 0x40;
            IO_STATUS_BLOCK io;
            uint opt = options | SYNC_IO | (noFollow ? OPEN_REPARSE_POINT : 0);
            int st = NtCreateFile(out h, access | SYNCHRONIZE, ref oa, out io, IntPtr.Zero, 0x80, SHARE_ALL, disposition, opt, IntPtr.Zero, 0);
            Log("NT_OPEN", name, "access=" + Hex((int)(access | SYNCHRONIZE)), "share=0x00000007", "attributes=0x00000080", "oa_attributes=0x00000040", "disposition=" + disposition, "options=" + Hex((int)opt), "status=" + Hex(st));
            return st;
        }
        finally { Marshal.FreeHGlobal(us); Marshal.FreeHGlobal(buf); }
    }

    // Vendor RealName(): the actual on-disk spelling of `name`, if it matches
    // case-insensitively; `name` otherwise. Never fatal.
    static string RealName(IntPtr dir, string name)
    {
        IntPtr e;
        if (Open(dir, name, READ_ATTRIBUTES, OPEN, 0, true, out e) < 0) { Log("REALNAME_FALLBACK", name); return name; }
        try
        {
            byte[] b = new byte[4 + 65536];
            if (!GetFileNameByHandle(e, 24, b, (uint)b.Length)) { Log("REALNAME_FALLBACK", name); return name; }
            int n = BitConverter.ToInt32(b, 0);
            if (n <= 0 || n > b.Length - 4) return name;
            string full = Encoding.Unicode.GetString(b, 4, n), last = full.Substring(full.LastIndexOf('\\') + 1);
            Log("REALNAME", name, "actual=" + last, "normalized=" + full);
            return string.Equals(last, name, StringComparison.OrdinalIgnoreCase) ? last : name;
        }
        finally { Close(ref e, "RealName"); }
    }

    // Vendor RenameTo(): same buffer size, offsets, zeroing and class.
    // RENAME_RETURN logs the raw NTSTATUS immediately after the call returns,
    // before any postcondition assertion can run.
    static int RenameTo(IntPtr h, IntPtr dir, string name, bool replace)
    {
        int n = name.Length * 2, off = 2 * IntPtr.Size + 4, size = off + n + 2;
        IntPtr buf = Marshal.AllocHGlobal(size);
        try
        {
            for (int k = 0; k < size; k++) Marshal.WriteByte(buf, k, 0);
            Marshal.WriteByte(buf, 0, (byte)(replace ? 1 : 0));
            Marshal.WriteIntPtr(buf, IntPtr.Size, dir);
            Marshal.WriteInt32(buf, 2 * IntPtr.Size, n);
            Marshal.Copy(name.ToCharArray(), 0, IntPtr.Add(buf, off), name.Length);
            Log("RENAME_CALL", "class=10", "replace_byte=" + (replace ? 1 : 0), "pointer_size=" + IntPtr.Size,
                "replace_offset=0", "root_offset=" + IntPtr.Size, "length_offset=" + (2 * IntPtr.Size),
                "name_offset=" + off, "name_bytes=" + n, "buffer_size=" + size, "root_handle=" + dir.ToInt64(), "basename=" + name);
            IO_STATUS_BLOCK io;
            int st = NtSetInformationFile(h, out io, buf, (uint)size, 10);
            Log("RENAME_RETURN", "status=" + Hex(st), "io_status=" + Hex(unchecked((int)io.Status.ToInt64())), "io_information=" + io.Information.ToInt64());
            return st;
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    // Fixture write through the vendor-style temp-file open
    // (GENERIC_WRITE|GENERIC_READ|DELETE, CREATE, NON_DIRECTORY_FILE,
    // OPEN_REPARSE_POINT), then closed.
    static void Put(IntPtr root, string name, byte[] b)
    {
        IntPtr h;
        int st = Open(root, name, GENERIC_WRITE | GENERIC_READ | DELETE, CREATE, NON_DIRECTORY_FILE, true, out h);
        Need(st >= 0, "fixture create " + name + " status=" + Hex(st));
        try { WriteAll(h, name, b); } finally { Close(ref h, "put:" + name); }
    }

    // Same open, but the handle stays open across the rename, exactly like
    // the product's held temp file.
    static IntPtr PutKeep(IntPtr root, string name, byte[] b)
    {
        IntPtr h;
        int st = Open(root, name, GENERIC_WRITE | GENERIC_READ | DELETE, CREATE, NON_DIRECTORY_FILE, true, out h);
        Need(st >= 0, "source create " + name + " status=" + Hex(st));
        WriteAll(h, name, b);
        return h;
    }

    static void WriteAll(IntPtr h, string name, byte[] b)
    {
        int w;
        Need(WriteFile(h, b, b.Length, out w, IntPtr.Zero) && w == b.Length, "WriteFile " + name);
        long ft = 116444736000000000L + 10000000L;
        Need(SetFileTime(h, IntPtr.Zero, IntPtr.Zero, ref ft), "SetFileTime " + name);
        Log("WRITE_COMPLETE", name, "bytes=" + b.Length, "sha256=" + Hash(b), "mtime_unix=1");
    }

    static string NativeName(IntPtr h)
    {
        byte[] b = new byte[4 + 65536];
        Need(GetFileNameByHandle(h, 24, b, (uint)b.Length), "FileNormalizedNameInfo");
        int n = BitConverter.ToInt32(b, 0);
        Need(n > 0 && n <= b.Length - 4, "native filename length");
        return Encoding.Unicode.GetString(b, 4, n);
    }

    // Read the full 4096 fixture bytes through the held handle, from offset 0.
    static byte[] HeldRead(IntPtr h, string stage)
    {
        long pos;
        Need(SetFilePointerEx(h, 0, out pos, 0), "seek held reader");
        byte[] b = new byte[4096]; int n;
        Need(ReadFile(h, b, b.Length, out n, IntPtr.Zero) && n == b.Length, "read held reader");
        string nn = "unavailable";
        try { nn = NativeName(h); } catch (Exception ex) { nn = "error: " + ex.Message; }
        Log("HELD_READ", stage, "bytes=" + n, "sha256=" + Hash(b), "first_byte=" + b[0], "last_byte=" + b[b.Length - 1], "native_name=" + nn);
        return b;
    }

    static void Snapshot(string dir, string stage)
    {
        string[] entries = Directory.GetFileSystemEntries(dir);
        Array.Sort(entries, StringComparer.Ordinal);
        foreach (string p in entries)
        {
            Need(files.Contains(p), "unplanned entry: " + p);
            Need((File.GetAttributes(p) & FileAttributes.ReparsePoint) == 0, "reparse fixture: " + p);
            byte[] b = File.ReadAllBytes(p);
            Log("SNAPSHOT", stage, "path=" + p, "basename=" + Path.GetFileName(p), "bytes=" + b.Length, "sha256=" + Hash(b));
        }
    }

    static bool RunCase(string kind, uint share, bool held)
    {
        try { Case(kind, share, held); caseResults.Add(kind + ":PASS"); Log("CASE_PASS", kind); return true; }
        catch (Exception e) { caseResults.Add(kind + ":FAIL"); Log("CASE_ASSERT_FAIL", kind, e.GetType().Name + ": " + e.Message); return false; }
    }

    static void Case(string kind, uint share, bool held)
    {
        string dir = Child(sandbox, kind);
        dirs.Add(dir);
        Log("PLAN_DIR", dir);
        Directory.CreateDirectory(dir);
        Log("MKDIR", dir, "reparse=" + ((File.GetAttributes(dir) & FileAttributes.ReparsePoint) != 0));
        string target = FilePlan(dir, TargetName), source = FilePlan(dir, SourceName), sentinel = FilePlan(dir, SentinelName);
        IntPtr root = IntPtr.Zero, h = IntPtr.Zero, reader = IntPtr.Zero;
        try
        {
            root = Top(dir);
            Put(root, TargetName, Old);
            Put(root, SentinelName, Sentinel);
            h = PutKeep(root, SourceName, New);
            Snapshot(dir, kind + "/before");
            if (held)
            {
                Log("READER_OPEN_ARGS", kind, target, "api=CreateFileW", "access=0x80000000", "share=0x" + share.ToString("X8"), "disposition=3", "flags=0x00000080", "security=null", "template=null");
                reader = CreateFileW(target, GENERIC_READ, share, IntPtr.Zero, 3, 0x80, IntPtr.Zero);
                Need(reader != new IntPtr(-1), "held reader open " + kind);
                Need(Hash(HeldRead(reader, kind + "/before")) == Hash(Old), "initial held bytes " + kind);
            }
            string actual = RealName(root, "report.txt");
            Need(actual == TargetName, "original basename lookup returned '" + actual + "'");
            Log("READER_LIVENESS_BEFORE_CALL", kind, "held=" + held, "reader_handle=" + reader.ToInt64(), "source_handle=" + h.ToInt64());
            int st = RenameTo(h, root, actual, true);
            if (held)
            {
                Need(Hash(HeldRead(reader, kind + "/after")) == Hash(Old), "held original bytes changed " + kind);
            }
            Log("CASE_RESULT", kind, "status=" + Hex(st), "held_across_call=" + held, "reader_handle=" + reader.ToInt64(), "source_handle=" + h.ToInt64());
            Close(ref reader, kind + "/held-reader");
            Close(ref h, kind + "/source");
            Snapshot(dir, kind + "/after");
            byte[] winning = File.ReadAllBytes(target);
            Need(Hash(winning) == Hash(st >= 0 ? New : Old), "winning bytes do not match observed status " + kind);
            Need(Hash(File.ReadAllBytes(sentinel)) == Hash(Sentinel), "sentinel changed " + kind);
            Need(File.Exists(source) == (st < 0), "source-name existence disagrees with status " + kind);
            Log("OBSERVATIONS_VALID", kind, "status=" + Hex(st), "winning_basename=" + actual, "winning_sha256=" + Hash(winning), "sentinel_sha256=" + Hash(Sentinel), "source_absent=" + (!File.Exists(source)));
            if (!held) Need(st == 0, "closed-target positive control FAILED (status=" + Hex(st) + ")");
        }
        finally
        {
            try { Close(ref reader, kind + "/held-reader"); }
            finally { try { Close(ref h, kind + "/source"); } finally { Close(ref root, kind + "/directory"); } }
        }
    }

    // Managed cleanup only: the measurement must not issue any other
    // NtSetInformationFile class, so the product's handle-based Unlink is
    // deliberately NOT used here.
    static bool Cleanup()
    {
        bool ok = true;
        for (int i = files.Count - 1; i >= 0; i--)
        {
            string p = files[i];
            try
            {
                if (File.Exists(p))
                {
                    Need((File.GetAttributes(p) & FileAttributes.ReparsePoint) == 0, "cleanup reparse " + p);
                    File.Delete(p);
                }
                Log("CLEANUP_FILE", p, "absent=" + (!File.Exists(p)));
            }
            catch (Exception e) { ok = false; Log("CLEANUP_ERROR", p, e.Message); }
        }
        for (int i = dirs.Count - 1; i >= 0; i--)
        {
            string p = dirs[i];
            try { if (Directory.Exists(p)) Directory.Delete(p, false); Log("CLEANUP_DIR", p, "absent=" + (!Directory.Exists(p))); }
            catch (Exception e) { ok = false; Log("CLEANUP_ERROR", p, e.Message); }
        }
        try { if (Directory.Exists(sandbox)) Directory.Delete(sandbox, false); Log("CLEANUP_SANDBOX", sandbox, "absent=" + (!Directory.Exists(sandbox))); }
        catch (Exception e) { ok = false; Log("CLEANUP_ERROR", sandbox, e.Message); }
        return ok;
    }

    static bool IsSandboxName(string n)
    {
        if (n.Length != SandboxPrefix.Length + 32) return false;
        if (!n.StartsWith(SandboxPrefix, StringComparison.Ordinal)) return false;
        for (int i = SandboxPrefix.Length; i < n.Length; i++)
        {
            char c = n[i];
            if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
        }
        return true;
    }

    public static int Main(string[] args)
    {
        int exitCode = 1;
        bool valid = false;
        try
        {
            Need(args.Length == 2, "usage: m3-native-rename <sandbox-dir> <events-file> (got " + args.Length + " args)");
            sandbox = Norm(args[0]);
            eventsPath = Path.GetFullPath(args[1]);
            string temp = Norm(Path.GetTempPath());
            string home = Norm(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
            Need(Path.IsPathRooted(temp) && !string.Equals(temp, Norm(Path.GetPathRoot(temp)), StringComparison.OrdinalIgnoreCase), "unsafe TEMP: " + temp);
            Need(string.Equals(Norm(Path.GetDirectoryName(sandbox)), temp, StringComparison.OrdinalIgnoreCase), "sandbox is not an immediate TEMP child: " + sandbox);
            Need(IsSandboxName(Path.GetFileName(sandbox)), "not a GUID sandbox name: " + Path.GetFileName(sandbox));
            if (home.Length >= 4) Need(!home.StartsWith(sandbox + "\\", StringComparison.OrdinalIgnoreCase), "home folder inside sandbox");
            Need(!eventsPath.StartsWith(sandbox + "\\", StringComparison.OrdinalIgnoreCase), "events file inside sandbox");
            Need(Directory.Exists(sandbox), "sandbox does not exist: " + sandbox);
            Need((File.GetAttributes(sandbox) & FileAttributes.ReparsePoint) == 0, "sandbox is a reparse point");
            Need(Directory.GetFileSystemEntries(sandbox).Length == 0, "sandbox is not empty");
            Need(!File.Exists(eventsPath) || new FileInfo(eventsPath).Length == 0, "events file is not empty at start");
            valid = true;
            Log("IDENTITY", "hostname=" + Environment.MachineName, "username=" + Environment.UserName,
                "os=" + Environment.OSVersion.VersionString, "pointer_size=" + IntPtr.Size, "temp=" + temp, "sandbox=" + sandbox);
            string volume = Path.GetPathRoot(sandbox);
            StringBuilder label = new StringBuilder(261), fsb = new StringBuilder(261);
            uint serial, maxc, vflags;
            Need(GetVolumeInformationW(volume, label, 261, out serial, out maxc, out vflags, fsb, 261), "volume information");
            Log("VOLUME", "root=" + volume, "filesystem=" + fsb, "serial=" + serial.ToString("X8"), "flags=" + vflags.ToString("X8"), "max_component=" + maxc);
            Log("EXPECTED_BYTES", "old_sha256=" + Hash(Old), "new_sha256=" + Hash(New), "sentinel_sha256=" + Hash(Sentinel),
                "fixture_total_bytes=" + (3 * (Old.Length + New.Length + Sentinel.Length)), "cases=3", "repetitions_per_case=1");
            IntPtr top = IntPtr.Zero;
            try
            {
                top = Top(sandbox);
                StringBuilder finalName = new StringBuilder(32768);
                Need(GetFinalPathNameByHandleW(top, finalName, 32768, 0) > 0, "resolved sandbox path");
                string real = finalName.ToString();
                if (real.StartsWith("\\\\?\\")) real = real.Substring(4);
                Need(string.Equals(Norm(real), sandbox, StringComparison.OrdinalIgnoreCase), "sandbox resolution differs: " + real);
                Log("SANDBOX_RESOLUTION", "final=" + real);
            }
            finally { Close(ref top, "sandbox-resolution"); }
            bool ok = RunCase("closed", 0, false);
            ok = RunCase("held-share-rwd", 7, true) && ok;
            ok = RunCase("held-share-rw", 3, true) && ok;
            exitCode = ok ? 0 : 1;
        }
        catch (Exception e) { Log("ERROR", e.ToString()); exitCode = 1; }
        finally
        {
            if (valid)
            {
                bool clean = Cleanup();
                bool absent = !Directory.Exists(sandbox);
                Log("SANDBOX_ABSENT", absent.ToString());
                if (!clean || !absent) exitCode = 2;
            }
            Log("HARNESS_EXIT", "code=" + exitCode, "cases=" + string.Join(";", caseResults.ToArray()));
        }
        return exitCode;
    }
}
