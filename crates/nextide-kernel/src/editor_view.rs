use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorViewState {
    pub path: PathBuf,
    pub cursor_offset: usize,
    pub scroll_line: usize,
    pub dirty: bool,
}

impl EditorViewState {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cursor_offset: 0,
            scroll_line: 0,
            dirty: false,
        }
    }
    pub fn move_cursor(&mut self, offset: usize) {
        self.cursor_offset = offset;
    }
    pub fn scroll_to(&mut self, line: usize) {
        self.scroll_line = line;
    }
    pub fn mark_dirty(&mut self, dirty: bool) {
        self.dirty = dirty;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_tracks_cursor_scroll_and_dirty_state() {
        let mut view = EditorViewState::new("src/main.rs");
        view.move_cursor(42);
        view.scroll_to(10);
        view.mark_dirty(true);
        assert_eq!(view.cursor_offset, 42);
        assert_eq!(view.scroll_line, 10);
        assert!(view.dirty);
    }
}
