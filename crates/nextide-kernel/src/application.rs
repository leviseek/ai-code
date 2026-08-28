use std::io;
use std::path::{Path, PathBuf};

use crate::{
    CapabilityRegistry, EditorKernel, EditorViewState, Event, EventBus, FileSystem, FileTreeModel,
    TabModel, WorkspaceState,
};

/// Platform-independent application state shared by desktop shells.
#[derive(Debug)]
pub struct ApplicationContext {
    pub editor: EditorKernel,
    pub capabilities: CapabilityRegistry,
    pub events: EventBus,
    pub workspace: WorkspaceState,
    pub file_tree: FileTreeModel,
    pub tabs: TabModel,
    pub views: Vec<EditorViewState>,
}

impl ApplicationContext {
    pub fn new(initial_text: impl Into<String>) -> Self {
        Self {
            editor: EditorKernel::new(initial_text),
            capabilities: CapabilityRegistry::default(),
            events: EventBus::default(),
            workspace: WorkspaceState::new("."),
            file_tree: FileTreeModel::default(),
            tabs: TabModel::default(),
            views: Vec::new(),
        }
    }

    pub fn open_file<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<()> {
        self.editor = EditorKernel::open(fs, path)?;
        self.workspace.open_file_with_events(path, &mut self.events);
        self.tabs.open(path);
        if !self.views.iter().any(|view| view.path == path) {
            self.views.push(EditorViewState::new(path));
        }
        self.events
            .publish(Event::new("editor.opened", path.to_string_lossy()));
        Ok(())
    }

    pub fn scan_workspace<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<()> {
        self.file_tree.scan(fs, path)?;
        self.events.publish(Event::new(
            "workspace.tree.updated",
            self.file_tree.nodes().len().to_string(),
        ));
        Ok(())
    }

    pub fn expand_directory<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<bool> {
        let expanded = self.file_tree.expand(fs, path)?;
        if expanded {
            self.events.publish(Event::new(
                "workspace.tree.expanded",
                path.to_string_lossy(),
            ));
        }
        Ok(expanded)
    }

    pub fn save_file<F: FileSystem>(&mut self, fs: &F) -> io::Result<()> {
        self.editor.save_with_events(fs, &mut self.events)?;
        if let Some(path) = self.editor.path().map(PathBuf::from) {
            self.tabs.set_dirty(&path, false);
            if let Some(view) = self.views.iter_mut().find(|view| view.path == path) {
                view.mark_dirty(false);
            }
        }
        Ok(())
    }

    pub fn close_file(&mut self, path: &Path, force: bool) -> io::Result<bool> {
        let dirty = self
            .views
            .iter()
            .any(|view| view.path == path && view.dirty);
        if dirty && !force {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "document has unsaved changes",
            ));
        }
        let path_buf = path.to_path_buf();
        let removed = self
            .workspace
            .close_file_with_events(&path_buf, &mut self.events);
        if !removed {
            return Ok(false);
        }
        self.tabs.close(&path_buf);
        self.views.retain(|view| view.path != path);
        self.events
            .publish(Event::new("editor.closed", path.to_string_lossy()));
        Ok(true)
    }

    pub fn bootstrap(initial_text: impl Into<String>) -> Self {
        let mut context = Self::new(initial_text);
        for (id, permission) in [
            ("workspace.read", "workspace.read"),
            ("workspace.write", "workspace.write"),
            ("terminal.execute", "terminal.execute"),
        ] {
            context
                .capabilities
                .register(crate::CapabilityDescriptor::new(id, "1.0").with_permission(permission))
                .expect("built-in capability ids are unique");
        }
        context
            .events
            .publish(crate::Event::new("application.ready", "kernel"));
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityDescriptor, EditTransaction, TextEdit};

    struct MemoryFs;
    impl FileSystem for MemoryFs {
        fn read(&self, _: &Path) -> io::Result<String> {
            Ok("hello".into())
        }
        fn write(&self, _: &Path, _: &str) -> io::Result<()> {
            Ok(())
        }
        fn exists(&self, _: &Path) -> bool {
            true
        }
        fn list(&self, path: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
            Ok(vec![
                (path.join("src"), true),
                (path.join("README.md"), false),
            ])
        }
    }

    #[test]
    fn open_file_synchronizes_workspace_tabs_and_views() {
        let mut context = ApplicationContext::new("");
        context
            .open_file(&MemoryFs, Path::new("src/main.rs"))
            .unwrap();
        assert_eq!(context.editor.document().text(), "hello");
        assert_eq!(
            context.tabs.active().unwrap().path,
            Path::new("src/main.rs")
        );
        assert_eq!(context.views.len(), 1);
        assert_eq!(context.events.pending("editor.opened"), 1);
    }

    #[test]
    fn scan_workspace_updates_tree_and_emits_event() {
        let mut context = ApplicationContext::new("");
        context.scan_workspace(&MemoryFs, Path::new(".")).unwrap();
        assert_eq!(context.file_tree.nodes().len(), 2);
        assert_eq!(context.events.pending("workspace.tree.updated"), 1);
    }

    #[test]
    fn save_file_clears_view_and_tab_dirty_state() {
        let mut context = ApplicationContext::new("");
        let path = Path::new("src/main.rs");
        context.open_file(&MemoryFs, path).unwrap();
        context.views[0].mark_dirty(true);
        context.tabs.set_dirty(&path.to_path_buf(), true);
        context.save_file(&MemoryFs).unwrap();
        assert!(!context.views[0].dirty);
        assert!(!context.tabs.active().unwrap().dirty);
        assert_eq!(context.events.pending("document.saved"), 1);
    }

    #[test]
    fn bootstrap_registers_core_capabilities() {
        let context = ApplicationContext::bootstrap("");
        assert!(context.capabilities.contains("workspace.read"));
        assert!(context.capabilities.contains("terminal.execute"));
        assert_eq!(context.events.pending("application.ready"), 1);
    }

    #[test]
    fn context_connects_editor_events_and_capabilities() {
        let mut context = ApplicationContext::new("hello");
        context
            .capabilities
            .register(CapabilityDescriptor::new("search", "1.0"))
            .unwrap();
        context
            .editor
            .apply_with_events(
                EditTransaction::new("edit", 0, vec![TextEdit::replace(0, 5, "world")]),
                &mut context.events,
            )
            .unwrap();
        assert_eq!(context.editor.document().text(), "world");
        assert_eq!(context.events.pending("document.changed"), 1);
        assert!(context.capabilities.contains("search"));
    }
}
