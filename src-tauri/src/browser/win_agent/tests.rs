//! All-platform wire/cache/guard tests. Windows filesystem tests live separately.
use super::*;
use crate::browser::transport::{RemoteNode, RemoteReader, RemoteWriter};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;

fn proven() -> WinAgent {
    WinAgent::new(format!(
        r"C:\Users\alice\AppData\Local\TailFiles\agent-{SOURCE_HASH}.exe"
    ))
}

#[derive(Default)]
struct Wire {
    input: Vec<u8>,
    closed: bool,
    waits: usize,
}
struct MockChannel {
    wire: Arc<Mutex<Wire>>,
    out: Vec<u8>,
    offset: usize,
    rc: Option<u32>,
    err: String,
    hang_read: bool,
    fail_io: bool,
}
impl MockChannel {
    fn boxed(out: &[u8], rc: Option<u32>) -> (Box<dyn ExecChannel>, Arc<Mutex<Wire>>) {
        let wire = Arc::new(Mutex::new(Wire::default()));
        (
            Box::new(Self {
                wire: wire.clone(),
                out: out.to_vec(),
                offset: 0,
                rc,
                err: String::new(),
                hang_read: false,
                fail_io: false,
            }),
            wire,
        )
    }
}
#[async_trait]
impl ExecChannel for MockChannel {
    async fn write_all(&mut self, d: &[u8]) -> Result<(), BrowserError> {
        if self.fail_io {
            return Err(BrowserError::other("broken write pipe"));
        }
        self.wire.lock().unwrap().input.extend_from_slice(d);
        Ok(())
    }
    async fn read(&mut self, b: &mut [u8]) -> Result<usize, BrowserError> {
        assert!(
            self.wire.lock().unwrap().closed,
            "stdin EOF must precede reply reading"
        );
        if self.hang_read {
            std::future::pending::<()>().await;
        }
        let n = b.len().min(self.out.len() - self.offset);
        b[..n].copy_from_slice(&self.out[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
    async fn close_stdin(&mut self) -> Result<(), BrowserError> {
        self.wire.lock().unwrap().closed = true;
        if self.fail_io {
            return Err(BrowserError::other("broken EOF pipe"));
        }
        Ok(())
    }
    async fn wait_exit(&mut self, _: Duration) -> Option<u32> {
        let mut w = self.wire.lock().unwrap();
        assert!(w.closed);
        w.waits += 1;
        self.rc
    }
    fn stderr_tail(&self) -> String {
        if self.fail_io && self.wire.lock().unwrap().waits == 0 {
            return String::new();
        }
        self.err.clone()
    }
}

/// Every destination SFTP operation panics, except the explicit installer fixture.
/// This is shared by all-platform mock channels and the real Windows local child.
struct TestTransport {
    agent: Option<WinAgent>,
    exts: BTreeMap<String, String>,
    opens: AtomicUsize,
    commands: Mutex<Vec<String>>,
    wires: Mutex<Vec<Arc<Mutex<Wire>>>>,
    proof: Vec<u8>,
    installed: bool,
    compile_failures: AtomicUsize,
    source_paths: Mutex<HashSet<String>>,
    broken_pipes: bool,
    uploaded: Arc<Mutex<Vec<u8>>>,
    mutations: AtomicUsize,
    gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    hang: bool,
    #[cfg(windows)]
    local_exe: Option<String>,
}
impl Default for TestTransport {
    fn default() -> Self {
        Self {
            agent: None,
            exts: Default::default(),
            opens: AtomicUsize::new(0),
            commands: Mutex::new(Vec::new()),
            wires: Mutex::new(Vec::new()),
            proof: b"1".to_vec(),
            installed: true,
            compile_failures: AtomicUsize::new(0),
            source_paths: Mutex::default(),
            broken_pipes: false,
            uploaded: Arc::default(),
            mutations: AtomicUsize::new(0),
            gate: None,
            hang: false,
            #[cfg(windows)]
            local_exe: None,
        }
    }
}
impl TestTransport {
    fn agent() -> Self {
        Self {
            agent: Some(proven()),
            ..Self::default()
        }
    }
}
struct SourceWriter(Arc<Mutex<Vec<u8>>>);
#[async_trait]
impl RemoteWriter for SourceWriter {
    async fn write_all(&mut self, d: &[u8]) -> Result<(), BrowserError> {
        self.0.lock().unwrap().extend_from_slice(d);
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), BrowserError> {
        Ok(())
    }
    async fn close(self: Box<Self>) -> Result<(), BrowserError> {
        Ok(())
    }
}
#[async_trait]
impl SftpTransport for TestTransport {
    async fn realpath(&self, _: &str) -> Result<String, BrowserError> {
        panic!("destination SFTP realpath not expected")
    }
    async fn read_dir(&self, _: &str) -> Result<Vec<RemoteNode>, BrowserError> {
        panic!("destination SFTP read_dir not expected")
    }
    async fn stat(&self, _: &str) -> Result<Option<RemoteNode>, BrowserError> {
        panic!("destination SFTP stat not expected")
    }
    async fn lstat(&self, _: &str) -> Result<Option<RemoteNode>, BrowserError> {
        panic!("destination SFTP lstat not expected")
    }
    async fn mkdir(&self, p: &str, _: u32) -> Result<(), BrowserError> {
        assert!(!self.installed);
        assert_eq!(p, "/C:/Users/alice/AppData/Local/TailFiles");
        self.mutations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn remove_file(&self, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP remove_file forbidden")
    }
    async fn remove_dir(&self, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP remove_dir forbidden")
    }
    async fn rename(&self, _: &str, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP rename forbidden")
    }
    async fn posix_rename(&self, _: &str, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP posix rename forbidden")
    }
    async fn hardlink(&self, _: &str, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP hardlink forbidden")
    }
    async fn symlink(&self, _: &str, _: &str) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP symlink forbidden")
    }
    async fn read_link(&self, _: &str) -> Result<String, BrowserError> {
        panic!("destination SFTP readlink not expected")
    }
    async fn open_read(&self, _: &str) -> Result<Box<dyn RemoteReader>, BrowserError> {
        panic!("destination SFTP read not expected")
    }
    async fn create_exclusive(
        &self,
        p: &str,
        _: u32,
    ) -> Result<Box<dyn RemoteWriter>, BrowserError> {
        assert!(!self.installed);
        assert!(p.starts_with(&format!(
            "/C:/Users/alice/AppData/Local/TailFiles/agent-{SOURCE_HASH}"
        )));
        assert!(p.ends_with(".cs"));
        if !self.source_paths.lock().unwrap().insert(p.to_string()) {
            return Err(BrowserError::new(
                ErrorKind::Exists,
                "installer source already exists",
            ));
        }
        self.mutations.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(SourceWriter(self.uploaded.clone())))
    }
    async fn set_attrs(&self, _: &str, _: Option<u32>, _: Option<i64>) -> Result<(), BrowserError> {
        panic!("Windows destination SFTP attrs forbidden")
    }
    fn has_hardlink(&self) -> bool {
        true
    }
    fn has_posix_rename(&self) -> bool {
        true
    }
    fn extensions(&self) -> &BTreeMap<String, String> {
        &self.exts
    }
    fn win_agent(&self) -> Option<WinAgent> {
        self.agent.clone().filter(|a| a.proven())
    }
    async fn close(self: Box<Self>) -> Result<(), BrowserError> {
        Ok(())
    }
    async fn open_exec(&self, cmd: &str) -> Result<Box<dyn ExecChannel>, BrowserError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.commands.lock().unwrap().push(cmd.to_owned());
        #[cfg(windows)]
        if let Some(exe) = &self.local_exe {
            assert_eq!(cmd, format!("cmd /c \"{exe}\""));
            return windows::ChildChannel::spawn(exe).await;
        }
        if let Some((entered, release)) = &self.gate {
            entered.notify_one();
            release.notified().await;
        }
        tokio::task::yield_now().await;
        let mut rc = Some(0);
        let out = if cmd.starts_with("powershell ") {
            let script = decode_ps(cmd);
            if script.contains("GetFolderPath") {
                format!(
                    "C:\\Users\\alice\\AppData\\Local\r\n{}\r\n",
                    if self.installed { "True" } else { "False" }
                )
                .into_bytes()
            } else {
                assert!(script.contains("Add-Type"));
                if self
                    .compile_failures
                    .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    rc = Some(1);
                    b"compile refused".to_vec()
                } else {
                    b"installed\r\n".to_vec()
                }
            }
        } else {
            self.proof.clone()
        };
        let (mut ch, wire) = MockChannel::boxed(&out, rc);
        self.wires.lock().unwrap().push(wire);
        if self.hang {
            ch = Box::new(MockChannel {
                wire: Arc::new(Mutex::new(Wire::default())),
                out,
                offset: 0,
                rc: Some(0),
                err: String::new(),
                hang_read: true,
                fail_io: false,
            });
        }
        if self.broken_pipes {
            ch = Box::new(MockChannel {
                wire: self.wires.lock().unwrap().last().unwrap().clone(),
                out: Vec::new(),
                offset: 0,
                rc: Some(9009),
                err: "is not recognized as an internal or external command".to_string(),
                hang_read: false,
                fail_io: true,
            });
        }
        Ok(ch)
    }
}
fn decode_ps(cmd: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cmd.split_whitespace().last().unwrap())
        .unwrap();
    String::from_utf16(
        &bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn vendored_csharp_is_plugin_exact() {
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(AGENT_CS.as_bytes()));
    assert_eq!(
        hash,
        "cfa1c1af6b1617180b3297d5e0f69b8c6dc322ce85807aba1ab47099fdaa5133"
    );
    assert_eq!(&hash[..16], SOURCE_HASH);
    assert!(AGENT_CS.contains("public class Pipe : Stream"));
    assert!(!AGENT_CS.contains('\r'));
}
#[test]
fn fixed_loader_is_utf16le_and_installer_has_pid_publish() {
    let script = "$d = 'café'; '😀'";
    let cmd = win_ps(script);
    assert_eq!(decode_ps(&cmd), script);
    assert!(
        cmd.starts_with("powershell -NoProfile -NonInteractive -InputFormat None -EncodedCommand ")
    );
    let s = compile_script(r"C:\Users\alice\AppData\Local\TailFiles", "agent-test.cs");
    for marker in [
        "$PID",
        "Add-Type -OutputAssembly $tmp -OutputType ConsoleApplication",
        "Move-Item -LiteralPath $tmp",
        "Remove-Item -LiteralPath $tmp",
    ] {
        assert!(s.contains(marker));
    }
}
#[test]
fn windows_absolute_and_nested_names_refuse_devices_and_shell_paths() {
    for p in ["/C:/Users/alice", r"C:\Users\alice", "C:/"] {
        validate_absolute(p).unwrap();
    }
    for p in [
        "/tmp/x",
        "/C:/CON/x",
        "C:/safe/AUX.txt",
        r"C:\safe\..\x",
        "C:/safe//x",
        "C:/safe/x.",
        "C:/safe/x ",
    ] {
        assert!(validate_absolute(p).is_err(), "{p}");
    }
    for p in [
        "x/CON/a",
        "x/aux.txt",
        "../x",
        "x/./y",
        "x//y",
        "x/a:b",
        r"x/a\b",
        "x/COM1.ext",
    ] {
        assert!(validate_relative(p).is_err(), "{p}");
    }
    for s in ["%", "$", "`", "&", ";", "!", "(", ")", "\n", "\""] {
        assert!(shell_path(&format!("C:\\Users\\a{s}b\\AppData")).is_err());
    }
    assert!(validate_exe(r"C:\TailFiles\agent-wrong.exe").is_err());
}
#[test]
fn protocol_exact_no_perl_prefix_or_shell_data() {
    assert_eq!(
        fields("exists", &["C:/data", "2", "one", "two"]).unwrap(),
        b"exists\0C:/data\x002\0one\0two\0"
    );
    assert_eq!(
        fields(
            "recv",
            &["C:/data", ".tailfiles-incoming-0123456789abcdef", "keep"]
        )
        .unwrap(),
        b"recv\0C:/data\0.tailfiles-incoming-0123456789abcdef\0keep\0"
    );
    assert!(fields("recv", &["bad\0field"]).is_err());
}
#[test]
fn failure_taxonomy_and_shared_revocation() {
    for (rc, kind) in [
        (17, ErrorKind::Exists),
        (4, ErrorKind::Notfound),
        (3, ErrorKind::Denied),
        (5, ErrorKind::Denied),
        (2, ErrorKind::Other),
    ] {
        assert_eq!(classify_failure(Some(rc), "short input").kind, kind);
    }
    assert_eq!(classify_failure(None, "").kind, ErrorKind::Unreachable);
    let a = proven();
    let b = a.clone();
    assert_eq!(
        a.failure(
            Some(1),
            "is not recognized as an internal or external command"
        )
        .kind,
        ErrorKind::Unsupported
    );
    assert!(!b.proven());
    assert!(b.command().is_err());
}
#[tokio::test]
async fn existing_exe_needs_exact_known_answer_and_account_singleflight() {
    let cache = WinAgentCache::default();
    let t = TestTransport::default();
    let (a, b) = tokio::join!(
        cache.get_or_probe("h", "alice", &t),
        cache.get_or_probe("h", "alice", &t)
    );
    assert!(a.unwrap().proven());
    assert!(b.unwrap().proven());
    assert_eq!(t.opens.load(Ordering::SeqCst), 2);
    assert!(cache.get("h", "bob").is_none());
    assert!(cache.get("other", "alice").is_none());
    let wires = t.wires.lock().unwrap();
    assert_eq!(
        wires[1].lock().unwrap().input,
        fields(
            "exists",
            &[
                r"C:\Users\alice\AppData\Local\TailFiles",
                "1",
                &format!("agent-{SOURCE_HASH}.exe")
            ]
        )
        .unwrap()
    );
    drop(wires);
    let a = cache.get("h", "alice").unwrap();
    cache.invalidate_host("h");
    assert!(!a.proven());
    assert!(cache.get("h", "alice").is_none());
    assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn forged_existing_exe_and_installed_marker_never_grant_capability() {
    for proof in [b"".as_slice(), b"0", b"11", b"1\n", b"installed"] {
        let t = TestTransport {
            proof: proof.to_vec(),
            ..TestTransport::default()
        };
        assert!(WinAgentCache::default()
            .get_or_probe("h", "u", &t)
            .await
            .is_none());
        assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
    }
    let t = TestTransport {
        installed: false,
        proof: b"0".to_vec(),
        ..TestTransport::default()
    };
    assert!(WinAgentCache::default()
        .get_or_probe("h", "u", &t)
        .await
        .is_none());
    assert_eq!(*t.uploaded.lock().unwrap(), AGENT_CS.as_bytes());
    assert_eq!(t.mutations.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn installer_source_is_the_only_sftp_write_and_is_hash_bound() {
    let t = TestTransport {
        installed: false,
        ..TestTransport::default()
    };
    let a = WinAgentCache::default()
        .get_or_probe("h", "u", &t)
        .await
        .unwrap();
    assert!(a.proven());
    assert_eq!(t.opens.load(Ordering::SeqCst), 3);
    assert_eq!(*t.uploaded.lock().unwrap(), AGENT_CS.as_bytes());
    assert_eq!(t.mutations.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn invalidate_inflight_probe_does_not_resurrect_or_retarget() {
    let cache = Arc::new(WinAgentCache::default());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let t = Arc::new(TestTransport {
        gate: Some((entered.clone(), release.clone())),
        ..TestTransport::default()
    });
    let c = cache.clone();
    let tt = t.clone();
    let task = tokio::spawn(async move { c.get_or_probe("h", "alice", tt.as_ref()).await });
    entered.notified().await;
    cache.invalidate_host("h");
    release.notify_one();
    entered.notified().await;
    release.notify_one();
    assert!(task.await.unwrap().is_none());
    assert!(cache.get("h", "alice").is_none());
}
#[tokio::test]
async fn cache_is_bounded_and_missing_helper_requires_reproof() {
    let cache = WinAgentCache::default();
    let t = TestTransport::default();
    let a = cache.get_or_probe("h", "u", &t).await.unwrap();
    a.failure(Some(1), "not recognized");
    assert!(cache.get("h", "u").is_none());
    assert!(cache.get_or_probe("h", "u", &t).await.is_some());
    assert_eq!(t.opens.load(Ordering::SeqCst), 4);
    for i in 1..CACHE_CAP {
        cache.inner.lock().unwrap().insert(
            format!("{i}\0u"),
            CacheEntry {
                at: Instant::now(),
                active: Arc::new(AtomicBool::new(true)),
                cell: Arc::new(OnceCell::new()),
            },
        );
    }
    assert!(cache.get_or_probe("overflow", "u", &t).await.is_none());
    assert_eq!(cache.inner.lock().unwrap().len(), CACHE_CAP);
    assert_eq!(t.opens.load(Ordering::SeqCst), 4);
}
#[tokio::test]
async fn capture_caps_while_reading_and_times_out_before_wait_exit() {
    let t = TestTransport {
        proof: vec![b'x'; 8192],
        ..TestTransport::default()
    };
    let e = capture(&t, "agent", b"", Duration::from_millis(100), 16)
        .await
        .unwrap_err();
    assert!(e.message.contains("cap"));
    assert!(t.wires.lock().unwrap()[0].lock().unwrap().closed);
    let t = TestTransport {
        hang: true,
        ..TestTransport::default()
    };
    let e = capture(&t, "agent", b"", Duration::from_millis(20), 16)
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unreachable);
}
#[tokio::test]
async fn sink_validates_every_component_and_skips_links_without_l() {
    let (ch, w) = MockChannel::boxed(b"Z", Some(0));
    let mut sink = WinAgentSink::start(ch, proven(), "C:/data", "0123456789abcdef", true)
        .await
        .unwrap();
    let prefix = w.lock().unwrap().input.len();
    for p in ["x/CON/a", "x/AUX.txt", "x/../a", "x/a."] {
        assert!(sink.dir_begin(p, 0, 0).await.is_err());
        assert_eq!(w.lock().unwrap().input.len(), prefix);
    }
    assert!(!sink.symlink("safe-link", "whatever").await.unwrap());
    assert_eq!(w.lock().unwrap().input.len(), prefix);
    sink.dir_begin("folder", 0o755, 123).await.unwrap();
    let meta = FileMeta {
        size: 3,
        mode: 0o644,
        mtime: 123,
    };
    sink.file_begin("folder/a", &meta).await.unwrap();
    sink.file_chunk(b"abc").await.unwrap();
    sink.file_end(0, 0).await.unwrap();
    sink.dir_end().await.unwrap();
    sink.place(&[], true).await.unwrap();
    sink.remove().await.unwrap();
    let wire = w.lock().unwrap();
    let mut expected = fields(
        "recv",
        &["C:/data", ".tailfiles-incoming-0123456789abcdef", "replace"],
    )
    .unwrap();
    expected.extend(agent::d_record("folder", 0o755, 123).unwrap());
    expected.extend(agent::f_record("folder/a", 0o644, 123, 3).unwrap());
    expected.extend(b"abcZ");
    assert_eq!(wire.input, expected);
    assert!(wire.closed);
}
#[tokio::test]
async fn replace_merge_rejects_case_variants_before_second_header() {
    for (first, second) in [
        ("A", "a"),
        ("folder/A", "folder/a"),
        ("Folder/a", "folder/b"),
    ] {
        let (ch, w) = MockChannel::boxed(b"Z", Some(0));
        let mut s = WinAgentSink::start(ch, proven(), "C:/data", "0123456789abcdef", true)
            .await
            .unwrap();
        let meta = FileMeta {
            size: 0,
            mode: 0,
            mtime: 0,
        };
        s.file_begin(first, &meta).await.unwrap();
        s.file_end(0, 0).await.unwrap();
        let before = w.lock().unwrap().input.len();
        assert_eq!(
            s.file_begin(second, &meta).await.unwrap_err().kind,
            ErrorKind::Denied
        );
        assert_eq!(w.lock().unwrap().input.len(), before);
        s.remove().await.unwrap();
    }
}
#[tokio::test]
async fn w3_overflow_remote_child_is_refused_before_any_offending_header() {
    let child = "a".repeat(32_769);
    // Source-side names may be legal on a non-Windows server; destination
    // admission must independently reject before C#'s ushort length conversion.
    assert!(super::super::listing::safe_remote_name(&child));
    let rel = format!("tree/{child}");
    for directory in [false, true] {
        let (ch, wire) = MockChannel::boxed(b"Z", Some(0));
        let mut sink = WinAgentSink::start(ch, proven(), "C:/data", "0123456789abcdef", false)
            .await
            .unwrap();
        sink.dir_begin("tree", 0o755, 123).await.unwrap();
        let before = wire.lock().unwrap().input.clone();
        let result = if directory {
            sink.dir_begin(&rel, 0o755, 123).await
        } else {
            sink.file_begin(
                &rel,
                &FileMeta {
                    size: 0,
                    mode: 0o644,
                    mtime: 123,
                },
            )
            .await
        };
        assert_eq!(result.unwrap_err().kind, ErrorKind::Denied);
        assert_eq!(
            wire.lock().unwrap().input,
            before,
            "offending header emitted"
        );
        assert!(sink.symlink(&rel, "target").await.is_err());
        sink.remove().await.unwrap();
    }
}

#[tokio::test]
async fn w3_overlong_components_are_refused_by_absolute_paths_and_every_verb() {
    for name in ["a".repeat(256), "😀".repeat(128), "a".repeat(32_769)] {
        let t = TestTransport::agent();
        let path = format!("C:/data/{name}");
        assert!(validate_absolute(&path).is_err());
        assert!(WinAgentSink::open(&t, &path, "0123456789abcdef", false)
            .await
            .is_err());
        assert!(exists(&t, "C:/data", std::slice::from_ref(&name))
            .await
            .is_err());
        assert!(rename(&t, "C:/data", &name, "ok").await.is_err());
        assert!(rename(&t, "C:/data", "ok", &name).await.is_err());
        assert!(delete(&t, &[path]).await.is_err());
        assert!(mkdir(&t, "C:/data", &name).await.is_err());
        assert_eq!(t.opens.load(Ordering::SeqCst), 0);
        assert!(t.wires.lock().unwrap().is_empty());
        assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
    }
    for name in ["a".repeat(255), format!("{}a", "😀".repeat(127))] {
        validate_absolute(&format!("C:/{name}")).unwrap();
        validate_relative(&format!("tree/{name}")).unwrap();
    }
}

#[tokio::test]
async fn short_and_oversize_payloads_fail_and_cleanup_is_eof_only() {
    let (ch, w) = MockChannel::boxed(b"", Some(2));
    let mut s = WinAgentSink::start(ch, proven(), "C:/data", "0123456789abcdef", false)
        .await
        .unwrap();
    s.file_begin(
        "a",
        &FileMeta {
            size: 4,
            mode: 0,
            mtime: 0,
        },
    )
    .await
    .unwrap();
    assert!(s.file_chunk(b"12345").await.is_err());
    s.file_chunk(b"1").await.unwrap();
    assert!(s.file_end(0, 0).await.is_err());
    assert!(s.place(&[], false).await.is_err());
    s.remove().await.unwrap();
    assert!(w.lock().unwrap().closed);
    assert!(!w.lock().unwrap().input.ends_with(b"Z"));
}
#[tokio::test]
async fn unproven_windows_writes_never_open_exec_or_mutate_sftp() {
    let t = TestTransport::default();
    assert!(WinAgentSink::open(&t, "C:/data", "0123456789abcdef", false)
        .await
        .is_err());
    assert!(mkdir(&t, "C:/data", "x").await.is_err());
    assert!(rename(&t, "C:/data", "x", "y").await.is_err());
    assert!(delete(&t, &["C:/data/x".into()]).await.is_err());
    assert!(exists(&t, "C:/data", &["x".into()]).await.is_err());
    assert_eq!(t.opens.load(Ordering::SeqCst), 0);
    assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn w1_mkdir_command_adapter_keeps_drive_root_absolute() {
    for parent in ["/C:/", "C:/", "/C:/nested/", "C:/nested/"] {
        let t = TestTransport {
            proof: b"Z".to_vec(),
            ..TestTransport::agent()
        };
        let path = format!("{parent}folder");
        super::super::commands::mkdir_windows_remote(&t, &path, "folder")
            .await
            .unwrap_or_else(|e| panic!("mkdir {path}: {e}"));
        let wire = t.wires.lock().unwrap()[0].lock().unwrap().input.clone();
        let expected_parent = if parent.ends_with("nested/") {
            parent.trim_end_matches('/')
        } else {
            parent
        };
        assert!(wire.starts_with(format!("recv\0{expected_parent}\0").as_bytes()));
        assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn w1_rename_command_adapter_keeps_drive_root_absolute() {
    for parent in ["/C:/", "C:/", "/C:/nested/", "C:/nested/"] {
        let t = TestTransport::agent();
        let old = format!("{parent}old");
        let new = format!("{parent}new");
        super::super::commands::rename_windows_remote(&t, &old, &new, "old", "new")
            .await
            .unwrap_or_else(|e| panic!("rename {old}: {e}"));
        let wire = t.wires.lock().unwrap()[0].lock().unwrap().input.clone();
        let expected_parent = if parent.ends_with("nested/") {
            parent.trim_end_matches('/')
        } else {
            parent
        };
        assert_eq!(
            wire,
            format!("rename\0{expected_parent}\0old\0new\0").as_bytes()
        );
        assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
    }
    assert!(validate_absolute("/C:").is_err());
    assert!(validate_absolute("C:").is_err());
}

#[tokio::test]
async fn proven_windows_mutations_use_only_native_verbs_and_mkdir_d_z() {
    let t = TestTransport {
        proof: b"Z".to_vec(),
        ..TestTransport::agent()
    };
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    mkdir(&t, "C:/data", "folder").await.unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let input = t.wires.lock().unwrap()[0].lock().unwrap().input.clone();
    assert!(input.starts_with(b"recv\0C:/data\0.tailfiles-incoming-"));
    let parts: Vec<_> = input.split(|b| *b == 0).collect();
    assert_eq!(parts.len(), 8);
    assert_eq!(parts[7], b"Z");
    assert_eq!(parts[3], b"keep");
    assert_eq!(parts[4], b"Dfolder");
    assert_eq!(parts[5], b"493");
    let tail = std::str::from_utf8(parts[6]).unwrap();
    let mtime: u64 = tail.parse().unwrap();
    assert!(
        (before..=after).contains(&mtime),
        "mkdir mtime {mtime} outside [{before}, {after}]"
    );
    rename(&t, "C:/data", "a", "b").await.unwrap();
    delete(&t, &["C:/data/x".into()]).await.unwrap();
    {
        let wires = t.wires.lock().unwrap();
        assert_eq!(wires[1].lock().unwrap().input, b"rename\0C:/data\0a\0b\0");
        assert_eq!(wires[2].lock().unwrap().input, b"delete\x001\0C:/data/x\0");
    }
    assert_eq!(t.mutations.load(Ordering::SeqCst), 0);
    let t = TestTransport::agent();
    assert_eq!(
        exists(&t, "C:/data", &["x".into()]).await.unwrap(),
        vec!["x"]
    );
}
#[test]
fn probe_wire_defaults_false_and_preflight_requires_matching_capability() {
    let p = super::super::probe::build_probe_info(
        "/C:/Users/alice".into(),
        "windows".into(),
        super::super::ServerFlavor::OpensshWindows,
        "alice".into(),
        BTreeMap::new(),
    );
    let mut v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["winAgent"], false);
    v.as_object_mut().unwrap().remove("winAgent");
    assert!(
        !serde_json::from_value::<super::super::ProbeInfo>(v)
            .unwrap()
            .win_agent
    );
    let mut req = super::super::CopyRequest {
        src_target: "local".into(),
        src_user: None,
        src_dir: "/src".into(),
        dst_target: "h".into(),
        dst_user: Some("alice".into()),
        dst_dir: "C:/data".into(),
        names: vec!["a".into()],
        replace: false,
    };
    assert_eq!(
        super::super::transfer::preflight_with_cap(&req, true, false)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    super::super::transfer::preflight_with_cap(&req, true, true).unwrap();
    req.names = vec!["CON".into()];
    assert_eq!(
        super::super::transfer::preflight_with_cap(&req, true, true)
            .unwrap_err()
            .kind,
        ErrorKind::Denied
    );
    req.names = vec!["a".into()];
    req.src_target = "h".into();
    req.src_user = Some("bob".into());
    assert_eq!(
        super::super::transfer::preflight_with_cap(&req, true, true)
            .unwrap_err()
            .kind,
        ErrorKind::Account
    );
}

#[tokio::test]
async fn sink_parses_conflicts_errors_and_final_z_with_bounded_exit() {
    let (ch, wire) = MockChannel::boxed(b"Cexists\0Ebad\0Z", Some(0));
    let mut sink = WinAgentSink::start(ch, proven(), "C:/data", "0123456789abcdef", false)
        .await
        .unwrap();
    let (conflicts, errors) = sink.place(&[], false).await.unwrap();
    assert_eq!(conflicts, vec!["exists"]);
    assert_eq!(
        errors,
        vec![(
            "bad".to_string(),
            "Windows helper placement failed".to_string()
        )]
    );
    sink.remove().await.unwrap();
    assert!(wire.lock().unwrap().closed);
    assert_eq!(wire.lock().unwrap().waits, 1);
}

#[tokio::test]
async fn failed_install_source_cannot_poison_next_attempt() {
    let t = TestTransport {
        installed: false,
        compile_failures: AtomicUsize::new(1),
        ..TestTransport::default()
    };
    assert!(install_and_prove(&t).await.is_err());
    // Preserve the failed attempt's source, just as an interrupted remote
    // compile may. The next attempt must not overwrite or unlink that name.
    let prior = t.source_paths.lock().unwrap().clone();
    assert_eq!(prior.len(), 1);
    assert!(install_and_prove(&t).await.unwrap().proven());
    let paths = t.source_paths.lock().unwrap();
    assert_eq!(paths.len(), 2);
    assert!(prior.is_subset(&paths));
}

#[tokio::test]
async fn missing_helper_revokes_proof_even_when_write_and_eof_both_fail() {
    let a = proven();
    let t = TestTransport {
        agent: Some(a.clone()),
        broken_pipes: true,
        ..TestTransport::default()
    };
    assert!(WinAgentSink::open(&t, "C:/data", "0123456789abcdef", false)
        .await
        .is_err());
    assert!(
        !a.proven(),
        "EOF failure must not bypass missing-helper diagnostics"
    );
    assert!(t.wires.lock().unwrap()[0].lock().unwrap().waits > 0);

    let a = proven();
    let t = TestTransport {
        agent: Some(a.clone()),
        broken_pipes: true,
        ..TestTransport::default()
    };
    assert!(rename(&t, "C:/data", "a", "b").await.is_err());
    assert!(
        !a.proven(),
        "verb EOF failure must still consume exit diagnostics"
    );
}

#[tokio::test]
async fn proven_windows_engine_streams_remote_source_only_through_csharp_sink() {
    use crate::browser::fake::{temp_root, FakeConfig, FakeServer};
    use crate::browser::transfer::{run_copy, Side};
    let root = temp_root("m3-engine");
    std::fs::create_dir_all(root.join("source/nested")).unwrap();
    std::fs::write(root.join("source/nested/a"), b"actual source bytes").unwrap();
    let srv = FakeServer::start(FakeConfig::openssh(root)).await;
    let src = srv.connect(&srv.root.join("trust")).await;
    let dst = TestTransport {
        proof: b"Z".to_vec(),
        ..TestTransport::agent()
    };
    let req = crate::browser::CopyRequest {
        src_target: "source".into(),
        src_user: Some("fakeuser".into()),
        src_dir: "/source".into(),
        dst_target: "windows".into(),
        dst_user: Some("alice".into()),
        dst_dir: "C:/data".into(),
        names: vec!["nested".into()],
        replace: false,
    };
    let outcome = run_copy(
        &req,
        &Side::Remote {
            transport: &src,
            windows: false,
        },
        &Side::Remote {
            transport: &dst,
            windows: true,
        },
        tokio_util::sync::CancellationToken::new(),
        Arc::new(|_, _, _| {}),
    )
    .await
    .unwrap();
    assert_eq!(outcome.bytes, b"actual source bytes".len() as u64);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(dst.opens.load(Ordering::SeqCst), 1);
    assert_eq!(dst.mutations.load(Ordering::SeqCst), 0);
    let input = dst.wires.lock().unwrap()[0].lock().unwrap().input.clone();
    assert!(input.starts_with(b"recv\0C:/data\0.tailfiles-incoming-"));
    assert!(input
        .windows(b"Fnested/a\0".len())
        .any(|w| w == b"Fnested/a\0"));
    assert!(input.ends_with(b"actual source bytesZ"));
    Box::new(src).close().await.unwrap();
}

#[cfg(windows)]
mod windows;
