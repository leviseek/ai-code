use crate::FileSystem;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileNodeKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileNodeKind,
    pub expanded: bool,
}

#[derive(Default, Debug)]
pub struct FileTreeModel {
    nodes: Vec<FileNode>,
}

impl FileTreeModel {
    pub fn add(&mut self, path: impl Into<PathBuf>, kind: FileNodeKind) {
        let path = path.into();
        if self.nodes.iter().any(|node| node.path == path) {
            return;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.nodes.push(FileNode {
            path,
            name,
            kind,
            expanded: false,
        });
        self.nodes.sort_by(|a, b| a.path.cmp(&b.path));
    }
    pub fn scan<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<()> {
        self.nodes.clear();
        for (entry, is_dir) in fs.list(path)? {
            self.add(
                entry,
                if is_dir {
                    FileNodeKind::Directory
                } else {
                    FileNodeKind::File
                },
            );
        }
        Ok(())
    }

    pub fn expand<F: FileSystem>(&mut self, fs: &F, path: &Path) -> io::Result<bool> {
        let Some(node) = self.nodes.iter().find(|node| node.path == path) else {
            return Ok(false);
        };
        if node.kind != FileNodeKind::Directory {
            return Ok(false);
        }
        if !node.expanded {
            for (entry, is_dir) in fs.list(path)? {
                self.add(
                    entry,
                    if is_dir {
                        FileNodeKind::Directory
                    } else {
                        FileNodeKind::File
                    },
                );
            }
            if let Some(node) = self.nodes.iter_mut().find(|node| node.path == path) {
                node.expanded = true;
            }
        }
        Ok(true)
    }

    pub fn toggle(&mut self, path: &PathBuf) -> bool {
        if let Some(node) = self.nodes.iter_mut().find(|node| &node.path == path) {
            node.expanded = !node.expanded;
            return node.expanded;
        }
        false
    }
    pub fn nodes(&self) -> &[FileNode] {
        &self.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_deduplicates_sorts_and_toggles_directories() {
        let mut tree = FileTreeModel::default();
        tree.add("src/lib.rs", FileNodeKind::File);
        tree.add("src", FileNodeKind::Directory);
        tree.add("src", FileNodeKind::Directory);
        assert_eq!(tree.nodes().len(), 2);
        assert!(tree.toggle(&PathBuf::from("src")));
        assert!(
            tree.nodes()
                .iter()
                .find(|node| node.path == PathBuf::from("src"))
                .unwrap()
                .expanded
        );
    }
}
