use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub trait FileSystem {
    fn read(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;
    fn atomic_write(&self, path: &Path, text: &str) -> io::Result<()> {
        self.write(path, text)
    }
    fn exists(&self, path: &Path) -> bool;
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
            Some(root) => root.join(path),
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

    fn exists(&self, path: &Path) -> bool {
        self.resolve(path)
            .map(|path| path.exists())
            .unwrap_or(false)
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
        assert!(fs.exists(file));
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
}
