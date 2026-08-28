use crate::ShellSnapshot;
use std::io;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRenderRow {
    pub path: std::path::PathBuf,
    pub label: String,
    pub x: u32,
    pub y: u32,
    pub selected: bool,
    pub icon: &'static str,
}

impl TreeRenderRow {
    pub fn hit(&self, point: crate::Point, width: u32) -> bool {
        point.y >= self.y && point.y < self.y + 24 && point.x >= self.x && point.x < width
    }

    pub fn toggle_hit(&self, point: crate::Point) -> bool {
        self.icon == "folder"
            && point.y >= self.y
            && point.y < self.y + 24
            && point.x >= self.x.saturating_sub(16)
            && point.x < self.x
    }
}

pub fn tree_rows(snapshot: &ShellSnapshot) -> Vec<TreeRenderRow> {
    snapshot
        .visible_tree
        .iter()
        .enumerate()
        .map(|(index, item)| TreeRenderRow {
            path: item.path.clone(),
            label: item.name.clone(),
            x: item.indent,
            y: index as u32 * 24,
            selected: item.selected,
            icon: if item.directory { "folder" } else { "file" },
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRowChange {
    Added(TreeRenderRow),
    Removed(std::path::PathBuf),
    Updated(TreeRenderRow),
}

impl TreeRowChange {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Added(_) => "added",
            Self::Removed(_) => "removed",
            Self::Updated(_) => "updated",
        }
    }

    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Added(row) | Self::Updated(row) => &row.path,
            Self::Removed(path) => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeChangeBatch {
    pub frame: u64,
    pub changes: Vec<TreeRowChange>,
}

impl TreeChangeBatch {
    pub fn counts(&self) -> (usize, usize, usize) {
        tree_change_counts(&self.changes)
    }
}

pub fn tree_change_counts(changes: &[TreeRowChange]) -> (usize, usize, usize) {
    changes.iter().fold(
        (0, 0, 0),
        |(added, removed, updated), change| match change {
            TreeRowChange::Added(_) => (added + 1, removed, updated),
            TreeRowChange::Removed(_) => (added, removed + 1, updated),
            TreeRowChange::Updated(_) => (added, removed, updated + 1),
        },
    )
}

pub fn diff_tree_rows(previous: &[TreeRenderRow], current: &[TreeRenderRow]) -> Vec<TreeRowChange> {
    let mut changes = Vec::new();
    for row in current {
        match previous.iter().find(|old| old.path == row.path) {
            None => changes.push(TreeRowChange::Added(row.clone())),
            Some(old) if old != row => changes.push(TreeRowChange::Updated(row.clone())),
            _ => {}
        }
    }
    for row in previous {
        if !current.iter().any(|new| new.path == row.path) {
            changes.push(TreeRowChange::Removed(row.path.clone()));
        }
    }
    changes
}

pub trait UiRenderer {
    fn render(&mut self, snapshot: &ShellSnapshot) -> io::Result<()>;
    fn render_tree_changes(&mut self, changes: &[TreeRowChange]) -> io::Result<()> {
        let _ = changes;
        Ok(())
    }
    fn render_tree(&mut self, snapshot: &ShellSnapshot) -> io::Result<Vec<TreeRenderRow>> {
        Ok(snapshot
            .visible_tree
            .iter()
            .enumerate()
            .map(|(index, item)| TreeRenderRow {
                path: item.path.clone(),
                label: item.name.clone(),
                x: item.indent,
                y: index as u32 * 24,
                selected: item.selected,
                icon: if item.directory { "folder" } else { "file" },
            })
            .collect())
    }
    fn render_tree_diff(
        &mut self,
        previous: &[TreeRenderRow],
        snapshot: &ShellSnapshot,
    ) -> io::Result<Vec<TreeRowChange>> {
        let current = self.render_tree(snapshot)?;
        Ok(diff_tree_rows(previous, &current))
    }
}

#[derive(Default, Debug)]
pub struct HeadlessRenderer {
    last: Option<ShellSnapshot>,
    frames: u64,
    tree_rows: Vec<TreeRenderRow>,
    tree_changes: Vec<TreeRowChange>,
}

impl HeadlessRenderer {
    pub fn last_snapshot(&self) -> Option<&ShellSnapshot> {
        self.last.as_ref()
    }
    pub fn frames(&self) -> u64 {
        self.frames
    }
    pub fn tree_rows(&self) -> &[TreeRenderRow] {
        &self.tree_rows
    }
    pub fn tree_changes(&self) -> &[TreeRowChange] {
        &self.tree_changes
    }
}

impl UiRenderer for HeadlessRenderer {
    fn render_tree(&mut self, snapshot: &ShellSnapshot) -> io::Result<Vec<TreeRenderRow>> {
        let rows = tree_rows(snapshot);
        self.tree_changes = diff_tree_rows(&self.tree_rows, &rows);
        self.tree_rows = rows.clone();
        Ok(rows)
    }

    fn render(&mut self, snapshot: &ShellSnapshot) -> io::Result<()> {
        self.last = Some(snapshot.clone());
        self.frames += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_tree_rows_reports_add_remove_and_update() {
        let old = vec![TreeRenderRow {
            path: "old.rs".into(),
            label: "old.rs".into(),
            x: 0,
            y: 0,
            selected: false,
            icon: "file",
        }];
        let current = vec![
            TreeRenderRow {
                path: "old.rs".into(),
                label: "old.rs".into(),
                x: 0,
                y: 0,
                selected: true,
                icon: "file",
            },
            TreeRenderRow {
                path: "new.rs".into(),
                label: "new.rs".into(),
                x: 0,
                y: 24,
                selected: false,
                icon: "file",
            },
        ];
        let changes = diff_tree_rows(&old, &current);
        assert!(changes
            .iter()
            .any(|change| matches!(change, TreeRowChange::Updated(row) if row.selected)));
        assert!(changes
            .iter()
            .any(|change| matches!(change, TreeRowChange::Added(row) if row.path == std::path::Path::new("new.rs"))));
        let removed = diff_tree_rows(&current, &old);
        assert!(removed
            .iter()
            .any(|change| matches!(change, TreeRowChange::Removed(path) if path == &std::path::PathBuf::from("new.rs"))));
    }

    #[test]
    fn tree_change_batch_reports_frame_and_counts() {
        let changes = vec![
            TreeRowChange::Added(TreeRenderRow {
                path: "src".into(),
                label: "src".into(),
                x: 0,
                y: 0,
                selected: false,
                icon: "folder",
            }),
            TreeRowChange::Updated(TreeRenderRow {
                path: "src/main.rs".into(),
                label: "main.rs".into(),
                x: 16,
                y: 24,
                selected: true,
                icon: "file",
            }),
            TreeRowChange::Removed("old.rs".into()),
        ];
        let batch = TreeChangeBatch { frame: 7, changes };
        assert_eq!(batch.frame, 7);
        assert_eq!(batch.counts(), (1, 1, 1));
    }

    #[test]
    fn tree_rows_projects_layout_and_metadata() {
        let snapshot = ShellSnapshot {
            title: "NextIDE".into(),
            open_files: 0,
            tabs: 0,
            editor_views: 0,
            tree_nodes: 2,
            tab_paths: vec![],
            active_tab: None,
            tree_paths: vec![],
            visible_tree_paths: vec![],
            visible_tree: vec![
                crate::TreeItem {
                    path: "src".into(),
                    name: "src".into(),
                    depth: 0,
                    indent: 0,
                    directory: true,
                    expanded: true,
                    selected: false,
                },
                crate::TreeItem {
                    path: "src/main.rs".into(),
                    name: "main.rs".into(),
                    depth: 1,
                    indent: 16,
                    directory: false,
                    expanded: false,
                    selected: true,
                },
            ],
            selected_tree: Some("src/main.rs".into()),
            palette_open: false,
            palette_query: String::new(),
            notifications: 0,
        };
        let rows = tree_rows(&snapshot);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[1].x, rows[1].y, rows[1].icon, rows[1].selected),
            (16, 24, "file", true)
        );
    }

    #[test]
    fn folder_toggle_hit_is_limited_to_arrow_region() {
        let row = TreeRenderRow {
            path: "src".into(),
            label: "src".into(),
            x: 16,
            y: 24,
            selected: false,
            icon: "folder",
        };
        assert!(row.toggle_hit(crate::Point { x: 8, y: 30 }));
        assert!(!row.toggle_hit(crate::Point { x: 20, y: 30 }));
        let file = TreeRenderRow {
            icon: "file",
            ..row
        };
        assert!(!file.toggle_hit(crate::Point { x: 8, y: 30 }));
    }

    #[test]
    fn tree_render_row_hit_respects_indent_and_bounds() {
        let row = TreeRenderRow {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            x: 16,
            y: 24,
            selected: false,
            icon: "file",
        };
        assert!(row.hit(crate::Point { x: 20, y: 30 }, 280));
        assert!(!row.hit(crate::Point { x: 10, y: 30 }, 280));
        assert!(!row.hit(crate::Point { x: 20, y: 50 }, 280));
    }

    #[test]
    fn headless_renderer_keeps_latest_snapshot() {
        let mut renderer = HeadlessRenderer::default();
        let snapshot = ShellSnapshot {
            title: "NextIDE".into(),
            open_files: 0,
            tabs: 0,
            editor_views: 0,
            tree_nodes: 0,
            tab_paths: vec![],
            active_tab: None,
            tree_paths: vec![],
            visible_tree_paths: vec![],
            visible_tree: vec![],
            selected_tree: None,
            palette_open: false,
            palette_query: String::new(),
            notifications: 0,
        };
        renderer.render(&snapshot).unwrap();
        assert_eq!(renderer.frames(), 1);
        assert_eq!(renderer.last_snapshot().unwrap().title, "NextIDE");
    }
}
