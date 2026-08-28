#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiRegion {
    FileTree,
    Tabs,
    Editor,
    CommandPalette,
    StatusBar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellLayout {
    pub file_tree: Rect,
    pub tabs: Rect,
    pub editor: Rect,
    pub status: Rect,
}

impl ShellLayout {
    pub fn for_size(width: u32, height: u32) -> Self {
        let tree_width = width.min(280);
        let tabs_height = height.min(40);
        let status_height = height.min(40);
        Self {
            file_tree: Rect {
                x: 0,
                y: 0,
                width: tree_width,
                height: height.saturating_sub(status_height),
            },
            tabs: Rect {
                x: tree_width,
                y: 0,
                width: width.saturating_sub(tree_width),
                height: tabs_height,
            },
            editor: Rect {
                x: tree_width,
                y: tabs_height,
                width: width.saturating_sub(tree_width),
                height: height.saturating_sub(tabs_height + status_height),
            },
            status: Rect {
                x: 0,
                y: height.saturating_sub(status_height),
                width,
                height: status_height,
            },
        }
    }

    pub fn default_1280x800() -> Self {
        Self {
            file_tree: Rect {
                x: 0,
                y: 0,
                width: 280,
                height: 760,
            },
            tabs: Rect {
                x: 280,
                y: 0,
                width: 1000,
                height: 40,
            },
            editor: Rect {
                x: 280,
                y: 40,
                width: 1000,
                height: 720,
            },
            status: Rect {
                x: 0,
                y: 760,
                width: 1280,
                height: 40,
            },
        }
    }
    pub fn hit_test(&self, point: Point) -> Option<UiRegion> {
        [
            (self.file_tree, UiRegion::FileTree),
            (self.tabs, UiRegion::Tabs),
            (self.editor, UiRegion::Editor),
            (self.status, UiRegion::StatusBar),
        ]
        .into_iter()
        .find_map(|(rect, region)| rect.contains(point).then_some(region))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hit_testing_identifies_regions() {
        let layout = ShellLayout::default_1280x800();
        assert_eq!(
            layout.hit_test(Point { x: 10, y: 10 }),
            Some(UiRegion::FileTree)
        );
        assert_eq!(
            layout.hit_test(Point { x: 500, y: 20 }),
            Some(UiRegion::Tabs)
        );
        assert_eq!(
            layout.hit_test(Point { x: 500, y: 400 }),
            Some(UiRegion::Editor)
        );
        assert_eq!(
            layout.hit_test(Point { x: 500, y: 780 }),
            Some(UiRegion::StatusBar)
        );
        assert_eq!(layout.hit_test(Point { x: 1300, y: 10 }), None);
    }
}
