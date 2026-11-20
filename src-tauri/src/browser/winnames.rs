//! Windows name validation — port of `windows_name_problem`
//! (bin/tailfiles:2295-2311). Pure function, testable on every platform.
//!
//! MANDATORY before any Windows create/rename: M0 proved the Windows
//! OpenSSH server ACCEPTED `CON` as an ordinary 8-byte file, so the server
//! will not reject reserved names for us — client-side validation is the
//! only guard. Windows peers are browse+pull only in M1, but this runs
//! now (and is unit-tested now) so the guard is real before the M3 gate.

/// Characters Windows refuses in a name: `\ / : * ? " < > |` plus the C0
/// control range. (The plugin also rejects `\x7f`; we include it too — it's
/// a control char and never valid in a Windows name.)
fn has_bad_char(name: &str) -> bool {
    name.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || (c as u32) < 0x20
            || c as u32 == 0x7f
    })
}

/// Reserved device names. `CON`, `PRN`, `AUX`, `NUL`, and `COM1..COM9` /
/// `LPT1..LPT9` — matched on the part before the first dot, case-insensitive
/// (so `con`, `CON.txt`, `Com1.log` are all refused).
fn is_reserved_stem(stem: &str) -> bool {
    let up = stem.to_ascii_uppercase();
    match up.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            // COM1..COM9 / LPT1..LPT9 (single digit only, per the plugin's
            // `range(1, 10)`).
            if let Some(rest) = up.strip_prefix("COM").or_else(|| up.strip_prefix("LPT")) {
                rest.len() == 1
                    && rest
                        .chars()
                        .next()
                        .is_some_and(|c| ('1'..='9').contains(&c))
            } else {
                false
            }
        }
    }
}

/// Why Windows won't accept `name`, or `None` if it's fine. Message strings
/// mirror the plugin so the UI reads the same.
///
/// Rust `&str` is always valid UTF-8, so the plugin's "isn't valid text"
/// branch (a `UnicodeEncodeError` guard) is structurally impossible here and
/// intentionally omitted — non-UTF-8 remote names never reach this function
/// (they are skipped+counted at the listing layer, per M0).
pub fn windows_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("the name is empty");
    }
    if has_bad_char(name) {
        return Some("Windows doesn't allow these characters in names: \\ / : * ? \" < > |");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("Windows doesn't allow names that end with a dot or a space");
    }
    let stem = name.split('.').next().unwrap_or(name);
    if is_reserved_stem(stem) {
        return Some("Windows reserves that device name");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for n in [
            "report.pdf",
            "with space",
            "café",
            "file-name_1.txt",
            "CONSOLE",      // starts with CON but not the reserved stem
            "CONTACTS.vcf", // ditto
            "mycom",        // not COM<digit>
            "a",
            ".hidden",
            "archive.tar.gz",
        ] {
            assert_eq!(windows_name_problem(n), None, "should accept {n:?}");
        }
    }

    #[test]
    fn refuses_bad_characters() {
        for n in [
            "a<b", "a>b", "a:b", "a\"b", "a/b", "a\\b", "a|b", "a?b", "a*b",
        ] {
            assert!(
                windows_name_problem(n).is_some(),
                "should refuse bad char in {n:?}"
            );
        }
    }

    #[test]
    fn refuses_control_chars() {
        assert!(windows_name_problem("a\u{0}b").is_some());
        assert!(windows_name_problem("a\u{1f}b").is_some());
        assert!(windows_name_problem("a\u{7f}b").is_some());
        assert!(windows_name_problem("tab\there").is_some());
    }

    #[test]
    fn refuses_trailing_dot_or_space() {
        assert!(windows_name_problem("name.").is_some());
        assert!(windows_name_problem("name ").is_some());
        assert!(windows_name_problem(".").is_some());
        // A dot in the middle is fine.
        assert_eq!(windows_name_problem("a.b"), None);
    }

    #[test]
    fn refuses_reserved_device_names() {
        for n in [
            "CON", "con", "Con", "PRN", "AUX", "NUL", "COM1", "com9", "LPT1", "lpt9",
        ] {
            assert!(
                windows_name_problem(n).is_some(),
                "should refuse reserved {n:?}"
            );
        }
    }

    #[test]
    fn refuses_reserved_names_with_extensions() {
        // The stem before the first dot is what matters.
        for n in ["CON.txt", "con.tar.gz", "NUL.log", "COM1.dat", "lpt3.x"] {
            assert!(
                windows_name_problem(n).is_some(),
                "should refuse reserved stem in {n:?}"
            );
        }
    }

    #[test]
    fn accepts_reserved_lookalikes() {
        // COM0 / COM10 / COMA are NOT reserved (only COM1..COM9).
        for n in [
            "COM0",
            "COM10",
            "COMA",
            "LPT0",
            "LPT10",
            "CONSOLE.txt",
            "NULX",
        ] {
            assert_eq!(
                windows_name_problem(n),
                None,
                "should accept non-reserved {n:?}"
            );
        }
    }

    #[test]
    fn refuses_empty() {
        assert!(windows_name_problem("").is_some());
    }
}
