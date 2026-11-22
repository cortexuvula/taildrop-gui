using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace TailFiles
{
    public class Stop : Exception
    {
        public int Code;
        public Stop(string message, int code) : base(message) { Code = code; }
    }

    // stdin/stdout/stderr as Windows' sshd hands them over: pipes that may
    // be opened for overlapped I/O, where a plain blocking read that has to
    // wait never completes. Every read and write here is started and then
    // waited for, which works for either kind of handle.
    public class Pipe : Stream
    {
        [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int which);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool ReadFile(IntPtr h, IntPtr buf, int n, IntPtr done, IntPtr ov);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool WriteFile(IntPtr h, IntPtr buf, int n, IntPtr done, IntPtr ov);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetOverlappedResult(IntPtr h, IntPtr ov, out int done, bool wait);
        [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateEventW(IntPtr attrs, bool manual, bool initial, IntPtr name);

        IntPtr h, ev, ov, mem = IntPtr.Zero;
        int memSize;

        public Pipe(int which)
        {
            h = GetStdHandle(which);
            ev = CreateEventW(IntPtr.Zero, true, false, IntPtr.Zero);
            ov = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(System.Threading.NativeOverlapped)));
        }

        int Io(bool write, byte[] b, int off, int n)
        {
            if (n > memSize) { if (mem != IntPtr.Zero) Marshal.FreeHGlobal(mem); mem = Marshal.AllocHGlobal(n); memSize = n; }
            if (write) Marshal.Copy(b, off, mem, n);
            System.Threading.NativeOverlapped o = new System.Threading.NativeOverlapped();
            o.EventHandle = ev;
            Marshal.StructureToPtr(o, ov, false);
            bool ok = write ? WriteFile(h, mem, n, IntPtr.Zero, ov) : ReadFile(h, mem, n, IntPtr.Zero, ov);
            int e = ok ? 0 : Marshal.GetLastWin32Error(), done;
            if (e == 109 || e == 38 || e == 232) return 0;          // the other end closed
            if (e != 0 && e != 997) throw new IOException("pipe error " + e);
            if (!GetOverlappedResult(h, ov, out done, true))
            {
                e = Marshal.GetLastWin32Error();
                if (e == 109 || e == 38 || e == 232) return 0;
                throw new IOException("pipe error " + e);
            }
            if (!write && done > 0) Marshal.Copy(mem, b, off, done);
            return done;
        }

        public override int Read(byte[] b, int off, int n) { return n == 0 ? 0 : Io(false, b, off, n); }
        public override void Write(byte[] b, int off, int n)
        {
            while (n > 0)
            {
                int k = Io(true, b, off, n);
                if (k <= 0) throw new IOException("the other end closed");
                off += k; n -= k;
            }
        }
        public override bool CanRead { get { return true; } }
        public override bool CanWrite { get { return true; } }
        public override bool CanSeek { get { return false; } }
        public override long Length { get { throw new NotSupportedException(); } }
        public override long Position { get { throw new NotSupportedException(); } set { throw new NotSupportedException(); } }
        public override void Flush() { }
        public override long Seek(long o, SeekOrigin s) { throw new NotSupportedException(); }
        public override void SetLength(long v) { throw new NotSupportedException(); }
    }

    // Everything is opened relative to a folder handle this program holds
    // (NtCreateFile with a RootDirectory), never following a junction or
    // symlink (FILE_OPEN_REPARSE_POINT), and examined, read, renamed and
    // deleted through that same handle.
    public static class Agent
    {
        [StructLayout(LayoutKind.Sequential)] struct UNICODE_STRING { public ushort Length; public ushort MaximumLength; public IntPtr Buffer; }
        [StructLayout(LayoutKind.Sequential)] struct OBJECT_ATTRIBUTES { public int Length; public IntPtr RootDirectory; public IntPtr ObjectName; public uint Attributes; public IntPtr SecurityDescriptor; public IntPtr SecurityQualityOfService; }
        [StructLayout(LayoutKind.Sequential)] struct IO_STATUS_BLOCK { public IntPtr Status; public IntPtr Information; }
        [StructLayout(LayoutKind.Sequential, Pack = 4)]
        struct BY_HANDLE_FILE_INFORMATION
        {
            public uint FileAttributes; public long CreationTime; public long LastAccessTime; public long LastWriteTime;
            public uint VolumeSerialNumber; public uint FileSizeHigh; public uint FileSizeLow; public uint NumberOfLinks;
            public uint FileIndexHigh; public uint FileIndexLow;
        }
        [StructLayout(LayoutKind.Sequential)] struct FILE_ATTRIBUTE_TAG_INFO { public uint FileAttributes; public uint ReparseTag; }

        [DllImport("ntdll.dll")] static extern int NtCreateFile(out IntPtr handle, uint access, ref OBJECT_ATTRIBUTES oa, out IO_STATUS_BLOCK io, IntPtr allocation, uint attributes, uint share, uint disposition, uint options, IntPtr ea, uint eaLength);
        [DllImport("ntdll.dll")] static extern int NtQueryDirectoryFile(IntPtr handle, IntPtr ev, IntPtr apc, IntPtr ctx, out IO_STATUS_BLOCK io, IntPtr info, uint length, int cls, [MarshalAs(UnmanagedType.U1)] bool single, IntPtr name, [MarshalAs(UnmanagedType.U1)] bool restart);
        [DllImport("ntdll.dll")] static extern int NtSetInformationFile(IntPtr handle, out IO_STATUS_BLOCK io, IntPtr info, uint length, int cls);
        [DllImport("ntdll.dll")] static extern int NtClose(IntPtr handle);
        [DllImport("ntdll.dll")] static extern uint RtlNtStatusToDosError(int status);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetFileInformationByHandle(IntPtr handle, out BY_HANDLE_FILE_INFORMATION info);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetFileInformationByHandleEx(IntPtr handle, int cls, out FILE_ATTRIBUTE_TAG_INFO info, uint size);
        [DllImport("kernel32.dll", SetLastError = true, EntryPoint = "GetFileInformationByHandleEx")] static extern bool GetFileNameByHandle(IntPtr handle, int cls, byte[] info, uint size);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetFileTime(IntPtr handle, IntPtr creation, IntPtr access, ref long write);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool ReadFile(IntPtr handle, byte[] buffer, int count, out int read, IntPtr overlapped);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool WriteFile(IntPtr handle, byte[] buffer, int count, out int written, IntPtr overlapped);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern uint GetFinalPathNameByHandleW(IntPtr handle, StringBuilder path, uint length, uint flags);

        const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000, DELETE = 0x10000, SYNCHRONIZE = 0x100000;
        const uint READ_ATTRIBUTES = 0x80, SHARE_ALL = 7, OPEN = 1, CREATE = 2;
        const uint DIRECTORY_FILE = 1, NON_DIRECTORY_FILE = 0x40, SYNC_IO = 0x20, OPEN_REPARSE_POINT = 0x200000;
        const uint ATTR_READONLY = 1, ATTR_DIRECTORY = 0x10, ATTR_REPARSE = 0x400;
        const int NAME_COLLISION = unchecked((int)0xC0000035), NOT_FOUND = unchecked((int)0xC0000034);
        const int NO_MORE_FILES = unchecked((int)0x80000006), BUFFER_OVERFLOW = unchecked((int)0x80000005);
        const long EPOCH = 116444736000000000L;
        const int MAX_DEPTH = 512;

        static Stream input;
        static byte[] inBuf = new byte[262144];
        static int inPos, inLen;
        static Stream output;

        // ------------------------------------------------------------ input
        static int Byte()
        {
            if (inPos == inLen)
            {
                inLen = input.Read(inBuf, 0, inBuf.Length);
                inPos = 0;
                if (inLen <= 0) { inLen = 0; return -1; }
            }
            return inBuf[inPos++];
        }
        static string Field()
        {
            List<byte> b = new List<byte>();
            while (true)
            {
                int c = Byte();
                if (c < 0) throw new Stop("short input", 2);
                if (c == 0) break;
                if (b.Count > 65536) throw new Stop("field too long", 2);
                b.Add((byte)c);
            }
            return new UTF8Encoding(false, true).GetString(b.ToArray());
        }
        static long Number()
        {
            string f = Field(); long n;
            if (f.Length == 0 || f.Length > 15 || !long.TryParse(f, out n) || n < 0) throw new Stop("bad number", 2);
            return n;
        }
        static void Exact(byte[] buf, int n)
        {
            int got = 0;
            while (got < n)
            {
                if (inPos == inLen)
                {
                    inLen = input.Read(inBuf, 0, inBuf.Length); inPos = 0;
                    if (inLen <= 0) { inLen = 0; throw new Stop("short input", 2); }
                }
                int take = Math.Min(n - got, inLen - inPos);
                Buffer.BlockCopy(inBuf, inPos, buf, got, take);
                inPos += take; got += take;
            }
        }

        // ----------------------------------------------------------- output
        static void Put(string s) { byte[] b = Encoding.UTF8.GetBytes(s); output.Write(b, 0, b.Length); }
        static void Rec(string type, params string[] fields)
        {
            Put(type);
            foreach (string f in fields) { Put(f); output.WriteByte(0); }
        }

        // ----------------------------------------------------------- native
        static string Why(int status)
        {
            return new System.ComponentModel.Win32Exception((int)RtlNtStatusToDosError(status)).Message;
        }

        // A single name: never a path, a stream (":") or "." / "..".
        static void CheckName(string name)
        {
            if (name.Length == 0 || name == "." || name == ".." || name.IndexOfAny(new char[] { '\\', '/', ':', '\0' }) >= 0)
                throw new Stop("bad name: " + name, 2);
        }

        static int Open(IntPtr root, string name, uint access, uint disposition, uint options, bool noFollow, out IntPtr h)
        {
            CheckName(name);
            IntPtr buf = Marshal.StringToHGlobalUni(name);
            IntPtr us = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(UNICODE_STRING)));
            try
            {
                UNICODE_STRING u; u.Length = (ushort)(name.Length * 2); u.MaximumLength = u.Length; u.Buffer = buf;
                Marshal.StructureToPtr(u, us, false);
                OBJECT_ATTRIBUTES oa = new OBJECT_ATTRIBUTES();
                oa.Length = Marshal.SizeOf(typeof(OBJECT_ATTRIBUTES));
                oa.RootDirectory = root; oa.ObjectName = us; oa.Attributes = 0x40; // case-insensitive, as Windows names are
                IO_STATUS_BLOCK io;
                return NtCreateFile(out h, access | SYNCHRONIZE, ref oa, out io, IntPtr.Zero, 0x80, SHARE_ALL, disposition,
                                    options | SYNC_IO | (noFollow ? OPEN_REPARSE_POINT : 0), IntPtr.Zero, 0);
            }
            finally { Marshal.FreeHGlobal(us); Marshal.FreeHGlobal(buf); }
        }

        static string WinPath(string p)
        {
            if (p.Length >= 3 && p[0] == '/' && p[2] == ':') p = p.Substring(1);
            return p.Replace('/', '\\');
        }

        static IntPtr Top(string path)
        {
            IntPtr h = CreateFileW(WinPath(path), GENERIC_READ, SHARE_ALL, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
            if (h == new IntPtr(-1)) throw new Stop("Can't open " + path + ": " + new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error()).Message, 3);
            Info i = Examine(h);
            if (!i.Dir) { NtClose(h); throw new Stop(path + " isn't a folder", 3); }
            return h;
        }

        class Info { public bool Dir, Link, Reparse, ReadOnly; public long Size, Mtime; public uint Volume, IdHigh, IdLow; }

        static Info Examine(IntPtr h)
        {
            BY_HANDLE_FILE_INFORMATION b;
            if (!GetFileInformationByHandle(h, out b)) throw new Stop("Can't examine an item", 6);
            FILE_ATTRIBUTE_TAG_INFO t;
            if (!GetFileInformationByHandleEx(h, 9, out t, 8)) t.ReparseTag = 0;
            Info i = new Info();
            i.Reparse = (b.FileAttributes & ATTR_REPARSE) != 0;
            // Junctions and symlinks are "name surrogates": they stand for
            // another path. Other reparse points (OneDrive files, dedup) are
            // the item itself.
            i.Link = i.Reparse && (t.ReparseTag & 0x20000000) != 0;
            i.Dir = (b.FileAttributes & ATTR_DIRECTORY) != 0;
            i.ReadOnly = (b.FileAttributes & ATTR_READONLY) != 0;
            i.Size = ((long)b.FileSizeHigh << 32) | b.FileSizeLow;
            i.Mtime = Math.Max(0, (b.LastWriteTime - EPOCH) / 10000000L);
            i.Volume = b.VolumeSerialNumber; i.IdHigh = b.FileIndexHigh; i.IdLow = b.FileIndexLow;
            return i;
        }

        static bool Same(Info a, Info b) { return a.Volume == b.Volume && a.IdHigh == b.IdHigh && a.IdLow == b.IdLow; }

        static List<string> Names(IntPtr dir)
        {
            List<string> names = new List<string>();
            IntPtr buf = Marshal.AllocHGlobal(65536);
            try
            {
                bool restart = true;
                while (true)
                {
                    IO_STATUS_BLOCK io;
                    int st = NtQueryDirectoryFile(dir, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, out io, buf, 65536, 12, false, IntPtr.Zero, restart);
                    restart = false;
                    if (st == NO_MORE_FILES) break;
                    if (st < 0) throw new Stop("Can't list a folder: " + Why(st), 6);
                    int off = 0;
                    while (true)
                    {
                        int next = Marshal.ReadInt32(buf, off);
                        int len = Marshal.ReadInt32(buf, off + 8);
                        string n = Marshal.PtrToStringUni(IntPtr.Add(buf, off + 12), len / 2);
                        if (n != "." && n != "..") names.Add(n);
                        if (next == 0) break;
                        off += next;
                    }
                }
            }
            finally { Marshal.FreeHGlobal(buf); }
            names.Sort(StringComparer.Ordinal);
            return names;
        }

        // Rename the object `h` holds to `name` inside the folder `dir` holds.
        static int RenameTo(IntPtr h, IntPtr dir, string name, bool replace)
        {
            CheckName(name);
            int n = name.Length * 2, off = 2 * IntPtr.Size + 4, size = off + n + 2;
            IntPtr buf = Marshal.AllocHGlobal(size);
            try
            {
                for (int k = 0; k < size; k++) Marshal.WriteByte(buf, k, 0);
                Marshal.WriteByte(buf, 0, (byte)(replace ? 1 : 0));
                Marshal.WriteIntPtr(buf, IntPtr.Size, dir);
                Marshal.WriteInt32(buf, 2 * IntPtr.Size, n);
                Marshal.Copy(name.ToCharArray(), 0, IntPtr.Add(buf, off), name.Length);
                IO_STATUS_BLOCK io;
                return NtSetInformationFile(h, out io, buf, (uint)size, 10);
            }
            finally { Marshal.FreeHGlobal(buf); }
        }

        // Delete the object `h` holds (opened with DELETE).
        static int Unlink(IntPtr h)
        {
            IntPtr buf = Marshal.AllocHGlobal(4);
            try
            {
                IO_STATUS_BLOCK io;
                Marshal.WriteInt32(buf, 0, 1 | 2 | 0x10); // delete, POSIX semantics, ignore read-only
                int st = NtSetInformationFile(h, out io, buf, 4, 64);
                if (st >= 0) return st;
                Marshal.WriteByte(buf, 0, 1);              // older systems and disks
                return NtSetInformationFile(h, out io, buf, 1, 13);
            }
            finally { Marshal.FreeHGlobal(buf); }
        }

        // Remove everything inside the folder `h` holds, through handles; a
        // junction or symlink is removed itself, never entered.
        static void Empty(IntPtr h, int depth)
        {
            if (depth > MAX_DEPTH) throw new Stop("folders nested too deeply", 6);
            foreach (string n in Names(h))
            {
                IntPtr c;
                int st = Open(h, n, DELETE | GENERIC_READ, OPEN, 0, true, out c);
                if (st < 0) throw new Stop("Can't delete " + n + ": " + Why(st), 1);
                try
                {
                    Info i = Examine(c);
                    if (i.Dir && !i.Link) Empty(c, depth + 1);
                    st = Unlink(c);
                    if (st < 0) throw new Stop("Can't delete " + n + ": " + Why(st), 1);
                }
                finally { NtClose(c); }
            }
        }

        static void SetTime(IntPtr h, long mtime)
        {
            long ft = mtime * 10000000L + EPOCH;
            SetFileTime(h, IntPtr.Zero, IntPtr.Zero, ref ft);
        }

        // ------------------------------------------------------------- send
        static long SizeAt(IntPtr parent, string name, int depth)
        {
            IntPtr h;
            if (depth > MAX_DEPTH || Open(parent, name, GENERIC_READ, OPEN, 0, true, out h) < 0) return 0;
            try
            {
                Info i = Examine(h);
                if (i.Link) return 0;
                if (!i.Dir) return i.Size;
                long t = 0;
                foreach (string n in Names(h)) t += SizeAt(h, n, depth + 1);
                return t;
            }
            finally { NtClose(h); }
        }

        static bool Sendable(string n)
        {
            foreach (char c in n) if (c < 32 || c == 127 || char.IsSurrogate(c)) return false;
            return true;
        }

        static void SendAt(IntPtr parent, string name, string rel, int depth)
        {
            if (depth > MAX_DEPTH) throw new Stop(rel + " has folders nested more than 512 deep", 6);
            IntPtr h;
            int st = Open(parent, name, GENERIC_READ, OPEN, 0, true, out h);
            if (st < 0) throw new Stop(rel + " changed while it was being copied (" + Why(st) + ")", 6);
            try
            {
                Info i = Examine(h);
                if (i.Link) { Put("S"); return; }
                if (i.Dir)
                {
                    Rec("D", rel, "493", i.Mtime.ToString());
                    foreach (string n in Names(h))
                    {
                        if (!Sendable(n)) { Put("S"); continue; }
                        SendAt(h, n, rel + "/" + n, depth + 1);
                    }
                    return;
                }
                IntPtr data = h;
                if (i.Reparse)
                {
                    // e.g. a OneDrive file: open it normally so its content
                    // is read, and only if it is still the same file.
                    st = Open(parent, name, GENERIC_READ, OPEN, NON_DIRECTORY_FILE, false, out data);
                    if (st < 0) throw new Stop("Can't read " + rel + " (" + Why(st) + ")", 6);
                    if (!Same(Examine(data), i)) { NtClose(data); throw new Stop(rel + " changed while it was being copied", 6); }
                }
                try
                {
                    long left = i.Size;
                    Rec("F", rel, i.ReadOnly ? "292" : "420", i.Mtime.ToString(), left.ToString());
                    byte[] buf = new byte[262144];
                    while (left > 0)
                    {
                        int got;
                        if (!ReadFile(data, buf, (int)Math.Min(left, buf.Length), out got, IntPtr.Zero) || got <= 0)
                            throw new Stop(rel + " got shorter while it was being copied", 6);
                        output.Write(buf, 0, got);
                        left -= got;
                    }
                }
                finally { if (data != h) NtClose(data); }
            }
            finally { NtClose(h); }
        }

        static int CmdSend()
        {
            string parent = Field(); long count = Number();
            List<string> names = new List<string>();
            for (long k = 0; k < count; k++) names.Add(Field());
            IntPtr P = Top(parent);
            long total = 0;
            foreach (string n in names) total += SizeAt(P, n, 0);
            Rec("T", total.ToString());
            foreach (string n in names) SendAt(P, n, n, 0);
            Put("Z");
            return 0;
        }

        // ------------------------------------------------------------- recv
        // Each top-level item (and each new item inside a folder being
        // merged) is created exclusively under a temporary name in the folder
        // it belongs in, filled through held handles, then renamed by handle
        // to its real name, refusing to replace anything (or, with Replace,
        // replacing a file or link atomically). It is never looked up by name
        // again, and lands with that folder's inherited permissions.
        class Frame
        {
            public string Rel; public IntPtr H; public bool New, Skip;
            public IntPtr Parent; public string Final; public long Mtime; public bool Temp;
        }

        static List<string> conflicts = new List<string>(), errors = new List<string>();
        static List<IntPtr> temps = new List<IntPtr>();
        static string tag;
        static int counter;

        static string TempName() { counter++; return tag + "-" + counter; }

        static void Finish(Frame f, bool replace)
        {
            if (f.Skip) return;
            if (f.New)
            {
                SetTime(f.H, f.Mtime);
                if (f.Temp)
                {
                    int st = RenameTo(f.H, f.Parent, f.Final, false);
                    if (st < 0)
                    {
                        (st == NAME_COLLISION ? conflicts : errors).Add(f.Rel);
                        Empty(f.H, 0); Unlink(f.H);
                    }
                    temps.Remove(f.H);
                }
            }
            if (f.Rel != "") NtClose(f.H);
        }

        // The name an item in `dir` matching `name` (any case) really has, so
        // replacing it keeps its spelling as Explorer does; `name` if none.
        static string RealName(IntPtr dir, string name)
        {
            IntPtr e;
            if (Open(dir, name, READ_ATTRIBUTES, OPEN, 0, true, out e) < 0) return name;
            try
            {
                byte[] b = new byte[4 + 65536];
                if (!GetFileNameByHandle(e, 24, b, (uint)b.Length)) return name;  // FileNormalizedNameInfo
                int n = BitConverter.ToInt32(b, 0);
                if (n <= 0 || n > b.Length - 4) return name;
                string full = Encoding.Unicode.GetString(b, 4, n);
                string last = full.Substring(full.LastIndexOf('\\') + 1);
                return string.Equals(last, name, StringComparison.OrdinalIgnoreCase) ? last : name;
            }
            finally { NtClose(e); }
        }

        static string CaseClash(string p)
        {
            return "Can't copy " + p + " to Windows: another item in its folder has the same name apart from "
                 + "upper and lower case, which Windows treats as the same name";
        }

        static void Drain(long n)
        {
            byte[] buf = new byte[262144];
            while (n > 0) { int k = (int)Math.Min(n, buf.Length); Exact(buf, k); n -= k; }
        }

        static void WriteData(IntPtr h, long n, string rel)
        {
            byte[] buf = new byte[262144];
            while (n > 0)
            {
                int k = (int)Math.Min(n, buf.Length), w;
                Exact(buf, k);
                if (!WriteFile(h, buf, k, out w, IntPtr.Zero) || w != k)
                    throw new Stop("Can't write " + rel + ": " + new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error()).Message, 1);
                n -= k;
            }
        }

        static int CmdRecv()
        {
            string dst = Field(); tag = Field(); bool replace = Field() == "replace";
            if (!System.Text.RegularExpressions.Regex.IsMatch(tag, "^\\.tailfiles-incoming-[0-9a-f]{16}$")) throw new Stop("bad tag", 2);
            IntPtr D = Top(dst);
            List<Frame> stack = new List<Frame>();
            Frame top = new Frame(); top.Rel = ""; top.H = D;
            stack.Add(top);
            try
            {
                while (true)
                {
                    int t = Byte();
                    if (t < 0) throw new Stop("short input", 2);
                    if (t == 'Z') break;
                    string p = Field();
                    int slash = p.LastIndexOf('/');
                    string par = slash < 0 ? "" : p.Substring(0, slash), name = p.Substring(slash + 1);
                    CheckName(name);
                    while (stack.Count > 1 && stack[stack.Count - 1].Rel != par) { Finish(stack[stack.Count - 1], replace); stack.RemoveAt(stack.Count - 1); }
                    Frame P = stack[stack.Count - 1];
                    if (P.Rel != par) throw new Stop("out of order: " + p, 2);
                    if (t == 'D')
                    {
                        long mode = Number(), mt = Number();
                        if (stack.Count > MAX_DEPTH) throw new Stop(p + " has folders nested more than 512 deep", 2);
                        Frame f = new Frame(); f.Rel = p; f.Mtime = mt;
                        if (P.Skip) { f.Skip = true; }
                        else if (P.New)
                        {
                            int st = Open(P.H, name, GENERIC_READ | GENERIC_WRITE | DELETE, CREATE, DIRECTORY_FILE, true, out f.H);
                            if (st == NAME_COLLISION) throw new Stop(CaseClash(p), 1);
                            if (st < 0) throw new Stop("Can't create " + p + " (" + Why(st) + ")", 1);
                            f.New = true;
                        }
                        else
                        {
                            IntPtr e;
                            int st = Open(P.H, name, GENERIC_READ, OPEN, 0, true, out e);
                            if (st >= 0)
                            {
                                Info i = Examine(e);
                                if (replace && i.Dir && !i.Link) { f.H = e; }  // merge into this real folder, held
                                else { NtClose(e); conflicts.Add(p); f.Skip = true; }
                            }
                            else
                            {
                                st = Open(P.H, TempName(), GENERIC_READ | GENERIC_WRITE | DELETE, CREATE, DIRECTORY_FILE, true, out f.H);
                                if (st < 0) throw new Stop("Can't write to " + (par == "" ? dst : par) + " (" + Why(st) + ")", 1);
                                f.New = true; f.Temp = true; f.Parent = P.H; f.Final = name;
                                temps.Add(f.H);
                            }
                        }
                        stack.Add(f);
                    }
                    else if (t == 'F')
                    {
                        long mode = Number(), mt = Number(), size = Number();
                        if (P.Skip) { Drain(size); continue; }
                        IntPtr h;
                        bool temp = !P.New;
                        int st = Open(P.H, temp ? TempName() : name, GENERIC_WRITE | GENERIC_READ | DELETE, CREATE, NON_DIRECTORY_FILE, true, out h);
                        if (st == NAME_COLLISION && !temp) throw new Stop(CaseClash(p), 1);
                        if (st < 0) throw new Stop("Can't create " + p + " (" + Why(st) + ")", 1);
                        if (temp) temps.Add(h);
                        try
                        {
                            WriteData(h, size, p);
                            SetTime(h, mt);
                            if (temp)
                            {
                                st = RenameTo(h, P.H, replace ? RealName(P.H, name) : name, replace);
                                if (st < 0) { (st == NAME_COLLISION || replace ? conflicts : errors).Add(p); Unlink(h); }
                                temps.Remove(h);
                            }
                        }
                        catch
                        {
                            // Cut off partway: remove the unfinished file through
                            // this handle while it is still open.
                            if (temps.Remove(h)) { try { Unlink(h); } catch { } }
                            throw;
                        }
                        finally { NtClose(h); }
                    }
                    else throw new Stop("bad record", 2);
                }
                while (stack.Count > 0) { Finish(stack[stack.Count - 1], replace); stack.RemoveAt(stack.Count - 1); }
            }
            catch
            {
                // Remove this copy's unfinished items, through their handles.
                foreach (IntPtr h in temps)
                {
                    try { if (Examine(h).Dir) Empty(h, 0); Unlink(h); } catch { }
                }
                throw;
            }
            NtClose(D);
            foreach (string c in conflicts) Rec("C", c);
            foreach (string e in errors) Rec("E", e);
            Put("Z");
            return 0;
        }

        // ----------------------------------------------------------- rename
        static int CmdRename()
        {
            string dir = Field(), old = Field(), neu = Field();
            IntPtr D = Top(dir), h;
            int st = Open(D, old, DELETE | READ_ATTRIBUTES, OPEN, 0, true, out h);
            if (st < 0) throw new Stop(old + ": " + Why(st), st == NOT_FOUND ? 4 : 1);
            st = RenameTo(h, D, neu, false);
            NtClose(h);
            if (st == NAME_COLLISION) throw new Stop("exists", 17);
            if (st < 0) throw new Stop(Why(st), 1);
            return 0;
        }

        // ----------------------------------------------------------- exists
        static int CmdExists()
        {
            string dir = Field(); long count = Number();
            IntPtr D = Top(dir);
            for (long k = 0; k < count; k++)
            {
                string n = Field(); IntPtr h;
                int st = Open(D, n, READ_ATTRIBUTES, OPEN, 0, true, out h);
                if (st >= 0) NtClose(h);
                Put(st >= 0 ? "1" : "0");
            }
            return 0;
        }

        // ----------------------------------------------------------- delete
        // Every path is checked before anything is deleted: not a drive, not
        // the profile folder or anything containing it, as written and as
        // the opened item really is (junctions in its parents resolved).
        static string Norm(string p) { return p.TrimEnd('\\').ToLowerInvariant(); }

        static int CmdDelete()
        {
            string home = Norm(Path.GetFullPath(Environment.GetEnvironmentVariable("USERPROFILE") ?? ""));
            if (home.Length < 4) throw new Stop("cannot resolve the home folder", 3);
            long count = Number();
            List<IntPtr[]> items = new List<IntPtr[]>();
            try
            {
                for (long k = 0; k < count; k++)
                {
                    string full = Path.GetFullPath(WinPath(Field())).TrimEnd('\\');
                    if (full.Length < 4 || (home + "\\").StartsWith(Norm(full) + "\\", StringComparison.Ordinal))
                        throw new Stop("Refusing to delete " + full, 5);
                    IntPtr P = Top(Path.GetDirectoryName(full)), h;
                    int st = Open(P, Path.GetFileName(full), DELETE | GENERIC_READ, OPEN, 0, true, out h);
                    if (st < 0) { NtClose(P); throw new Stop("Can't delete " + full + ": " + Why(st), 1); }
                    items.Add(new IntPtr[] { P, h });
                    StringBuilder sb = new StringBuilder(32768);
                    if (GetFinalPathNameByHandleW(h, sb, 32768, 0) == 0) throw new Stop("Can't resolve " + full, 1);
                    string real = sb.ToString();
                    if (real.StartsWith("\\\\?\\UNC\\")) real = "\\\\" + real.Substring(8);
                    else if (real.StartsWith("\\\\?\\")) real = real.Substring(4);
                    if (Norm(real).Length < 4 || (home + "\\").StartsWith(Norm(real) + "\\", StringComparison.Ordinal))
                        throw new Stop("Refusing to delete " + full + " (it is " + real + ")", 5);
                }
                foreach (IntPtr[] it in items)
                {
                    Info i = Examine(it[1]);
                    if (i.Dir && !i.Link) Empty(it[1], 0);
                    int st = Unlink(it[1]);
                    if (st < 0) throw new Stop("Can't delete: " + Why(st), 1);
                }
            }
            finally { foreach (IntPtr[] it in items) { NtClose(it[1]); NtClose(it[0]); } }
            return 0;
        }

        // ------------------------------------------------------------- main
        public static int Main(string[] args)
        {
            return Run(new Pipe(-10));
        }

        public static int Run(Stream stdin)
        {
            input = stdin;
            output = new BufferedStream(new Pipe(-11), 262144);
            try
            {
                string cmd = Field();
                int rc;
                if (cmd == "send") rc = CmdSend();
                else if (cmd == "recv") rc = CmdRecv();
                else if (cmd == "rename") rc = CmdRename();
                else if (cmd == "exists") rc = CmdExists();
                else if (cmd == "delete") rc = CmdDelete();
                else throw new Stop("unknown command", 2);
                output.Flush();
                return rc;
            }
            catch (Stop e) { try { output.Flush(); } catch { } Err(e.Message); return e.Code; }
            catch (Exception e) { try { output.Flush(); } catch { } Err(e.Message); return 1; }
        }

        static void Err(string m)
        {
            byte[] b = Encoding.UTF8.GetBytes(m + "\n");
            try { new Pipe(-12).Write(b, 0, b.Length); } catch { }
        }
    }
}
