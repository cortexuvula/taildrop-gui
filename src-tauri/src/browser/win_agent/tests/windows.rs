//! REAL Windows-only C# execution. Missing PowerShell 5.1/.NET Framework csc
//! is a hard failure, never a skip. Uses production WinAgentSink and exact NUL.
use super::*;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub(super) struct ChildChannel {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: Arc<Mutex<Vec<u8>>>,
    drain: Option<tokio::task::JoinHandle<()>>,
    exit: Option<u32>,
}
impl ChildChannel {
    pub(super) async fn spawn(exe: &str) -> Result<Box<dyn ExecChannel>, BrowserError> {
        // Exercise the actual cmd /c launcher too, not just the exe directly.
        let mut command = Command::new("cmd.exe");
        command.arg("/c").arg(exe);
        Self::spawn_command(command).await
    }
    async fn spawn_command(mut command: Command) -> Result<Box<dyn ExecChannel>, BrowserError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        let mut err = child.stderr.take().unwrap();
        let drain = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let n = err.read(&mut buf).await.expect("child stderr read");
                if n == 0 {
                    break;
                }
                let mut out = sink.lock().unwrap();
                let room = agent::STDERR_CAP.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]); /* keep draining even at cap */
            }
        });
        Ok(Box::new(Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().unwrap(),
            child,
            stderr,
            drain: Some(drain),
            exit: None,
        }))
    }
}
#[async_trait]
impl ExecChannel for ChildChannel {
    async fn write_all(&mut self, d: &[u8]) -> Result<(), BrowserError> {
        let input = self.stdin.as_mut().expect("stdin open");
        input.write_all(d).await?;
        input.flush().await?;
        Ok(())
    }
    async fn read(&mut self, b: &mut [u8]) -> Result<usize, BrowserError> {
        Ok(self.stdout.read(b).await?)
    }
    async fn close_stdin(&mut self) -> Result<(), BrowserError> {
        self.stdin = None;
        Ok(())
    }
    async fn wait_exit(&mut self, budget: Duration) -> Option<u32> {
        if self.exit.is_some() {
            return self.exit;
        }
        self.exit = tokio::time::timeout(budget, self.child.wait())
            .await
            .ok()?
            .ok()?
            .code()
            .map(|c| c as u32);
        if let Some(mut drain) = self.drain.take() {
            if tokio::time::timeout(Duration::from_secs(2), &mut drain)
                .await
                .is_err()
            {
                drain.abort();
                panic!("stderr drain did not finish");
            }
        }
        self.exit
    }
    fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }
}
// A test panic must not leave a started spawn_blocking reader alive during
// Tokio runtime shutdown. Also give the reader its own finite deadline.
struct StopReader(Arc<AtomicBool>);
impl Drop for StopReader {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Fixture {
    root: PathBuf,
    t: TestTransport,
    dst: PathBuf,
}
impl Fixture {
    async fn new(label: &str) -> Self {
        let root = crate::browser::fake::temp_root(label);
        let folder = root.join("TailFiles");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(format!("agent-{SOURCE_HASH}.cs")), AGENT_CS).unwrap();
        let script=format!("$ErrorActionPreference='Stop'\nif ($PSVersionTable.PSVersion.Major -ne 5) {{ throw 'Windows PowerShell 5.1 required' }}\n$csc=Join-Path $env:WINDIR 'Microsoft.NET\\Framework64\\v4.0.30319\\csc.exe'\nif (!(Test-Path -LiteralPath $csc)) {{ throw '.NET Framework 4 csc required' }}\n{}",compile_script(&folder.to_string_lossy(), &format!("agent-{SOURCE_HASH}.cs")));
        let command = win_ps(&script);
        let args: Vec<_> = command.split_whitespace().skip(1).collect();
        let output = tokio::time::timeout(
            INSTALL_WAIT,
            Command::new("powershell.exe")
                .args(args)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("PowerShell compile exceeded 180s")
        .expect("Windows PowerShell 5.1 missing: test must fail, not skip");
        assert!(
            output.status.success(),
            "PowerShell/.NET csc compile failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let exe = folder
            .join(format!("agent-{SOURCE_HASH}.exe"))
            .to_string_lossy()
            .into_owned();
        validate_exe(&exe).unwrap();
        assert!(Path::new(&exe).is_file());
        let t = TestTransport {
            agent: Some(WinAgent::new(exe.clone())),
            local_exe: Some(exe),
            ..TestTransport::default()
        };
        // Even the loopback binary must prove exact exists(dir,count,basename), rc0.
        let (out, rc, err) = capture(
            &t,
            &t.agent.as_ref().unwrap().command().unwrap(),
            &fields(
                "exists",
                &[
                    &folder.to_string_lossy(),
                    "1",
                    &format!("agent-{SOURCE_HASH}.exe"),
                ],
            )
            .unwrap(),
            IO_WAIT,
            16,
        )
        .await
        .unwrap();
        assert_eq!(rc, Some(0), "{err}");
        assert_eq!(out, b"1");
        let dst = root.join("destination");
        std::fs::create_dir(&dst).unwrap();
        Self { root, t, dst }
    }
    fn dir(&self) -> String {
        self.dst.to_string_lossy().into_owned()
    }
    async fn sink(&self, tag: &str, replace: bool) -> WinAgentSink {
        WinAgentSink::open(&self.t, &self.dir(), tag, replace)
            .await
            .unwrap()
    }
    fn assert_zero_sftp(&self) {
        assert_eq!(self.t.mutations.load(Ordering::SeqCst), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).expect("Windows fixture cleanup");
    }
}
async fn put(s: &mut WinAgentSink, name: &str, data: &[u8], mode: u32, mtime: i64) {
    s.file_begin(
        name,
        &FileMeta {
            size: data.len() as u64,
            mode,
            mtime,
        },
    )
    .await
    .unwrap();
    for chunk in data.chunks(64 * 1024) {
        s.file_chunk(chunk).await.unwrap();
    }
    s.file_end(mode, mtime).await.unwrap();
}
async fn placed(s: &mut WinAgentSink) {
    let (c, e) = s.place(&[], true).await.unwrap();
    assert!(c.is_empty(), "{c:?}");
    assert!(e.is_empty(), "{e:?}");
    s.remove().await.unwrap();
}
async fn wait_for_file(p: &Path, min_size: u64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::metadata(p).is_ok_and(|m| m.len() >= min_size) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("helper did not reach partial-file barrier");
}

#[tokio::test]
async fn csharp_loader_compile_and_known_answer_under_both_login_shells() {
    let f = Fixture::new("m3 shell paths").await;
    let folder = f.root.join("TailFiles");
    let agent = f.t.agent.as_ref().unwrap();
    for powershell_login in [false, true] {
        let source_name = format!(
            "agent-{SOURCE_HASH}.{}.cs",
            super::super::super::transfer::new_stage_tag()
        );
        let source = folder.join(&source_name);
        std::fs::write(&source, AGENT_CS).unwrap();
        let install = win_ps(&compile_script(&folder.to_string_lossy(), &source_name));
        let mut login = if powershell_login {
            let mut c = Command::new("powershell.exe");
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-InputFormat",
                "None",
                "-Command",
            ]);
            c
        } else {
            let mut c = Command::new("cmd.exe");
            c.arg("/c");
            c
        };
        let output = tokio::time::timeout(
            INSTALL_WAIT,
            login
                .arg(&install)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("login-shell install deadline")
        .expect("login shell required");
        assert!(
            output.status.success(),
            "shell={powershell_login}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!source.exists(), "compile finally must remove its source");

        let mut login = if powershell_login {
            let mut c = Command::new("powershell.exe");
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-InputFormat",
                "None",
                "-Command",
            ]);
            c.arg(agent.command().unwrap());
            c
        } else {
            let mut c = Command::new("cmd.exe");
            c.arg("/c").arg(&agent.exe);
            c
        };
        // Stdio is assigned by spawn_command, exactly as in the other tests.
        login.kill_on_drop(true);
        let mut ch = ChildChannel::spawn_command(login).await.unwrap();
        let payload = fields(
            "exists",
            &[
                &folder.to_string_lossy(),
                "1",
                &format!("agent-{SOURCE_HASH}.exe"),
            ],
        )
        .unwrap();
        tokio::time::timeout(IO_WAIT, async {
            ch.write_all(&payload).await.unwrap();
            ch.close_stdin().await.unwrap();
            let mut out = Vec::new();
            let mut buf = [0; 64];
            loop {
                let n = ch.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                assert!(out.len() <= 64, "native stdout cap");
            }
            assert_eq!(ch.wait_exit(IO_WAIT).await, Some(0), "{}", ch.stderr_tail());
            assert_eq!(
                out, b"1",
                "shell={powershell_login} must preserve the binary record stream"
            );
        })
        .await
        .expect("login-shell helper protocol deadline");
    }
    f.assert_zero_sftp();
}

#[tokio::test]
async fn csharp_tree_mtime_modes_ignored_and_client_con_refusal() {
    let f = Fixture::new("m3-tree").await;
    let mut s = f.sink("0000000000000001", false).await;
    s.dir_begin("tree", 0, 1_700_000_000).await.unwrap();
    put(&mut s, "tree/a", b"payload", 0, 1_700_000_001).await;
    s.dir_end().await.unwrap();
    assert!(!s.symlink("link", "tree/a").await.unwrap());
    assert_eq!(
        s.file_begin(
            "tree/CON",
            &FileMeta {
                size: 0,
                mode: 0,
                mtime: 0
            }
        )
        .await
        .unwrap_err()
        .kind,
        ErrorKind::Denied
    );
    placed(&mut s).await;
    assert_eq!(std::fs::read(f.dst.join("tree/a")).unwrap(), b"payload");
    assert!(!f.dst.join("link").exists());
    let m = std::fs::metadata(f.dst.join("tree/a")).unwrap();
    assert!(
        !m.permissions().readonly(),
        "receive ignores POSIX mode=0; inherited Windows ACL permits write"
    );
    assert_eq!(
        m.modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_700_000_001
    );
    let mt = std::fs::metadata(f.dst.join("tree"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(mt, 1_700_000_000);
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_toctou_keep_and_atomic_replace_preserve_realname() {
    let f = Fixture::new("m3-place").await;
    let mut s = f.sink("0000000000000002", false).await;
    s.file_begin(
        "race",
        &FileMeta {
            size: 3,
            mode: 0,
            mtime: 1,
        },
    )
    .await
    .unwrap();
    // Hold the payload until the agent has opened its exclusive temp.
    // This plants the competing winner after native receive has started.
    wait_for_file(&f.dst.join(".tailfiles-incoming-0000000000000002-1"), 0).await;
    assert!(!f.dst.join("race").exists());
    std::fs::write(f.dst.join("race"), b"winner").unwrap();
    s.file_chunk(b"new").await.unwrap();
    s.file_end(0, 1).await.unwrap();
    let (c, e) = s.place(&[], false).await.unwrap();
    assert_eq!(c, vec!["race"]);
    assert!(e.is_empty());
    s.remove().await.unwrap();
    assert_eq!(std::fs::read(f.dst.join("race")).unwrap(), b"winner");
    let old = vec![b'o'; 1024 * 1024];
    let new = vec![b'n'; 1024 * 1024];
    std::fs::write(f.dst.join("Report.TXT"), &old).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_unwind = StopReader(stop.clone());
    let ready = Arc::new(tokio::sync::Notify::new());
    let old_seen = Arc::new(AtomicUsize::new(0));
    let new_seen = Arc::new(AtomicUsize::new(0));
    let new_ready = Arc::new(tokio::sync::Notify::new());
    let path = f.dst.join("Report.TXT");
    let st = stop.clone();
    let rd = ready.clone();
    let olds = old_seen.clone();
    let news = new_seen.clone();
    let nr = new_ready.clone();
    let reader = tokio::task::spawn_blocking(move || {
        let initial = std::fs::read(&path).expect("initial atomic-replace reader");
        assert_eq!(initial, vec![b'o'; 1024 * 1024]);
        olds.fetch_add(1, Ordering::SeqCst);
        rd.notify_one();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !st.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "atomic-replace reader deadline"
            );
            let data =
                std::fs::read(&path).expect("atomic replacement must never expose a missing path");
            assert!(
                data == vec![b'o'; 1024 * 1024] || data == vec![b'n'; 1024 * 1024],
                "atomic replacement exposed partial data"
            );
            if data[0] == b'n' {
                news.fetch_add(1, Ordering::SeqCst);
                nr.notify_one();
            } else {
                olds.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(10), ready.notified())
        .await
        .expect("atomic-replace reader readiness deadline");
    let mut s = f.sink("0000000000000003", true).await;
    put(&mut s, "report.txt", &new, 0o644, 1).await;
    placed(&mut s).await;
    // Require the concurrent reader to observe the complete replacement too.
    tokio::time::timeout(Duration::from_secs(10), new_ready.notified())
        .await
        .expect("atomic-replace reader never observed new contents");
    stop.store(true, Ordering::SeqCst);
    reader.await.unwrap();
    assert!(old_seen.load(Ordering::SeqCst) > 0);
    assert!(new_seen.load(Ordering::SeqCst) > 0);
    assert_eq!(std::fs::read(f.dst.join("Report.TXT")).unwrap(), new);
    let names: Vec<_> = std::fs::read_dir(&f.dst)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"Report.TXT".to_owned()));
    assert!(!names.contains(&"report.txt".to_owned()));
    f.assert_zero_sftp();
}

// ---------------------------------------------------- busy-file replacement
// Owner ruling 2026-11-22: preserve successful atomic Replace while readers
// keep the old destination open under the normal reader share
// READ|WRITE|DELETE. The active successor requests FileRenameInformationEx
// with REPLACE_IF_EXISTS|POSIX_SEMANTICS. The legacy upstream bytes stay
// callable ONLY through this native fixture's reference-source seam, so the
// held-open regression is RED under the original payload and GREEN under the
// successor — never a lucky gap in a reopen loop.

/// Compile an arbitrary agent source into TailFiles as agent-<hash16>.exe.
/// Test seam only: production always installs the pinned SOURCE_HASH and
/// never falls back to another payload.
async fn compile_reference(folder: &Path, source: &str, hash16: &str) -> PathBuf {
    let source_name = format!(
        "agent-{hash16}.{}.cs",
        super::super::super::transfer::new_stage_tag()
    );
    std::fs::write(folder.join(&source_name), source).unwrap();
    let script = format!(
        "$ErrorActionPreference='Stop'\nif ($PSVersionTable.PSVersion.Major -ne 5) {{ throw 'Windows PowerShell 5.1 required' }}\n$csc=Join-Path $env:WINDIR 'Microsoft.NET\\Framework64\\v4.0.30319\\csc.exe'\nif (!(Test-Path -LiteralPath $csc)) {{ throw '.NET Framework 4 csc required' }}\n{}",
        compile_script_for(&folder.to_string_lossy(), &source_name, hash16)
    );
    let command = win_ps(&script);
    let args: Vec<_> = command.split_whitespace().skip(1).collect();
    let output = tokio::time::timeout(
        INSTALL_WAIT,
        Command::new("powershell.exe")
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("PowerShell compile exceeded 180s")
    .expect("Windows PowerShell 5.1 missing: test must fail, not skip");
    assert!(
        output.status.success(),
        "PowerShell/.NET csc compile failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = folder.join(format!("agent-{hash16}.exe"));
    assert!(exe.is_file());
    assert!(
        !folder.join(&source_name).exists(),
        "compile finally must remove its source"
    );
    exe
}

/// Drive one replacing recv against an already-compiled agent exe with the
/// exact production wire encoding (fields/f_record), bypassing the WinAgent
/// proof layer: only this fixture may address a non-production payload.
async fn raw_recv_replace(
    exe: &str,
    dir: &str,
    tag: &str,
    name: &str,
    data: &[u8],
) -> (Vec<String>, Vec<String>, Option<u32>) {
    let mut ch = ChildChannel::spawn(exe).await.unwrap();
    let mut input = fields("recv", &[dir, &stage_dir_name(tag), "replace"]).unwrap();
    input.extend_from_slice(&agent::f_record(name, 0o644, 1, data.len() as u64).unwrap());
    input.extend_from_slice(data);
    input.push(b'Z');
    ch.write_all(&input).await.unwrap();
    ch.close_stdin().await.unwrap();
    let mut reader = ReplyReader::new();
    let mut conflicts = Vec::new();
    let mut errors = Vec::new();
    let mut done = false;
    while let Some(reply) = tokio::time::timeout(PLACE_WAIT, reader.next(ch.as_mut()))
        .await
        .expect("raw recv reply deadline")
        .unwrap()
    {
        match reply {
            AgentReply::Conflict(p) => conflicts.push(p),
            AgentReply::Error(p) => errors.push(p),
            AgentReply::Done => {
                done = true;
                break;
            }
        }
    }
    let rc = ch.wait_exit(PLACE_WAIT).await;
    assert!(
        done,
        "helper must finish its reply stream: {}",
        ch.stderr_tail()
    );
    (conflicts, errors, rc)
}

fn incoming_temps(dst: &Path, tag: &str) -> Vec<String> {
    std::fs::read_dir(dst)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!(".tailfiles-incoming-{tag}")))
        .collect()
}

#[tokio::test]
async fn csharp_busy_destination_replacement_succeeds_under_held_reader() {
    let f = Fixture::new("m3-busy").await;
    let old = vec![b'o'; 64 * 1024];
    let new = vec![b'n'; 64 * 1024];
    std::fs::write(f.dst.join("Report.TXT"), &old).unwrap();
    std::fs::write(f.dst.join("sentinel"), b"keep").unwrap();
    // A normal reader: Rust std opens READ|WRITE|DELETE sharing, and the one
    // handle stays open across the whole replacement — no reopen loop whose
    // gaps a rename could slip through.
    let mut held = std::fs::File::open(f.dst.join("Report.TXT")).unwrap();
    let mut seen = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut seen).unwrap();
    assert_eq!(seen, old);
    let tag = "0000000000000008";
    let mut s = f.sink(tag, true).await;
    put(&mut s, "report.txt", &new, 0o644, 1).await;
    placed(&mut s).await; // the successor must place while the destination is held
                          // The held handle keeps reading the complete OLD bytes (POSIX semantics).
    std::io::Seek::seek(&mut held, std::io::SeekFrom::Start(0)).unwrap();
    let mut after = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut after).unwrap();
    assert_eq!(after, old);
    // Subsequent opens read the complete NEW bytes, at the retained spelling.
    assert_eq!(std::fs::read(f.dst.join("Report.TXT")).unwrap(), new);
    let names: Vec<_> = std::fs::read_dir(&f.dst)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"Report.TXT".to_owned()));
    assert!(!names.contains(&"report.txt".to_owned()));
    assert!(incoming_temps(&f.dst, tag).is_empty());
    assert_eq!(std::fs::read(f.dst.join("sentinel")).unwrap(), b"keep");
    f.assert_zero_sftp();
}

#[tokio::test]
async fn legacy_upstream_busy_destination_replacement_fails_closed_red_witness() {
    let f = Fixture::new("m3-legacy-red").await;
    let legacy = compile_reference(
        &f.root.join("TailFiles"),
        UPSTREAM_AGENT_CS,
        UPSTREAM_SOURCE_HASH,
    )
    .await;
    let old = vec![b'o'; 64 * 1024];
    let new = vec![b'n'; 64 * 1024];
    std::fs::write(f.dst.join("Report.TXT"), &old).unwrap();
    let mut held = std::fs::File::open(f.dst.join("Report.TXT")).unwrap();
    let mut seen = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut seen).unwrap();
    assert_eq!(seen, old);
    let tag = "0000000000000009";
    let (conflicts, errors, rc) =
        raw_recv_replace(&legacy.to_string_lossy(), &f.dir(), tag, "Report.TXT", &new).await;
    // Legacy FileRenameInformation cannot replace a held-open destination
    // even under full sharing: the helper reports its conflict, exits
    // cleanly, removes only its staged temp, and the old destination and the
    // held reader's bytes are intact. That failure is this RED witness.
    assert_eq!(conflicts, vec!["Report.TXT".to_owned()]);
    assert!(errors.is_empty());
    assert_eq!(rc, Some(0));
    assert_eq!(std::fs::read(f.dst.join("Report.TXT")).unwrap(), old);
    std::io::Seek::seek(&mut held, std::io::SeekFrom::Start(0)).unwrap();
    let mut after = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut after).unwrap();
    assert_eq!(after, old);
    assert!(incoming_temps(&f.dst, tag).is_empty());
    f.assert_zero_sftp();
}

#[tokio::test]
async fn csharp_busy_destination_without_delete_share_fails_closed() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new("m3-busy-noshare").await;
    let old = vec![b'o'; 4096];
    let new = vec![b'n'; 4096];
    std::fs::write(f.dst.join("Report.TXT"), &old).unwrap();
    // READ|WRITE sharing only: the holder denies delete sharing, so even the
    // POSIX-semantics successor must fail closed — never bypass a
    // restrictive handle.
    let mut held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(f.dst.join("Report.TXT"))
        .unwrap();
    let mut seen = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut seen).unwrap();
    assert_eq!(seen, old);
    let tag = "0000000000000010";
    let mut s = f.sink(tag, true).await;
    put(&mut s, "report.txt", &new, 0o644, 1).await;
    let (c, e) = s.place(&[], true).await.unwrap();
    assert_eq!(c, vec!["report.txt".to_owned()]);
    assert!(e.is_empty());
    s.remove().await.unwrap();
    assert_eq!(std::fs::read(f.dst.join("Report.TXT")).unwrap(), old);
    std::io::Seek::seek(&mut held, std::io::SeekFrom::Start(0)).unwrap();
    let mut after = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut after).unwrap();
    assert_eq!(after, old);
    assert!(incoming_temps(&f.dst, tag).is_empty());
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_eof_cancel_removes_unfinished_only_and_leaves_lookalike() {
    let f = Fixture::new("m3-cancel").await;
    let tag = "0000000000000004";
    let look = f.dst.join(format!(".tailfiles-incoming-{tag}-999"));
    std::fs::write(&look, b"not ours").unwrap();
    let mut s = f.sink(tag, false).await;
    put(&mut s, "committed", b"keep", 0, 0).await;
    s.file_begin(
        "partial",
        &FileMeta {
            size: 1024 * 1024,
            mode: 0,
            mtime: 0,
        },
    )
    .await
    .unwrap();
    // C# WriteData fills one entire 256 KiB Exact() buffer BEFORE WriteFile.
    // Send that full buffer (still short of the 1 MiB declaration), otherwise
    // the disk-write barrier can never be reached and the test just times out.
    let chunk = crate::browser::transfer::CHUNK;
    s.file_chunk(&vec![b'p'; chunk]).await.unwrap();
    let temp = f.dst.join(format!(".tailfiles-incoming-{tag}-2"));
    wait_for_file(&temp, chunk as u64).await;
    // Same production cleanup used on cooperative cancellation and short-payload errors.
    assert!(s.file_end(0, 0).await.is_err());
    s.remove().await.unwrap();
    assert!(!temp.exists());
    assert!(!f.dst.join("partial").exists());
    assert_eq!(std::fs::read(f.dst.join("committed")).unwrap(), b"keep");
    assert_eq!(std::fs::read(&look).unwrap(), b"not ours");
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_eof_cancel_removes_unfinished_directory_tree() {
    let f = Fixture::new("m3-dir-cancel").await;
    let tag = "0000000000000007";
    let mut s = f.sink(tag, false).await;
    s.dir_begin("tree", 0o755, 123).await.unwrap();
    s.dir_begin("tree/nested", 0o755, 123).await.unwrap();
    put(
        &mut s,
        "tree/nested/complete",
        b"inside unfinished tree",
        0o644,
        123,
    )
    .await;
    let chunk = crate::browser::transfer::CHUNK;
    s.file_begin(
        "tree/nested/partial",
        &FileMeta {
            size: (chunk * 2) as u64,
            mode: 0o644,
            mtime: 123,
        },
    )
    .await
    .unwrap();
    s.file_chunk(&vec![b'p'; chunk]).await.unwrap();
    let temp = f.dst.join(format!(".tailfiles-incoming-{tag}-1"));
    wait_for_file(&temp.join("nested/partial"), chunk as u64).await;
    assert_eq!(
        std::fs::read(temp.join("nested/complete")).unwrap(),
        b"inside unfinished tree"
    );
    assert!(!f.dst.join("tree").exists());
    s.remove().await.unwrap();
    assert!(!temp.exists());
    assert!(!f.dst.join("tree").exists());
    assert_eq!(std::fs::read_dir(&f.dst).unwrap().count(), 0);
    f.assert_zero_sftp();
}

#[tokio::test]
async fn csharp_planted_exact_next_temp_and_lookalike_untouched() {
    let f = Fixture::new("m3-planted").await;
    let tag = "0000000000000005";
    let exact = f.dst.join(format!(".tailfiles-incoming-{tag}-1"));
    let look = f.dst.join(format!(".tailfiles-incoming-{tag}-1-lookalike"));
    std::fs::write(&exact, b"exact owner").unwrap();
    std::fs::write(&look, b"look owner").unwrap();
    let mut s = f.sink(tag, false).await;
    // Zero payload avoids a broken-pipe race after the exclusive temp open
    // refuses; the refusal may surface during the following Z write or reply.
    put(&mut s, "victim", b"", 0, 0).await;
    assert!(s.place(&[], false).await.is_err());
    s.remove().await.unwrap();
    assert_eq!(std::fs::read(exact).unwrap(), b"exact owner");
    assert_eq!(std::fs::read(look).unwrap(), b"look owner");
    assert!(!f.dst.join("victim").exists());
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_replace_merge_case_clash_is_client_refused() {
    let f = Fixture::new("m3-case").await;
    std::fs::create_dir(f.dst.join("folder")).unwrap();
    let mut s = f.sink("0000000000000006", true).await;
    s.dir_begin("folder", 0, 0).await.unwrap();
    put(&mut s, "folder/A", b"first", 0, 0).await;
    assert_eq!(
        s.file_begin(
            "folder/a",
            &FileMeta {
                size: 6,
                mode: 0,
                mtime: 0
            }
        )
        .await
        .unwrap_err()
        .kind,
        ErrorKind::Denied
    );
    s.remove().await.unwrap();
    assert_eq!(std::fs::read(f.dst.join("folder/A")).unwrap(), b"first");
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_native_mkdir_rename_readonly_delete_and_junction_sentinel() {
    let f = Fixture::new("m3-delete space").await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    mkdir(&f.t, &f.dir(), "folder").await.unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let folder = std::fs::metadata(f.dst.join("folder")).unwrap();
    assert!(folder.is_dir());
    let mtime = folder
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        (before..=after).contains(&mtime),
        "native mkdir mtime {mtime} outside [{before}, {after}]"
    );
    rename(&f.t, &f.dir(), "folder", "renamed").await.unwrap();
    assert!(f.dst.join("renamed").is_dir());
    assert_eq!(
        exists(&f.t, &f.dir(), &["renamed".into(), "missing".into()])
            .await
            .unwrap(),
        vec!["renamed"]
    );
    let ro = f.dst.join("readonly");
    std::fs::write(&ro, b"delete readonly").unwrap();
    let mut perms = std::fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).unwrap();
    let sentinel = f.root.join("sentinel");
    std::fs::create_dir(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep"), b"sentinel").unwrap();
    let junction = f.dst.join("junction");
    let tree = f.dst.join("recursive");
    std::fs::create_dir_all(tree.join("nested")).unwrap();
    std::fs::write(tree.join("nested/ordinary"), b"delete inside tree").unwrap();
    let nested_junction = tree.join("nested").join("junction");
    for link in [&junction, &nested_junction] {
        // Pass paths as individual args. An already-quoted command passed
        // through arg() gets CRT backslash-escaped quotes, which cmd /c does
        // not decode. Component joins also avoid mklink's mixed-separator path.
        let status = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(&sentinel)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(
            status.status.success(),
            "junction fixture creation for {}: {}\n{}",
            link.display(),
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        use std::os::windows::fs::MetadataExt;
        assert_ne!(
            std::fs::symlink_metadata(link).unwrap().file_attributes() & 0x400,
            0,
            "fixture must be an actual reparse point"
        );
        assert_eq!(std::fs::read(link.join("keep")).unwrap(), b"sentinel");
    }
    delete(
        &f.t,
        &[
            ro.to_string_lossy().into_owned(),
            junction.to_string_lossy().into_owned(),
            tree.to_string_lossy().into_owned(),
        ],
    )
    .await
    .unwrap();
    assert!(!ro.exists());
    assert!(!junction.exists());
    assert!(!tree.exists());
    assert_eq!(std::fs::read(sentinel.join("keep")).unwrap(), b"sentinel");
    f.assert_zero_sftp();
}
#[tokio::test]
async fn csharp_delete_all_target_home_guard_precedes_first_mutation() {
    let f = Fixture::new("m3-homeguard").await;
    let keep = f.dst.join("keep");
    std::fs::write(&keep, b"safe").unwrap();
    let home = std::env::var("USERPROFILE").expect("Windows USERPROFILE required");
    let error = delete(&f.t, &[keep.to_string_lossy().into_owned(), home])
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Denied);
    assert_eq!(std::fs::read(keep).unwrap(), b"safe");
    f.assert_zero_sftp();
}
