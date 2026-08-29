//! Pure, dependency-free Git diff adaptation for the shell UI.
//!
//! This is a thin *trait/model layer* that lives entirely in the shell: it
//! maps a kernel [`GitDiffSnapshot`] into typed, stably-ordered file and line
//! views plus a line-count summary that UI code can consume directly. The
//! kernel is never modified and no shell behaviour is changed — the mapping is
//! pure (`&GitDiffSnapshot` in, value types out) and adds no dependencies. The
//! shell never runs git; snapshots come from outside (for example from
//! `nextide_kernel::git::diff`), so this module has no `Command`, no `Path`
//! I/O and no kernel changes.
//!
//! The mapping is deliberately one snapshot → stable views:
//!
//! - [`file_views`] projects every file of a snapshot into a [`DiffFileView`]
//!   and re-sorts them *stably* by path, so the model's path order is its own
//!   contract even if the snapshot's internal ordering ever changed; files
//!   with equal paths keep their input order. Each view keeps the kernel's
//!   current (`b/`) path — already decoded from git's C-style quoting — the
//!   `binary` flag and the hunks in output order.
//! - Every hunk line becomes a [`DiffLineView`] that preserves the kernel's
//!   1-based `old_line`/`new_line` (both `Some` for context, `None` on the
//!   absent side for added/removed lines) and its [`DiffLineKind`], which also
//!   exposes a stable label and a stable single-character marker (`' '` /
//!   `'+'` / `'-'`) for gutter badges. Hunk and line order is the kernel's
//!   output order, so the numbers and kinds are stable for a given snapshot.
//! - [`summary`] derives the status-bar counts ([`DiffSummary`]): changed
//!   files, binary files and the total added/removed/context lines. Binary
//!   files carry no hunks, so they count as files but never as lines; a file
//!   whose change is metadata-only (for example a mode change) counts as a
//!   file with zero lines. A clean snapshot is all zeros.
//! - [`summary_text`] projects a [`DiffSummary`] into the stable status-bar
//!   string (the function's docs give the exact format): `"clean"` when
//!   nothing is pending, otherwise a `"changed: …"` line — always the same
//!   keys, order and single-space separators, so the text is parseable and
//!   never changes between releases.
//! - [`file_view_for`] looks up one file's view by path; paths are matched
//!   as-is (snapshot paths are relative to the repository top level, which
//!   equals the workspace root in the normal case). For the (rare) case of
//!   multiple files with the same path, the first in snapshot order wins,
//!   mirroring the status layer's `marker_for`.
//! - Display projections turn each view into stable text without drawing
//!   anything: [`DiffLineView::display_text`] renders `"<marker> <text>"`
//!   (the single-character kind marker, one space, then the line text),
//!   [`DiffHunkView::display_header`] renders the raw `@@` header line, and
//!   [`DiffFileView::file_label`] renders the file's path — with a `" (binary)"`
//!   suffix for binary files. The formats are fixed and documented, so a diff
//!   panel can lay these out as text (no GPU, no real drawing) and the text
//!   never changes between releases.
//!
//! [`GitDiffAdapter`] is the mapping contract, with default methods that
//! delegate to the free functions; [`DefaultGitDiffAdapter`] is the
//! ready-to-use unit implementation. Consumers may pass the adapter around as
//! a trait object or call the free functions directly.

use nextide_kernel::{GitDiffFile, GitDiffHunk, GitDiffLine, GitDiffLineKind, GitDiffSnapshot};
use std::path::{Path, PathBuf};

/// UI-facing classification of one line of a diff hunk, mapped purely from a
/// kernel [`GitDiffLineKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    /// A line present in both the old and the new file.
    Context,
    /// A line that exists only in the new file.
    Added,
    /// A line that exists only in the old file.
    Removed,
}

impl DiffLineKind {
    /// A stable, lowercase label suitable for UI badges and event payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }

    /// A stable, single-character visual marker for diff gutters.
    ///
    /// Each variant maps to exactly one ASCII character, so the gutter column
    /// can keep a fixed width and the mapping never changes between releases:
    ///
    /// - [`DiffLineKind::Context`] → `" "` (a single space: no marker)
    /// - [`DiffLineKind::Added`] → `"+"`
    /// - [`DiffLineKind::Removed`] → `"-"`
    ///
    /// The marker is derived purely from the kind, so it is available even
    /// without a kernel snapshot.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Context => " ",
            Self::Added => "+",
            Self::Removed => "-",
        }
    }
}

impl From<GitDiffLineKind> for DiffLineKind {
    fn from(kind: GitDiffLineKind) -> Self {
        match kind {
            GitDiffLineKind::Context => Self::Context,
            GitDiffLineKind::Added => Self::Added,
            GitDiffLineKind::Removed => Self::Removed,
        }
    }
}

/// One UI-consumable line of a diff hunk, with its stable 1-based line
/// numbers.
///
/// Line numbers are relative to the repository's file content and are
/// preserved exactly from the kernel: a context line exists on both sides,
/// an [`DiffLineKind::Added`] line only on the new side, a
/// [`DiffLineKind::Removed`] line only on the old side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLineView {
    /// Whether this line is context, an addition or a removal.
    pub kind: DiffLineKind,
    /// The 1-based line number in the old (before) file; `None` for added
    /// lines.
    pub old_line: Option<u32>,
    /// The 1-based line number in the new (after) file; `None` for removed
    /// lines.
    pub new_line: Option<u32>,
    /// The line content without the leading diff marker or the trailing
    /// newline. Bytes that were not valid UTF-8 were replaced with U+FFFD by
    /// the kernel.
    pub text: String,
}

impl DiffLineView {
    /// Projects one kernel diff line into a view without re-ordering.
    pub fn from_line(line: &GitDiffLine) -> Self {
        Self {
            kind: DiffLineKind::from(line.kind),
            old_line: line.old_line,
            new_line: line.new_line,
            text: line.text.clone(),
        }
    }

    /// The stable display text for this line: the single-character kind
    /// marker, one space, then the line text.
    ///
    /// The exact format is `"<marker> <text>"` (marker + one space + text),
    /// so the marker column keeps a fixed width of one character:
    ///
    /// - [`DiffLineKind::Context`] → `"  <text>"` (the `' '` marker plus the
    ///   separator, i.e. two leading spaces before the text)
    /// - [`DiffLineKind::Added`] → `"+ <text>"`
    /// - [`DiffLineKind::Removed`] → `"- <text>"`
    ///
    /// The separator is always present, so a blank line (empty `text`) still
    /// renders as `"<marker> "` with its marker and one trailing space. The
    /// projection is pure — it is derived solely from `kind` and `text` — and
    /// the exact string is the contract: it never changes between releases.
    pub fn display_text(&self) -> String {
        format!("{} {}", self.kind.marker(), self.text)
    }
}

/// One UI-consumable hunk of a file diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunkView {
    /// The raw hunk header line as git printed it, for example
    /// `@@ -1,5 +1,6 @@` (with any section heading preserved).
    pub header: String,
    /// The hunk's lines in output order.
    pub lines: Vec<DiffLineView>,
}

impl DiffHunkView {
    /// Projects one kernel diff hunk into a view without re-ordering.
    pub fn from_hunk(hunk: &GitDiffHunk) -> Self {
        Self {
            header: hunk.header.clone(),
            lines: hunk.lines.iter().map(DiffLineView::from_line).collect(),
        }
    }

    /// The stable display text for this hunk's header: the raw `@@` line
    /// exactly as git printed it, for example `@@ -1,5 +1,6 @@` (with any
    /// section heading preserved).
    ///
    /// The projection is pure — it returns the stored header verbatim — so a
    /// diff panel can render the hunk header as text (no GPU, no real
    /// drawing) and the exact string never changes between releases.
    pub fn display_header(&self) -> &str {
        &self.header
    }
}

/// The UI-consumable diff of one file.
///
/// The path is the kernel's `b/` side — the file's current path relative to
/// the repository top level, with git's C-style quoting already decoded.
/// Hunks stay in the kernel's output order, so line numbers are stable for a
/// given snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFileView {
    /// The file's path relative to the repository top level (the workspace
    /// root in the normal case).
    pub path: PathBuf,
    /// Whether git reported this as a binary diff (`Binary files ...
    /// differ`); binary files carry no hunks.
    pub binary: bool,
    /// The hunks in output order; empty for binary files and for changes that
    /// only touch metadata (for example a mode change).
    pub hunks: Vec<DiffHunkView>,
}

impl DiffFileView {
    /// Projects one kernel diff file into a view without re-ordering.
    pub fn from_file(file: &GitDiffFile) -> Self {
        Self {
            path: PathBuf::from(file.path.as_str()),
            binary: file.binary,
            hunks: file.hunks.iter().map(DiffHunkView::from_hunk).collect(),
        }
    }

    /// The stable file-section label for a diff panel: the file's path with
    /// `/` separators, plus a `" (binary)"` suffix for binary files.
    ///
    /// The exact format is `"<path>"` for text files and `"<path> (binary)"`
    /// for binary files — the only suffix that exists, so the label is
    /// parseable (`strip_suffix`) and never changes between releases. The
    /// path is rebuilt from its components joined by `/`, so the label is
    /// identical on every platform even though the stored [`Self::path`] is a
    /// platform-native `PathBuf`; the projection is pure — it never touches
    /// git or the filesystem.
    pub fn file_label(&self) -> String {
        let path = self
            .path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if self.binary {
            format!("{path} (binary)")
        } else {
            path
        }
    }

    /// Whether git reported this as a binary diff.
    pub fn is_binary(&self) -> bool {
        self.binary
    }

    /// The number of hunks (0 for binary and metadata-only changes).
    pub fn hunk_count(&self) -> usize {
        self.hunks.len()
    }

    /// The total number of lines across all hunks.
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }

    /// The number of added lines across all hunks.
    pub fn added_lines(&self) -> usize {
        self.lines_by_kind(DiffLineKind::Added)
    }

    /// The number of removed lines across all hunks.
    pub fn removed_lines(&self) -> usize {
        self.lines_by_kind(DiffLineKind::Removed)
    }

    /// The number of context lines across all hunks.
    pub fn context_lines(&self) -> usize {
        self.lines_by_kind(DiffLineKind::Context)
    }

    fn lines_by_kind(&self, kind: DiffLineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == kind)
            .count()
    }
}

/// UI-facing summary counts derived from a diff snapshot — the status-bar
/// model.
///
/// `files` is the snapshot's file count (binary and metadata-only changes
/// included). Line counts are derived from the hunks: binary files carry no
/// hunks and therefore never contribute lines, and
/// `added + removed + context` equals the total number of lines across all
/// hunks of all files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffSummary {
    /// The number of changed files in the snapshot.
    pub files: usize,
    /// Files git reported as binary (`Binary files ... differ`).
    pub binary_files: usize,
    /// The total number of added lines across all files.
    pub added: usize,
    /// The total number of removed lines across all files.
    pub removed: usize,
    /// The total number of context lines across all files.
    pub context: usize,
}

impl DiffSummary {
    /// Whether the snapshot has no changed files.
    pub fn is_clean(&self) -> bool {
        self.files == 0
    }

    /// The total number of lines across all files (`added + removed +
    /// context`).
    pub fn total_lines(&self) -> usize {
        self.added + self.removed + self.context
    }

    /// Projects this summary into the stable status-bar text; see the free
    /// [`summary_text`] function for the exact format.
    pub fn summary_text(&self) -> String {
        summary_text(self)
    }
}

impl From<&GitDiffSnapshot> for DiffSummary {
    fn from(snapshot: &GitDiffSnapshot) -> Self {
        summary(snapshot)
    }
}

/// Maps a diff snapshot into typed file views, stably sorted by path.
///
/// The snapshot's files are projected one-for-one and re-sorted with a stable
/// sort by [`DiffFileView::path`], so the returned order is guaranteed by this
/// module regardless of the snapshot's internal ordering; files with equal
/// paths keep their input order.
pub fn file_views(snapshot: &GitDiffSnapshot) -> Vec<DiffFileView> {
    let mut views: Vec<DiffFileView> = snapshot.files.iter().map(DiffFileView::from_file).collect();
    views.sort_by(|a, b| a.path.cmp(&b.path));
    views
}

/// Looks up the view for one file path, if the snapshot reports it.
///
/// Paths are matched as-is against the snapshot's files, whose paths are
/// relative to the repository top level. Returns `None` for paths the
/// snapshot does not report. For the (rare) case of multiple files with the
/// same path, the first in snapshot order wins.
pub fn file_view_for(snapshot: &GitDiffSnapshot, path: &Path) -> Option<DiffFileView> {
    snapshot
        .files
        .iter()
        .find(|file| Path::new(file.path.as_str()) == path)
        .map(DiffFileView::from_file)
}

/// Derives the status-bar summary counts for a diff snapshot.
///
/// `files` is the snapshot's own file count; `binary_files` counts the files
/// whose `binary` flag is set; and the line buckets are summed across every
/// hunk of every file, so binary files (which carry no hunks) and
/// metadata-only changes never contribute lines.
pub fn summary(snapshot: &GitDiffSnapshot) -> DiffSummary {
    let mut out = DiffSummary {
        files: snapshot.files.len(),
        ..DiffSummary::default()
    };
    for file in &snapshot.files {
        if file.binary {
            out.binary_files += 1;
        }
        for hunk in &file.hunks {
            for line in &hunk.lines {
                match DiffLineKind::from(line.kind) {
                    DiffLineKind::Added => out.added += 1,
                    DiffLineKind::Removed => out.removed += 1,
                    DiffLineKind::Context => out.context += 1,
                }
            }
        }
    }
    out
}

/// Projects a summary into the stable status-bar text.
///
/// The returned string is a fixed, parseable format that never changes
/// between releases:
///
/// - `"clean"` — no pending changes at all (`files`, `binary_files`, `added`,
///   `removed` and `context` are all zero; this is the only state that does
///   not carry counts).
/// - `"changed: files={n} added={n} removed={n} context={n} binary_files={n}"`
///   — at least one bucket is non-zero. Every key is always present, in this
///   order.
///
/// Counts use `Display` and fields are separated by single spaces, so UIs can
/// render the string as-is or parse it with `strip_prefix`/`split`. The
/// projection is pure: it is derived solely from the summary's own counts,
/// never from the snapshot, so a hand-built summary projects identically to
/// one produced by [`summary`].
pub fn summary_text(summary: &DiffSummary) -> String {
    if summary.files == 0
        && summary.binary_files == 0
        && summary.added == 0
        && summary.removed == 0
        && summary.context == 0
    {
        "clean".to_string()
    } else {
        format!(
            "changed: files={} added={} removed={} context={} binary_files={}",
            summary.files, summary.added, summary.removed, summary.context, summary.binary_files
        )
    }
}

/// The mapping contract from a kernel diff snapshot to the UI-facing models.
///
/// Every method has a default implementation delegating to the module's free
/// functions, so a custom adapter (for example one that re-themes line
/// markers) only overrides what it needs. The free functions are the canonical
/// mapping: adapters that do not override a method observe exactly their
/// results.
pub trait GitDiffAdapter {
    /// Maps a snapshot into typed file views, stably sorted by path.
    fn file_views(&self, snapshot: &GitDiffSnapshot) -> Vec<DiffFileView> {
        file_views(snapshot)
    }

    /// Looks up the view for one file path, if the snapshot reports it.
    fn file_view_for(&self, snapshot: &GitDiffSnapshot, path: &Path) -> Option<DiffFileView> {
        file_view_for(snapshot, path)
    }

    /// Derives the status-bar summary counts for a snapshot.
    fn summary(&self, snapshot: &GitDiffSnapshot) -> DiffSummary {
        summary(snapshot)
    }

    /// Projects a summary into the stable status-bar text.
    fn summary_text(&self, summary: &DiffSummary) -> String {
        summary_text(summary)
    }
}

/// The default adapter: exactly the free-function mapping, nothing more.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultGitDiffAdapter;

impl GitDiffAdapter for DefaultGitDiffAdapter {}

#[cfg(test)]
mod tests {
    use super::*;
    use nextide_kernel::GitDiffLineKind;

    fn line(kind: GitDiffLineKind, old: Option<u32>, new: Option<u32>, text: &str) -> GitDiffLine {
        GitDiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: text.to_string(),
        }
    }

    fn hunk(header: &str, lines: Vec<GitDiffLine>) -> GitDiffHunk {
        GitDiffHunk {
            header: header.to_string(),
            lines,
        }
    }

    fn file(path: &str, binary: bool, hunks: Vec<GitDiffHunk>) -> GitDiffFile {
        GitDiffFile {
            path: path.to_string(),
            binary,
            hunks,
        }
    }

    fn snapshot(files: Vec<GitDiffFile>) -> GitDiffSnapshot {
        GitDiffSnapshot { files }
    }

    /// One canonical two-hunk diff used by several tests.
    fn sample_files() -> Vec<GitDiffFile> {
        vec![
            file(
                "src/lib.rs",
                false,
                vec![
                    hunk(
                        "@@ -1,5 +1,6 @@",
                        vec![
                            line(GitDiffLineKind::Context, Some(1), Some(1), "use std::io;"),
                            line(GitDiffLineKind::Removed, Some(2), None, "let x = 1;"),
                            line(GitDiffLineKind::Added, None, Some(2), "let x = 2;"),
                            line(GitDiffLineKind::Added, None, Some(3), "let y = x + 1;"),
                            line(GitDiffLineKind::Context, Some(3), Some(4), "fn main() {}"),
                        ],
                    ),
                    hunk(
                        "@@ -10,3 +12,3 @@",
                        vec![
                            line(GitDiffLineKind::Context, Some(10), Some(12), "mod tests;"),
                            line(GitDiffLineKind::Removed, Some(11), None, "old tail"),
                        ],
                    ),
                ],
            ),
            file(
                "docs/notes.md",
                false,
                vec![hunk(
                    "@@ -1,2 +1,1 @@",
                    vec![
                        line(GitDiffLineKind::Context, Some(1), Some(1), "title"),
                        line(GitDiffLineKind::Removed, Some(2), None, "obsolete line"),
                    ],
                )],
            ),
            file("assets/logo.png", true, Vec::new()),
        ]
    }

    #[test]
    fn diff_line_kind_labels_and_markers_are_stable_and_distinct() {
        assert_eq!(DiffLineKind::Context.as_str(), "context");
        assert_eq!(DiffLineKind::Added.as_str(), "added");
        assert_eq!(DiffLineKind::Removed.as_str(), "removed");
        assert_eq!(DiffLineKind::Context.marker(), " ");
        assert_eq!(DiffLineKind::Added.marker(), "+");
        assert_eq!(DiffLineKind::Removed.marker(), "-");
        // Every kind has its own marker and every marker is exactly one
        // character, so a gutter column never conflates two kinds.
        let markers = [
            DiffLineKind::Context.marker(),
            DiffLineKind::Added.marker(),
            DiffLineKind::Removed.marker(),
        ];
        let mut sorted = markers.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), markers.len());
        assert!(markers.iter().all(|marker| marker.len() == 1));
    }

    #[test]
    fn diff_line_kind_maps_from_the_kernel_kind() {
        assert_eq!(
            DiffLineKind::from(GitDiffLineKind::Context),
            DiffLineKind::Context
        );
        assert_eq!(
            DiffLineKind::from(GitDiffLineKind::Added),
            DiffLineKind::Added
        );
        assert_eq!(
            DiffLineKind::from(GitDiffLineKind::Removed),
            DiffLineKind::Removed
        );
    }

    #[test]
    fn from_line_preserves_kind_numbers_and_text() {
        let view = DiffLineView::from_line(&line(
            GitDiffLineKind::Added,
            None,
            Some(7),
            "let fresh = true;",
        ));
        assert_eq!(view.kind, DiffLineKind::Added);
        assert_eq!(view.old_line, None);
        assert_eq!(view.new_line, Some(7));
        assert_eq!(view.text, "let fresh = true;");
        // Removed lines carry the old number and no new number.
        let removed =
            DiffLineView::from_line(&line(GitDiffLineKind::Removed, Some(4), None, "gone"));
        assert_eq!(removed.old_line, Some(4));
        assert_eq!(removed.new_line, None);
        // Context lines carry both numbers.
        let context =
            DiffLineView::from_line(&line(GitDiffLineKind::Context, Some(2), Some(3), "both"));
        assert_eq!(context.old_line, Some(2));
        assert_eq!(context.new_line, Some(3));
    }

    #[test]
    fn display_text_renders_marker_separator_and_text() {
        // The exact string is the contract: marker (one char) + one space +
        // text. The context marker is a space, so context lines get two
        // leading spaces.
        assert_eq!(
            DiffLineView::from_line(&line(
                GitDiffLineKind::Context,
                Some(1),
                Some(1),
                "use std::io;"
            ))
            .display_text(),
            "  use std::io;"
        );
        assert_eq!(
            DiffLineView::from_line(&line(GitDiffLineKind::Added, None, Some(2), "let x = 2;"))
                .display_text(),
            "+ let x = 2;"
        );
        assert_eq!(
            DiffLineView::from_line(&line(GitDiffLineKind::Removed, Some(2), None, "let x = 1;"))
                .display_text(),
            "- let x = 1;"
        );
        // The display text matches the kind marker exactly, so a consumer can
        // reconstruct the kind from the first character.
        let added = DiffLineView::from_line(&line(GitDiffLineKind::Added, None, Some(9), "z"));
        assert_eq!(
            added.display_text(),
            format!("{}{}", added.kind.marker(), " z")
        );
        assert_eq!(added.display_text().as_bytes()[0], b'+');
    }

    #[test]
    fn display_text_keeps_the_separator_for_blank_lines() {
        // The separator is always present: a blank line still carries its
        // marker and one trailing space.
        assert_eq!(
            DiffLineView::from_line(&line(GitDiffLineKind::Added, None, Some(3), ""))
                .display_text(),
            "+ "
        );
        assert_eq!(
            DiffLineView::from_line(&line(GitDiffLineKind::Context, Some(3), Some(3), ""))
                .display_text(),
            "  "
        );
    }

    #[test]
    fn display_header_returns_the_raw_hunk_header() {
        let view = DiffHunkView::from_hunk(&hunk(
            "@@ -1,5 +1,6 @@ section",
            vec![line(GitDiffLineKind::Context, Some(1), Some(1), "title")],
        ));
        assert_eq!(view.display_header(), "@@ -1,5 +1,6 @@ section");
        // The projection is verbatim, including the common heading-less form.
        assert_eq!(view.display_header(), view.header);
        let plain = DiffHunkView::from_hunk(&hunk("@@ -10,3 +12,3 @@", Vec::new()));
        assert_eq!(plain.display_header(), "@@ -10,3 +12,3 @@");
    }

    #[test]
    fn file_label_is_stable_for_text_and_binary_files() {
        let files = sample_files();
        let text = DiffFileView::from_file(&files[0]);
        assert_eq!(text.file_label(), "src/lib.rs");
        assert_eq!(
            DiffFileView::from_file(&files[1]).file_label(),
            "docs/notes.md"
        );
        // Binary files carry the only existing suffix.
        assert_eq!(
            DiffFileView::from_file(&files[2]).file_label(),
            "assets/logo.png (binary)"
        );
    }

    #[test]
    fn file_label_always_joins_path_components_with_forward_slashes() {
        // The stored path is a platform-native PathBuf, but the label must be
        // identical on every platform, so it is rebuilt from the components
        // joined by `/` — never `Path::display`.
        let nested = std::path::PathBuf::from("a").join("b").join("c.rs");
        let view = DiffFileView {
            path: nested.clone(),
            binary: false,
            hunks: Vec::new(),
        };
        assert_eq!(view.file_label(), "a/b/c.rs");
        let binary = DiffFileView {
            path: nested,
            binary: true,
            hunks: Vec::new(),
        };
        assert_eq!(binary.file_label(), "a/b/c.rs (binary)");
    }

    #[test]
    fn display_projections_compose_across_hunks_and_files() {
        let view = DiffFileView::from_file(&sample_files()[0]);
        assert_eq!(view.file_label(), "src/lib.rs");
        assert_eq!(view.hunks[0].display_header(), "@@ -1,5 +1,6 @@");
        assert_eq!(view.hunks[1].display_header(), "@@ -10,3 +12,3 @@");
        let rendered: Vec<String> = view
            .hunks
            .iter()
            .flat_map(|hunk| {
                std::iter::once(hunk.display_header().to_string())
                    .chain(hunk.lines.iter().map(DiffLineView::display_text))
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "@@ -1,5 +1,6 @@",
                "  use std::io;",
                "- let x = 1;",
                "+ let x = 2;",
                "+ let y = x + 1;",
                "  fn main() {}",
                "@@ -10,3 +12,3 @@",
                "  mod tests;",
                "- old tail",
            ]
        );
        // Binary files render only their label: no hunks, no lines.
        let binary = DiffFileView::from_file(&sample_files()[2]);
        assert_eq!(binary.file_label(), "assets/logo.png (binary)");
        assert!(binary.hunks.is_empty());
    }

    #[test]
    fn file_views_project_and_sort_stably_by_path() {
        // Unsorted input: the model must re-sort into its own stable order.
        let snapshot = snapshot(vec![
            file("z-last.rs", false, Vec::new()),
            file("a-first.rs", false, Vec::new()),
            file("m-mid.rs", true, Vec::new()),
        ]);
        let views = file_views(&snapshot);
        let paths: Vec<&std::path::Path> = views.iter().map(|view| view.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                std::path::Path::new("a-first.rs"),
                std::path::Path::new("m-mid.rs"),
                std::path::Path::new("z-last.rs"),
            ]
        );
        assert!(!views[0].binary);
        assert!(views[1].binary);
    }

    #[test]
    fn file_views_keep_input_order_for_equal_paths() {
        let snapshot = snapshot(vec![
            file("dup.rs", false, Vec::new()),
            file("dup.rs", true, Vec::new()),
        ]);
        let views = file_views(&snapshot);
        assert_eq!(views.len(), 2);
        assert!(!views[0].binary);
        assert!(views[1].binary);
    }

    #[test]
    fn file_view_preserves_hunks_binary_and_count_derivations() {
        let files = sample_files();
        let view = DiffFileView::from_file(&files[0]);
        assert_eq!(view.path, std::path::PathBuf::from("src/lib.rs"));
        assert!(!view.is_binary());
        assert_eq!(view.hunk_count(), 2);
        assert_eq!(view.line_count(), 7);
        // 1 context + 2 added + 1 removed in hunk 1; 1 context + 1 removed in
        // hunk 2.
        assert_eq!(view.added_lines(), 2);
        assert_eq!(view.removed_lines(), 2);
        assert_eq!(view.context_lines(), 3);
        assert_eq!(view.hunks[0].header, "@@ -1,5 +1,6 @@");
        assert_eq!(view.hunks[0].lines.len(), 5);
        assert_eq!(view.hunks[0].lines[0].kind, DiffLineKind::Context);
        assert_eq!(view.hunks[0].lines[1].kind, DiffLineKind::Removed);
        assert_eq!(view.hunks[0].lines[2].kind, DiffLineKind::Added);

        let binary = DiffFileView::from_file(&files[2]);
        assert!(binary.is_binary());
        assert_eq!(binary.hunk_count(), 0);
        assert_eq!(binary.line_count(), 0);
        assert_eq!(binary.added_lines(), 0);
    }

    #[test]
    fn file_view_for_looks_up_paths_and_misses_others() {
        let snapshot = snapshot(sample_files());
        let found = file_view_for(&snapshot, std::path::Path::new("src/lib.rs")).unwrap();
        assert_eq!(found.path, std::path::PathBuf::from("src/lib.rs"));
        assert_eq!(found.added_lines(), 2);
        assert_eq!(
            file_view_for(&snapshot, std::path::Path::new("docs/notes.md"))
                .unwrap()
                .removed_lines(),
            1
        );
        assert_eq!(
            file_view_for(&snapshot, std::path::Path::new("Cargo.toml")),
            None
        );
        assert_eq!(file_view_for(&snapshot, std::path::Path::new("src")), None);
    }

    #[test]
    fn summary_counts_files_binary_files_and_lines() {
        let summary = summary(&snapshot(sample_files()));
        assert_eq!(
            summary,
            DiffSummary {
                files: 3,
                binary_files: 1,
                added: 2,
                removed: 3,
                context: 4,
            }
        );
        assert_eq!(summary.total_lines(), 9);
        assert!(!summary.is_clean());
        assert_eq!(DiffSummary::from(&snapshot(sample_files())), summary);
    }

    #[test]
    fn summary_of_clean_snapshot_is_clean() {
        let clean = summary(&snapshot(Vec::new()));
        assert!(clean.is_clean());
        assert_eq!(
            clean,
            DiffSummary {
                files: 0,
                binary_files: 0,
                added: 0,
                removed: 0,
                context: 0,
            }
        );
        assert_eq!(clean.total_lines(), 0);
        assert!(file_views(&snapshot(Vec::new())).is_empty());
        assert_eq!(
            file_view_for(&snapshot(Vec::new()), std::path::Path::new("x.rs")),
            None
        );
    }

    #[test]
    fn summary_counts_metadata_only_and_binary_files_without_lines() {
        // A mode-only change and a binary change are files with zero lines.
        let snapshot = snapshot(vec![
            file("exec.sh", false, Vec::new()),
            file("img.png", true, Vec::new()),
        ]);
        let summary = summary(&snapshot);
        assert_eq!(
            summary,
            DiffSummary {
                files: 2,
                binary_files: 1,
                added: 0,
                removed: 0,
                context: 0,
            }
        );
        assert_eq!(summary.total_lines(), 0);
    }

    #[test]
    fn summary_text_projects_clean_and_changed() {
        assert_eq!(DiffSummary::default().summary_text(), "clean");
        assert_eq!(summary_text(&DiffSummary::default()), "clean");

        let changed = DiffSummary {
            files: 3,
            binary_files: 1,
            added: 2,
            removed: 3,
            context: 4,
        };
        assert_eq!(
            summary_text(&changed),
            "changed: files=3 added=2 removed=3 context=4 binary_files=1"
        );
        assert_eq!(changed.summary_text(), summary_text(&changed));
    }

    #[test]
    fn summary_text_is_deterministic_for_any_counts() {
        // The projection reads the buckets themselves, not `files`, so even a
        // hand-built summary that disagrees with its own file count projects
        // deterministically: all-zero buckets are clean, and a zero-file
        // summary with lines still reports them.
        assert_eq!(
            summary_text(&DiffSummary {
                files: 0,
                added: 1,
                ..DiffSummary::default()
            }),
            "changed: files=0 added=1 removed=0 context=0 binary_files=0"
        );
        // A file with no lines (for example a metadata-only change) is still
        // a change: `files` is a bucket like the rest.
        assert_eq!(
            summary_text(&DiffSummary {
                files: 1,
                ..DiffSummary::default()
            }),
            "changed: files=1 added=0 removed=0 context=0 binary_files=0"
        );
        // Only an all-zero summary is clean.
        assert_eq!(summary_text(&DiffSummary::default()), "clean");
    }

    #[test]
    fn summary_text_uses_fixed_keys_in_fixed_order() {
        // The exact string is the contract; re-assert the key order so a
        // reformat can never silently reorder fields.
        let changed = DiffSummary {
            files: 2,
            binary_files: 1,
            added: 5,
            removed: 3,
            context: 8,
        };
        let text = summary_text(&changed);
        assert!(text.starts_with("changed: "));
        let fields: Vec<&str> = text["changed: ".len()..].split(' ').collect();
        assert_eq!(
            fields,
            vec![
                "files=2",
                "added=5",
                "removed=3",
                "context=8",
                "binary_files=1"
            ]
        );
    }

    #[test]
    fn summary_text_projects_a_real_snapshot_summary() {
        let summary = summary(&snapshot(sample_files()));
        assert_eq!(
            summary.summary_text(),
            "changed: files=3 added=2 removed=3 context=4 binary_files=1"
        );
    }

    #[test]
    fn default_adapter_matches_the_free_functions() {
        let adapter = DefaultGitDiffAdapter;
        let snapshot = snapshot(sample_files());
        assert_eq!(adapter.file_views(&snapshot), file_views(&snapshot));
        assert_eq!(
            adapter.file_view_for(&snapshot, std::path::Path::new("src/lib.rs")),
            file_view_for(&snapshot, std::path::Path::new("src/lib.rs"))
        );
        assert_eq!(adapter.summary(&snapshot), summary(&snapshot));
        assert_eq!(
            adapter.summary_text(&summary(&snapshot)),
            summary_text(&summary(&snapshot))
        );
    }

    #[test]
    fn adapter_can_be_consumed_as_a_trait_object() {
        let adapter: &dyn GitDiffAdapter = &DefaultGitDiffAdapter;
        let snapshot = snapshot(sample_files());
        let views = adapter.file_views(&snapshot);
        assert_eq!(views.len(), 3);
        assert_eq!(adapter.summary(&snapshot).files, 3);
    }

    #[test]
    fn projections_are_stable_across_repeated_calls() {
        let snapshot = snapshot(sample_files());
        let first = file_views(&snapshot);
        let second = file_views(&snapshot);
        assert_eq!(first, second);
        assert_eq!(summary(&snapshot), summary(&snapshot));
    }
}
