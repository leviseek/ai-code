//! Workspace text search over a [`FileSystem`].
//!
//! The module is dependency-free and uses only `std`. A search walks a
//! directory tree recursively through the [`FileSystem`] abstraction, reads
//! every regular file it can, and reports the lines containing the query
//! needle.
//!
//! Robustness rules:
//!
//! - Files that cannot be decoded as UTF-8 (binary files), files that contain
//!   a NUL byte, and files or directories the filesystem refuses to access
//!   are skipped silently.
//! - The search root itself must be readable; an unreadable or missing root
//!   propagates the underlying [`io::Error`].
//! - Results are stably sorted by `(path, line, column)` and truncated to
//!   [`SearchOptions::max_results`].
//! - Path safety is delegated to the rooted [`FileSystem`]: traversal only
//!   ever visits paths produced by `read_dir` entries, so escapes (including
//!   symlinks pointing outside the workspace root) surface as per-entry
//!   access errors and are skipped, and [`SearchOptions::max_depth`] keeps
//!   recursive walks finite even when the filesystem reports cycles through
//!   symlinked directories.

use crate::vfs::{FileEntry, FileKind, FileSystem};
use std::io;
use std::path::{Path, PathBuf};

/// Controls how a [`SearchQuery`] is executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    /// Match the needle respecting case. Defaults to `true`.
    pub case_sensitive: bool,
    /// Upper bound on the number of results returned; `None` returns every
    /// match. Defaults to `None`.
    pub max_results: Option<usize>,
    /// Maximum recursion depth below the query root; `None` walks without a
    /// bound. The default of 64 keeps the walk finite even when the
    /// underlying filesystem reports cycles through symlinked directories.
    pub max_depth: Option<usize>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            max_results: None,
            max_depth: Some(64),
        }
    }
}

/// A request to find every line containing `needle` under `root`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// The substring to search for. An empty needle matches nothing.
    pub needle: String,
    /// Directory to start the search from, relative to the filesystem root.
    pub root: PathBuf,
    /// Options controlling the execution.
    pub options: SearchOptions,
}

impl SearchQuery {
    pub fn new(
        needle: impl Into<String>,
        root: impl Into<PathBuf>,
        options: SearchOptions,
    ) -> Self {
        Self {
            needle: needle.into(),
            root: root.into(),
            options,
        }
    }

    /// Runs this query against `fs` and returns the matches.
    pub fn run<F: FileSystem>(&self, fs: &F) -> io::Result<Vec<SearchMatch>> {
        search(fs, self)
    }
}

/// One occurrence of the needle inside a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// Path of the matched file, relative to the filesystem root.
    pub path: PathBuf,
    /// 1-based line number of the match.
    pub line: usize,
    /// Byte offset of the needle inside the matched line, 0-based.
    pub column: usize,
    /// Text of the matched line, without the trailing newline.
    pub line_text: String,
}

/// Searches `query.root` inside `fs` for every line containing `query.needle`
/// and returns the matches stably sorted by `(path, line, column)`, truncated
/// to [`SearchOptions::max_results`].
///
/// Only regular files are read. Files that cannot be decoded as UTF-8
/// (binary files), files that contain a NUL byte, and files or directories
/// the filesystem refuses to access are skipped silently. An unreadable or
/// missing search root propagates the underlying error.
pub fn search<F: FileSystem>(fs: &F, query: &SearchQuery) -> io::Result<Vec<SearchMatch>> {
    if query.needle.is_empty() {
        return Ok(Vec::new());
    }
    let root = normalize_root(&query.root);
    let mut matches = Vec::new();
    walk(fs, query, &root, 0, &mut matches)?;
    matches.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
    });
    if let Some(max) = query.options.max_results {
        matches.truncate(max);
    }
    Ok(matches)
}

/// Maps the conventional "." root to an empty path so that entry paths come
/// back relative to the filesystem root without a leading `.\` or `./`.
fn normalize_root(root: &Path) -> PathBuf {
    if root.as_os_str().is_empty() || root == Path::new(".") {
        PathBuf::new()
    } else {
        root.to_path_buf()
    }
}

fn walk<F: FileSystem>(
    fs: &F,
    query: &SearchQuery,
    directory: &Path,
    depth: usize,
    matches: &mut Vec<SearchMatch>,
) -> io::Result<()> {
    if let Some(max_depth) = query.options.max_depth {
        if depth > max_depth {
            return Ok(());
        }
    }
    let entries = fs.read_dir(directory)?;
    for entry in entries {
        match entry.kind {
            FileKind::Directory => {
                // Unreadable or denied subdirectories are skipped like
                // unreadable files; only the search root is required to be
                // readable.
                if walk(fs, query, &entry.path, depth + 1, matches).is_err() {}
            }
            FileKind::File => {
                if let Some(hits) = search_file(fs, query, &entry) {
                    matches.extend(hits);
                }
            }
            FileKind::Other => {}
        }
    }
    Ok(())
}

fn search_file<F: FileSystem>(
    fs: &F,
    query: &SearchQuery,
    entry: &FileEntry,
) -> Option<Vec<SearchMatch>> {
    let text = fs.read(&entry.path).ok()?;
    if text.contains('\0') {
        // Treat NUL-containing files as binary even when they are valid UTF-8.
        return None;
    }
    let needle = &query.needle;
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for column in match_columns(line, needle, query.options.case_sensitive) {
            hits.push(SearchMatch {
                path: entry.path.clone(),
                line: index + 1,
                column,
                line_text: line.to_string(),
            });
        }
    }
    (!hits.is_empty()).then_some(hits)
}

/// Byte offsets of every needle occurrence inside `line`.
fn match_columns(line: &str, needle: &str, case_sensitive: bool) -> Vec<usize> {
    if case_sensitive {
        line.match_indices(needle).map(|(index, _)| index).collect()
    } else {
        let needle: Vec<char> = needle.chars().collect();
        line.char_indices()
            .filter(|(index, _)| chars_match_case_insensitive(&line[*index..], &needle))
            .map(|(index, _)| index)
            .collect()
    }
}

/// Whether `haystack` starts with `needle`, comparing characters with case
/// folding. Offsets stay valid because matching happens on the original
/// string, never on a lowercased copy.
fn chars_match_case_insensitive(haystack: &str, needle: &[char]) -> bool {
    let mut hay = haystack.chars();
    for &expected in needle {
        match hay.next() {
            Some(actual) if actual.to_lowercase().eq(expected.to_lowercase()) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalFileSystem;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nextide-search-{name}-{}", std::process::id()))
    }

    #[test]
    fn finds_text_matches_across_files_and_lines() {
        let root = temp_root("hits");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(
            Path::new("src/main.rs"),
            "fn main() {\n    println!(\"hello world\");\n}\n",
        )
        .unwrap();
        fs.write(Path::new("README.md"), "hello IDE\n").unwrap();
        fs.write(Path::new("src/other.txt"), "nothing here\n")
            .unwrap();

        let query = SearchQuery::new("hello", ".", SearchOptions::default());
        let matches = query.run(&fs).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].path, Path::new("README.md"));
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].column, 0);
        assert_eq!(matches[0].line_text, "hello IDE");
        assert_eq!(matches[1].path, Path::new("src/main.rs"));
        assert_eq!(matches[1].line, 2);
        assert_eq!(matches[1].column, 14);
        assert_eq!(matches[1].line_text, "    println!(\"hello world\");");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn case_sensitivity_respects_the_option() {
        let root = temp_root("case");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("a.txt"), "Hello\nhello\nHELLO\n")
            .unwrap();

        let sensitive = SearchQuery::new("hello", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert_eq!(sensitive.len(), 1);
        assert_eq!(sensitive[0].line, 2);

        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };
        let matches = SearchQuery::new("hello", ".", options).run(&fs).unwrap();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[1].line, 2);
        assert_eq!(matches[2].line, 3);
        assert!(matches.iter().all(|m| m.column == 0));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn respects_max_results_after_stable_sorting() {
        let root = temp_root("limit");
        let fs = LocalFileSystem::rooted(&root);
        for i in 0..10 {
            fs.write(Path::new(&format!("{i:02}.txt")), "needle in file\n")
                .unwrap();
        }
        let options = SearchOptions {
            max_results: Some(3),
            ..Default::default()
        };
        let matches = SearchQuery::new("needle", ".", options).run(&fs).unwrap();
        assert_eq!(matches.len(), 3);
        let paths: Vec<&Path> = matches.iter().map(|m| m.path.as_path()).collect();
        assert_eq!(
            paths,
            [
                Path::new("00.txt"),
                Path::new("01.txt"),
                Path::new("02.txt")
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn results_are_stably_sorted_by_path_then_line_then_column() {
        let root = temp_root("sort");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("b.txt"), "needle\nplain\nneedle\n")
            .unwrap();
        fs.write(Path::new("a.txt"), "needle needle\n").unwrap();

        let matches = SearchQuery::new("needle", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        let keys: Vec<(PathBuf, usize, usize)> = matches
            .iter()
            .map(|m| (m.path.clone(), m.line, m.column))
            .collect();
        assert_eq!(
            keys,
            [
                (PathBuf::from("a.txt"), 1, 0),
                (PathBuf::from("a.txt"), 1, 7),
                (PathBuf::from("b.txt"), 1, 0),
                (PathBuf::from("b.txt"), 3, 0),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_binary_and_nul_containing_files() {
        let root = temp_root("binary");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("ok.txt"), "needle here\n").unwrap();
        // Invalid UTF-8: cannot be decoded as text.
        fs::write(
            root.join("binary.dat"),
            [0xFF, 0xFE, b'n', b'e', b'e', b'd', b'l', b'e'],
        )
        .unwrap();
        // Valid UTF-8 but NUL-containing: treated as binary.
        fs::write(root.join("nul.txt"), "needle\0more").unwrap();

        let matches = SearchQuery::new("needle", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, Path::new("ok.txt"));
        let _ = fs::remove_dir_all(root);
    }

    /// A rooted filesystem that refuses access to chosen paths, standing in
    /// for genuinely unreadable files and directories on any platform.
    struct FaultyFs {
        inner: LocalFileSystem,
        denied: Vec<PathBuf>,
    }

    impl FileSystem for FaultyFs {
        fn read(&self, path: &Path) -> io::Result<String> {
            if self.denied.iter().any(|denied| denied == path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "read denied",
                ));
            }
            self.inner.read(path)
        }

        fn write(&self, path: &Path, text: &str) -> io::Result<()> {
            self.inner.write(path, text)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<FileEntry>> {
            if self.denied.iter().any(|denied| denied == path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "read_dir denied",
                ));
            }
            self.inner.read_dir(path)
        }
    }

    #[test]
    fn skips_unreadable_files_and_directories() {
        let root = temp_root("unreadable");
        let inner = LocalFileSystem::rooted(&root);
        inner.write(Path::new("ok.txt"), "needle ok\n").unwrap();
        inner
            .write(Path::new("secret.txt"), "needle secret\n")
            .unwrap();
        inner
            .write(Path::new("locked/hidden.txt"), "needle hidden\n")
            .unwrap();
        inner.write(Path::new("open.txt"), "needle open\n").unwrap();
        let fs = FaultyFs {
            inner,
            denied: vec![PathBuf::from("secret.txt"), PathBuf::from("locked")],
        };

        let matches = SearchQuery::new("needle", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        let paths: Vec<&Path> = matches.iter().map(|m| m.path.as_path()).collect();
        assert_eq!(paths, [Path::new("ok.txt"), Path::new("open.txt")]);
        let _ = fs::remove_dir_all(root);
    }

    fn create_dir_symlink(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
    }

    #[test]
    fn symlink_escapes_outside_the_root_are_skipped() {
        let outside = temp_root("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("leak.txt"), "needle from outside\n").unwrap();
        let root = temp_root("escape");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("inside.txt"), "needle inside\n").unwrap();

        // Creating symlinks needs privileges on Windows; when unavailable the
        // test degrades to a plain in-root search.
        if create_dir_symlink(&outside, &root.join("link")).is_err() {
            let _ = fs::remove_dir_all(outside);
            let _ = fs::remove_dir_all(root);
            return;
        }
        #[cfg(unix)]
        {
            // A symlinked file pointing outside the root is skipped too.
            fs::write(outside.join("leak2.txt"), "needle from outside file\n").unwrap();
            std::os::unix::fs::symlink(&outside.join("leak2.txt"), root.join("leak-link.txt"))
                .unwrap();
        }

        let fs = LocalFileSystem::rooted(&root);
        let matches = SearchQuery::new("needle", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, Path::new("inside.txt"));
        assert_eq!(matches[0].line_text, "needle inside");

        let _ = fs::remove_dir_all(outside);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlink_cycles_are_bounded_by_max_depth() {
        #[cfg(unix)]
        {
            let root = temp_root("cycle");
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("sub/file.txt"), "needle\n").unwrap();
            if std::os::unix::fs::symlink(&root.join("sub"), root.join("sub/link")).is_err() {
                let _ = fs::remove_dir_all(root);
                return;
            }
            let fs = LocalFileSystem::rooted(&root);
            let options = SearchOptions {
                max_depth: Some(2),
                ..Default::default()
            };
            let matches = SearchQuery::new("needle", ".", options).run(&fs).unwrap();
            let paths: Vec<&Path> = matches.iter().map(|m| m.path.as_path()).collect();
            assert_eq!(
                paths,
                [Path::new("sub/file.txt"), Path::new("sub/link/file.txt"),]
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn empty_needle_matches_nothing() {
        let root = temp_root("empty");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("a.txt"), "needle\n").unwrap();
        let matches = SearchQuery::new("", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert!(matches.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_or_file_root_propagates_the_error() {
        let root = temp_root("root-errors");
        let fs = LocalFileSystem::rooted(&root);
        let missing = SearchQuery::new("x", "missing", SearchOptions::default()).run(&fs);
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
        fs.write(Path::new("plain.txt"), "x\n").unwrap();
        let not_dir = SearchQuery::new("x", "plain.txt", SearchOptions::default()).run(&fs);
        assert_eq!(not_dir.unwrap_err().kind(), io::ErrorKind::NotADirectory);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multibyte_columns_are_byte_offsets_in_the_original_line() {
        let root = temp_root("multibyte");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("zh.txt"), "你好世界\n").unwrap();
        let matches = SearchQuery::new("世界", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].column, 6); // "你好" is 6 bytes in UTF-8
        assert_eq!(matches[0].line_text, "你好世界");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn default_options_are_case_sensitive_and_bounded() {
        let options = SearchOptions::default();
        assert!(options.case_sensitive);
        assert_eq!(options.max_results, None);
        assert_eq!(options.max_depth, Some(64));
    }
}
