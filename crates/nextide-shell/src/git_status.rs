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
//!   classify as [`StatusKind::Clean`]. Each kind also exposes a stable
//!   single-character badge via [`StatusKind::marker`] (`!`/`M`/`S`/`U`/`?`/
//!   single space).
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
//! - [`summary_text`] projects a [`GitStatusSummary`] into the stable
//!   status-bar string (the function's docs give the exact format): `"clean"`
//!   when nothing is pending, a `"changed: …"` line when changes exist without
//!   conflicts, and a `"conflicted: …"` line when any entry is unmerged —
//!   always the same keys, order and single-space separators, so the text is
//!   parseable and never changes between releases.
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

    /// A stable, single-character visual marker for tree-row badges.
    ///
    /// Each variant maps to exactly one ASCII character, so badge columns can
    /// keep a fixed width and the mapping never changes between releases:
    ///
    /// - [`StatusKind::Conflicted`] → `"!"` (unmerged/conflict)
    /// - [`StatusKind::StagedAndUnstaged`] → `"M"` (modified on both sides)
    /// - [`StatusKind::Staged`] → `"S"` (staged in the index)
    /// - [`StatusKind::Unstaged`] → `"U"` (worktree-only change)
    /// - [`StatusKind::Untracked`] → `"?"`
    /// - [`StatusKind::Clean`] → `" "` (a single space: no badge)
    ///
    /// The marker is derived purely from the kind, so it is available even
    /// without a kernel snapshot; UIs that need the exact porcelain letter
    /// (`M`/`A`/`D`/…) can read [`TreeStatus::state`] instead.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Conflicted => "!",
            Self::StagedAndUnstaged => "M",
            Self::Staged => "S",
            Self::Unstaged => "U",
            Self::Untracked => "?",
            Self::Clean => " ",
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

    /// Projects this summary into the stable status-bar text; see the free
    /// [`summary_text`] function for the exact format.
    pub fn summary_text(&self) -> String {
        summary_text(self)
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

/// Projects a summary into the stable status-bar text.
///
/// The returned string is a fixed, parseable format that never changes
/// between releases:
///
/// - `"clean"` — no pending changes at all (`conflicted`, `staged`,
///   `unstaged` and `untracked` are all zero; this is the only state that
///   does not carry counts).
/// - `"changed: staged={n} unstaged={n} untracked={n}"` — changes exist but
///   no entry is unmerged. Every key is always present, in this order.
/// - `"conflicted: conflicted={n} staged={n} unstaged={n} untracked={n}"` —
///   at least one unmerged entry. Conflicts outrank ordinary changes, and
///   again every key is always present in this fixed order.
///
/// Counts use `Display`, keys are the lowercase [`StatusKind::as_str`] labels
/// (plus the state words `clean`/`changed`/`conflicted`), and fields are
/// separated by single spaces, so UIs can render the string as-is or parse it
/// with `strip_prefix`/`split`. The projection is pure: it is derived solely
/// from the summary's own counts, never from the snapshot, so a hand-built
/// summary projects identically to one produced by [`summary`].
pub fn summary_text(summary: &GitStatusSummary) -> String {
    if summary.conflicted == 0
        && summary.staged == 0
        && summary.unstaged == 0
        && summary.untracked == 0
    {
        "clean".to_string()
    } else if summary.conflicted > 0 {
        format!(
            "conflicted: conflicted={} staged={} unstaged={} untracked={}",
            summary.conflicted, summary.staged, summary.unstaged, summary.untracked
        )
    } else {
        format!(
            "changed: staged={} unstaged={} untracked={}",
            summary.staged, summary.unstaged, summary.untracked
        )
    }
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

    /// Projects a summary into the stable status-bar text.
    fn summary_text(&self, summary: &GitStatusSummary) -> String {
        summary_text(summary)
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
    fn status_kind_markers_are_stable_distinct_and_fixed_width() {
        let markers: Vec<&'static str> = [
            StatusKind::Conflicted,
            StatusKind::StagedAndUnstaged,
            StatusKind::Staged,
            StatusKind::Unstaged,
            StatusKind::Untracked,
            StatusKind::Clean,
        ]
        .iter()
        .map(|kind| kind.marker())
        .collect();
        assert_eq!(markers, vec!["!", "M", "S", "U", "?", " "]);
        // Every variant has its own marker, so a badge column never
        // conflates two kinds, and every marker is exactly one character.
        let mut sorted = markers.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), markers.len());
        assert!(markers.iter().all(|marker| marker.len() == 1));
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
    fn summary_text_projects_clean_changed_and_conflicted() {
        assert_eq!(GitStatusSummary::default().summary_text(), "clean");
        assert_eq!(summary_text(&GitStatusSummary::default()), "clean");

        let changed = GitStatusSummary {
            total: 4,
            staged: 1,
            unstaged: 2,
            untracked: 1,
            conflicted: 0,
        };
        assert_eq!(
            summary_text(&changed),
            "changed: staged=1 unstaged=2 untracked=1"
        );

        let conflicted = GitStatusSummary {
            total: 3,
            staged: 1,
            unstaged: 0,
            untracked: 0,
            conflicted: 2,
        };
        assert_eq!(
            summary_text(&conflicted),
            "conflicted: conflicted=2 staged=1 unstaged=0 untracked=0"
        );
    }

    #[test]
    fn summary_text_conflicts_outrank_ordinary_changes() {
        let summary = GitStatusSummary {
            total: 5,
            staged: 3,
            unstaged: 1,
            untracked: 1,
            conflicted: 1,
        };
        assert_eq!(
            summary_text(&summary),
            "conflicted: conflicted=1 staged=3 unstaged=1 untracked=1"
        );
    }

    #[test]
    fn summary_text_is_deterministic_for_any_counts() {
        // Classification reads the buckets themselves, not `total`, so even a
        // hand-built summary that disagrees with its own total projects
        // deterministically: all-zero buckets are clean, and a zero-total
        // summary with conflicts still reports them.
        assert_eq!(
            summary_text(&GitStatusSummary {
                total: 0,
                conflicted: 1,
                ..GitStatusSummary::default()
            }),
            "conflicted: conflicted=1 staged=0 unstaged=0 untracked=0"
        );
        assert_eq!(
            summary_text(&GitStatusSummary {
                total: 1,
                ..GitStatusSummary::default()
            }),
            "clean"
        );
    }

    #[test]
    fn summary_text_uses_fixed_keys_in_fixed_order() {
        // The exact strings are the contract; re-assert the key order so a
        // reformat can never silently reorder fields.
        let changed = GitStatusSummary {
            total: 6,
            staged: 1,
            unstaged: 2,
            untracked: 3,
            conflicted: 0,
        };
        let text = summary_text(&changed);
        assert!(text.starts_with("changed: "));
        let fields: Vec<&str> = text["changed: ".len()..].split(' ').collect();
        assert_eq!(fields, vec!["staged=1", "unstaged=2", "untracked=3"]);

        let conflicted = GitStatusSummary {
            total: 6,
            staged: 1,
            unstaged: 2,
            untracked: 3,
            conflicted: 4,
        };
        let text = summary_text(&conflicted);
        assert!(text.starts_with("conflicted: "));
        let fields: Vec<&str> = text["conflicted: ".len()..].split(' ').collect();
        assert_eq!(
            fields,
            vec!["conflicted=4", "staged=1", "unstaged=2", "untracked=3"]
        );
    }

    #[test]
    fn summary_text_projects_a_real_snapshot_summary() {
        let snapshot = snapshot(vec![
            entry("??", "untracked.txt", None),
            entry("M ", "staged.rs", None),
            entry(" M", "unstaged.rs", None),
            entry("UU", "conflict.rs", None),
        ]);
        assert_eq!(
            summary(&snapshot).summary_text(),
            "conflicted: conflicted=1 staged=1 unstaged=1 untracked=1"
        );
    }

    #[test]
    fn summary_text_matches_the_model_method_and_the_adapter() {
        let adapter = DefaultGitStatusAdapter;
        let summary = GitStatusSummary {
            total: 2,
            staged: 1,
            unstaged: 0,
            untracked: 0,
            conflicted: 1,
        };
        assert_eq!(summary.summary_text(), summary_text(&summary));
        assert_eq!(adapter.summary_text(&summary), summary_text(&summary));
        let clean = GitStatusSummary::default();
        assert_eq!(clean.summary_text(), adapter.summary_text(&clean));
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
