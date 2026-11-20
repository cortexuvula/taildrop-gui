//! Pure safety-decision cores ported from the plugin's guards. No I/O here —
//! every function is a decision the caller acts on, so they are unit-testable
//! on all platforms. Ports:
//!   - `control_char_problem`  ← check_path (bin/tailfiles:252-257)
//!   - `guard_key`             ← guard_key (bin/tailfiles:2447-2456)
//!   - `guard_delete_problem`  ← guard_delete (bin/tailfiles:2459-2473)
//!   - `rel_path_problem`      ← check_rel (bin/tailfiles:1378-1381)
//!   - `number_field_problem`  ← Reader.number (bin/tailfiles:1360-1365)
//!   - copy preconditions      ← cmd_copy (bin/tailfiles:2360-2406)
//!
//! These enforce the §7.4 v1 column: delete guards (roots/home/ancestors),
//! resource caps (depth 512 / field 64 KiB / ≤15-digit numbers), unique
//! single-parent copy selection, and copy-into-itself refusal.

use crate::browser::{MAX_DEPTH, MAX_FIELD_BYTES, MAX_NUMBER_DIGITS};

/// Refuse strings containing a control character (C0 range or DEL). A newline
/// would forge a line in text protocols; NUL truncates C strings. Paths are
/// data, never shell text, but we still refuse these at every boundary.
pub fn control_char_problem(s: &str) -> Option<&'static str> {
    if s.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
        Some("contains a control character, so it can't be handled safely")
    } else {
        None
    }
}

/// Comparable form of a path for guard decisions. Windows paths are
/// case-insensitive, accept either slash, and may be written `/C:/x` or
/// `C:/x` — normalize all three so a guard can't be dodged by respelling.
/// Faithful port of guard_key.
pub fn guard_key(path: &str, windows: bool) -> String {
    let mut p = path.to_string();
    if windows {
        p = p.replace('\\', "/");
        if is_drive_qualified(&p) {
            p = format!("/{p}");
        }
        p = p.to_lowercase();
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// True for `C:` or `C:/…` (a drive-qualified Windows path), matching the
/// plugin's `^[A-Za-z]:(/|$)`.
fn is_drive_qualified(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'/')
}

/// True for a bare drive root after normalization (`/c:`), matching the
/// plugin's `^/[a-z]:$` (case-insensitive).
fn is_windows_drive_root(norm: &str) -> bool {
    let b = norm.as_bytes();
    b.len() == 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

/// Refuse anything but a plain absolute path that is not a root, a drive, a
/// home folder of the machine it is on (as spelled and as resolved), or an
/// ancestor of one. Without a known home, nothing is deleted (fail closed).
/// Returns a human-readable refusal reason, or `None` if the path is safe.
///
/// Callers must check BOTH the path as given and its server-resolved form
/// (a symlinked parent can't hide a home folder). Faithful port of
/// guard_delete.
pub fn guard_delete_problem(path: &str, homes: &[&str], windows: bool) -> Option<String> {
    if let Some(why) = control_char_problem(path) {
        return Some(format!("{path:?} {why}"));
    }
    let norm_homes: Vec<String> = homes
        .iter()
        .filter(|h| !h.is_empty())
        .map(|h| guard_key(h, windows))
        .collect();
    if norm_homes.is_empty() {
        // Fail closed: the probe must have found the home.
        return Some(
            "Refusing to delete: couldn't determine the home folder on that machine".into(),
        );
    }
    let norm = guard_key(path, windows);
    let bad = !norm.starts_with('/')
        || norm.split('/').any(|seg| seg == "." || seg == "..")
        || norm == "/"
        || is_windows_drive_root(&norm)
        || norm_homes
            .iter()
            .any(|h| norm == *h || h.starts_with(&format!("{norm}/")));
    if bad {
        Some(format!("Refusing to delete {path}"))
    } else {
        None
    }
}

/// Validate a copy-stream relative path against the selected roots. Faithful
/// port of check_rel: refuse >512 components, a first component outside the
/// selected names, any empty/`.`/`..` component, or a control char. This is
/// the depth-bomb / traversal guard for streamed records.
pub fn rel_path_problem(rel: &str, roots: &[&str]) -> Option<&'static str> {
    if control_char_problem(rel).is_some() {
        return Some("unexpected control character in copy stream path");
    }
    if rel.len() > MAX_FIELD_BYTES {
        return Some("copy stream path longer than any real one");
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() > MAX_DEPTH {
        return Some("copy stream path nested too deeply");
    }
    if parts
        .iter()
        .any(|x| x.is_empty() || *x == "." || *x == "..")
    {
        return Some("unexpected item in the copy stream path");
    }
    if !roots.contains(&parts[0]) {
        return Some("copy stream path outside the selected items");
    }
    None
}

/// Validate a server-supplied decimal number field (size/total/mtime).
/// Faithful port of Reader.number: exactly 1..=15 ASCII digits, nothing else.
/// Refuses oversized numbers and non-digit junk a hostile server could send.
/// Pure record-parser guard (plugin port); exercised by unit tests, wired
/// with the M2 exec-channel agents.
#[allow(dead_code)]
pub fn number_field_problem(field: &str) -> Option<&'static str> {
    if field.is_empty() || field.len() > MAX_NUMBER_DIGITS {
        return Some("server sent a number of an impossible length");
    }
    if !field.bytes().all(|b| b.is_ascii_digit()) {
        return Some("server sent a number that doesn't make sense");
    }
    // ≤15 digits always fits u64, but be explicit.
    if field.parse::<u64>().is_err() {
        return Some("server sent a number too large");
    }
    None
}

/// Refuse a server-supplied name/field longer than any real one (64 KiB cap).
/// Pure record-parser guard (plugin port); exercised by unit tests.
#[allow(dead_code)]
pub fn field_len_problem(len: usize) -> Option<&'static str> {
    if len > MAX_FIELD_BYTES {
        Some("the other machine sent a name longer than any real one")
    } else {
        None
    }
}

/// Last path component (basename). Handles a trailing slash the way the
/// plugin does after rstrip: `basename_of("a/b/")` is not meaningful, so
/// callers strip first; here we split on the final `/`.
/// Pure path helper (plugin port); exercised by unit tests.
#[allow(dead_code)]
pub fn basename_of(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((_, rest)) => rest,
        None => path,
    }
}

/// Copy-precondition decision core (cmd_copy, bin/tailfiles:2372-2385).
/// `names` are the bare item names to copy (all inside `src_dir`). Returns a
/// refusal reason, or `None` if the selection is well-formed.
///
/// Enforces: no control chars (names + both dirs), each item has its own
/// non-empty name that isn't `.`/`..` and contains no `/`, names are unique,
/// and (same target) a folder isn't copied into itself.
pub fn copy_precondition_problem(
    src_target: &str,
    dst_target: &str,
    src_dir: &str,
    dst_dir: &str,
    names: &[String],
    windows: bool,
) -> Option<String> {
    // Control chars refused in every path (2372-2374).
    for p in names.iter().map(|s| s.as_str()).chain([src_dir, dst_dir]) {
        if let Some(why) = control_char_problem(p) {
            return Some(format!("{p:?} {why}"));
        }
    }
    if names.is_empty() {
        return Some("Nothing selected to copy".into());
    }
    // Each item needs its own name: non-empty, not . / .., no slash (2376-2378).
    for n in names {
        if n.is_empty() || n == "." || n == ".." || n.contains('/') {
            return Some("Each item to copy needs its own name".into());
        }
    }
    // Unique basenames (2377).
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if !seen.insert(guard_key(n, windows)) {
            return Some("Each item to copy needs its own name".into());
        }
    }
    // Copy-into-itself (2383-2385): only meaningful when src and dst are the
    // same machine. Refuse when dst_dir is the source folder itself, or sits
    // inside one of the selected items.
    if src_target == dst_target {
        let dk = guard_key(dst_dir, windows);
        let sk = guard_key(src_dir, windows);
        if dk == sk {
            return Some("Can't copy a folder onto itself".into());
        }
        for n in names {
            let item = if sk == "/" {
                format!("/{n}")
            } else {
                format!("{sk}/{n}")
            };
            let item = guard_key(&item, windows);
            if dk == item || dk.starts_with(&format!("{item}/")) {
                return Some("Can't copy a folder into itself".into());
            }
        }
    }
    None
}

/// Validate a user-supplied new name for mkdir/rename (App.qml:365-368:
/// no empty, no `/`, no `.`/`..`), plus the universal control-char refusal.
pub fn new_name_problem(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some("The name can't be empty".to_string());
    }
    if value.contains('/') {
        return Some("The name can't contain a slash".to_string());
    }
    if value == "." || value == ".." {
        return Some("That name is reserved".to_string());
    }
    if let Some(why) = control_char_problem(value) {
        return Some(format!("{value:?} {why}"));
    }
    None
}

/// The "already exists" message, ported from exists_text (2354-2357): names
/// the first three clashes, notes partial completion when applicable.
pub fn exists_message(clash: &[String], folder: &str, partial: bool) -> String {
    let shown: Vec<&str> = clash.iter().take(3).map(|s| s.as_str()).collect();
    let mut msg = shown.join(", ");
    if clash.len() > 3 {
        msg.push('…');
    }
    let plural = if clash.len() == 1 { "s" } else { "" };
    msg.push_str(&format!(" already exist{plural} in {folder}"));
    if partial {
        msg.push_str("; everything else was copied");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- control chars ----
    #[test]
    fn refuses_control_chars() {
        assert!(control_char_problem("a\nb").is_some());
        assert!(control_char_problem("a\u{0}b").is_some());
        assert!(control_char_problem("a\u{7f}b").is_some());
        assert!(control_char_problem("tab\there").is_some());
        assert_eq!(control_char_problem("plain name.txt"), None);
        assert_eq!(control_char_problem("café *?[\"'\\"), None);
    }

    // ---- guard_key ----
    #[test]
    fn guard_key_posix_strips_trailing_slash() {
        assert_eq!(guard_key("/a/b/", false), "/a/b");
        assert_eq!(guard_key("/", false), "/");
        assert_eq!(guard_key("", false), "/");
        // POSIX is case-sensitive — no lowercasing.
        assert_eq!(guard_key("/A/B", false), "/A/B");
    }

    #[test]
    fn guard_key_windows_normalizes() {
        assert_eq!(guard_key("C:\\Users\\X", true), "/c:/users/x");
        assert_eq!(guard_key("/C:/Users/X", true), "/c:/users/x");
        assert_eq!(guard_key("c:/users/x/", true), "/c:/users/x");
        assert_eq!(guard_key("D:", true), "/d:");
    }

    #[test]
    fn guard_key_windows_case_insensitive_collision() {
        // Two spellings of the same Windows path collapse to one key.
        assert_eq!(
            guard_key("C:\\A\\B.TXT", true),
            guard_key("c:/a/b.txt", true)
        );
    }

    // ---- guard_delete ----
    #[test]
    fn delete_refuses_root_and_home_and_ancestors() {
        let homes = ["/home/alice"];
        assert!(guard_delete_problem("/", &homes, false).is_some());
        assert!(guard_delete_problem("/home", &homes, false).is_some()); // ancestor
        assert!(guard_delete_problem("/home/alice", &homes, false).is_some()); // home itself
        assert!(guard_delete_problem("/home/alice/", &homes, false).is_some());
    }

    #[test]
    fn delete_allows_descendant_of_home() {
        let homes = ["/home/alice"];
        assert_eq!(
            guard_delete_problem("/home/alice/docs/x.txt", &homes, false),
            None
        );
        assert_eq!(
            guard_delete_problem("/home/alice/file", &homes, false),
            None
        );
    }

    #[test]
    fn delete_fails_closed_without_home() {
        assert!(guard_delete_problem("/home/alice/x", &[], false).is_some());
        assert!(guard_delete_problem("/home/alice/x", &[""], false).is_some());
    }

    #[test]
    fn delete_refuses_dotdot_and_relative() {
        let homes = ["/home/alice"];
        assert!(guard_delete_problem("../etc", &homes, false).is_some());
        assert!(guard_delete_problem("/home/alice/../../etc", &homes, false).is_some());
        assert!(guard_delete_problem("relative/path", &homes, false).is_some());
    }

    #[test]
    fn delete_refuses_control_chars() {
        let homes = ["/home/alice"];
        assert!(guard_delete_problem("/home/alice/a\nb", &homes, false).is_some());
    }

    #[test]
    fn delete_windows_drive_root_and_home() {
        let homes = ["C:\\Users\\corte"];
        // Drive root refused.
        assert!(guard_delete_problem("C:\\", &homes, true).is_some());
        assert!(guard_delete_problem("/C:", &homes, true).is_some());
        // Home + ancestor refused (case-insensitive, either slash).
        assert!(guard_delete_problem("c:/users/corte", &homes, true).is_some());
        assert!(guard_delete_problem("C:\\Users", &homes, true).is_some());
        // Descendant allowed.
        assert_eq!(
            guard_delete_problem("C:\\Users\\corte\\docs\\f", &homes, true),
            None
        );
    }

    #[test]
    fn delete_checks_resolved_home_too() {
        // Caller passes both the spelled home and the resolved home; a
        // symlinked-parent path that resolves onto home is still refused
        // because the caller checks the resolved form against homes.
        let homes = ["/home/alice", "/real/alice"];
        assert!(guard_delete_problem("/real/alice", &homes, false).is_some());
        assert!(guard_delete_problem("/real", &homes, false).is_some());
    }

    // ---- rel_path (check_rel) ----
    #[test]
    fn rel_path_refuses_traversal_and_outside() {
        let roots = ["docs", "a.txt"];
        assert_eq!(rel_path_problem("docs/sub/f.txt", &roots), None);
        assert_eq!(rel_path_problem("a.txt", &roots), None);
        assert!(rel_path_problem("../escape", &roots).is_some());
        assert!(rel_path_problem("docs/../../escape", &roots).is_some());
        assert!(rel_path_problem("other/f.txt", &roots).is_some()); // outside roots
        assert!(rel_path_problem("/abs/docs", &roots).is_some()); // empty first comp
        assert!(rel_path_problem("docs//f", &roots).is_some()); // empty comp
        assert!(rel_path_problem("docs/./f", &roots).is_some());
    }

    #[test]
    fn rel_path_depth_bomb() {
        let roots = ["top"];
        let deep = format!(
            "top/{}",
            (0..MAX_DEPTH)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("/")
        );
        assert!(rel_path_problem(&deep, &roots).is_some());
        let ok = format!(
            "top/{}",
            (0..10).map(|i| i.to_string()).collect::<Vec<_>>().join("/")
        );
        assert_eq!(rel_path_problem(&ok, &roots), None);
    }

    // ---- number field (Reader.number) ----
    #[test]
    fn number_field_rejects_oversized_and_junk() {
        assert_eq!(number_field_problem("0"), None);
        assert_eq!(number_field_problem("123456789"), None);
        assert_eq!(number_field_problem("999999999999999"), None); // 15 digits ok
        assert!(number_field_problem("1000000000000000").is_some()); // 16 digits
        assert!(number_field_problem("").is_some()); // short/empty
        assert!(number_field_problem("-5").is_some());
        assert!(number_field_problem("12a4").is_some());
        assert!(number_field_problem(" 12").is_some());
        assert!(number_field_problem("+12").is_some());
    }

    #[test]
    fn field_len_cap() {
        assert_eq!(field_len_problem(MAX_FIELD_BYTES), None);
        assert!(field_len_problem(MAX_FIELD_BYTES + 1).is_some());
    }

    // ---- basename ----
    #[test]
    fn basename_splits_last_component() {
        assert_eq!(basename_of("/a/b/c.txt"), "c.txt");
        assert_eq!(basename_of("c.txt"), "c.txt");
        assert_eq!(basename_of("/a/b/"), "");
    }

    // ---- copy preconditions ----
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn copy_rejects_empty_and_slash_and_dots() {
        assert!(copy_precondition_problem("local", "h", "/a", "/b", &[], false).is_some());
        assert!(copy_precondition_problem("local", "h", "/a", "/b", &s(&["x/y"]), false).is_some());
        assert!(copy_precondition_problem("local", "h", "/a", "/b", &s(&["."]), false).is_some());
        assert!(copy_precondition_problem("local", "h", "/a", "/b", &s(&[".."]), false).is_some());
        assert!(copy_precondition_problem("local", "h", "/a", "/b", &s(&[""]), false).is_some());
    }

    #[test]
    fn copy_rejects_duplicate_names() {
        // Same name twice (or a case-variant clash on Windows).
        assert!(
            copy_precondition_problem("local", "h", "/a", "/b", &s(&["x", "x"]), false).is_some()
        );
        assert!(
            copy_precondition_problem("local", "h", "/a", "/b", &s(&["X", "x"]), true).is_some()
        );
        // Case-distinct on POSIX is fine.
        assert_eq!(
            copy_precondition_problem("local", "h", "/a", "/b", &s(&["X", "x"]), false),
            None
        );
    }

    #[test]
    fn copy_rejects_control_chars() {
        assert!(
            copy_precondition_problem("local", "h", "/a", "/b", &s(&["a\nb"]), false).is_some()
        );
        assert!(
            copy_precondition_problem("local", "h", "/a\nx", "/b", &s(&["ok"]), false).is_some()
        );
    }

    #[test]
    fn copy_rejects_into_itself_same_target() {
        // Copying /a/docs into /a/docs/inner (inside the selected folder).
        assert!(
            copy_precondition_problem("h", "h", "/a", "/a/docs", &s(&["docs"]), false).is_some()
        );
        // Onto itself.
        assert!(copy_precondition_problem("h", "h", "/a", "/a", &s(&["docs"]), false).is_some());
        // A sibling folder is fine.
        assert_eq!(
            copy_precondition_problem("h", "h", "/a", "/a/other", &s(&["docs"]), false),
            None
        );
    }

    #[test]
    fn copy_allows_same_folder_name_different_target() {
        // local → remote into an identically-named folder is NOT into-itself.
        assert_eq!(
            copy_precondition_problem("local", "h", "/a", "/a/docs", &s(&["docs"]), false),
            None
        );
    }

    #[test]
    fn copy_windows_into_itself_case_insensitive() {
        assert!(
            copy_precondition_problem("h", "h", "C:\\a", "c:/a/DOCS", &s(&["docs"]), true)
                .is_some()
        );
    }

    // ---- exists_message ----
    #[test]
    fn exists_message_shapes() {
        assert_eq!(
            exists_message(&s(&["a.txt"]), "/dst", false),
            "a.txt already exists in /dst"
        );
        assert_eq!(
            exists_message(&s(&["a", "b"]), "/dst", true),
            "a, b already exist in /dst; everything else was copied"
        );
        let many = s(&["a", "b", "c", "d"]);
        assert_eq!(
            exists_message(&many, "/dst", false),
            "a, b, c… already exist in /dst"
        );
    }
}
