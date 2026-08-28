use crate::{CommandPalette, NotificationCenter};
use nextide_kernel::ApplicationContext;

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
    pub palette_open: bool,
    pub palette_query: String,
    pub notifications: usize,
}

impl ShellSnapshot {
    pub fn from_context(
        title: impl Into<String>,
        app: &ApplicationContext,
        palette: &CommandPalette,
        notifications: &NotificationCenter,
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
            palette_open: palette.is_open(),
            palette_query: palette.query().to_string(),
            notifications: notifications.items().len(),
        }
    }
}
