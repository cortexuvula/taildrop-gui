#![allow(clippy::doc_overindented_list_items)] // ASCII placement matrix in the module doc
//! Pure placement decision core — the port of place_local's rules
//! (bin/tailfiles:975-1013) and the remote tier selection (M0 "Placement
//! conclusion for v1"). Separated from the fd/SFTP mechanics so the full
//! conflict/merge/replace matrix is testable without a filesystem.
//!
//! Rules (plugin-exact):
//!   - no-replace rename succeeds            → Placed
//!   - rename fails "can't be atomic"        → FailNonAtomic (fail CLOSED:
//!     EINVAL/ENOSYS/EOPNOTSUPP — the item is NOT moved, ever)
//!   - rename fails EEXIST, no replace       → Conflict
//!   - rename fails EEXIST, replace:
//!       dir  vs dir                         → MergeDirs (both held open and
//!                                              identity-checked; depth≤512)
//!       file/link vs file/link              → ReplaceAtomic (one atomic
//!                                              rename over the target)
//!       file ↔ dir (either way)             → Conflict ("a folder can only
//!                                              replace a folder, and a file
//!                                              a file or link")
//!       target vanished/unreadable          → Conflict
//!   - any other rename error (EXDEV, EACCES…) → FailOther
//!
//! Remote tier selection (M0): hardlink@openssh.com advertised → attempt
//! hardlink no-replace placement and VERIFY the collision behavior; not
//! advertised → check-then-rename with mandatory consent (TOCTOU ceiling
//! documented). posix-rename REPLACES and vanilla rename semantics differ
//! per flavor, so neither is ever used for no-replace placement.

/// Item kinds as placement sees them: the plugin only distinguishes
/// directories from everything else (files and links replace alike).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Dir,
    FileOrLink,
}

/// What the atomic no-replace rename attempt returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    Placed,
    /// EEXIST — the destination name is taken.
    Exists,
    /// EINVAL/ENOSYS/EOPNOTSUPP — the filesystem cannot refuse atomically.
    NonAtomic,
    /// EXDEV, EACCES, EIO, ... — anything else.
    OtherError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceAction {
    /// Item is in place; nothing more to do.
    Placed,
    /// Existed without consent, or kind mismatch under replace: report the
    /// name in `conflicts` (fails the copy with kind=exists when !replace).
    Conflict,
    /// Recurse: merge the staged dir into the existing dir (replace mode).
    MergeDirs,
    /// One atomic replacing rename (file/link over file/link, consented).
    ReplaceAtomic,
    /// Fail closed: the disk can't rename atomically — item NOT moved and
    /// the whole copy fails with the explicit non-atomic message.
    FailNonAtomic,
    /// Any other placement failure — item left in staging, copy fails.
    FailOther,
}

/// The decision matrix. `merge_allowed` is false at nesting depth ≥ 512
/// (plugin: `depth < 512` on the merge branch, else conflict).
pub fn place_action(
    outcome: RenameOutcome,
    replace: bool,
    mine: ItemKind,
    theirs: Option<ItemKind>,
    merge_allowed: bool,
) -> PlaceAction {
    match outcome {
        RenameOutcome::Placed => PlaceAction::Placed,
        RenameOutcome::NonAtomic => PlaceAction::FailNonAtomic,
        RenameOutcome::OtherError => PlaceAction::FailOther,
        RenameOutcome::Exists => {
            if !replace {
                return PlaceAction::Conflict;
            }
            match (mine, theirs) {
                (ItemKind::Dir, Some(ItemKind::Dir)) if merge_allowed => PlaceAction::MergeDirs,
                (ItemKind::FileOrLink, Some(ItemKind::FileOrLink)) => PlaceAction::ReplaceAtomic,
                // file↔dir either way, dir at depth cap, or vanished target
                _ => PlaceAction::Conflict,
            }
        }
    }
}

/// Remote placement tier (M0-proven): which no-replace mechanism to use for
/// a regular file on a remote destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTier {
    /// hardlink@openssh.com advertised: hardlink staged→final (atomic
    /// no-replace, same volume), collision must fail, then unlink stage.
    HardlinkNoReplace,
    /// Not advertised: lstat-then-rename. NOT race-free — only ever used
    /// behind the conflict pre-check + explicit Replace consent, with the
    /// TOCTOU ceiling documented (report §6.4, M0 §4).
    CheckThenRename,
}

pub fn remote_tier(has_hardlink_ext: bool) -> RemoteTier {
    if has_hardlink_ext {
        RemoteTier::HardlinkNoReplace
    } else {
        RemoteTier::CheckThenRename
    }
}

/// Map a raw errno from renameat2/renameatx_np into the outcome taxonomy.
/// `can_not_be_atomic` port: EINVAL, ENOSYS, EOPNOTSUPP.
#[cfg(unix)]
pub fn rename_errno_outcome(errno: i32) -> RenameOutcome {
    match errno {
        libc::EEXIST => RenameOutcome::Exists,
        libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP => RenameOutcome::NonAtomic,
        _ => RenameOutcome::OtherError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PlaceAction::*;

    const MERGE: bool = true;

    #[test]
    fn placed_is_placed() {
        assert_eq!(
            place_action(
                RenameOutcome::Placed,
                false,
                ItemKind::FileOrLink,
                None,
                MERGE
            ),
            Placed
        );
    }

    #[test]
    fn exists_without_replace_is_conflict() {
        for mine in [ItemKind::Dir, ItemKind::FileOrLink] {
            for theirs in [None, Some(ItemKind::Dir), Some(ItemKind::FileOrLink)] {
                assert_eq!(
                    place_action(RenameOutcome::Exists, false, mine, theirs, MERGE),
                    Conflict
                );
            }
        }
    }

    #[test]
    fn replace_matrix() {
        // dir vs dir → merge
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::Dir,
                Some(ItemKind::Dir),
                MERGE
            ),
            MergeDirs
        );
        // file/link vs file/link → atomic replace
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::FileOrLink,
                Some(ItemKind::FileOrLink),
                MERGE
            ),
            ReplaceAtomic
        );
        // file ↔ dir, both directions → conflict
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::FileOrLink,
                Some(ItemKind::Dir),
                MERGE
            ),
            Conflict
        );
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::Dir,
                Some(ItemKind::FileOrLink),
                MERGE
            ),
            Conflict
        );
        // target vanished between rename and lstat → conflict (fail safe)
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::FileOrLink,
                None,
                MERGE
            ),
            Conflict
        );
        // depth cap: dirs at ≥512 can't merge → conflict
        assert_eq!(
            place_action(
                RenameOutcome::Exists,
                true,
                ItemKind::Dir,
                Some(ItemKind::Dir),
                false
            ),
            Conflict
        );
    }

    #[test]
    fn non_atomic_fails_closed_even_with_replace() {
        // The item must NOT be moved by any fallback when the filesystem
        // can't refuse atomically — regardless of consent.
        assert_eq!(
            place_action(
                RenameOutcome::NonAtomic,
                true,
                ItemKind::FileOrLink,
                Some(ItemKind::FileOrLink),
                MERGE
            ),
            FailNonAtomic
        );
        assert_eq!(
            place_action(RenameOutcome::NonAtomic, false, ItemKind::Dir, None, MERGE),
            FailNonAtomic
        );
    }

    #[test]
    fn other_errors_fail() {
        // EXDEV/EACCES/EIO → FailOther (never a silent retry elsewhere).
        assert_eq!(
            place_action(
                RenameOutcome::OtherError,
                true,
                ItemKind::FileOrLink,
                Some(ItemKind::FileOrLink),
                MERGE
            ),
            FailOther
        );
    }

    #[test]
    fn tier_selection() {
        assert_eq!(remote_tier(true), RemoteTier::HardlinkNoReplace);
        assert_eq!(remote_tier(false), RemoteTier::CheckThenRename);
    }

    #[cfg(unix)]
    #[test]
    fn errno_mapping() {
        assert_eq!(rename_errno_outcome(libc::EEXIST), RenameOutcome::Exists);
        assert_eq!(rename_errno_outcome(libc::EINVAL), RenameOutcome::NonAtomic);
        assert_eq!(rename_errno_outcome(libc::ENOSYS), RenameOutcome::NonAtomic);
        assert_eq!(
            rename_errno_outcome(libc::EOPNOTSUPP),
            RenameOutcome::NonAtomic
        );
        assert_eq!(rename_errno_outcome(libc::EXDEV), RenameOutcome::OtherError);
        assert_eq!(
            rename_errno_outcome(libc::EACCES),
            RenameOutcome::OtherError
        );
        assert_eq!(
            rename_errno_outcome(libc::EISDIR),
            RenameOutcome::OtherError
        );
    }
}
