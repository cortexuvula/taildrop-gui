//! Windows C# helper: installer, proven host/account capability and D/F/Z sink.
//! See agents/THIRD_PARTY.md for provenance and honest receive limitations.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use tokio::sync::OnceCell;

use super::agent::{self, AgentReply, ExecChannel, ReplyReader, CLEANUP_WAIT, PLACE_WAIT};
use super::transfer::{stage_dir_name, CopySink, FileMeta};
use super::transport::SftpTransport;
use super::{BrowserError, ErrorKind, MAX_DEPTH, MAX_FIELD_BYTES, MAX_LISTING_ENTRIES};

pub const AGENT_CS: &str = include_str!("../../agents/agent.cs");
/// Pin of the ACTIVE successor payload (reviewed vendor divergence; the
/// upstream c95f1b7 bytes are preserved and pinned separately in
/// agents/agent.upstream-c95f1b7.cs). See agents/THIRD_PARTY.md.
pub const SOURCE_HASH: &str = "adefa12c41424c98";
const INSTALL_WAIT: Duration = Duration::from_secs(180);
const IO_WAIT: Duration = Duration::from_secs(30);
const CACHE_TTL: Duration = Duration::from_secs(600);
const CACHE_CAP: usize = 128;

pub fn unavailable() -> BrowserError {
    BrowserError::unsupported("Windows helper is not proven for this host and account — browse and pull remain available; retry the probe or check PowerShell/exec permissions")
}

/// Only constructed after a known-answer execution. Invalidation revokes all
/// outstanding clones (including transports already opened by another call).
#[derive(Clone, Debug)]
pub struct WinAgent {
    exe: String,
    valid: Arc<AtomicBool>,
}
impl WinAgent {
    fn new(exe: String) -> Self {
        Self {
            exe,
            valid: Arc::new(AtomicBool::new(true)),
        }
    }
    pub fn proven(&self) -> bool {
        self.valid.load(Ordering::SeqCst)
    }
    fn revoke(&self) {
        self.valid.store(false, Ordering::SeqCst);
    }
    fn command(&self) -> Result<String, BrowserError> {
        if !self.proven() {
            return Err(unavailable());
        }
        validate_exe(&self.exe)?;
        Ok(format!("cmd /c \"{}\"", self.exe))
    }
    fn failure(&self, rc: Option<u32>, stderr: &str) -> BrowserError {
        if helper_missing(stderr) {
            self.revoke();
        }
        classify_failure(rc, stderr)
    }
}

pub fn helper_missing(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("not recognized")
        || s.contains("cannot find the file")
        || s.contains("could not be found")
        || s.contains("command not found")
}
pub fn classify_failure(rc: Option<u32>, stderr: &str) -> BrowserError {
    let kind = if helper_missing(stderr) {
        ErrorKind::Unsupported
    } else {
        match rc {
            Some(17) => ErrorKind::Exists,
            Some(4) => ErrorKind::Notfound,
            Some(3 | 5) => ErrorKind::Denied,
            None => ErrorKind::Unreachable,
            _ => super::transport::classify_message(stderr),
        }
    };
    let mut e = agent::classify_agent_failure(rc, stderr);
    e.kind = kind;
    e
}

/// Fixed loader: UTF-16LE, never the login shell's quoting dialect.
pub fn win_ps(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell -NoProfile -NonInteractive -InputFormat None -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The sole path allowed in shell text: a validated, hash-bound installed exe.
/// Stricter than upstream WIN_PATH_RE: also refuse & ; ! ( ) to narrow both shells.
fn shell_path(path: &str) -> Result<(), BrowserError> {
    let b = path.as_bytes();
    if b.len() < 4
        || !b[0].is_ascii_alphabetic()
        || b[1] != b':'
        || b[2] != b'\\'
        || path
            .chars()
            .any(|c| c.is_control() || "\"<>|?*%$`&;!()".contains(c))
    {
        return Err(BrowserError::unsupported(
            "Windows helper cannot start from this account's local app path",
        ));
    }
    validate_absolute(path)
}
fn validate_exe(exe: &str) -> Result<(), BrowserError> {
    shell_path(exe)?;
    if !exe.ends_with(&format!("\\TailFiles\\agent-{SOURCE_HASH}.exe")) {
        return Err(unavailable());
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), BrowserError> {
    if let Some(why) = super::winnames::windows_name_problem(name) {
        return Err(BrowserError::new(
            ErrorKind::Denied,
            format!("{name:?}: {why}"),
        ));
    }
    Ok(())
}
/// Drive prefix is exempt from name checks; every remaining component is not.
/// Supports the SFTP /C:/ spelling and native C:\\ spelling, not UNC/device paths.
pub fn validate_absolute(path: &str) -> Result<(), BrowserError> {
    if path.len() > MAX_FIELD_BYTES {
        return Err(BrowserError::other("Windows path too long"));
    }
    let p = path.strip_prefix('/').unwrap_or(path);
    let b = p.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || !matches!(b[2], b'/' | b'\\') {
        return Err(BrowserError::new(
            ErrorKind::Denied,
            "Windows helper requires an absolute drive path",
        ));
    }
    let rest = &p[3..];
    if rest.is_empty() {
        return Ok(());
    }
    for n in rest.split(['/', '\\']) {
        check_name(n)?;
    }
    Ok(())
}
fn validate_relative(rel: &str) -> Result<(), BrowserError> {
    if rel.len() > MAX_FIELD_BYTES || rel.split('/').count() > MAX_DEPTH {
        return Err(BrowserError::other(
            "Windows relative path exceeds protocol limits",
        ));
    }
    for n in rel.split('/') {
        check_name(n)?;
    }
    Ok(())
}
fn fields(verb: &str, args: &[&str]) -> Result<Vec<u8>, BrowserError> {
    let mut bytes = Vec::new();
    for f in std::iter::once(verb).chain(args.iter().copied()) {
        if f.len() > MAX_FIELD_BYTES || f.contains('\0') {
            return Err(BrowserError::other("invalid Windows helper field"));
        }
        bytes.extend_from_slice(f.as_bytes());
        bytes.push(0);
    }
    Ok(bytes)
}
fn timeout_error() -> BrowserError {
    BrowserError::new(
        ErrorKind::Unreachable,
        "Windows helper I/O timed out; no SFTP write fallback",
    )
}

/// Cap WHILE reading, and bound the ENTIRE open/write/EOF/read/exit operation.
async fn capture(
    t: &dyn SftpTransport,
    cmd: &str,
    input: &[u8],
    budget: Duration,
    cap: usize,
) -> Result<(Vec<u8>, Option<u32>, String), BrowserError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut ch = tokio::time::timeout_at(deadline, t.open_exec(cmd))
        .await
        .map_err(|_| timeout_error())??;
    let result = tokio::time::timeout_at(deadline, async {
        // Even a failed write must close stdin before waiting (PS InputFormat None).
        let write = ch.write_all(input).await;
        let eof = ch.close_stdin().await;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = ch.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if n > cap.saturating_sub(out.len()) {
                return Err(BrowserError::other("Windows helper output exceeded cap"));
            }
            out.extend_from_slice(&buf[..n]);
        }
        let rc = ch.wait_exit(budget).await;
        let stderr = ch.stderr_tail();
        if let Err(e) = write.and(eof) {
            return Err(if rc == Some(0) {
                e
            } else {
                classify_failure(rc, &stderr)
            });
        }
        Ok((out, rc, stderr))
    })
    .await
    .unwrap_or_else(|_| Err(timeout_error()));
    if result.is_err() {
        // EOF failure must never suppress diagnostic collection. A process
        // that exited before write/EOF may only report 'not recognized' here.
        let rc = tokio::time::timeout(CLEANUP_WAIT, async {
            let _ = ch.close_stdin().await;
            ch.wait_exit(CLEANUP_WAIT).await
        })
        .await
        .ok()
        .flatten();
        let stderr = ch.stderr_tail();
        if helper_missing(&stderr) {
            return Err(classify_failure(rc, &stderr));
        }
    }
    result
}

fn compile_script_for(folder: &str, source_name: &str, hash: &str) -> String {
    // Source is exclusive and attempt-unique, while the final exe stays
    // source-hash-bound. An interrupted upload can leave an orphan source,
    // never poison subsequent retries or cause us to overwrite its name.
    format!("$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'\n$d = {}; $h = '{hash}'\n$src = Join-Path $d {}\n$tmp = Join-Path $d ('agent-' + $h + '.' + $PID + '.exe')\ntry {{\nAdd-Type -OutputAssembly $tmp -OutputType ConsoleApplication -TypeDefinition ([IO.File]::ReadAllText($src))\ntry {{ Move-Item -LiteralPath $tmp -Destination (Join-Path $d ('agent-' + $h + '.exe')) }} catch {{ if (!(Test-Path -LiteralPath (Join-Path $d ('agent-' + $h + '.exe')) -PathType Leaf)) {{ throw }} }}\n}} finally {{\nRemove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue\nRemove-Item -LiteralPath $src -ErrorAction SilentlyContinue\n}}\n'installed'\n", ps_quote(folder), ps_quote(source_name))
}
fn compile_script(folder: &str, source_name: &str) -> String {
    compile_script_for(folder, source_name, SOURCE_HASH)
}
async fn install_and_prove(t: &dyn SftpTransport) -> Result<WinAgent, BrowserError> {
    tokio::time::timeout(INSTALL_WAIT, async {
        let query = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8\n$d = [Environment]::GetFolderPath('LocalApplicationData'); $d\nTest-Path -LiteralPath (Join-Path $d 'TailFiles\\agent-{SOURCE_HASH}.exe')\n");
        let (out, rc, err) = capture(t, &win_ps(&query), b"", IO_WAIT, 4096).await?;
        if rc != Some(0) { return Err(classify_failure(rc, &err)); }
        let text = String::from_utf8(out).map_err(|_| BrowserError::other("invalid Windows app folder output"))?;
        let clean = text.replace('\r', "");
        let lines: Vec<_> = clean.trim_end_matches('\n').split('\n').collect();
        if lines.len() != 2 || !matches!(lines[1], "True" | "False") { return Err(unavailable()); }
        let local = lines[0].trim_start_matches('\u{feff}');
        shell_path(local)?;
        let folder = format!("{}\\TailFiles", local.trim_end_matches('\\'));
        let exe = format!("{folder}\\agent-{SOURCE_HASH}.exe");
        validate_exe(&exe)?;
        if lines[1] != "True" {
            // The ONLY Windows SFTP write exception: installer source placement.
            let sftp_dir = format!("/{}", folder.replace('\\', "/"));
            match t.mkdir(&sftp_dir, 0o700).await {
                Ok(()) => (),
                Err(e) => { if !t.stat(&sftp_dir).await?.is_some_and(|n| n.is_dir && !n.is_symlink) { return Err(e); } }
            }
            let source_name = format!("agent-{SOURCE_HASH}.{}.cs", super::transfer::new_stage_tag());
            let src = format!("{sftp_dir}/{source_name}");
            let mut writer = t.create_exclusive(&src, 0o600).await?;
            let write = writer.write_all(AGENT_CS.as_bytes()).await;
            let close = writer.close().await;
            write?; close?;
            let (out, rc, err) = capture(t, &win_ps(&compile_script(&folder, &source_name)), b"", INSTALL_WAIT, 4096).await?;
            if rc != Some(0) || !out.windows(9).any(|w| w == b"installed") { return Err(classify_failure(rc, &err)); }
        }
        // Test-Path/installed alone is NEVER capability proof, even for existing exe.
        let agent = WinAgent::new(exe);
        let basename = format!("agent-{SOURCE_HASH}.exe");
        let payload = fields("exists", &[&folder, "1", &basename])?;
        let (out, rc, err) = capture(t, &agent.command()?, &payload, IO_WAIT, 16).await?;
        if rc != Some(0) || out != b"1" { return Err(agent.failure(rc, &err)); }
        Ok(agent)
    }).await.map_err(|_| timeout_error())?
}

type CacheCell = Arc<OnceCell<Option<WinAgent>>>;
struct CacheEntry {
    at: Instant,
    active: Arc<AtomicBool>,
    cell: CacheCell,
}
#[derive(Default)]
pub struct WinAgentCache {
    inner: Mutex<HashMap<String, CacheEntry>>,
}
impl WinAgentCache {
    fn key(host: &str, user: &str) -> String {
        format!("{host}\0{user}")
    }
    pub fn get(&self, host: &str, user: &str) -> Option<WinAgent> {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let e = map.get(&Self::key(host, user))?;
        if e.at.elapsed() > CACHE_TTL || !e.active.load(Ordering::SeqCst) {
            return None;
        }
        e.cell.get()?.as_ref().filter(|a| a.proven()).cloned()
    }
    pub async fn get_or_probe(
        &self,
        host: &str,
        user: &str,
        t: &dyn SftpTransport,
    ) -> Option<WinAgent> {
        let key = Self::key(host, user);
        let (cell, active) = {
            let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            map.retain(|_, e| {
                e.at.elapsed() <= CACHE_TTL
                    && !e
                        .cell
                        .get()
                        .is_some_and(|a| a.as_ref().is_some_and(|a| !a.proven()))
            });
            if let Some(e) = map.get(&key) {
                (e.cell.clone(), e.active.clone())
            } else {
                if map.len() >= CACHE_CAP {
                    // Fail closed instead of evicting a live single-flight and duplicating installs.
                    return None;
                }
                let cell = Arc::new(OnceCell::new());
                let active = Arc::new(AtomicBool::new(true));
                map.insert(
                    key,
                    CacheEntry {
                        at: Instant::now(),
                        active: active.clone(),
                        cell: cell.clone(),
                    },
                );
                (cell, active)
            }
        };
        let value = cell
            .get_or_init(|| async {
                match install_and_prove(t).await {
                    Ok(a) => Some(a),
                    Err(e) => {
                        log::info!("Windows helper unproven on {host} for {user}: {e}");
                        None
                    }
                }
            })
            .await;
        if !active.load(Ordering::SeqCst) {
            if let Some(a) = value {
                a.revoke();
            }
            return None;
        }
        value.as_ref().filter(|a| a.proven()).cloned()
    }
    pub fn invalidate_host(&self, host: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|key, e| {
            if !key.starts_with(&format!("{host}\0")) {
                return true;
            }
            e.active.store(false, Ordering::SeqCst);
            if let Some(Some(a)) = e.cell.get() {
                a.revoke();
            }
            false
        });
    }
}

fn require(t: &dyn SftpTransport) -> Result<WinAgent, BrowserError> {
    t.win_agent().filter(|a| a.proven()).ok_or_else(unavailable)
}
async fn verb(
    t: &dyn SftpTransport,
    verb: &str,
    args: &[&str],
    budget: Duration,
) -> Result<Vec<u8>, BrowserError> {
    let a = require(t)?;
    let captured = capture(
        t,
        &a.command()?,
        &fields(verb, args)?,
        budget,
        MAX_LISTING_ENTRIES,
    )
    .await;
    let (out, rc, err) = match captured {
        Ok(value) => value,
        Err(e) => {
            if helper_missing(&e.message) {
                a.revoke();
            }
            return Err(e);
        }
    };
    if rc != Some(0) {
        return Err(a.failure(rc, &err));
    }
    Ok(out)
}
pub async fn exists(
    t: &dyn SftpTransport,
    dir: &str,
    names: &[String],
) -> Result<Vec<String>, BrowserError> {
    validate_absolute(dir)?;
    for n in names {
        check_name(n)?;
    }
    let count = names.len().to_string();
    let mut args = vec![dir, count.as_str()];
    args.extend(names.iter().map(String::as_str));
    let out = verb(t, "exists", &args, IO_WAIT).await?;
    if out.len() != names.len() || !out.iter().all(|b| matches!(b, b'0' | b'1')) {
        return Err(BrowserError::other("invalid Windows exists reply"));
    }
    Ok(names
        .iter()
        .zip(out)
        .filter(|(_, b)| *b == b'1')
        .map(|(n, _)| n.clone())
        .collect())
}
pub async fn rename(
    t: &dyn SftpTransport,
    dir: &str,
    old: &str,
    new: &str,
) -> Result<(), BrowserError> {
    validate_absolute(dir)?;
    check_name(old)?;
    check_name(new)?;
    verb(t, "rename", &[dir, old, new], IO_WAIT)
        .await
        .map(|_| ())
}
pub async fn delete(t: &dyn SftpTransport, paths: &[String]) -> Result<(), BrowserError> {
    for p in paths {
        validate_absolute(p)?;
    }
    let count = paths.len().to_string();
    let mut args = vec![count.as_str()];
    args.extend(paths.iter().map(String::as_str));
    verb(t, "delete", &args, PLACE_WAIT).await.map(|_| ())
}
pub async fn mkdir(t: &dyn SftpTransport, dir: &str, name: &str) -> Result<(), BrowserError> {
    check_name(name)?;
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| BrowserError::other("current time is outside Windows helper range"))?;
    let mut sink = WinAgentSink::open(t, dir, &super::transfer::new_stage_tag(), false).await?;
    let result = async {
        sink.dir_begin(name, 0o755, mtime).await?;
        sink.dir_end().await?;
        let (conflicts, errors) = sink.place(&[name.to_string()], false).await?;
        if !conflicts.is_empty() {
            return Err(BrowserError::exists(conflicts, dir, false));
        }
        if !errors.is_empty() {
            return Err(BrowserError::other(
                "Windows helper could not create folder",
            ));
        }
        Ok(())
    }
    .await;
    let cleanup = sink.remove().await;
    result.and(cleanup)
}

pub struct WinAgentSink {
    ch: Box<dyn ExecChannel>,
    agent: WinAgent,
    placed: bool,
    stdin_closed: bool,
    paths: HashMap<String, String>,
    records: HashSet<String>,
    remaining: Option<u64>,
}
impl WinAgentSink {
    pub async fn open(
        t: &dyn SftpTransport,
        dir: &str,
        tag: &str,
        replace: bool,
    ) -> Result<Self, BrowserError> {
        let agent = require(t)?;
        validate_absolute(dir)?;
        let ch = tokio::time::timeout(IO_WAIT, t.open_exec(&agent.command()?))
            .await
            .map_err(|_| timeout_error())??;
        Self::start(ch, agent, dir, tag, replace).await
    }
    async fn start(
        ch: Box<dyn ExecChannel>,
        agent: WinAgent,
        dir: &str,
        tag: &str,
        replace: bool,
    ) -> Result<Self, BrowserError> {
        validate_absolute(dir)?;
        if tag.len() != 16
            || !tag
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(BrowserError::other("invalid Windows temp tag"));
        }
        let mut sink = Self {
            ch,
            agent,
            placed: false,
            stdin_closed: false,
            paths: HashMap::new(),
            records: HashSet::new(),
            remaining: None,
        };
        let input = fields(
            "recv",
            &[
                dir,
                &stage_dir_name(tag),
                if replace { "replace" } else { "keep" },
            ],
        )?;
        if let Err(e) = sink.write(&input).await {
            let _ = sink.remove().await;
            return Err(e);
        }
        Ok(sink)
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<(), BrowserError> {
        tokio::time::timeout(IO_WAIT, self.ch.write_all(bytes))
            .await
            .map_err(|_| timeout_error())?
    }
    fn admit(&mut self, rel: &str) -> Result<(), BrowserError> {
        validate_relative(rel)?;
        if self.remaining.is_some() {
            return Err(BrowserError::other("unfinished Windows file payload"));
        }
        if self.records.len() >= MAX_LISTING_ENTRIES {
            return Err(BrowserError::other("too many Windows stream entries"));
        }
        let mut path = String::new();
        for n in rel.split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(n);
            let folded = path.to_uppercase();
            if self.paths.get(&folded).is_some_and(|p| p != &path) {
                return Err(BrowserError::new(
                    ErrorKind::Denied,
                    format!("Windows case clash in {rel}"),
                ));
            }
            if self.paths.len() >= MAX_LISTING_ENTRIES && !self.paths.contains_key(&folded) {
                return Err(BrowserError::other("too many Windows path components"));
            }
            self.paths.insert(folded, path.clone());
        }
        if !self.records.insert(rel.to_owned()) {
            return Err(BrowserError::other("duplicate Windows stream record"));
        }
        Ok(())
    }
    async fn eof(&mut self) -> Result<(), BrowserError> {
        if !self.stdin_closed {
            self.stdin_closed = true;
            tokio::time::timeout(CLEANUP_WAIT, self.ch.close_stdin())
                .await
                .map_err(|_| timeout_error())??;
        }
        Ok(())
    }
}
#[async_trait]
impl CopySink for WinAgentSink {
    async fn dir_begin(&mut self, rel: &str, mode: u32, mtime: i64) -> Result<(), BrowserError> {
        self.admit(rel)?;
        if mtime < 0 {
            return Err(BrowserError::other("negative Windows mtime"));
        }
        self.write(&agent::d_record(rel, mode & 0o777, mtime)?)
            .await
    }
    async fn dir_end(&mut self) -> Result<(), BrowserError> {
        Ok(())
    }
    async fn file_begin(&mut self, rel: &str, meta: &FileMeta) -> Result<(), BrowserError> {
        self.admit(rel)?;
        if meta.mtime < 0 || meta.size > 999_999_999_999_999 {
            return Err(BrowserError::other(
                "Windows numeric field exceeds protocol limits",
            ));
        }
        self.write(&agent::f_record(rel, meta.mode, meta.mtime, meta.size)?)
            .await?;
        self.remaining = Some(meta.size);
        Ok(())
    }
    async fn file_chunk(&mut self, data: &[u8]) -> Result<(), BrowserError> {
        let left = self
            .remaining
            .ok_or_else(|| BrowserError::other("Windows file header missing"))?;
        if data.len() as u64 > left {
            return Err(BrowserError::other(
                "Windows file payload longer than declared",
            ));
        }
        self.write(data).await?;
        self.remaining = Some(left - data.len() as u64);
        Ok(())
    }
    async fn file_end(&mut self, _mode: u32, _mtime: i64) -> Result<(), BrowserError> {
        if self.remaining != Some(0) {
            return Err(BrowserError::other("short Windows file payload"));
        }
        self.remaining = None;
        Ok(())
    }
    async fn symlink(&mut self, rel: &str, _target: &str) -> Result<bool, BrowserError> {
        validate_relative(rel)?;
        Ok(false) // upstream C# has no L verb; walker counts skip.
    }
    async fn place(
        &mut self,
        _names: &[String],
        _replace: bool,
    ) -> Result<(Vec<String>, Vec<(String, String)>), BrowserError> {
        if self.remaining.is_some() {
            return Err(BrowserError::other("unfinished Windows file payload"));
        }
        let result = tokio::time::timeout(PLACE_WAIT, async {
            self.write(b"Z").await?;
            self.eof().await?;
            let mut reader = ReplyReader::new();
            let mut conflicts = Vec::new();
            let mut errors = Vec::new();
            let mut done = false;
            while let Some(reply) = reader.next(self.ch.as_mut()).await? {
                match reply {
                    AgentReply::Conflict(p) => conflicts.push(p),
                    AgentReply::Error(p) => {
                        errors.push((p, "Windows helper placement failed".to_owned()))
                    }
                    AgentReply::Done => {
                        done = true;
                        break;
                    }
                }
            }
            let rc = self.ch.wait_exit(PLACE_WAIT).await;
            if !done || rc != Some(0) {
                return Err(self.agent.failure(rc, &self.ch.stderr_tail()));
            }
            self.placed = true;
            Ok((conflicts, errors))
        })
        .await
        .map_err(|_| timeout_error())?;
        result
    }
    async fn remove(&mut self) -> Result<(), BrowserError> {
        // EOF cleanup ONLY. Never delete a temporary path by client-known name.
        tokio::time::timeout(CLEANUP_WAIT, async {
            let eof = self.eof().await;
            if !self.placed || eof.is_err() {
                let rc = self.ch.wait_exit(CLEANUP_WAIT).await;
                if helper_missing(&self.ch.stderr_tail()) {
                    self.agent.revoke();
                }
                if rc.is_none() {
                    return Err(timeout_error());
                }
            }
            eof
        })
        .await
        .map_err(|_| timeout_error())?
    }
}

#[cfg(test)]
mod tests;
