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
    pub depth: usize,
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
        let depth = path.components().count().saturating_sub(1);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.nodes.push(FileNode {
            path,
            name,
            kind,
            expanded: false,
            depth,
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

    pub fn collapse(&mut self, path: &Path) -> bool {
        if let Some(node) = self
            .nodes
            .iter_mut()
            .find(|node| node.path == path && node.kind == FileNodeKind::Directory)
        {
            node.expanded = false;
            return true;
        }
        false
    }

    pub fn toggle(&mut self, path: &PathBuf) -> bool {
        if let Some(node) = self.nodes.iter_mut().find(|node| &node.path == path) {
            node.expanded = !node.expanded;
            return node.expanded;
        }
        false
    }
    pub fn visible_nodes(&self) -> Vec<&FileNode> {
        self.nodes
            .iter()
            .filter(|node| {
                let mut parent = node.path.parent();
                while let Some(path) = parent {
                    if let Some(directory) = self.nodes.iter().find(|candidate| {
                        candidate.path == path && candidate.kind == FileNodeKind::Directory
                    }) {
                        if !directory.expanded {
                            return false;
                        }
                    }
                    parent = path.parent();
                }
                true
            })
            .collect()
    }

    pub fn nodes(&self) -> &[FileNode] {
        &self.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_nodes_hide_children_of_collapsed_directories() {
        let mut tree = FileTreeModel::default();
        tree.add("src", FileNodeKind::Directory);
        tree.add("src/main.rs", FileNodeKind::File);
        assert_eq!(tree.visible_nodes().len(), 1);
        assert!(tree.toggle(&PathBuf::from("src")));
        assert_eq!(tree.visible_nodes().len(), 2);
        assert!(tree.collapse(Path::new("src")));
        assert_eq!(tree.visible_nodes().len(), 1);
    }

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
                .find(|node| node.path == Path::new("src"))
                .unwrap()
                .expanded
        );
    }
}
