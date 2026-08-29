//! NextIDE's transactional editor kernel.
//!
//! The kernel is intentionally UI- and platform-independent. All callers submit
//! changes as transactions so undo/redo and recovery share one source of truth.

use std::fmt;
use std::io;
use std::path::Path;

pub mod application;
pub mod async_task;
pub mod capability;
pub mod editor_view;
pub mod events;
pub mod file_tree;
pub mod git;
pub mod platform;
pub mod process;
pub mod render;
pub mod rpc;
pub mod search;
pub mod tabs;
pub mod task;
pub mod task_events;
pub mod task_manager;
pub mod terminal;
pub mod vfs;
pub mod window;
pub mod workspace;

pub use application::ApplicationContext;
pub use async_task::{AsyncTaskEvent, AsyncTaskHandle, AsyncTaskSnapshot, AsyncTaskStatus};
pub use capability::{CapabilityDescriptor, CapabilityRegistry, CapabilitySummary, RegistryError};
pub use editor_view::EditorViewState;
pub use events::{Event, EventBus};
pub use file_tree::{FileNode, FileNodeKind, FileTreeModel};
pub use git::{
    branches, checkout, commit, diff, stage, status, GitBranch, GitBranchSnapshot, GitCommitResult,
    GitDiffFile, GitDiffHunk, GitDiffLine, GitDiffLineKind, GitDiffSnapshot, GitFileState,
    GitStatusCode, GitStatusEntry, GitStatusSnapshot,
};
pub use platform::{PlatformServices, ProcessHandle, TerminalSession, WindowsPlatform};
pub use process::{run_capture, run_capture_timeout, ProcessOutput};
pub use render::{HeadlessSurface, RenderSurface, SurfaceSize};
pub use rpc::{LocalRpcRouter, RpcError, RpcHandler, RpcRequest, RpcResponse};
pub use search::{SearchMatch, SearchMatcher, SearchOptions, SearchQuery};
pub use tabs::{Tab, TabModel};
pub use task::{Task, TaskError, TaskResult, TaskSpec, TaskState};
pub use task_events::{AsyncTaskEventSink, TaskEventReceiver};
pub use task_manager::{TaskManager, TaskSummary};
pub use terminal::TerminalProfile;
pub use vfs::{FileEntry, FileKind, FileMetadata, FileSystem, LocalFileSystem};
pub use window::{
    HeadlessWindowService, WindowEvent, WindowHandle, WindowOptions, WindowService, WindowSize,
};
pub use workspace::WorkspaceState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    text: String,
    revision: u64,
}

impl Document {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            revision: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

impl TextEdit {
    pub fn replace(start: usize, end: usize, replacement: impl Into<String>) -> Self {
        Self {
            start,
            end,
            replacement: replacement.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub revision: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KernelError {
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    InvalidUtf8Boundary {
        offset: usize,
    },
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    EmptyTransaction,
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange { start, end, len } => write!(
                f,
                "invalid edit range {start}..{end} for document length {len}"
            ),
            Self::InvalidUtf8Boundary { offset } => {
                write!(f, "offset {offset} is not a UTF-8 boundary")
            }
            Self::RevisionConflict { expected, actual } => {
                write!(f, "revision conflict: expected {expected}, actual {actual}")
            }
            Self::EmptyTransaction => write!(f, "transaction must contain at least one edit"),
        }
    }
}

impl std::error::Error for KernelError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditTransaction {
    pub label: String,
    pub expected_revision: u64,
    pub edits: Vec<TextEdit>,
}

impl EditTransaction {
    pub fn new(label: impl Into<String>, expected_revision: u64, edits: Vec<TextEdit>) -> Self {
        Self {
            label: label.into(),
            expected_revision,
            edits,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedTransaction {
    pub label: String,
    pub before: Snapshot,
    pub after: Snapshot,
}

#[derive(Debug)]
pub struct EditorKernel {
    document: Document,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    path: Option<String>,
    dirty: bool,
}

impl EditorKernel {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            document: Document::new(text),
            undo: Vec::new(),
            redo: Vec::new(),
            path: None,
            dirty: false,
        }
    }

    pub fn open<F: FileSystem>(fs: &F, path: &Path) -> io::Result<Self> {
        let mut kernel = Self::new(fs.read(path)?);
        kernel.path = Some(path.to_string_lossy().into_owned());
        Ok(kernel)
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn save<F: FileSystem>(&mut self, fs: &F) -> io::Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "document has no path"))?;
        fs.atomic_write(Path::new(path), self.document.text())?;
        self.dirty = false;
        Ok(())
    }

    pub fn save_with_events<F: FileSystem>(
        &mut self,
        fs: &F,
        bus: &mut EventBus,
    ) -> io::Result<()> {
        self.save(fs)?;
        bus.publish(Event::new(
            "document.saved",
            format!("revision={}", self.document.revision),
        ));
        Ok(())
    }

    pub fn save_as<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<()> {
        fs.atomic_write(path, self.document.text())?;
        self.path = Some(path.to_string_lossy().into_owned());
        self.dirty = false;
        Ok(())
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.document.text.clone(),
            revision: self.document.revision,
        }
    }

    pub fn apply(&mut self, tx: EditTransaction) -> Result<AppliedTransaction, KernelError> {
        if tx.edits.is_empty() {
            return Err(KernelError::EmptyTransaction);
        }
        if tx.expected_revision != self.document.revision {
            return Err(KernelError::RevisionConflict {
                expected: tx.expected_revision,
                actual: self.document.revision,
            });
        }

        let before = self.snapshot();
        let mut text = self.document.text.clone();
        let mut edits = tx.edits;
        edits.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
        for edit in edits {
            if edit.start > edit.end || edit.end > text.len() {
                return Err(KernelError::InvalidRange {
                    start: edit.start,
                    end: edit.end,
                    len: text.len(),
                });
            }
            if !text.is_char_boundary(edit.start) {
                return Err(KernelError::InvalidUtf8Boundary { offset: edit.start });
            }
            if !text.is_char_boundary(edit.end) {
                return Err(KernelError::InvalidUtf8Boundary { offset: edit.end });
            }
            text.replace_range(edit.start..edit.end, &edit.replacement);
        }

        self.undo.push(before.clone());
        self.redo.clear();
        self.document.text = text;
        self.document.revision += 1;
        self.dirty = true;
        let after = self.snapshot();
        Ok(AppliedTransaction {
            label: tx.label,
            before,
            after,
        })
    }

    pub fn apply_with_events(
        &mut self,
        tx: EditTransaction,
        bus: &mut EventBus,
    ) -> Result<AppliedTransaction, KernelError> {
        let applied = self.apply(tx)?;
        bus.publish(Event::new(
            "document.changed",
            format!("revision={}", applied.after.revision),
        ));
        Ok(applied)
    }

    pub fn undo(&mut self) -> Option<Snapshot> {
        let previous = self.undo.pop()?;
        self.redo.push(self.snapshot());
        self.document.text = previous.text.clone();
        self.document.revision += 1;
        Some(self.snapshot())
    }

    pub fn redo(&mut self) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.undo.push(self.snapshot());
        self.document.text = next.text.clone();
        self.document.revision += 1;
        Some(self.snapshot())
    }

    pub fn restore(&mut self, snapshot: Snapshot) {
        self.undo.push(self.snapshot());
        self.redo.clear();
        self.document.text = snapshot.text;
        self.document.revision = snapshot.revision.max(self.document.revision + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_document_changed_after_transaction() {
        let mut kernel = EditorKernel::new("a");
        let mut bus = EventBus::default();
        kernel
            .apply_with_events(
                EditTransaction::new("insert", 0, vec![TextEdit::replace(1, 1, "b")]),
                &mut bus,
            )
            .unwrap();
        assert_eq!(bus.pending("document.changed"), 1);
        assert_eq!(bus.drain("document.changed")[0].payload, "revision=1");
    }

    #[test]
    fn applies_multiple_edits_in_reverse_order() {
        let mut kernel = EditorKernel::new("hello world");
        let tx = EditTransaction::new(
            "rename",
            0,
            vec![
                TextEdit::replace(6, 11, "IDE"),
                TextEdit::replace(0, 5, "hello"),
            ],
        );
        let result = kernel.apply(tx).unwrap();
        assert_eq!(result.after.text, "hello IDE");
        assert_eq!(kernel.document().revision(), 1);
    }

    #[test]
    fn undo_and_redo_are_transactional() {
        let mut kernel = EditorKernel::new("a");
        kernel
            .apply(EditTransaction::new(
                "insert",
                0,
                vec![TextEdit::replace(1, 1, "b")],
            ))
            .unwrap();
        assert_eq!(kernel.document().text(), "ab");
        kernel.undo().unwrap();
        assert_eq!(kernel.document().text(), "a");
        kernel.redo().unwrap();
        assert_eq!(kernel.document().text(), "ab");
    }

    #[test]
    fn rejects_stale_transactions() {
        let mut kernel = EditorKernel::new("abc");
        kernel
            .apply(EditTransaction::new(
                "first",
                0,
                vec![TextEdit::replace(0, 1, "A")],
            ))
            .unwrap();
        let error = kernel
            .apply(EditTransaction::new(
                "stale",
                0,
                vec![TextEdit::replace(1, 2, "B")],
            ))
            .unwrap_err();
        assert_eq!(
            error,
            KernelError::RevisionConflict {
                expected: 0,
                actual: 1
            }
        );
    }

    #[test]
    fn rejects_invalid_utf8_boundaries() {
        let mut kernel = EditorKernel::new("中文");
        let error = kernel
            .apply(EditTransaction::new(
                "bad",
                0,
                vec![TextEdit::replace(1, 2, "x")],
            ))
            .unwrap_err();
        assert_eq!(error, KernelError::InvalidUtf8Boundary { offset: 1 });
    }

    #[test]
    fn opens_edits_and_saves_through_vfs() {
        let root = std::env::temp_dir().join(format!("nextide-kernel-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let path = Path::new("src/main.rs");
        fs.write(path, "fn main() {}\n").unwrap();
        let mut kernel = EditorKernel::open(&fs, path).unwrap();
        assert_eq!(kernel.path(), Some("src/main.rs"));
        assert!(!kernel.is_dirty());
        kernel
            .apply(EditTransaction::new(
                "rename",
                0,
                vec![TextEdit::replace(3, 7, "start")],
            ))
            .unwrap();
        assert!(kernel.is_dirty());
        kernel.save(&fs).unwrap();
        assert!(!kernel.is_dirty());
        assert_eq!(fs.read(path).unwrap(), "fn start() {}\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publishes_saved_after_vfs_save() {
        let root = std::env::temp_dir().join(format!("nextide-saved-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let path = Path::new("main.rs");
        fs.write(path, "a").unwrap();
        let mut kernel = EditorKernel::open(&fs, path).unwrap();
        kernel
            .apply(EditTransaction::new(
                "edit",
                0,
                vec![TextEdit::replace(0, 1, "b")],
            ))
            .unwrap();
        let mut bus = EventBus::default();
        kernel.save_with_events(&fs, &mut bus).unwrap();
        assert_eq!(bus.drain("document.saved")[0].payload, "revision=1");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn save_without_path_is_rejected() {
        let mut kernel = EditorKernel::new("text");
        let fs = LocalFileSystem::unrestricted();
        assert_eq!(
            kernel.save(&fs).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn restores_snapshot_and_clears_redo() {
        let mut kernel = EditorKernel::new("before");
        let snapshot = kernel.snapshot();
        kernel
            .apply(EditTransaction::new(
                "change",
                0,
                vec![TextEdit::replace(0, 6, "after")],
            ))
            .unwrap();
        kernel.restore(snapshot);
        assert_eq!(kernel.document().text(), "before");
        assert!(kernel.redo().is_none());
    }
}
