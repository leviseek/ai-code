use crate::{CommandPalette, DiffSummary, GitStatusSummary, NotificationCenter, StatusKind};
use nextide_kernel::{ApplicationContext, FileNodeKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeItem {
    pub path: std::path::PathBuf,
    pub name: String,
    pub depth: usize,
    pub indent: u32,
    pub directory: bool,
    pub expanded: bool,
    pub selected: bool,
    /// The Git status marker for this path.
    ///
    /// [`StatusKind::Clean`] when the path has no pending change or when no
    /// Git snapshot has been applied yet. Porcelain status names files, not
    /// directories, so a file without its own entry stays `Clean`; a
    /// directory carries the aggregated status of everything below it (the
    /// most severe descendant kind, or its own entry when it has one, for
    /// example a submodule), so it is marked whenever any changed entry is
    /// under it.
    pub status: StatusKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSnapshot {
    pub title: String,
    pub open_files: usize,
    pub tabs: usize,
    pub editor_views: usize,
    pub tree_nodes: usize,
    pub tab_paths: Vec<std::path::PathBuf>,
    pub active_tab: Option<std::path::PathBuf>,
    pub tree_paths: Vec<std::path::PathBuf>,
    pub visible_tree_paths: Vec<std::path::PathBuf>,
    pub visible_tree: Vec<TreeItem>,
    pub selected_tree: Option<std::path::PathBuf>,
    pub palette_open: bool,
    pub palette_query: String,
    pub notifications: usize,
    /// The Git status-bar summary, when a Git snapshot has been applied to
    /// the shell; `None` while no Git integration is active.
    pub git_summary: Option<GitStatusSummary>,
    /// The diff status-bar summary, when a diff panel snapshot has been
    /// applied to the shell; `None` while no diff panel is active.
    pub diff_summary: Option<DiffSummary>,
}

impl ShellSnapshot {
    /// Builds a snapshot from kernel/app state.
    ///
    /// Git and diff status are deliberately not part of this constructor:
    /// markers default to [`StatusKind::Clean`], [`git_summary`](Self::git_summary)
    /// and [`diff_summary`](Self::diff_summary) to `None`.
    /// [`crate::DesktopShell::snapshot`] applies externally supplied Git and
    /// diff snapshots on top afterwards.
    pub fn from_context(
        title: impl Into<String>,
        app: &ApplicationContext,
        palette: &CommandPalette,
        notifications: &NotificationCenter,
        selected_tree: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            title: title.into(),
            open_files: app.workspace.open_files().count(),
            tabs: app.tabs.tabs().len(),
            editor_views: app.views.len(),
            tree_nodes: app.file_tree.nodes().len(),
            tab_paths: app.tabs.tabs().iter().map(|tab| tab.path.clone()).collect(),
            active_tab: app.tabs.active().map(|tab| tab.path.clone()),
            tree_paths: app
                .file_tree
                .nodes()
                .iter()
                .map(|node| node.path.clone())
                .collect(),
            visible_tree_paths: app
                .file_tree
                .visible_nodes()
                .iter()
                .map(|node| node.path.clone())
                .collect(),
            selected_tree: selected_tree.clone(),
            visible_tree: app
                .file_tree
                .visible_nodes()
                .iter()
                .map(|node| TreeItem {
                    path: node.path.clone(),
                    name: node.name.clone(),
                    depth: node.depth,
                    indent: (node.depth as u32) * 16,
                    directory: node.kind == FileNodeKind::Directory,
                    expanded: node.expanded,
                    selected: selected_tree.as_ref() == Some(&node.path),
                    status: StatusKind::Clean,
                })
                .collect(),
            palette_open: palette.is_open(),
            palette_query: palette.query().to_string(),
            notifications: notifications.items().len(),
            git_summary: None,
            diff_summary: None,
        }
    }

    /// The stable status-bar text projected from the applied Git summary, if
    /// any; see [`GitStatusSummary::summary_text`] for the exact format.
    ///
    /// `Some` exactly when [`git_summary`](Self::git_summary) is `Some`, so
    /// `Some("clean")` means Git integration is active and the repository has
    /// no pending changes, while `None` means no Git snapshot has been
    /// applied to this shell yet.
    pub fn git_status_text(&self) -> Option<String> {
        self.git_summary
            .as_ref()
            .map(GitStatusSummary::summary_text)
    }

    /// The stable status-bar text projected from the applied diff summary, if
    /// any; see [`DiffSummary::summary_text`] for the exact format.
    ///
    /// `Some` exactly when [`diff_summary`](Self::diff_summary) is `Some`, so
    /// `Some("clean")` means a diff panel is active with no changes, while
    /// `None` means no diff snapshot has been applied to this shell yet.
    pub fn diff_status_text(&self) -> Option<String> {
        self.diff_summary.as_ref().map(DiffSummary::summary_text)
    }
}
