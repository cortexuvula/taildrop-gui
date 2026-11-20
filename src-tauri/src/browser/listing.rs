//! Listing: local scandir + remote SFTP readdir (report §7.1). Produces the
//! `Listing` wire shape the panes render. Two safety rules carried from the
//! plugin here (report §4.8 superseded for the text-forgery class by binary
//! SFTP attrs, but these remain):
//!   - names that cannot be read safely (control chars, empty, non-UTF-8)
//!     are SKIPPED and COUNTED, never rendered and never guessed (§4.16,
//!     M0: russh-sftp filenames are UTF-8 String, so a non-UTF-8 remote name
//!     never becomes a corrupt Entry — it's skipped at the transport/listing
//!     boundary and counted exactly once).
//!   - directory symlinks resolve to `kind: dir` so Enter opens them like
//!     folders (bounded by MAX_LINK_PROBES; §8 "start with links-as-files"
//!     is upgraded here to a bounded follow since it's cheap and matches the
//!     plugin's `-cd`/`pwd` behavior).
//!
//! Paths are data: nothing here shells out or concatenates into a command.

use std::path::{Path, PathBuf};

use crate::browser::guards;
use crate::browser::transport::{RemoteNode, SftpTransport};
use crate::browser::{BrowserError, Entry, EntryKind, Listing, MAX_LINK_PROBES};

/// True when a name is safe to show and act on: non-empty, not `.`/`..`,
/// no `/`, no control characters. Mirrors the plugin's `safe_name`.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && guards::control_char_problem(name).is_none()
}

fn mtime_of(sys: &std::fs::Metadata) -> i64 {
    sys.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn perm_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn perm_of(meta: &std::fs::Metadata) -> u32 {
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

/// Local listing. `path` empty or `~` means the user's home. Skips (and
/// counts) names that can't be read safely; resolves directory symlinks
/// within a bounded probe budget.
pub fn list_local(path: &str) -> Result<Listing, BrowserError> {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let resolved = expand_home(path, &home);
    let dir = PathBuf::from(&resolved);
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();

    let rd = std::fs::read_dir(&dir).map_err(|e| {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => crate::browser::ErrorKind::Notfound,
            std::io::ErrorKind::PermissionDenied => crate::browser::ErrorKind::Denied,
            _ => crate::browser::ErrorKind::Other,
        };
        BrowserError::new(kind, format!("Can't open {}: {e}", resolved))
    })?;

    let mut entries = Vec::new();
    let mut skipped = 0u32;
    let mut link_probes = 0usize;
    for item in rd {
        let de = match item {
            Ok(de) => de,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        // A non-UTF-8 filename can never be rendered safely — skip+count.
        let name = match de.file_name().into_string() {
            Ok(n) => n,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if !safe_name(&name) {
            // `.`/`..` are structural, not "unsafe" — don't count them.
            if name != "." && name != ".." {
                skipped += 1;
            }
            continue;
        }
        let lm = match de.metadata() {
            // read_dir's metadata follows symlinks on some platforms; take
            // symlink_metadata for the link flag, then follow when needed.
            Ok(_) => match std::fs::symlink_metadata(de.path()) {
                Ok(m) => m,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            },
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let is_link = lm.file_type().is_symlink();
        let mut is_dir = lm.is_dir();
        if is_link && !is_dir && link_probes < MAX_LINK_PROBES {
            link_probes += 1;
            if let Ok(target) = std::fs::metadata(de.path()) {
                is_dir = target.is_dir();
            }
        }
        let size = if is_dir { 0 } else { lm.len() };
        entries.push(Entry {
            name,
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            link: is_link,
            size,
            mtime: mtime_of(&lm),
            perm: perm_of(&lm),
        });
    }

    Ok(Listing {
        path: resolved,
        entries,
        skipped,
        user,
        home,
    })
}

/// Expand a leading `~` against `home` (the pane's own machine home). Empty
/// path means home itself.
pub fn expand_home(path: &str, home: &str) -> String {
    if path.is_empty() {
        return home.to_string();
    }
    if path == "~" {
        return home.to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{}", home.trim_end_matches('/'), rest);
    }
    path.to_string()
}

/// Remote listing over the transport. `path` empty means the session home
/// (resolved via realpath("."), the plugin's Windows-safe home discovery).
/// Directory symlinks resolve to `kind: dir` within a bounded probe budget.
pub async fn list_remote(
    t: &dyn SftpTransport,
    path: &str,
    home_hint: &str,
    user: &str,
) -> Result<Listing, BrowserError> {
    let resolved = if path.is_empty() {
        t.realpath(".")
            .await
            .unwrap_or_else(|_| home_hint.to_string())
    } else {
        // Ask the server to canonicalize so `..`, `//`, and symlinks in the
        // path collapse to the true folder (the pane shows the real path).
        t.realpath(path).await.unwrap_or_else(|_| path.to_string())
    };

    let nodes = t.read_dir(&resolved).await?;
    let mut entries = Vec::new();
    let mut skipped = 0u32;
    let mut link_probes = 0usize;
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        if !safe_name(&node.name) {
            if node.name != "." && node.name != ".." {
                skipped += 1;
            }
            continue;
        }
        // Duplicate names in one listing are a server defect — keep only the
        // first, count the rest as skipped (plugin: names present exactly once).
        if !seen.insert(node.name.clone()) {
            skipped += 1;
            continue;
        }
        let mut is_dir = node.is_dir;
        let is_link = node.is_symlink;
        if is_link && !is_dir && link_probes < MAX_LINK_PROBES {
            link_probes += 1;
            let child = join(&resolved, &node.name);
            if let Ok(Some(st)) = t.stat(&child).await {
                is_dir = st.is_dir;
            }
        }
        entries.push(entry_from_node(node, is_dir));
    }

    let home = if home_hint.is_empty() {
        resolved.clone()
    } else {
        home_hint.to_string()
    };
    Ok(Listing {
        path: resolved,
        entries,
        skipped,
        user: user.to_string(),
        home,
    })
}

fn entry_from_node(node: RemoteNode, is_dir: bool) -> Entry {
    Entry {
        name: node.name,
        kind: if is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        },
        link: node.is_symlink,
        size: node.size,
        mtime: node.mtime,
        perm: node.perm,
    }
}

/// POSIX-style join used for link probing (SFTP always uses `/`).
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", dir.trim_end_matches('/'), name)
    }
}

/// The home folder used by delete guards: for a remote target it must come
/// fresh from the probe (never cached), for local from the OS.
pub fn local_home() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Convenience for tests and open(): is this path inside `dir`?
pub fn path_within(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_rules() {
        assert!(safe_name("report.pdf"));
        assert!(safe_name("with space"));
        assert!(safe_name("café"));
        assert!(safe_name("literal*?["));
        assert!(!safe_name(""));
        assert!(!safe_name("."));
        assert!(!safe_name(".."));
        assert!(!safe_name("a/b"));
        assert!(!safe_name("line\nbreak"));
        assert!(!safe_name("tab\tx"));
    }

    #[test]
    fn expand_home_rules() {
        assert_eq!(expand_home("", "/home/alice"), "/home/alice");
        assert_eq!(expand_home("~", "/home/alice"), "/home/alice");
        assert_eq!(expand_home("~/docs", "/home/alice"), "/home/alice/docs");
        assert_eq!(expand_home("~/docs", "/home/alice/"), "/home/alice/docs");
        assert_eq!(expand_home("/etc", "/home/alice"), "/etc");
        // A name that merely starts with ~ but isn't ~/ is left alone.
        assert_eq!(expand_home("~bob/x", "/home/alice"), "~bob/x");
    }

    #[test]
    fn join_rules() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/a/", "b"), "/a/b");
        assert_eq!(join("/", "b"), "/b");
        assert_eq!(join("", "b"), "/b");
    }

    #[test]
    fn local_listing_skips_unsafe_and_counts() {
        let dir = tempdir("listing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.txt"), b"hi").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        // A name with a newline is unsafe to render — must be skipped+counted.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bad = std::ffi::OsStr::from_bytes(b"line\nbreak");
            let _ = std::fs::write(Path::new(&dir).join(bad), b"x");
        }
        let path = dir.to_string_lossy().to_string();
        let listing = list_local(&path).unwrap();
        assert!(listing
            .entries
            .iter()
            .any(|e| e.name == "ok.txt" && e.kind == EntryKind::File));
        assert!(listing
            .entries
            .iter()
            .any(|e| e.name == "sub" && e.kind == EntryKind::Dir));
        // No unsafe name reached the entries.
        assert!(listing.entries.iter().all(|e| safe_name(&e.name)));
        #[cfg(unix)]
        assert!(
            listing.skipped >= 1,
            "newline name should be skipped+counted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_listing_dir_symlink_resolves_to_dir() {
        #[cfg(unix)]
        {
            let dir = tempdir("links");
            std::fs::create_dir_all(dir.join("real")).unwrap();
            std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
            let listing = list_local(&dir.to_string_lossy()).unwrap();
            let link = listing.entries.iter().find(|e| e.name == "link").unwrap();
            assert!(link.link, "link flag set");
            assert_eq!(link.kind, EntryKind::Dir, "dir symlink opens like a dir");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "taildrop-m1-{tag}-{}-{nonce:x}",
            std::process::id()
        ))
    }
}
