use nextide_kernel::ApplicationContext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSnapshot {
    pub title: String,
    pub open_files: usize,
    pub tabs: usize,
    pub editor_views: usize,
    pub tree_nodes: usize,
}

impl ShellSnapshot {
    pub fn from_context(title: impl Into<String>, app: &ApplicationContext) -> Self {
        Self {
            title: title.into(),
            open_files: app.workspace.open_files().count(),
            tabs: app.tabs.tabs().len(),
            editor_views: app.views.len(),
            tree_nodes: app.file_tree.nodes().len(),
        }
    }
}
