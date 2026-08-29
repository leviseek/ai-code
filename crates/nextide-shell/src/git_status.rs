//! Pure, dependency-free Git status adaptation for the shell UI.
//!
//! This is a thin *trait/model layer* that lives entirely in the shell: it
//! maps a kernel [`GitStatusSnapshot`] into typed, stably-ordered file-tree
//! status markers and a summary that UI code can consume directly. The kernel
//! is never modified and no shell behaviour is changed — the mapping is pure
//! (`&GitStatusSnapshot` in, value types out) and adds no dependencies.
//!
//! The mapping is deliberately one entry → one marker:
//!
//! - [`status_kind`] classifies a raw porcelain [`GitFileState`] (the `XY`
//!   columns) into a [`StatusKind`]. Untracked wins over everything,
//!   conflicts beat ordinary changes, a two-sided change
//!   ([`StatusKind::StagedAndUnstaged`]) beats a one-sided one, and states
//!   with no change (including `Ignored`, which `status` never reports)
//!   classify as [`StatusKind::Clean`].
//! - [`tree_statuses`] projects every entry of a snapshot into a
//!   [`TreeStatus`] and re-sorts them stably by path, so the model's path
//!   order is its own contract even if the snapshot's ordering ever changed.
//!   Renames and copies keep the source path in [`TreeStatus::original`]; the
//!   model does not invent a second marker for the disappearing path — UIs
//!   that render it can derive it from `original`.
//! - [`summary`] derives the classic status-bar counts ([`GitStatusSummary`]):
//!   a two-sided entry counts toward both `staged` and `unstaged`, conflicts
//!   form their own bucket, and untracked entries are separate. `total` is the
//!   snapshot's entry count, so a clean snapshot is all zeros.
//! - [`marker_for`] looks up one path's marker for tree-row badges; paths are
//!   matched as-is (snapshot paths are relative to the repository top level,
//!   which equals the workspace root in the normal case).
//!
//! [`GitStatusAdapter`] is the mapping contract, with default methods that
//! delegate to the free functions; [`DefaultGitStatusAdapter`] is the
//! ready-to-use unit implementation. Consumers may pass the adapter around as
//! a trait object or call the free functions directly.

use nextide_kernel::git::{GitFileState, GitStatusCode, GitStatusEntry, GitStatusSnapshot};
use std::path::{Path, PathBuf};

/// UI-facing classification of one path's Git status, derived purely from a
/// porcelain [`GitFileState`].
///
/// Variants are ordered by severity (ascending `Ord`): conflicts outrank
/// two-sided changes, which outrank one-sided changes, which outrank
/// untracked, which outranks clean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatusKind {
    /// An unmerged (conflict) side: `U` in either the index or worktree
    /// column.
    Conflicted,
    /// Changes staged in the index *and* further changes in the worktree
    /// (for example `MM`).
    StagedAndUnstaged,
    /// A change staged in the index only (for example `M `, `A `, `D `,
    /// `R `, `C `).
    Staged,
    /// A change present in the worktree only (for example ` M`, ` D`).
    Unstaged,
    /// Untracked (`??`).
    Untracked,
    /// No pending change. `status` never reports plain no-change entries;
    /// this also covers `Ignored` states, which are only emitted with
    /// `--ignored` and therefore never reach this layer.
    Clean,
}

impl StatusKind {
    /// A stable, lowercase label suitable for UI badges and event payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conflicted => "conflicted",
            Self::StagedAndUnstaged => "staged-and-unstaged",
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Untracked => "untracked",
            Self::Clean => "clean",
        }
    }

    /// Whether this kind carries any pending change (everything but
    /// [`StatusKind::Clean`]).
    pub fn is_change(self) -> bool {
        self != Self::Clean
    }
}

/// One file-tree status marker: a path plus its UI classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeStatus {
    /// The path relative to the repository top level (the workspace root in
    /// the normal case).
    pub path: PathBuf,
    /// The combined UI classification of the entry's `XY` state.
    pub kind: StatusKind,
    /// The raw porcelain state, for UIs that want the exact two columns.
    pub state: GitFileState,
    /// The source path for renames and copies; `None` for all other statuses.
    pub original: Option<PathBuf>,
}

impl TreeStatus {
    /// Projects one kernel status entry into a tree marker without
    /// re-ordering.
    pub fn from_entry(entry: &GitStatusEntry) -> Self {
        Self {
            path: PathBuf::from(entry.path.as_str()),
            kind: status_kind(entry.state),
            state: entry.state,
            original: entry.original.as_ref().map(PathBuf::from),
        }
    }

    /// Whether this entry is a rename or copy (it carries an `original`).
    pub fn is_rename_or_copy(&self) -> bool {
        self.original.is_some()
    }
}

/// UI-facing summary counts derived from a snapshot — the status-bar model.
///
/// `staged` and `unstaged` each merge the two-sided entries that belong to
/// them ([`StatusKind::StagedAndUnstaged`] counts toward both), conflicts
/// count only toward `conflicted`, and untracked entries only toward
/// `untracked`, so `total` is exactly the snapshot's entry count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GitStatusSummary {
    /// The number of status entries in the snapshot.
    pub total: usize,
    /// Entries with a staged (index-side) change, two-sided entries included.
    pub staged: usize,
    /// Entries with an unstaged (worktree-side) change, two-sided entries
    /// included; untracked entries are not counted here.
    pub unstaged: usize,
    /// Untracked entries (`??`).
    pub untracked: usize,
    /// Unmerged (conflict) entries.
    pub conflicted: usize,
}

impl GitStatusSummary {
    /// Whether the repository has no pending changes at all.
    pub fn is_clean(&self) -> bool {
        self.total == 0
    }
}

impl From<&GitStatusSnapshot> for GitStatusSummary {
    fn from(snapshot: &GitStatusSnapshot) -> Self {
        summary(snapshot)
    }
}

/// Classifies a raw porcelain state into a UI-facing [`StatusKind`].
///
/// Untracked (`??`) wins over everything; any unmerged side wins over
/// ordinary changes; a change on both sides beats a one-sided change; states
/// with no change classify as [`StatusKind::Clean`].
pub fn status_kind(state: GitFileState) -> StatusKind {
    if state.is_untracked() {
        StatusKind::Untracked
    } else if state.index == GitStatusCode::Unmerged || state.worktree == GitStatusCode::Unmerged {
        StatusKind::Conflicted
    } else if state.is_staged() && state.is_unstaged() {
        StatusKind::StagedAndUnstaged
    } else if state.is_staged() {
        StatusKind::Staged
    } else if state.is_unstaged() {
        StatusKind::Unstaged
    } else {
        StatusKind::Clean
    }
}

/// Maps a snapshot into typed tree markers, stably sorted by path.
///
/// The snapshot's entries are projected one-for-one and re-sorted with a
/// stable sort by [`TreeStatus::path`], so the returned order is guaranteed by
/// this module regardless of the snapshot's internal ordering; entries with
/// equal paths keep their input order.
pub fn tree_statuses(snapshot: &GitStatusSnapshot) -> Vec<TreeStatus> {
    let mut statuses: Vec<TreeStatus> = snapshot
        .entries
        .iter()
        .map(TreeStatus::from_entry)
        .collect();
    statuses.sort_by(|a, b| a.path.cmp(&b.path));
    statuses
}

/// Derives the status-bar summary counts for a snapshot.
pub fn summary(snapshot: &GitStatusSnapshot) -> GitStatusSummary {
    let mut out = GitStatusSummary {
        total: snapshot.entries.len(),
        ..GitStatusSummary::default()
    };
    for entry in &snapshot.entries {
        match status_kind(entry.state) {
            StatusKind::Conflicted => out.conflicted += 1,
            StatusKind::StagedAndUnstaged => {
                out.staged += 1;
                out.unstaged += 1;
            }
            StatusKind::Staged => out.staged += 1,
            StatusKind::Unstaged => out.unstaged += 1,
            StatusKind::Untracked => out.untracked += 1,
            StatusKind::Clean => {}
        }
    }
    out
}

/// Looks up the marker for one tree path, if the snapshot reports it.
///
/// Paths are matched as-is against the snapshot's entries, whose paths are
/// relative to the repository top level. Returns `None` for paths the
/// snapshot does not report (clean or absent paths). For the (rare) case of
/// multiple entries with the same path, the first in snapshot order wins.
pub fn marker_for(snapshot: &GitStatusSnapshot, path: &Path) -> Option<StatusKind> {
    snapshot
        .entries
        .iter()
        .find(|entry| Path::new(entry.path.as_str()) == path)
        .map(|entry| status_kind(entry.state))
}

/// The mapping contract from a kernel snapshot to the UI-facing models.
///
/// Every method has a default implementation delegating to the module's free
/// functions, so a custom adapter (for example one that re-themes markers)
/// only overrides what it needs. The free functions are the canonical
/// mapping: adapters that do not override a method observe exactly their
/// results.
pub trait GitStatusAdapter {
    /// Classifies one porcelain state into a [`StatusKind`].
    fn kind_of(&self, state: GitFileState) -> StatusKind {
        status_kind(state)
    }

    /// Maps a snapshot into typed tree markers, stably sorted by path.
    fn tree_statuses(&self, snapshot: &GitStatusSnapshot) -> Vec<TreeStatus> {
        tree_statuses(snapshot)
    }

    /// Derives the status-bar summary counts for a snapshot.
    fn summary(&self, snapshot: &GitStatusSnapshot) -> GitStatusSummary {
        summary(snapshot)
    }

    /// Looks up the marker for one tree path, if the snapshot reports it.
    fn marker_for(&self, snapshot: &GitStatusSnapshot, path: &Path) -> Option<StatusKind> {
        marker_for(snapshot, path)
    }
}

/// The default adapter: exactly the free-function mapping, nothing more.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultGitStatusAdapter;

impl GitStatusAdapter for DefaultGitStatusAdapter {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps one porcelain byte to its kernel code (mirrors the kernel's
    /// private parser so tests can build any XY combination).
    fn code(byte: u8) -> GitStatusCode {
        match byte {
            b' ' => GitStatusCode::Unmodified,
            b'M' => GitStatusCode::Modified,
            b'T' => GitStatusCode::TypeChanged,
            b'A' => GitStatusCode::Added,
            b'D' => GitStatusCode::Deleted,
            b'R' => GitStatusCode::Renamed,
            b'C' => GitStatusCode::Copied,
            b'U' => GitStatusCode::Unmerged,
            b'?' => GitStatusCode::Untracked,
            b'!' => GitStatusCode::Ignored,
            other => GitStatusCode::Unknown(other as char),
        }
    }

    fn state(xy: &str) -> GitFileState {
        let bytes = xy.as_bytes();
        assert_eq!(bytes.len(), 2, "XY code must be two bytes");
        GitFileState::new(code(bytes[0]), code(bytes[1]))
    }

    fn entry(xy: &str, path: &str, original: Option<&str>) -> GitStatusEntry {
        GitStatusEntry {
            state: state(xy),
            path: path.to_string(),
            original: original.map(str::to_string),
        }
    }

    fn snapshot(entries: Vec<GitStatusEntry>) -> GitStatusSnapshot {
        GitStatusSnapshot { entries }
    }

    #[test]
    fn status_kind_classifies_every_xy_combination() {
        let cases = [
            ("  ", StatusKind::Clean),
            ("??", StatusKind::Untracked),
            ("M ", StatusKind::Staged),
            ("A ", StatusKind::Staged),
            ("D ", StatusKind::Staged),
            ("T ", StatusKind::Staged),
            ("R ", StatusKind::Staged),
            ("C ", StatusKind::Staged),
            (" M", StatusKind::Unstaged),
            (" A", StatusKind::Unstaged),
            (" D", StatusKind::Unstaged),
            (" T", StatusKind::Unstaged),
            ("MM", StatusKind::StagedAndUnstaged),
            ("RM", StatusKind::StagedAndUnstaged),
            ("AM", StatusKind::StagedAndUnstaged),
            ("UU", StatusKind::Conflicted),
            ("AU", StatusKind::Conflicted),
            ("UA", StatusKind::Conflicted),
            ("UD", StatusKind::Conflicted),
            ("DU", StatusKind::Conflicted),
            ("!!", StatusKind::Clean),
        ];
        for (xy, expected) in cases {
            assert_eq!(status_kind(state(xy)), expected, "XY = {xy}");
        }
    }

    #[test]
    fn status_kind_labels_are_stable_and_documentable() {
        assert_eq!(StatusKind::Conflicted.as_str(), "conflicted");
        assert_eq!(
            StatusKind::StagedAndUnstaged.as_str(),
            "staged-and-unstaged"
        );
        assert_eq!(StatusKind::Staged.as_str(), "staged");
        assert_eq!(StatusKind::Unstaged.as_str(), "unstaged");
        assert_eq!(StatusKind::Untracked.as_str(), "untracked");
        assert_eq!(StatusKind::Clean.as_str(), "clean");
        assert!(StatusKind::Staged.is_change());
        assert!(!StatusKind::Clean.is_change());
    }

    #[test]
    fn tree_statuses_project_entries_and_sort_stably_by_path() {
        let snapshot = snapshot(vec![
            entry("??", "notes.md", None),
            entry("MM", "z-last.rs", None),
            entry(" M", "b-unstaged.rs", None),
            entry("R ", "c-renamed.rs", Some("old-name.rs")),
            entry("M ", "a-staged.rs", None),
        ]);
        let statuses = tree_statuses(&snapshot);
        let paths: Vec<&std::path::Path> = statuses
            .iter()
            .map(|status| status.path.as_path())
            .collect();
        assert_eq!(
            paths,
            vec![
                std::path::Path::new("a-staged.rs"),
                std::path::Path::new("b-unstaged.rs"),
                std::path::Path::new("c-renamed.rs"),
                std::path::Path::new("notes.md"),
                std::path::Path::new("z-last.rs"),
            ]
        );
        assert_eq!(statuses[0].kind, StatusKind::Staged);
        assert_eq!(statuses[1].kind, StatusKind::Unstaged);
        assert_eq!(statuses[2].kind, StatusKind::Staged);
        assert_eq!(statuses[3].kind, StatusKind::Untracked);
        assert_eq!(statuses[4].kind, StatusKind::StagedAndUnstaged);
        assert_eq!(
            statuses[2].original,
            Some(std::path::PathBuf::from("old-name.rs"))
        );
    }

    #[test]
    fn tree_statuses_keep_input_order_for_equal_paths() {
        let snapshot = snapshot(vec![
            entry("M ", "dup.rs", None),
            entry(" M", "dup.rs", None),
        ]);
        let statuses = tree_statuses(&snapshot);
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].kind, StatusKind::Staged);
        assert_eq!(statuses[1].kind, StatusKind::Unstaged);
    }

    #[test]
    fn tree_status_from_entry_keeps_rename_and_copy_originals() {
        let renamed = TreeStatus::from_entry(&entry("R ", "new.rs", Some("old.rs")));
        assert!(renamed.is_rename_or_copy());
        assert_eq!(renamed.original, Some(std::path::PathBuf::from("old.rs")));
        assert_eq!(renamed.state, state("R "));
        let copied = TreeStatus::from_entry(&entry("C ", "copy.rs", Some("src.rs")));
        assert!(copied.is_rename_or_copy());
        let modified = TreeStatus::from_entry(&entry(" M", "plain.rs", None));
        assert!(!modified.is_rename_or_copy());
        assert_eq!(modified.original, None);
    }

    #[test]
    fn summary_counts_typed_buckets_and_total() {
        let snapshot = snapshot(vec![
            entry("??", "untracked.txt", None),
            entry("M ", "staged.rs", None),
            entry(" M", "unstaged.rs", None),
            entry("MM", "both.rs", None),
            entry("UU", "conflict.rs", None),
            entry("R ", "renamed.rs", Some("old.rs")),
            entry(" A", "added-wt.rs", None),
            entry(" D", "deleted-wt.rs", None),
        ]);
        let summary = summary(&snapshot);
        assert_eq!(summary.total, 8);
        // staged.rs, both.rs, renamed.rs
        assert_eq!(summary.staged, 3);
        // unstaged.rs, both.rs, added-wt.rs, deleted-wt.rs
        assert_eq!(summary.unstaged, 4);
        assert_eq!(summary.untracked, 1);
        assert_eq!(summary.conflicted, 1);
        assert!(!summary.is_clean());
        assert_eq!(GitStatusSummary::from(&snapshot), summary);
    }

    #[test]
    fn summary_of_clean_snapshot_is_clean() {
        let clean = summary(&snapshot(Vec::new()));
        assert!(clean.is_clean());
        assert_eq!(
            clean,
            GitStatusSummary {
                total: 0,
                staged: 0,
                unstaged: 0,
                untracked: 0,
                conflicted: 0,
            }
        );
        assert!(tree_statuses(&snapshot(Vec::new())).is_empty());
    }

    #[test]
    fn marker_for_looks_up_paths_and_misses_others() {
        let snapshot = snapshot(vec![
            entry("M ", "src/main.rs", None),
            entry("??", "scratch/notes.md", None),
        ]);
        assert_eq!(
            marker_for(&snapshot, std::path::Path::new("src/main.rs")),
            Some(StatusKind::Staged)
        );
        assert_eq!(
            marker_for(&snapshot, std::path::Path::new("scratch/notes.md")),
            Some(StatusKind::Untracked)
        );
        assert_eq!(
            marker_for(&snapshot, std::path::Path::new("Cargo.toml")),
            None
        );
        assert_eq!(marker_for(&snapshot, std::path::Path::new("src")), None);
    }

    #[test]
    fn default_adapter_matches_the_free_functions() {
        let adapter = DefaultGitStatusAdapter;
        let snapshot = snapshot(vec![
            entry("MM", "both.rs", None),
            entry("R ", "renamed.rs", Some("old.rs")),
            entry("??", "new.txt", None),
        ]);
        assert_eq!(adapter.kind_of(state("AU")), status_kind(state("AU")));
        assert_eq!(adapter.tree_statuses(&snapshot), tree_statuses(&snapshot));
        assert_eq!(adapter.summary(&snapshot), summary(&snapshot));
        assert_eq!(
            adapter.marker_for(&snapshot, std::path::Path::new("new.txt")),
            marker_for(&snapshot, std::path::Path::new("new.txt"))
        );
    }
}
