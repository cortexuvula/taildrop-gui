//! Pure plan-validation cores for the copy stream. This is the port of the
//! plugin's streamed-record validation (check_rel, Reader.number/field caps,
//! the sink's stack discipline) adapted to the plan-then-stream shape:
//! everything a remote server tells us about names, sizes and structure is
//! validated here BEFORE any byte is written, and the same builder drives
//! local walks so both sources obey identical caps.
//!
//! Hostile-input coverage (acceptance gate 1): short/empty fields, oversized
//! numbers, depth bombs, out-of-order paths, path escapes, control chars.

use crate::browser::guards;
use crate::browser::{BrowserError, ErrorKind, MAX_DEPTH, MAX_FIELD_BYTES};

/// Largest believable server-reported file size: 15 decimal digits, the
/// plugin's Reader.number bound (10^15 - 1 ≈ 1 PiB).
pub const MAX_REMOTE_SIZE: u64 = 999_999_999_999_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanKind {
    Dir,
    File,
    Link { target: String },
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    /// Path relative to the copy root; first component is one of the
    /// selected names (check_rel rule).
    pub rel: String,
    pub kind: PlanKind,
    /// Exact expected size for files (short reads fail the copy — plugin's
    /// "got shorter while it was being copied").
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct CopyPlan {
    /// Pre-walked total bytes for progress (the plugin's `T` record).
    pub total: u64,
    pub items: Vec<PlanItem>,
    /// Special files / unsafe names / unreadable links — counted, never
    /// silently dropped, never guessed at.
    pub skipped: u32,
}

fn bad(message: &str) -> BrowserError {
    BrowserError::new(ErrorKind::Other, message.to_string())
}

/// Incremental plan builder enforcing, per pushed item:
///   - check_rel rules (roots, no `.`/`..`/empty components, no control
///     chars, ≤64 KiB, ≤512 deep)
///   - number caps (sizes ≤ 15 digits)
///   - depth-first order via a directory stack (a child whose parent was
///     already closed is a protocol error — the plugin's sink stack rule)
pub struct PlanBuilder {
    roots: Vec<String>,
    /// Stack of open directory rel-paths ("" = the copy root itself).
    stack: Vec<String>,
    items: Vec<PlanItem>,
    total: u64,
    skipped: u32,
}

impl PlanBuilder {
    pub fn new(names: &[String]) -> Self {
        Self {
            roots: names.to_vec(),
            stack: vec![String::new()],
            items: Vec::new(),
            total: 0,
            skipped: 0,
        }
    }

    /// Validate one relative path against roots/caps/components.
    fn check_rel(&self, rel: &str) -> Result<(), BrowserError> {
        let roots: Vec<&str> = self.roots.iter().map(|s| s.as_str()).collect();
        if let Some(why) = guards::rel_path_problem(rel, &roots) {
            return Err(bad(&format!(
                "Unexpected item in the copy: {why} ({})",
                truncate(rel)
            )));
        }
        Ok(())
    }

    /// Pop closed directories until `parent` is the stack top; error when
    /// the parent was never opened (out-of-order stream).
    fn align_to_parent(&mut self, rel: &str) -> Result<(), BrowserError> {
        let parent = match rel.rsplit_once('/') {
            Some((p, _)) => p,
            None => "",
        };
        while self.stack.len() > 1 && self.stack.last().map(String::as_str) != Some(parent) {
            self.stack.pop();
        }
        if self.stack.last().map(String::as_str) != Some(parent) {
            return Err(bad(&format!(
                "Unexpected item in the copy: {} arrived after its folder closed",
                truncate(rel)
            )));
        }
        Ok(())
    }

    pub fn push(&mut self, item: PlanItem) -> Result<(), BrowserError> {
        self.check_rel(&item.rel)?;
        match &item.kind {
            PlanKind::File => {
                if item.size > MAX_REMOTE_SIZE {
                    return Err(bad(&format!(
                        "The other machine sent a number that doesn't make sense ({}), so the \
                         copy stopped",
                        item.size
                    )));
                }
            }
            PlanKind::Link { target } => {
                if target.len() > MAX_FIELD_BYTES {
                    return Err(bad(
                        "The other machine sent a link target longer than any real one",
                    ));
                }
                if guards::control_char_problem(target).is_some() {
                    return Err(bad(
                        "The other machine sent a link target with control characters",
                    ));
                }
            }
            PlanKind::Dir => {}
        }
        self.align_to_parent(&item.rel)?;
        if matches!(item.kind, PlanKind::Dir) {
            if self.stack.len() > MAX_DEPTH {
                return Err(bad(&format!(
                    "{} has folders nested more than {MAX_DEPTH} deep, so it can't be copied",
                    truncate(&item.rel)
                )));
            }
            self.stack.push(item.rel.clone());
        }
        if matches!(item.kind, PlanKind::File) {
            self.total += item.size;
        }
        self.items.push(item);
        Ok(())
    }

    /// Count a skipped item (special file, unsafe name, unreadable link).
    pub fn push_skipped(&mut self) {
        self.skipped += 1;
    }

    pub fn finish(mut self) -> CopyPlan {
        self.stack.clear();
        CopyPlan {
            total: self.total,
            items: self.items,
            skipped: self.skipped,
        }
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["docs".to_string(), "a.txt".to_string()]
    }

    fn dir(rel: &str) -> PlanItem {
        PlanItem {
            rel: rel.into(),
            kind: PlanKind::Dir,
            size: 0,
            mode: 0o755,
            mtime: 0,
        }
    }

    fn file(rel: &str, size: u64) -> PlanItem {
        PlanItem {
            rel: rel.into(),
            kind: PlanKind::File,
            size,
            mode: 0o644,
            mtime: 0,
        }
    }

    fn link(rel: &str, target: &str) -> PlanItem {
        PlanItem {
            rel: rel.into(),
            kind: PlanKind::Link {
                target: target.into(),
            },
            size: 0,
            mode: 0,
            mtime: 0,
        }
    }

    #[test]
    fn accepts_wellformed_dfs_stream() {
        let mut b = PlanBuilder::new(&names());
        b.push(dir("docs")).unwrap();
        b.push(file("docs/x.txt", 10)).unwrap();
        b.push(dir("docs/sub")).unwrap();
        b.push(file("docs/sub/y.bin", 20)).unwrap();
        b.push(link("docs/l", "x.txt")).unwrap();
        b.push(file("a.txt", 5)).unwrap(); // back to root after dirs closed
        let p = b.finish();
        assert_eq!(p.total, 35);
        assert_eq!(p.items.len(), 6);
        assert_eq!(p.skipped, 0);
    }

    #[test]
    fn refuses_path_escape_and_traversal() {
        let mut b = PlanBuilder::new(&names());
        assert!(b.push(file("../escape.txt", 1)).is_err());
        assert!(b.push(file("other/x", 1)).is_err()); // outside roots
        assert!(b.push(file("docs/../../x", 1)).is_err());
        assert!(b.push(file("/docs/abs", 1)).is_err());
        assert!(b.push(file("docs//double", 1)).is_err());
        assert!(b.push(file("docs/./dot", 1)).is_err());
    }

    #[test]
    fn refuses_short_and_control_fields() {
        let mut b = PlanBuilder::new(&names());
        assert!(b.push(file("", 1)).is_err()); // empty field
        assert!(b.push(file("docs/a\nb", 1)).is_err()); // control char
        assert!(b.push(link("docs/l", "tar\nget")).is_err());
        let long = "x".repeat(MAX_FIELD_BYTES + 1);
        assert!(b.push(link("docs/l2", &long)).is_err());
    }

    #[test]
    fn refuses_oversized_numbers() {
        let mut b = PlanBuilder::new(&names());
        b.push(dir("docs")).unwrap(); // DFS discipline: parent first
        assert!(b.push(file("docs/big", MAX_REMOTE_SIZE)).is_ok());
        assert!(b.push(file("docs/toobig", MAX_REMOTE_SIZE + 1)).is_err());
        assert!(b.push(file("a.txt", u64::MAX)).is_err());
    }

    #[test]
    fn refuses_depth_bomb() {
        let mut b = PlanBuilder::new(&names());
        b.push(dir("docs")).unwrap();
        let mut rel = "docs".to_string();
        let mut ok = 0;
        for i in 0..(MAX_DEPTH + 10) {
            rel = format!("{rel}/d{i}");
            match b.push(dir(&rel)) {
                Ok(()) => ok += 1,
                Err(_) => break,
            }
        }
        assert!(ok <= MAX_DEPTH, "depth bomb must be refused by {MAX_DEPTH}");
        assert!(ok >= MAX_DEPTH - 2, "legit depth must be allowed, got {ok}");
    }

    #[test]
    fn refuses_out_of_order_paths() {
        // A child of a folder that was never opened (stream reordered).
        let mut b = PlanBuilder::new(&names());
        assert!(b.push(file("docs/never-opened/x", 1)).is_err());
        // A child arriving after its parent's sibling closed the parent.
        let mut b = PlanBuilder::new(&names());
        b.push(dir("docs")).unwrap();
        b.push(dir("docs/sub")).unwrap();
        b.push(file("a.txt", 1)).unwrap(); // pops docs/sub AND docs
        assert!(b.push(file("docs/sub/late", 1)).is_err());
    }

    #[test]
    fn skipped_counting() {
        let mut b = PlanBuilder::new(&names());
        b.push_skipped();
        b.push_skipped();
        b.push(file("a.txt", 3)).unwrap();
        let p = b.finish();
        assert_eq!(p.skipped, 2);
        assert_eq!(p.total, 3);
    }
}
