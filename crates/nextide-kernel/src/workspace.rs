use crate::{Event, EventBus};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceState {
    pub root: PathBuf,
    open_files: BTreeSet<PathBuf>,
    active_file: Option<PathBuf>,
}

impl WorkspaceState {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            open_files: BTreeSet::new(),
            active_file: None,
        }
    }

    pub fn open_file(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.open_files.insert(path.clone());
        self.active_file = Some(path);
    }

    pub fn open_file_with_events(&mut self, path: impl Into<PathBuf>, events: &mut EventBus) {
        let path = path.into();
        self.open_file(path.clone());
        events.publish(Event::new("workspace.file.opened", path.to_string_lossy()));
    }

    pub fn close_file_with_events(&mut self, path: &PathBuf, events: &mut EventBus) -> bool {
        let removed = self.close_file(path);
        if removed {
            events.publish(Event::new("workspace.file.closed", path.to_string_lossy()));
        }
        removed
    }

    pub fn close_file(&mut self, path: &PathBuf) -> bool {
        let removed = self.open_files.remove(path);
        if self.active_file.as_ref() == Some(path) {
            self.active_file = self.open_files.iter().next_back().cloned();
        }
        removed
    }

    pub fn open_files(&self) -> impl Iterator<Item = &PathBuf> {
        self.open_files.iter()
    }
    pub fn active_file(&self) -> Option<&PathBuf> {
        self.active_file.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_publishes_file_events() {
        let mut state = WorkspaceState::new("project");
        let mut events = EventBus::default();
        state.open_file_with_events("main.rs", &mut events);
        let path = PathBuf::from("main.rs");
        state.close_file_with_events(&path, &mut events);
        assert_eq!(events.drain("workspace.file.opened")[0].payload, "main.rs");
        assert_eq!(events.drain("workspace.file.closed")[0].payload, "main.rs");
    }

    #[test]
    fn workspace_tracks_open_and_active_files() {
        let mut state = WorkspaceState::new("project");
        state.open_file("src/main.rs");
        state.open_file("src/lib.rs");
        assert_eq!(state.open_files().count(), 2);
        assert_eq!(state.active_file(), Some(&PathBuf::from("src/lib.rs")));
        assert!(state.close_file(&PathBuf::from("src/lib.rs")));
        assert_eq!(state.active_file(), Some(&PathBuf::from("src/main.rs")));
    }
}
