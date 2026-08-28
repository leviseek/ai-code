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

pub trait UiRenderer {
    fn render(&mut self, snapshot: &ShellSnapshot) -> io::Result<()>;
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
}

#[derive(Default, Debug)]
pub struct HeadlessRenderer {
    last: Option<ShellSnapshot>,
    frames: u64,
    tree_rows: Vec<TreeRenderRow>,
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
}

impl UiRenderer for HeadlessRenderer {
    fn render_tree(&mut self, snapshot: &ShellSnapshot) -> io::Result<Vec<TreeRenderRow>> {
        let rows = snapshot
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
            .collect::<Vec<_>>();
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
