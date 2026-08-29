//! Workspace text search over a [`FileSystem`].
//!
//! The module is dependency-free and uses only `std`. A search walks a
//! directory tree recursively through the [`FileSystem`] abstraction, reads
//! every regular file it can, and reports the lines matching the query
//! pattern.
//!
//! Matching is pluggable through [`SearchMatcher`]. [`SearchMatcher::Literal`]
//! (the default) keeps the original behavior: a line matches when it contains
//! the pattern as a plain substring. [`SearchMatcher::Glob`] interprets the
//! pattern as a simple glob (wildcard) pattern where `*` matches any run of
//! characters and `?` matches exactly one character. Glob matching is
//! deliberately not regular-expression matching: there are no character
//! classes, alternation, anchors, escapes, or quantifiers beyond `*`/`?`, and
//! every other character — including regex metacharacters such as `.`, `[`,
//! `(`, or `+` — matches itself literally. The implementation is a compact
//! dynamic program over characters, so matching a line is O(pattern × line)
//! in time and O(line) in memory even for patterns with many wildcards.
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

/// How a [`SearchQuery`] interprets its `needle` pattern.
///
/// The matcher is the reusable matching strategy behind a query: it can be
/// used standalone through [`SearchMatcher::find_columns`] and
/// [`SearchMatcher::is_match`] on any text, independent of the filesystem
/// walk in [`SearchQuery`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMatcher {
    /// The needle is a plain substring: every occurrence inside a line
    /// matches. This is the default and keeps the historical search behavior.
    #[default]
    Literal,
    /// The needle is a simple glob (wildcard) pattern:
    ///
    /// - `*` matches any run of characters, including the empty run;
    /// - `?` matches exactly one character;
    /// - every other character matches itself.
    ///
    /// Like [`SearchMatcher::Literal`], a glob matches *substrings* of a
    /// line: the pattern must match some contiguous region of the line.
    /// Leading `*`s only widen the search window and never start matches of
    /// their own, so `*error*` is equivalent to "contains `error`" and
    /// reports the column where `error` starts, `gr?y` matches `grey` and
    /// `gray` but not `gravy`, and `fn *(` matches `fn main(` and `fn (x)`
    /// but not `fn main`. A pattern whose anchored remainder is empty (only
    /// `*`s) matches nothing.
    ///
    /// This is a glob matcher, not a regular expression: character classes
    /// (`[a-z]`), alternation (`a|b`), anchors (`^`/`$`), escapes, grouping,
    /// and other regex syntax are not supported, and those metacharacters
    /// match literally.
    Glob,
}

impl SearchMatcher {
    /// Byte offsets (0-based) of every occurrence of `pattern` inside `line`,
    /// honoring `case_sensitive`.
    ///
    /// [`SearchMatcher::Literal`] reports every substring occurrence;
    /// [`SearchMatcher::Glob`] reports the start of every contiguous substring
    /// the glob matches (see the [`SearchMatcher::Glob`] documentation for
    /// the pattern language and column semantics). An empty `pattern` matches
    /// nothing in either mode.
    pub fn find_columns(&self, pattern: &str, line: &str, case_sensitive: bool) -> Vec<usize> {
        if pattern.is_empty() {
            return Vec::new();
        }
        match self {
            SearchMatcher::Literal => literal_columns(line, pattern, case_sensitive),
            SearchMatcher::Glob => glob_columns(line, pattern, case_sensitive),
        }
    }

    /// Whether `pattern` occurs anywhere inside `line`.
    pub fn is_match(&self, pattern: &str, line: &str, case_sensitive: bool) -> bool {
        !self.find_columns(pattern, line, case_sensitive).is_empty()
    }
}

/// A request to find every line matching `needle` under `root`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// The pattern to search for, interpreted according to
    /// [`SearchQuery::matcher`]. An empty pattern matches nothing.
    pub needle: String,
    /// Directory to start the search from, relative to the filesystem root.
    pub root: PathBuf,
    /// Options controlling the execution.
    pub options: SearchOptions,
    /// How [`SearchQuery::needle`] is interpreted. Defaults to
    /// [`SearchMatcher::Literal`], so a bare word still searches for a plain
    /// substring.
    pub matcher: SearchMatcher,
}

impl SearchQuery {
    /// Creates a literal-substring query: `needle` is matched as a plain
    /// substring, with case sensitivity and limits taken from `options`.
    ///
    /// This is the historical constructor; its behavior is unchanged.
    pub fn new(
        needle: impl Into<String>,
        root: impl Into<PathBuf>,
        options: SearchOptions,
    ) -> Self {
        Self {
            needle: needle.into(),
            root: root.into(),
            options,
            matcher: SearchMatcher::Literal,
        }
    }

    /// Creates a glob query: `needle` is interpreted as a simple glob pattern
    /// (see [`SearchMatcher::Glob`] for the pattern language).
    pub fn glob(
        pattern: impl Into<String>,
        root: impl Into<PathBuf>,
        options: SearchOptions,
    ) -> Self {
        Self {
            needle: pattern.into(),
            root: root.into(),
            options,
            matcher: SearchMatcher::Glob,
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

/// Searches `query.root` inside `fs` for every line matching `query.needle`
/// under `query.matcher` and returns the matches stably sorted by
/// `(path, line, column)`, truncated to [`SearchOptions::max_results`].
///
/// [`SearchMatcher::Literal`] (the default) finds lines containing the needle
/// as a substring; [`SearchMatcher::Glob`] finds lines containing a substring
/// matched by the simple glob pattern (see [`SearchMatcher::Glob`] for the
/// pattern language). An empty needle matches nothing.
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
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for column in query
            .matcher
            .find_columns(&query.needle, line, query.options.case_sensitive)
        {
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

/// Byte offsets of every literal occurrence of `needle` inside `line`.
fn literal_columns(line: &str, needle: &str, case_sensitive: bool) -> Vec<usize> {
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

/// Byte offsets of the start of every substring of `line` matched by the glob
/// `pattern` (`*` matches any run of characters, `?` matches exactly one).
///
/// A glob matches at `start` when the pattern can be consumed against the
/// characters from `start` onwards, ending anywhere before or at the end of
/// the line, so matching is substring-based: `*X*` is equivalent to "contains
/// `X`", and `gr?y` matches both `grey` and `gray`. Leading `*`s only widen
/// the search window and never start matches of their own — the reported
/// columns are the starts of the anchored remainder of the pattern, so
/// `*error*` reports the column of `error` itself. A pattern whose anchored
/// remainder is empty (only `*`s) matches nothing.
///
/// The implementation runs a dynamic program of the pattern's characters
/// against the reversed line (a glob is symmetric under reversal), which is
/// O(pattern × line) in time and O(line) in memory and stays safe even for
/// patterns with many wildcards. Columns are byte offsets, so they are always
/// valid UTF-8 boundaries.
fn glob_columns(line: &str, pattern: &str, case_sensitive: bool) -> Vec<usize> {
    let pattern: Vec<char> = pattern.chars().skip_while(|&c| c == '*').collect();
    let text: Vec<char> = line.chars().collect();
    if pattern.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let byte_offsets: Vec<usize> = line.char_indices().map(|(index, _)| index).collect();
    // `ends[j]` is true when the reversed pattern prefix processed so far
    // matches `reversed_line[k..j]` for some `k`; the empty pattern matches
    // the empty substring ending at any `j`, which is what lets a match start
    // anywhere. A reversed match ending at `j` corresponds to an original
    // match starting at `text.len() - j`.
    let mut ends = vec![true; text.len() + 1];
    for &pattern_char in pattern.iter().rev() {
        let mut next = vec![false; text.len() + 1];
        if pattern_char == '*' {
            next[0] = ends[0];
            for j in 1..=text.len() {
                // `*` matches the empty run (ends[j]) or one more character
                // (next[j - 1]).
                next[j] = ends[j] || next[j - 1];
            }
        } else if pattern_char == '?' {
            // `?` matches exactly one character, whatever it is: each
            // previous end position simply advances by one character.
            next[1..].copy_from_slice(&ends[..text.len()]);
        } else {
            for j in 1..=text.len() {
                next[j] =
                    ends[j - 1] && chars_equal(pattern_char, text[text.len() - j], case_sensitive);
            }
        }
        ends = next;
    }
    let mut columns = Vec::new();
    for (j, &matched) in ends.iter().enumerate().rev() {
        if matched {
            let start = text.len() - j;
            columns.push(byte_offsets.get(start).copied().unwrap_or(line.len()));
        }
    }
    columns
}

/// Whether two characters are equal, optionally case-folding both sides.
fn chars_equal(a: char, b: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.to_lowercase().eq(b.to_lowercase())
    }
}

/// Whether `haystack` starts with `needle`, comparing characters with case
/// folding. Offsets stay valid because matching happens on the original
/// string, never on a lowercased copy.
fn chars_match_case_insensitive(haystack: &str, needle: &[char]) -> bool {
    let mut hay = haystack.chars();
    for &expected in needle {
        match hay.next() {
            Some(actual) if chars_equal(actual, expected, false) => {}
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
    fn new_query_defaults_to_literal_and_glob_constructor_sets_matcher() {
        let literal = SearchQuery::new("x", ".", SearchOptions::default());
        assert_eq!(literal.matcher, SearchMatcher::Literal);
        let glob = SearchQuery::glob("x*", ".", SearchOptions::default());
        assert_eq!(glob.matcher, SearchMatcher::Glob);
    }

    #[test]
    fn glob_query_finds_lines_matching_the_pattern() {
        let root = temp_root("glob");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(
            Path::new("a.txt"),
            "fn main() {\nlet x = 1;\n// TODO: fix\nfn helper()\n",
        )
        .unwrap();
        let matches = SearchQuery::glob("fn *(", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].column, 0);
        assert_eq!(matches[0].line_text, "fn main() {");
        assert_eq!(matches[1].line, 4);
        assert_eq!(matches[1].column, 0);
        assert_eq!(matches[1].line_text, "fn helper()");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_query_respects_case_sensitivity() {
        let root = temp_root("glob-case");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("a.txt"), "Hello world\n").unwrap();
        let sensitive = SearchQuery::glob("h?llo*", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert!(sensitive.is_empty());
        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };
        let matches = SearchQuery::glob("h?llo*", ".", options).run(&fs).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].column, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_query_empty_pattern_matches_nothing() {
        let root = temp_root("glob-empty");
        let fs = LocalFileSystem::rooted(&root);
        fs.write(Path::new("a.txt"), "anything\n").unwrap();
        let matches = SearchQuery::glob("", ".", SearchOptions::default())
            .run(&fs)
            .unwrap();
        assert!(matches.is_empty());
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

#[cfg(test)]
mod matcher_tests {
    use super::*;

    #[test]
    fn literal_finds_every_occurrence() {
        let matcher = SearchMatcher::Literal;
        assert_eq!(matcher.find_columns("ab", "abab", true), vec![0, 2]);
        assert_eq!(matcher.find_columns("ab", "ABab", false), vec![0, 2]);
        assert_eq!(matcher.find_columns("", "ab", true), Vec::<usize>::new());
        assert!(matcher.is_match("ab", "xaby", true));
        assert!(!matcher.is_match("ab", "xay", true));
    }

    #[test]
    fn glob_star_matches_any_run_including_the_empty_run() {
        let matcher = SearchMatcher::Glob;
        assert!(matcher.is_match("a*b", "ab", true));
        assert!(matcher.is_match("a*b", "a123b", true));
        assert!(!matcher.is_match("a*b", "ac", true));
        // `a*` matches every `a` followed by anything.
        assert_eq!(matcher.find_columns("a*", "banana", true), vec![1, 3, 5]);
        // A pattern of only `*`s has no anchored part and matches nothing.
        assert!(matcher.find_columns("*", "ab", true).is_empty());
        assert!(!matcher.is_match("*", "ab", true));
    }

    #[test]
    fn glob_question_matches_exactly_one_character() {
        let matcher = SearchMatcher::Glob;
        assert!(matcher.is_match("gr?y", "grey", true));
        assert!(matcher.is_match("gr?y", "gray", true));
        assert!(!matcher.is_match("gr?y", "gravy", true));
        assert!(!matcher.is_match("gr?y", "gr", true));
        assert!(!matcher.is_match("gr?y", "gy", true));
    }

    #[test]
    fn glob_matches_substrings_and_keeps_literal_behavior_without_wildcards() {
        let matcher = SearchMatcher::Glob;
        // A glob without wildcards behaves exactly like a literal search.
        assert_eq!(
            matcher.find_columns("needle", "needle in needle", true),
            vec![0, 10]
        );
        assert!(matcher.is_match("gr?y", "a grey sky", true));
        assert_eq!(matcher.find_columns("gr?y", "grey gray", true), vec![0, 5]);
        // Leading `*`s widen the window; columns point at the anchored part.
        assert_eq!(
            matcher.find_columns("*error*", "xx error yy", true),
            vec![3]
        );
    }

    #[test]
    fn glob_respects_case_sensitivity() {
        let matcher = SearchMatcher::Glob;
        assert!(!matcher.is_match("h?llo", "HELLO", true));
        assert!(matcher.is_match("h?llo", "HELLO", false));
        assert_eq!(matcher.find_columns("h*", "Hello", false), vec![0]);
        assert!(matcher.find_columns("h*", "Hello", true).is_empty());
    }

    #[test]
    fn glob_question_matches_one_character_and_columns_are_byte_offsets() {
        let matcher = SearchMatcher::Glob;
        // `?` consumes exactly one character, so it covers a whole
        // multibyte char, and columns stay byte offsets.
        assert!(matcher.is_match("?界", "世界", true));
        assert_eq!(matcher.find_columns("?界", "世界", true), vec![0]);
        assert_eq!(matcher.find_columns("*好*", "你好世界", true), vec![3]);
    }

    #[test]
    fn glob_treats_regex_metacharacters_as_literals() {
        let matcher = SearchMatcher::Glob;
        // Not a regex: character classes and alternation match literally.
        assert!(matcher.is_match("[ab]", "x[ab]y", true));
        assert!(!matcher.is_match("[ab]", "xay", true));
        assert!(matcher.is_match("a+b", "a+b", true));
        assert!(!matcher.is_match("a+b", "ab", true));
    }
}
