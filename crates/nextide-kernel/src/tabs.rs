use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub path: PathBuf,
    pub pinned: bool,
    pub dirty: bool,
}

#[derive(Default, Debug)]
pub struct TabModel {
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl TabModel {
    pub fn open(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active = Some(index);
            return;
        }
        self.tabs.push(Tab {
            path,
            pinned: false,
            dirty: false,
        });
        self.active = Some(self.tabs.len() - 1);
    }
    pub fn close(&mut self, path: &PathBuf) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| &tab.path == path) else {
            return false;
        };
        self.tabs.remove(index);
        self.active = match self.active {
            Some(active) if active == index => self.tabs.len().checked_sub(1),
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
        true
    }
    pub fn set_dirty(&mut self, path: &PathBuf, dirty: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.path == path) {
            tab.dirty = dirty;
        }
    }
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|index| self.tabs.get(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tab_model_deduplicates_and_tracks_active_tab() {
        let mut model = TabModel::default();
        model.open("main.rs");
        model.open("lib.rs");
        model.open("main.rs");
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(model.active().unwrap().path, PathBuf::from("main.rs"));
        model.set_dirty(&PathBuf::from("main.rs"), true);
        assert!(model.active().unwrap().dirty);
        assert!(model.close(&PathBuf::from("main.rs")));
        assert_eq!(model.active().unwrap().path, PathBuf::from("lib.rs"));
    }
}
