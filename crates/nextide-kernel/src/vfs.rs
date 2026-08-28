use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The kind of a filesystem entry as reported by [`FileSystem::metadata`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// Any other entry (device, socket, pipe, ...).
    Other,
}

/// Read-only, typed metadata for a filesystem entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    /// The kind of the entry.
    pub kind: FileKind,
    /// Size of the entry in bytes (0 for directories and non-file entries).
    pub len: u64,
    /// Whether the entry is marked read-only.
    pub readonly: bool,
}

impl FileMetadata {
    /// Whether this entry is a regular file.
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }

    /// Whether this entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }
}

pub trait FileSystem {
    fn read(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;
    fn atomic_write(&self, path: &Path, text: &str) -> io::Result<()> {
        self.write(path, text)
    }

    /// Whether `path` exists inside the workspace.
    ///
    /// This is a typed, read-only query built on [`FileSystem::metadata`]:
    /// existing entries report `Ok(true)`, missing entries report `Ok(false)`,
    /// and access failures such as workspace-root escapes report `Err`.
    /// Implementations must keep workspace-root safety constraints: paths that
    /// escape the workspace root must fail with
    /// [`io::ErrorKind::PermissionDenied`].
    fn exists(&self, path: &Path) -> io::Result<bool> {
        match self.metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Reads typed, read-only metadata for `path`.
    ///
    /// Implementations must keep workspace-root safety constraints: paths that
    /// escape the workspace root must fail with
    /// [`io::ErrorKind::PermissionDenied`], and missing entries with
    /// [`io::ErrorKind::NotFound`].
    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "metadata is not supported",
        ))
    }

    /// Whether `path` names a regular file.
    ///
    /// Returns `Ok(false)` for missing entries and `Err` for access failures
    /// such as workspace-root escapes.
    fn is_file(&self, path: &Path) -> io::Result<bool> {
        match self.metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Whether `path` names a directory.
    ///
    /// Returns `Ok(false)` for missing entries and `Err` for access failures
    /// such as workspace-root escapes.
    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        match self.metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn list(&self, path: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory listing is not supported",
        ))
    }
}

#[derive(Clone, Debug)]
pub struct LocalFileSystem {
    root: Option<PathBuf>,
}

impl LocalFileSystem {
    pub fn unrestricted() -> Self {
        Self { root: None }
    }

    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
        }
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        if self.root.is_some()
            && path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path escapes workspace root",
            ));
        }
        let resolved = match &self.root {
            Some(root) if path.is_absolute() => path.to_path_buf(),
            Some(root) => {
                // Join against the canonical root so that missing children
                // (which cannot be canonicalized themselves) still compare
                // against the same prefix as existing entries.
                let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                root.join(path)
            }
            None => path.to_path_buf(),
        };
        if let Some(root) = &self.root {
            let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            let candidate = if resolved.exists() {
                fs::canonicalize(&resolved).unwrap_or(resolved.clone())
            } else {
                resolved.clone()
            };
            if !candidate.starts_with(&root) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "path escapes workspace root",
                ));
            }
        }
        Ok(resolved)
    }
}

impl FileSystem for LocalFileSystem {
    fn read(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(self.resolve(path)?)
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        let resolved = self.resolve(path)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(resolved, text)
    }

    fn atomic_write(&self, path: &Path, text: &str) -> io::Result<()> {
        let resolved = self.resolve(path)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = resolved.with_extension("nextide-tmp");
        fs::write(&temp, text)?;
        fs::rename(temp, resolved)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let resolved = self.resolve(path)?;
        let metadata = fs::metadata(resolved)?;
        let kind = if metadata.is_dir() {
            FileKind::Directory
        } else if metadata.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };
        Ok(FileMetadata {
            kind,
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
        })
    }

    fn list(&self, path: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
        let directory = self.resolve(path)?;
        let mut entries = fs::read_dir(directory)?
            .map(|entry| {
                let entry = entry?;
                Ok((path.join(entry.file_name()), entry.file_type()?.is_dir()))
            })
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_filesystem_reads_and_writes_inside_root() {
        let root = std::env::temp_dir().join(format!("nextide-vfs-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let file = Path::new("src/main.rs");
        fs.write(file, "fn main() {}\n").unwrap();
        assert_eq!(fs.read(file).unwrap(), "fn main() {}\n");
        assert!(fs.exists(file).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_filesystem_rejects_escape() {
        let root = std::env::temp_dir().join(format!("nextide-vfs-escape-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let error = fs.read(Path::new("..\\outside.txt")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_filesystem_reports_file_and_directory_kinds() {
        let root = std::env::temp_dir().join(format!("nextide-vfs-kind-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let file = Path::new("src/main.rs");
        let directory = Path::new("src");
        fs.write(file, "fn main() {}\n").unwrap();

        assert!(fs.is_file(file).unwrap());
        assert!(!fs.is_dir(file).unwrap());
        assert!(fs.is_dir(directory).unwrap());
        assert!(!fs.is_file(directory).unwrap());
        assert!(fs.exists(file).unwrap());
        assert!(fs.exists(directory).unwrap());

        let metadata = fs.metadata(file).unwrap();
        assert_eq!(metadata.kind, FileKind::File);
        assert_eq!(metadata.len, "fn main() {}\n".len() as u64);
        assert!(!metadata.readonly);
        assert!(metadata.is_file());
        assert!(!metadata.is_dir());

        let directory_metadata = fs.metadata(directory).unwrap();
        assert_eq!(directory_metadata.kind, FileKind::Directory);
        assert!(directory_metadata.is_dir());
        assert!(!directory_metadata.is_file());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_filesystem_missing_entries_are_reported_not_errors() {
        let root = std::env::temp_dir().join(format!("nextide-vfs-missing-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let missing = Path::new("does-not-exist.txt");
        assert!(!fs.exists(missing).unwrap());
        assert!(!fs.is_file(missing).unwrap());
        assert!(!fs.is_dir(missing).unwrap());
        assert_eq!(
            fs.metadata(missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_filesystem_metadata_rejects_escape() {
        let root =
            std::env::temp_dir().join(format!("nextide-vfs-meta-escape-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let escape = Path::new("..").join("outside.txt");
        assert_eq!(
            fs.is_file(&escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            fs.is_dir(&escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            fs.metadata(&escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            fs.exists(&escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_filesystem_rejects_absolute_and_nested_escape_paths() {
        let root =
            std::env::temp_dir().join(format!("nextide-vfs-abs-escape-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let outside = std::env::temp_dir().join(format!("nextide-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        assert_eq!(
            fs.metadata(&outside).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let nested = Path::new("src").join("..").join("..").join("outside.txt");
        assert_eq!(
            fs.is_file(&nested).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            fs.is_dir(&nested).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            fs.exists(&nested).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn rooted_filesystem_exists_is_typed_and_root_safe() {
        let root = std::env::temp_dir().join(format!("nextide-vfs-exists-{}", std::process::id()));
        let fs = LocalFileSystem::rooted(&root);
        let file = Path::new("src/main.rs");
        let directory = Path::new("src");
        fs.write(file, "fn main() {}\n").unwrap();

        // Existing entries inside the root report Ok(true).
        assert!(fs.exists(file).unwrap());
        assert!(fs.exists(directory).unwrap());

        // Missing entries inside the root report Ok(false), not Err.
        assert!(!fs.exists(Path::new("missing.txt")).unwrap());

        // Escapes and out-of-root absolute paths are Err, not a silent false.
        let escape = Path::new("..").join("outside.txt");
        assert_eq!(
            fs.exists(&escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let outside = std::env::temp_dir().join(format!("nextide-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        assert_eq!(
            fs.exists(&outside).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }
}
