//! Pure, dependency-free diff-panel state model for the shell UI.
//!
//! This module layers *panel state* on top of the [`crate::git_diff`] model
//! layer: a [`DiffPanelSnapshot`] holds the projected [`DiffFileView`]s of a
//! kernel [`GitDiffSnapshot`], remembers which files are expanded, and keeps a
//! stable selection over the *visible* diff lines. It is a pure model — it
//! never draws, never runs git, never touches the kernel, and adds no
//! dependencies — so a diff panel can be driven entirely from this type:
//! expand/collapse files, move the selected line, and read the stable visible
//! line list plus the summary counts.
//!
//! The state is deliberately one panel → stable projections:
//!
//! - Construction is pure: [`DiffPanelSnapshot::from_snapshot`] projects a
//!   kernel [`GitDiffSnapshot`] through the [`crate::git_diff`] layer's
//!   [`file_views`], so the panel's file order is always the model's stable
//!   path order (equal paths keep their input order, as in `file_views`).
//!   Every file starts **expanded** and nothing is selected.
//!   [`DiffPanelSnapshot::from_views`] accepts a ready-made view list and
//!   enforces the same order contract, and [`Default`] is the empty panel
//!   (no files, nothing visible, nothing selected).
//! - [`DiffPanelSnapshot::visible_lines`] is the stable visible line list:
//!   for every expanded file — in model order — every hunk — in output
//!   order — and every line — in output order. The projection is pure, so the
//!   list is identical across repeated calls and unaffected by toggling any
//!   other file. Binary files and metadata-only changes carry no hunks, so
//!   they never contribute lines (their files still exist and can be
//!   collapsed).
//! - Expansion is per file path: [`DiffPanelSnapshot::toggle_file`] flips one
//!   file's expansion (returns `false`, changing nothing, for paths the panel
//!   does not report) and [`DiffPanelSnapshot::is_expanded`] observes it.
//!   Files with equal paths share one expansion key, mirroring the
//!   `file_views` duplicate-path contract.
//! - Selection is an **index into the visible line list**. The index always
//!   names a visible line while any are visible, and is `None` when the panel
//!   has no visible lines:
//!   - [`DiffPanelSnapshot::select_next`] moves one line forward and clamps at
//!     the last visible line; with nothing selected it selects the **first**
//!     visible line. [`DiffPanelSnapshot::select_prev`] is the mirror image:
//!     one line backward, clamped at the first, and from nothing it selects
//!     the **last** visible line. With no visible lines both are no-ops that
//!     keep the selection clear.
//!   - [`DiffPanelSnapshot::toggle_file`] re-clamps the selection to the new
//!     visible list, so it never dangles: collapsing the file above the
//!     selection moves it to the end of the visible list, and collapsing every
//!     visible file clears it.
//! - [`DiffPanelSnapshot::summary`] derives the whole-snapshot
//!   [`DiffSummary`] (changed files, binary files, added/removed/context
//!   lines) purely from the panel's views; it is identical to the
//!   [`crate::git_diff`] layer's `summary` of the same kernel snapshot and is
//!   deliberately **unaffected by expansion** — collapsing files never changes
//!   the status-bar counts.
//! - [`DiffPanelSnapshot::render_lines`] is the stable *text* projection of
//!   the panel, one [`String`] per rendered line, so the whole panel can be
//!   laid out as text (no GPU, no real drawing): every file — in model
//!   order — renders its [`DiffFileView::file_label`] line first, and every
//!   expanded file additionally renders each hunk's
//!   [`crate::git_diff::DiffHunkView::display_header`] line followed by its
//!   lines' [`DiffLineView::display_text`] lines. Collapsed files render only
//!   their label line — they stay visible and togglable — and collapsing
//!   never changes [`DiffPanelSnapshot::summary`], so the status-bar counts
//!   are preserved. The projection is pure and deterministic: repeated calls
//!   return identical lines, and toggling one file only removes or restores
//!   that file's block.
//!   [`DiffPanelSnapshot::visible_text`] joins the same lines with `'\n'`;
//!   every rendered line is a single physical line (the kernel parses each
//!   diff line from one output line, so line texts never contain a newline),
//!   which makes the joined text unambiguous — splitting on `'\n'` recovers
//!   `render_lines()` exactly.
//!
//! All state transitions are `&mut self` methods that return cheaply and
//! never allocate more than the projection itself needs; observers return
//! borrowed views, so a UI can render `visible_lines()` (for example through
//! [`DiffLineView::display_text`]) without cloning.

use crate::git_diff::{file_views, DiffFileView, DiffLineKind, DiffLineView, DiffSummary};
use nextide_kernel::GitDiffSnapshot;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The pure state model of a diff panel.
///
/// See the [module docs](self) for the exact contracts: file order is the
/// stable model path order, every file starts expanded, `visible_lines()` is
/// the stable projection of the expanded files' lines, `render_lines()` /
/// `visible_text()` project the panel to stable text (file labels, hunk
/// headers, line display text; collapsed files keep their label line and the
/// summary never changes), and the selection is an index into the visible
/// list that is always re-clamped to a visible line after every mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffPanelSnapshot {
    /// The file views in the stable model order (path order; equal paths keep
    /// their input order, as in [`crate::git_diff::file_views`]).
    files: Vec<DiffFileView>,
    /// The paths of the expanded files. Every key is a path present in
    /// `files`, so duplicate paths share one key.
    expanded: BTreeSet<PathBuf>,
    /// The index of the selected line into the current `visible_lines()`
    /// list; `None` while the panel has no visible lines or nothing is
    /// selected.
    selected: Option<usize>,
}

impl DiffPanelSnapshot {
    /// Builds a panel from a kernel diff snapshot: files projected through
    /// the model layer's [`file_views`] (stable path order), every file
    /// expanded, nothing selected.
    pub fn from_snapshot(snapshot: &GitDiffSnapshot) -> Self {
        Self::from_views(file_views(snapshot))
    }

    /// Builds a panel from ready-made file views: every file expanded,
    /// nothing selected.
    ///
    /// The views are re-sorted stably by path, so the panel's order contract
    /// (the model's stable path order) holds for any input, exactly as for
    /// views produced by [`file_views`].
    pub fn from_views(mut files: Vec<DiffFileView>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let expanded = files.iter().map(|file| file.path.clone()).collect();
        Self {
            files,
            expanded,
            selected: None,
        }
    }

    /// The file views in the stable model order.
    pub fn files(&self) -> &[DiffFileView] {
        &self.files
    }

    /// The number of files in the panel (expanded or not).
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Whether the file with `path` is currently expanded.
    ///
    /// Paths the panel does not report are never expanded (they are not in
    /// the expansion set), so this is `false` for them.
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Toggles the expansion of the file with `path`, returning whether the
    /// panel reports such a file.
    ///
    /// A known path is expanded when collapsed and collapsed when expanded;
    /// an unknown path changes nothing and returns `false`. Files with equal
    /// paths share one expansion key, so toggling collapses or expands all of
    /// them together. The selection is re-clamped afterwards so it still
    /// names a visible line (or is cleared when nothing remains visible).
    pub fn toggle_file(&mut self, path: &Path) -> bool {
        if !self.files.iter().any(|file| file.path == path) {
            return false;
        }
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.revalidate_selection();
        true
    }

    /// The number of visible lines: every line of every expanded file, in
    /// model/hunk/line order.
    pub fn visible_line_count(&self) -> usize {
        self.visible_lines().len()
    }

    /// The stable visible line list: every line of every expanded file, in
    /// model/hunk/line order.
    ///
    /// The projection is pure and deterministic: the list is identical across
    /// repeated calls, and toggling one file only removes or restores that
    /// file's lines — no other lines move. Binary files and metadata-only
    /// changes carry no hunks and therefore contribute no lines.
    pub fn visible_lines(&self) -> Vec<&DiffLineView> {
        let mut lines = Vec::new();
        for file in &self.files {
            if !self.is_expanded(&file.path) {
                continue;
            }
            for hunk in &file.hunks {
                lines.extend(hunk.lines.iter());
            }
        }
        lines
    }

    /// The stable text projection of the panel: one [`String`] per rendered
    /// line, ready to lay out as text (no GPU, no real drawing).
    ///
    /// Every file — in model order — renders its [`DiffFileView::file_label`]
    /// line first, so collapsed files still appear and stay togglable. For
    /// every expanded file the label is followed by each hunk's
    /// [`crate::git_diff::DiffHunkView::display_header`] line and then by each
    /// of its lines' [`DiffLineView::display_text`]; collapsed files render
    /// nothing more than their label line. The projection is pure and
    /// deterministic: it reads only the panel's own views and expansion
    /// state, repeated calls return identical lines, and toggling one file
    /// only removes or restores that file's block — while collapsing never
    /// changes [`DiffPanelSnapshot::summary`], so the status-bar counts are
    /// preserved.
    pub fn render_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for file in &self.files {
            lines.push(file.file_label());
            if !self.is_expanded(&file.path) {
                continue;
            }
            for hunk in &file.hunks {
                lines.push(hunk.display_header().to_string());
                lines.extend(hunk.lines.iter().map(DiffLineView::display_text));
            }
        }
        lines
    }

    /// The stable panel text: [`DiffPanelSnapshot::render_lines`] joined with
    /// `'\n'`.
    ///
    /// Every rendered line is a single physical line — the kernel parses each
    /// diff line from one output line, so line texts never contain a
    /// newline — which makes the joined text unambiguous: splitting on `'\n'`
    /// recovers `render_lines()` exactly. An empty panel renders as the empty
    /// string.
    pub fn visible_text(&self) -> String {
        self.render_lines().join("\n")
    }

    /// The index of the selected line into the current visible line list, if
    /// any.
    ///
    /// The index is always in range while any line is visible, and `None`
    /// when the panel has no visible lines or nothing has been selected yet.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// The selected visible line, if any.
    pub fn selected_line(&self) -> Option<&DiffLineView> {
        let index = self.selected?;
        self.visible_lines().get(index).copied()
    }

    /// The file that contains the selected visible line, if any.
    pub fn selected_file(&self) -> Option<&DiffFileView> {
        let index = self.selected?;
        let mut offset = 0usize;
        for file in &self.files {
            if !self.is_expanded(&file.path) {
                continue;
            }
            let count = file.line_count();
            if index >= offset && index - offset < count {
                return Some(file);
            }
            offset += count;
        }
        None
    }

    /// Moves the selection to the next visible line.
    ///
    /// From no selection it selects the first visible line; at the last
    /// visible line it stays (no wrap-around); with no visible lines the
    /// selection stays clear.
    pub fn select_next(&mut self) {
        let len = self.visible_line_count();
        if len == 0 {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            None => 0,
            Some(index) => index.saturating_add(1).min(len - 1),
        });
    }

    /// Moves the selection to the previous visible line.
    ///
    /// From no selection it selects the last visible line; at the first
    /// visible line it stays (no wrap-around); with no visible lines the
    /// selection stays clear.
    pub fn select_prev(&mut self) {
        let len = self.visible_line_count();
        if len == 0 {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            None => len - 1,
            Some(index) => index.saturating_sub(1),
        });
    }

    /// The whole-snapshot summary counts, derived purely from the panel's
    /// views.
    ///
    /// The result equals the [`crate::git_diff`] layer's `summary` of the
    /// kernel snapshot this panel was built from, and is deliberately
    /// unaffected by expansion: collapsing or expanding files never changes
    /// the status-bar counts.
    pub fn summary(&self) -> DiffSummary {
        summary_of(&self.files)
    }

    /// Re-clamps the selection into the current visible line list.
    ///
    /// Invariant: while any line is visible the selection (when `Some`) is
    /// always a valid visible index, so every mutating method ends with this.
    fn revalidate_selection(&mut self) {
        let len = self.visible_line_count();
        if len == 0 {
            self.selected = None;
        } else if let Some(index) = self.selected {
            if index >= len {
                self.selected = Some(len - 1);
            }
        }
    }
}

impl Default for DiffPanelSnapshot {
    /// The empty panel: no files, nothing visible, nothing selected.
    fn default() -> Self {
        Self {
            files: Vec::new(),
            expanded: BTreeSet::new(),
            selected: None,
        }
    }
}

/// Derives the whole-diff [`DiffSummary`] counts from a list of file views.
///
/// `files` counts as changed files (binary and metadata-only changes
/// included), `binary_files` counts the views whose `binary` flag is set, and
/// the line buckets sum across every hunk of every file, so binary files and
/// metadata-only changes never contribute lines. The projection is pure — it
/// reads only the given views, never git and never the filesystem — and is
/// the same counting rule as the [`crate::git_diff`] layer's `summary`, so a
/// panel built from a snapshot reports exactly that snapshot's summary.
fn summary_of(files: &[DiffFileView]) -> DiffSummary {
    let mut out = DiffSummary {
        files: files.len(),
        ..DiffSummary::default()
    };
    for file in files {
        if file.binary {
            out.binary_files += 1;
        }
        for hunk in &file.hunks {
            for line in &hunk.lines {
                match line.kind {
                    DiffLineKind::Added => out.added += 1,
                    DiffLineKind::Removed => out.removed += 1,
                    DiffLineKind::Context => out.context += 1,
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_summary;
    use crate::git_diff::file_view_for;
    use nextide_kernel::{GitDiffFile, GitDiffHunk, GitDiffLine, GitDiffLineKind};

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

    /// One canonical three-file diff: `src/lib.rs` (7 lines over 2 hunks),
    /// `docs/notes.md` (2 lines), and a binary `assets/logo.png` (no lines).
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

    fn sample_panel() -> DiffPanelSnapshot {
        DiffPanelSnapshot::from_snapshot(&snapshot(sample_files()))
    }

    #[test]
    fn default_panel_is_empty_with_clear_selection() {
        let panel = DiffPanelSnapshot::default();
        assert_eq!(panel.file_count(), 0);
        assert!(panel.files().is_empty());
        assert_eq!(panel.visible_line_count(), 0);
        assert!(panel.visible_lines().is_empty());
        assert_eq!(panel.selected_index(), None);
        assert_eq!(panel.selected_line(), None);
        assert_eq!(panel.selected_file(), None);
        assert!(panel.summary().is_clean());
        assert_eq!(panel.summary().total_lines(), 0);
    }

    #[test]
    fn empty_panel_ignores_all_mutations() {
        let mut panel = DiffPanelSnapshot::default();
        assert!(!panel.toggle_file(Path::new("missing.rs")));
        panel.select_next();
        panel.select_prev();
        assert_eq!(panel.selected_index(), None);
        assert_eq!(panel, DiffPanelSnapshot::default());
    }

    #[test]
    fn from_snapshot_expands_every_file_and_matches_the_model_summary() {
        let panel = sample_panel();
        assert_eq!(panel.file_count(), 3);
        let paths: Vec<&Path> = panel
            .files()
            .iter()
            .map(|file| file.path.as_path())
            .collect();
        assert_eq!(
            paths,
            vec![
                Path::new("assets/logo.png"),
                Path::new("docs/notes.md"),
                Path::new("src/lib.rs"),
            ]
        );
        for file in panel.files() {
            assert!(
                panel.is_expanded(&file.path),
                "{} starts expanded",
                file.file_label()
            );
        }
        assert_eq!(panel.visible_line_count(), 9);
        // The panel summary is the whole-snapshot summary, projected from the
        // views.
        assert_eq!(panel.summary(), diff_summary(&snapshot(sample_files())),);
        assert_eq!(
            panel.summary(),
            DiffSummary {
                files: 3,
                binary_files: 1,
                added: 2,
                removed: 3,
                context: 4,
            }
        );
        assert_eq!(panel.selected_index(), None);
    }

    #[test]
    fn visible_lines_are_stable_and_in_model_hunk_line_order() {
        let panel = sample_panel();
        let first = panel.visible_lines();
        let second = panel.visible_lines();
        assert_eq!(first, second);
        assert_eq!(first.len(), 9);
        // Binary files contribute no lines: all 9 come from the two text
        // files, starting with the first hunk of the first text file in model
        // (path) order.
        assert_eq!(first[0].text, "title");
        assert_eq!(first[0].kind, DiffLineKind::Context);
        assert_eq!(first[1].text, "obsolete line");
        assert_eq!(first[1].kind, DiffLineKind::Removed);
        assert_eq!(first[2].text, "use std::io;");
        assert_eq!(first[3].text, "let x = 1;");
        assert_eq!(first[4].text, "let x = 2;");
        assert_eq!(first[5].text, "let y = x + 1;");
        assert_eq!(first[6].text, "fn main() {}");
        assert_eq!(first[7].text, "mod tests;");
        assert_eq!(first[8].text, "old tail");
        // Hunk and line order inside a file is the model's output order.
        let lib = file_view_for(&snapshot(sample_files()), Path::new("src/lib.rs")).unwrap();
        assert_eq!(lib.hunks[1].lines.len(), 2);
    }

    #[test]
    fn toggle_file_collapses_and_restores_that_files_lines_only() {
        let mut panel = sample_panel();
        assert!(panel.toggle_file(Path::new("src/lib.rs")));
        assert!(!panel.is_expanded(Path::new("src/lib.rs")));
        assert_eq!(panel.visible_line_count(), 2);
        let visible: Vec<&str> = panel
            .visible_lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(visible, vec!["title", "obsolete line"]);
        // Unrelated files keep their expansion and the summary never moves.
        assert!(panel.is_expanded(Path::new("docs/notes.md")));
        assert!(panel.is_expanded(Path::new("assets/logo.png")));
        assert_eq!(panel.summary().files, 3);

        // Toggling back restores the identical, stable line list.
        assert!(panel.toggle_file(Path::new("src/lib.rs")));
        assert_eq!(panel.visible_lines(), sample_panel().visible_lines());
        assert_eq!(panel.visible_line_count(), 9);
    }

    #[test]
    fn toggle_file_on_unknown_path_is_a_noop() {
        let mut panel = sample_panel();
        let before = panel.clone();
        assert!(!panel.toggle_file(Path::new("Cargo.toml")));
        assert!(!panel.toggle_file(Path::new("src")));
        assert_eq!(panel, before);
    }

    #[test]
    fn toggle_file_toggles_binary_and_metadata_only_files_without_lines() {
        let mut panel = sample_panel();
        // The binary file has no lines either way; toggling only changes its
        // expansion flag.
        assert!(panel.toggle_file(Path::new("assets/logo.png")));
        assert!(!panel.is_expanded(Path::new("assets/logo.png")));
        assert_eq!(panel.visible_line_count(), 9);
        assert_eq!(panel.summary().binary_files, 1);

        // A metadata-only change (no hunks at all) behaves like the binary
        // file but counts as a plain file.
        let mut panel = DiffPanelSnapshot::from_snapshot(&snapshot(vec![
            file("exec.sh", false, Vec::new()),
            file("src/lib.rs", false, sample_files()[0].hunks.clone()),
        ]));
        assert_eq!(panel.file_count(), 2);
        assert_eq!(panel.visible_line_count(), 7);
        assert!(panel.toggle_file(Path::new("exec.sh")));
        assert!(!panel.is_expanded(Path::new("exec.sh")));
        assert_eq!(panel.visible_line_count(), 7);
        assert_eq!(panel.summary().files, 2);
    }

    #[test]
    fn duplicate_paths_share_one_expansion_key() {
        let panel = DiffPanelSnapshot::from_snapshot(&snapshot(vec![
            file(
                "dup.rs",
                false,
                vec![hunk(
                    "@@ -1 +1 @@",
                    vec![line(GitDiffLineKind::Added, None, Some(1), "one")],
                )],
            ),
            file(
                "dup.rs",
                false,
                vec![hunk(
                    "@@ -1 +1 @@",
                    vec![line(GitDiffLineKind::Added, None, Some(1), "two")],
                )],
            ),
        ]));
        assert_eq!(panel.file_count(), 2);
        assert_eq!(panel.visible_line_count(), 2);
        let mut panel = panel;
        assert!(panel.toggle_file(Path::new("dup.rs")));
        // Both views share the key, so both collapse together.
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(panel.selected_index(), None);
        assert!(panel.toggle_file(Path::new("dup.rs")));
        assert_eq!(panel.visible_line_count(), 2);
    }

    #[test]
    fn select_next_moves_forward_and_clamps_at_the_last_line() {
        let mut panel = sample_panel();
        // Nothing selected: next picks the first visible line.
        panel.select_next();
        assert_eq!(panel.selected_index(), Some(0));
        assert_eq!(panel.selected_line().unwrap().text, "title");
        assert_eq!(
            panel.selected_file().unwrap().path,
            PathBuf::from("docs/notes.md")
        );

        for expected in 1..9 {
            panel.select_next();
            assert_eq!(panel.selected_index(), Some(expected));
        }
        // Clamped at the last visible line: further nexts stay put.
        panel.select_next();
        assert_eq!(panel.selected_index(), Some(8));
        assert_eq!(panel.selected_line().unwrap().text, "old tail");
    }

    #[test]
    fn select_prev_moves_backward_and_clamps_at_the_first_line() {
        let mut panel = sample_panel();
        // Nothing selected: prev picks the last visible line.
        panel.select_prev();
        assert_eq!(panel.selected_index(), Some(8));
        assert_eq!(panel.selected_line().unwrap().text, "old tail");

        for expected in (0..8).rev() {
            panel.select_prev();
            assert_eq!(panel.selected_index(), Some(expected));
        }
        // Clamped at the first visible line: further prevs stay put.
        panel.select_prev();
        assert_eq!(panel.selected_index(), Some(0));
    }

    #[test]
    fn selection_crosses_file_boundaries_in_model_order() {
        let mut panel = sample_panel();
        // docs/notes.md has 2 lines; src/lib.rs has 7. The boundary sits
        // between visible indexes 1 and 2, so three nexts (None → 0 → 1 → 2)
        // land on the first line of the second file.
        for _ in 0..3 {
            panel.select_next();
        }
        assert_eq!(panel.selected_index(), Some(2));
        assert_eq!(
            panel.selected_file().unwrap().path,
            PathBuf::from("src/lib.rs")
        );
        assert_eq!(panel.selected_line().unwrap().text, "use std::io;");

        // Stepping back across the boundary returns to the previous file.
        panel.select_prev();
        assert_eq!(panel.selected_index(), Some(1));
        assert_eq!(
            panel.selected_file().unwrap().path,
            PathBuf::from("docs/notes.md")
        );
    }

    #[test]
    fn collapsing_the_file_containing_the_selection_reclamps_to_a_visible_line() {
        let mut panel = sample_panel();
        // Select the last line of src/lib.rs (visible index 8).
        for _ in 0..9 {
            panel.select_next();
        }
        assert_eq!(panel.selected_line().unwrap().text, "old tail");
        // Collapsing that file hides its 7 lines: the index 8 must re-clamp
        // to the last remaining visible line (the last line of notes.md).
        assert!(panel.toggle_file(Path::new("src/lib.rs")));
        assert_eq!(panel.selected_index(), Some(1));
        assert_eq!(panel.selected_line().unwrap().text, "obsolete line");
        assert_eq!(
            panel.selected_file().unwrap().path,
            PathBuf::from("docs/notes.md")
        );

        // Collapsing a file *above* the selection also re-clamps: with the
        // notes file collapsed too, nothing is visible and the selection is
        // cleared.
        assert!(panel.toggle_file(Path::new("docs/notes.md")));
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(panel.selected_index(), None);
        assert_eq!(panel.selected_line(), None);
        assert_eq!(panel.selected_file(), None);
    }

    #[test]
    fn collapsing_every_file_clears_the_selection_and_next_prev_stay_clear() {
        let mut panel = sample_panel();
        panel.select_next();
        assert_eq!(panel.selected_index(), Some(0));
        let paths: Vec<PathBuf> = panel.files().iter().map(|file| file.path.clone()).collect();
        for path in &paths {
            panel.toggle_file(path);
        }
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(panel.selected_index(), None);
        panel.select_next();
        panel.select_prev();
        assert_eq!(panel.selected_index(), None);
    }

    #[test]
    fn expanding_a_file_after_collapse_allows_selection_again() {
        let mut panel = sample_panel();
        let paths: Vec<PathBuf> = panel.files().iter().map(|file| file.path.clone()).collect();
        for path in &paths {
            panel.toggle_file(path);
        }
        assert_eq!(panel.visible_line_count(), 0);
        assert!(panel.toggle_file(Path::new("docs/notes.md")));
        assert_eq!(panel.visible_line_count(), 2);
        assert_eq!(panel.selected_index(), None);
        panel.select_next();
        assert_eq!(panel.selected_index(), Some(0));
        assert_eq!(panel.selected_line().unwrap().text, "title");
        panel.select_prev();
        // Two visible lines: prev from the first line stays at the first.
        assert_eq!(panel.selected_index(), Some(0));
        panel.select_prev();
        assert_eq!(panel.selected_index(), Some(0));
    }

    #[test]
    fn summary_is_unaffected_by_expansion() {
        let mut panel = sample_panel();
        let full = panel.summary();
        assert!(!full.is_clean());
        let paths: Vec<PathBuf> = panel.files().iter().map(|file| file.path.clone()).collect();
        for path in &paths {
            panel.toggle_file(path);
        }
        assert_eq!(panel.summary(), full);
        assert_eq!(panel.summary().total_lines(), 9);
        // And the panel summary always matches the model layer's snapshot
        // summary, including for a clean snapshot.
        assert_eq!(panel.summary(), diff_summary(&snapshot(sample_files())));
        assert!(DiffPanelSnapshot::from_snapshot(&snapshot(Vec::new()))
            .summary()
            .is_clean());
    }

    #[test]
    fn selected_line_always_matches_the_visible_line_at_the_selected_index() {
        let mut panel = sample_panel();
        // Walk forward across every visible line: the selection must always
        // name exactly the line the visible list reports at that index.
        for _ in 0..panel.visible_line_count() {
            panel.select_next();
            let index = panel.selected_index().unwrap();
            let expected = panel.visible_lines().get(index).copied().unwrap();
            assert_eq!(panel.selected_line(), Some(expected));
        }
        // And backward across every visible line, same invariant.
        for _ in 0..panel.visible_line_count() {
            panel.select_prev();
            let index = panel.selected_index().unwrap();
            let expected = panel.visible_lines().get(index).copied().unwrap();
            assert_eq!(panel.selected_line(), Some(expected));
        }
        assert_eq!(panel.selected_index(), Some(0));
    }

    #[test]
    fn from_views_enforces_the_model_order_contract() {
        // Unsorted input must be re-sorted stably by path, like file_views.
        let panel = DiffPanelSnapshot::from_views(vec![
            file_view_for(&snapshot(sample_files()), Path::new("src/lib.rs")).unwrap(),
            file_view_for(&snapshot(sample_files()), Path::new("docs/notes.md")).unwrap(),
        ]);
        let paths: Vec<&Path> = panel
            .files()
            .iter()
            .map(|file| file.path.as_path())
            .collect();
        assert_eq!(
            paths,
            vec![Path::new("docs/notes.md"), Path::new("src/lib.rs")]
        );
        assert_eq!(panel.visible_line_count(), 9);
        assert!(panel.is_expanded(Path::new("src/lib.rs")));
    }

    #[test]
    fn from_snapshot_and_from_views_agree() {
        let snapshot = snapshot(sample_files());
        let from_snapshot = DiffPanelSnapshot::from_snapshot(&snapshot);
        let views = crate::git_diff::file_views(&snapshot);
        assert_eq!(DiffPanelSnapshot::from_views(views), from_snapshot);
    }

    #[test]
    fn render_lines_emit_file_labels_hunk_headers_and_display_text_in_order() {
        let panel = sample_panel();
        assert_eq!(
            panel.render_lines(),
            vec![
                "assets/logo.png (binary)",
                "docs/notes.md",
                "@@ -1,2 +1,1 @@",
                "  title",
                "- obsolete line",
                "src/lib.rs",
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
        // The line count is one label per file plus, per expanded file, one
        // header per hunk and one display-text line per line.
        assert_eq!(panel.render_lines().len(), 3 + 3 + 9);
    }

    #[test]
    fn render_lines_collapsed_files_keep_only_their_label_line() {
        let mut panel = sample_panel();
        assert!(panel.toggle_file(Path::new("src/lib.rs")));
        assert_eq!(
            panel.render_lines(),
            vec![
                "assets/logo.png (binary)",
                "docs/notes.md",
                "@@ -1,2 +1,1 @@",
                "  title",
                "- obsolete line",
                "src/lib.rs",
            ]
        );
        // Collapsing every file leaves exactly the label lines, so the files
        // stay visible and togglable — while the summary counts never move.
        let full_summary = sample_panel().summary();
        let mut panel = sample_panel();
        let paths: Vec<PathBuf> = panel.files().iter().map(|file| file.path.clone()).collect();
        for path in &paths {
            panel.toggle_file(path);
        }
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(
            panel.render_lines(),
            vec!["assets/logo.png (binary)", "docs/notes.md", "src/lib.rs"]
        );
        assert_eq!(panel.summary(), full_summary);
        assert_eq!(panel.summary().total_lines(), 9);
    }

    #[test]
    fn render_lines_is_stable_and_only_the_toggled_files_block_changes() {
        let panel = sample_panel();
        let full = panel.render_lines();
        assert_eq!(panel.render_lines(), full);
        let mut panel = panel;
        assert!(panel.toggle_file(Path::new("docs/notes.md")));
        // Only notes.md's block (label + header + its 2 lines) disappears;
        // every other file's lines are untouched.
        assert_eq!(
            panel.render_lines(),
            vec![
                "assets/logo.png (binary)",
                "docs/notes.md",
                "src/lib.rs",
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
        assert!(panel.toggle_file(Path::new("docs/notes.md")));
        assert_eq!(panel.render_lines(), full);
    }

    #[test]
    fn visible_text_joins_render_lines_and_splits_back_exactly() {
        let panel = sample_panel();
        let lines = panel.render_lines();
        assert_eq!(panel.visible_text(), lines.join("\n"));
        // Because every rendered line is a single physical line, splitting the
        // joined text on '\n' recovers render_lines() exactly.
        let text = panel.visible_text();
        let split: Vec<&str> = text.split('\n').collect();
        let expected: Vec<&str> = lines.iter().map(String::as_str).collect();
        assert_eq!(split, expected);
        // The exact joined string is the contract for the sample panel.
        assert_eq!(
            panel.visible_text(),
            "assets/logo.png (binary)\ndocs/notes.md\n@@ -1,2 +1,1 @@\n  title\n- obsolete line\nsrc/lib.rs\n@@ -1,5 +1,6 @@\n  use std::io;\n- let x = 1;\n+ let x = 2;\n+ let y = x + 1;\n  fn main() {}\n@@ -10,3 +12,3 @@\n  mod tests;\n- old tail"
        );
        // An empty panel renders as the empty string with no lines.
        let empty = DiffPanelSnapshot::default();
        assert_eq!(empty.render_lines(), Vec::<String>::new());
        assert_eq!(empty.visible_text(), "");
    }

    #[test]
    fn render_lines_keeps_blank_line_display_text() {
        // Blank lines render with their marker and separator, exactly like
        // DiffLineView::display_text: an added blank line is "+ ".
        let panel = DiffPanelSnapshot::from_snapshot(&snapshot(vec![file(
            "blank.txt",
            false,
            vec![hunk(
                "@@ -1 +1,2 @@",
                vec![
                    line(GitDiffLineKind::Context, Some(1), Some(1), "before"),
                    line(GitDiffLineKind::Added, None, Some(2), ""),
                ],
            )],
        )]));
        assert_eq!(
            panel.render_lines(),
            vec!["blank.txt", "@@ -1 +1,2 @@", "  before", "+ "]
        );
    }

    #[test]
    fn render_lines_agree_with_visible_lines_on_counts() {
        let mut panel = sample_panel();
        // Fully expanded: the display-text lines of render_lines are exactly
        // the visible lines, so the per-file nesting holds for the counts.
        assert_eq!(panel.render_lines().len(), 15);
        assert_eq!(panel.visible_line_count(), 9);
        // Collapsing one file removes exactly its label + headers + lines.
        assert!(panel.toggle_file(Path::new("src/lib.rs")));
        assert_eq!(panel.render_lines().len(), 6);
        assert_eq!(panel.visible_line_count(), 2);
        assert_eq!(panel.summary(), sample_panel().summary());
        // A metadata-only file and a binary file render their labels only.
        let panel = DiffPanelSnapshot::from_snapshot(&snapshot(vec![
            file("exec.sh", false, Vec::new()),
            file("img.png", true, Vec::new()),
        ]));
        assert_eq!(panel.render_lines(), vec!["exec.sh", "img.png (binary)"]);
        assert_eq!(panel.visible_line_count(), 0);
        assert_eq!(panel.summary().files, 2);
    }
}
