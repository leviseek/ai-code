//! Minimal, dependency-free Git status capability.
//!
//! [`status`] runs `git -C <root> status --porcelain=v1` through
//! [`std::process::Command`] and parses the porcelain output into a typed,
//! stably sorted [`GitStatusSnapshot`]. The module uses only `std` and never
//! invokes a shell: `root` is passed to `git -C` as a single argument, so the
//! root path is never interpreted as shell syntax.
//!
//! # Root safety
//!
//! - `root` must exist ([`io::ErrorKind::NotFound`]) and be a directory
//!   ([`io::ErrorKind::NotADirectory`]).
//! - Porcelain v1 reports paths relative to the repository top level. When
//!   `root` is the repository top level — the normal case for a workspace
//!   root — every reported path is relative to `root` exactly, and callers
//!   can join it with `root.join(&entry.path)`. For a `root` nested inside a
//!   larger repository, git may also report sibling paths that live outside
//!   `root`; the snapshot still never names anything outside the repository.
//! - As defense in depth, any parsed path that is absolute (under either
//!   platform's rules) or contains a parent (`..`) component is rejected with
//!   [`io::ErrorKind::InvalidData`], so a snapshot can never escape the
//!   repository root.
//! - A missing `git` executable surfaces as [`io::ErrorKind::NotFound`]; a
//!   non-zero exit (for example `root` is not inside a Git repository)
//!   surfaces as [`io::ErrorKind::Other`] with git's stderr in the message.
//!
//! Renamed or copied entries (`R`/`C`) keep both paths:
//! [`GitStatusEntry::path`] is the current path and
//! [`GitStatusEntry::original`] is where it came from (porcelain v1 emits
//! `ORIG -> PATH`). Paths git had to quote are decoded from C-style octal
//! escapes (for example `"with space \344\270\255\346\226\207.txt"` decodes
//! to `with space 中文.txt`). The literal sequence ` -> ` inside an unquoted
//! path is a known porcelain v1 ambiguity and is parsed as a rename
//! separator.

use std::io;
use std::path::{Component, Path};
use std::process::Command;

/// One half (`X` or `Y`) of a porcelain v1 status code.
///
/// Porcelain v1 encodes each path as two letters `XY`: `X` is the index
/// (staged) status and `Y` is the worktree status. Codes git may emit in
/// future are preserved as [`GitStatusCode::Unknown`] instead of failing the
/// parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStatusCode {
    /// No change (` `).
    Unmodified,
    /// Modified (`M`).
    Modified,
    /// Type changed (`T`), for example a file replaced by a directory.
    TypeChanged,
    /// Added (`A`).
    Added,
    /// Deleted (`D`).
    Deleted,
    /// Renamed (`R`).
    Renamed,
    /// Copied (`C`).
    Copied,
    /// Unmerged (`U`), for example a conflict side of a merge.
    Unmerged,
    /// Untracked (`?`).
    Untracked,
    /// Ignored (`!`); only emitted with `--ignored`, which `status` does not
    /// pass, but kept for completeness.
    Ignored,
    /// A code this version of the parser does not know.
    Unknown(char),
}

impl GitStatusCode {
    /// Whether this code represents a change to the file's content, state or
    /// name (as opposed to unmodified, untracked or ignored).
    pub fn is_change(self) -> bool {
        matches!(
            self,
            Self::Modified
                | Self::TypeChanged
                | Self::Added
                | Self::Deleted
                | Self::Renamed
                | Self::Copied
                | Self::Unmerged
        )
    }

    fn from_byte(byte: u8) -> Self {
        match byte {
            b' ' => Self::Unmodified,
            b'M' => Self::Modified,
            b'T' => Self::TypeChanged,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'U' => Self::Unmerged,
            b'?' => Self::Untracked,
            b'!' => Self::Ignored,
            other => Self::Unknown(other as char),
        }
    }
}

/// The two-character `XY` status of one path: the index (staged) side and the
/// worktree side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitFileState {
    /// The index (staged) status — the `X` column.
    pub index: GitStatusCode,
    /// The worktree status — the `Y` column.
    pub worktree: GitStatusCode,
}

impl GitFileState {
    /// Creates a state from its two porcelain columns.
    pub fn new(index: GitStatusCode, worktree: GitStatusCode) -> Self {
        Self { index, worktree }
    }

    /// Whether the index side reports a change (for example `M `, `A `, `R `).
    pub fn is_staged(&self) -> bool {
        self.index.is_change()
    }

    /// Whether the worktree side reports a change (for example ` M`, ` D`).
    pub fn is_unstaged(&self) -> bool {
        self.worktree.is_change()
    }

    /// Whether this is an untracked entry (`??`).
    pub fn is_untracked(&self) -> bool {
        self.index == GitStatusCode::Untracked && self.worktree == GitStatusCode::Untracked
    }
}

/// One parsed porcelain v1 status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStatusEntry {
    /// The two-column status of this path.
    pub state: GitFileState,
    /// The path, relative to the repository top level (see the module docs).
    ///
    /// For renames and copies this is the current (new) path.
    pub path: String,
    /// For renames and copies, the path the entry came from; `None` for all
    /// other statuses.
    pub original: Option<String>,
}

/// The parsed result of [`status`]: every changed, untracked or renamed path
/// of the repository, stably sorted by path.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitStatusSnapshot {
    /// The entries, stably sorted by [`GitStatusEntry::path`] in byte-wise
    /// order. An empty vec means the working tree is clean.
    pub entries: Vec<GitStatusEntry>,
}

impl GitStatusSnapshot {
    /// Whether the repository has no pending changes, i.e. no entries.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of status entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no status entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Runs `git status --porcelain=v1` in `root` and returns the parsed,
/// stably sorted snapshot.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository); git's stderr is included in the message.
/// - [`io::ErrorKind::InvalidData`] if git's output cannot be parsed as
///   porcelain v1, is not valid UTF-8, or reports a path that escapes the
///   repository root.
pub fn status(root: &Path) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    run_status("git", root)
}

/// Runs `program -C root status --porcelain=v1`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_status(program: &str, root: &Path) -> io::Result<GitStatusSnapshot> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1"])
        .output()?;
    if !output.status.success() {
        let code = match output.status.code() {
            Some(code) => format!("exit code {code}"),
            None => "terminated by signal".to_string(),
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git status failed ({code}): {}",
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git status output is not valid UTF-8",
        )
    })?;
    parse_porcelain(&stdout)
}

/// Parses porcelain v1 stdout into a snapshot, stably sorted by path.
fn parse_porcelain(output: &str) -> io::Result<GitStatusSnapshot> {
    let mut entries = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        entries.push(parse_line(line)?);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(GitStatusSnapshot { entries })
}

/// Parses one `XY path` (or `XY orig -> path`) porcelain v1 line.
fn parse_line(line: &str) -> io::Result<GitStatusEntry> {
    let bytes = line.as_bytes();
    if bytes.len() < 3 || bytes[2] != b' ' {
        return Err(malformed("status line must start with XY and a space"));
    }
    let state = GitFileState::new(
        GitStatusCode::from_byte(bytes[0]),
        GitStatusCode::from_byte(bytes[1]),
    );
    let (first, rest) = parse_field(&line[3..]).ok_or_else(|| malformed("malformed path"))?;
    if rest.is_empty() {
        check_safe_path(&first)?;
        return Ok(GitStatusEntry {
            state,
            path: first,
            original: None,
        });
    }
    // A rename or copy: `orig -> path`.
    let after_separator = rest
        .strip_prefix(" -> ")
        .ok_or_else(|| malformed("expected \" -> \" between the two paths"))?;
    let (second, tail) = parse_field(after_separator).ok_or_else(|| malformed("malformed path"))?;
    if !tail.is_empty() {
        return Err(malformed("trailing data after path"));
    }
    check_safe_path(&first)?;
    check_safe_path(&second)?;
    Ok(GitStatusEntry {
        state,
        path: second,
        original: Some(first),
    })
}

/// Parses one path field: either a plain run of characters or git's
/// C-style quoted form (`"..."`). Returns the decoded path and the unparsed
/// remainder of the line (which, for a rename, starts with the ` -> `
/// separator), or `None` if the field is malformed.
fn parse_field(input: &str) -> Option<(String, &str)> {
    if input.is_empty() {
        return None;
    }
    if input.as_bytes()[0] != b'"' {
        // Plain field: everything up to the first rename separator.
        match input.find(" -> ") {
            Some(position) => Some((input[..position].to_string(), &input[position..])),
            None => Some((input.to_string(), "")),
        }
    } else {
        // Quoted C-style field using git's escape subset: `\"`, `\\`,
        // `\a \b \t \n \v \f \r` and `\NNN` octal byte escapes.
        let mut decoded = Vec::new();
        let mut index = 1;
        loop {
            match input.as_bytes().get(index) {
                None => return None, // unterminated quote
                Some(b'"') => {
                    let rest = &input[index + 1..];
                    return Some((String::from_utf8(decoded).ok()?, rest));
                }
                Some(b'\\') => {
                    index += 1;
                    let escape = *input.as_bytes().get(index)?;
                    match escape {
                        b'"' | b'\\' => decoded.push(escape),
                        b'a' => decoded.push(0x07),
                        b'b' => decoded.push(0x08),
                        b't' => decoded.push(b'\t'),
                        b'n' => decoded.push(b'\n'),
                        b'v' => decoded.push(0x0B),
                        b'f' => decoded.push(0x0C),
                        b'r' => decoded.push(b'\r'),
                        b'0'..=b'7' => {
                            let mut value = u32::from(escape - b'0');
                            for _ in 0..2 {
                                match input.as_bytes().get(index + 1) {
                                    Some(digit @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(*digit - b'0');
                                        index += 1;
                                    }
                                    _ => break,
                                }
                            }
                            decoded.push(value as u8);
                        }
                        _ => return None, // unsupported escape sequence
                    }
                    index += 1;
                }
                Some(_) => {
                    let ch = input[index..].chars().next().expect("input is valid UTF-8");
                    let mut buffer = [0u8; 4];
                    decoded.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                    index += ch.len_utf8();
                }
            }
        }
    }
}

/// Rejects paths that could name something outside the repository.
///
/// Porcelain v1 always reports relative paths, so this is defense in depth:
/// a line that slips through as absolute or with a parent component would
/// let a corrupt/malicious git output escape the root. The check is
/// deliberately conservative — it also rejects paths that only *look*
/// absolute under the other platform's rules (`/`- or `\`-leading, drive
/// letters, UNC prefixes).
fn check_safe_path(path: &str) -> io::Result<()> {
    if path.is_empty() {
        return Err(malformed("empty path"));
    }
    let bytes = path.as_bytes();
    let looks_absolute = bytes[0] == b'/'
        || bytes[0] == b'\\'
        || bytes.starts_with(b"//")
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic());
    let escapes = Path::new(path).is_absolute()
        || looks_absolute
        || Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if escapes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("path escapes repository root: {path:?}"),
        ));
    }
    Ok(())
}

fn malformed(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed git status: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // ---- parsing ----

    #[test]
    fn parses_unstaged_and_untracked_lines() {
        let snapshot = parse_porcelain(" M a.txt\n?? b.txt\n").unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        let unstaged = &snapshot.entries[0];
        assert_eq!(
            unstaged.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        assert_eq!(unstaged.path, "a.txt");
        assert!(unstaged.original.is_none());
        let untracked = &snapshot.entries[1];
        assert!(untracked.state.is_untracked());
        assert_eq!(untracked.path, "b.txt");
    }

    #[test]
    fn parses_every_porcelain_status_code() {
        let output = " M m1.txt\nM  m2.txt\nT  t.txt\nA  a.txt\nD  d.txt\n\
                      R  r1.txt -> r2.txt\nC  c1.txt -> c2.txt\nUU u.txt\n\
                      ?? uq.txt\n!! ig.txt\n";
        let snapshot = parse_porcelain(output).unwrap();
        let expected = [
            (
                "a.txt",
                GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified),
            ),
            (
                "c2.txt",
                GitFileState::new(GitStatusCode::Copied, GitStatusCode::Unmodified),
            ),
            (
                "d.txt",
                GitFileState::new(GitStatusCode::Deleted, GitStatusCode::Unmodified),
            ),
            (
                "ig.txt",
                GitFileState::new(GitStatusCode::Ignored, GitStatusCode::Ignored),
            ),
            (
                "m1.txt",
                GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified),
            ),
            (
                "m2.txt",
                GitFileState::new(GitStatusCode::Modified, GitStatusCode::Unmodified),
            ),
            (
                "r2.txt",
                GitFileState::new(GitStatusCode::Renamed, GitStatusCode::Unmodified),
            ),
            (
                "t.txt",
                GitFileState::new(GitStatusCode::TypeChanged, GitStatusCode::Unmodified),
            ),
            (
                "u.txt",
                GitFileState::new(GitStatusCode::Unmerged, GitStatusCode::Unmerged),
            ),
            (
                "uq.txt",
                GitFileState::new(GitStatusCode::Untracked, GitStatusCode::Untracked),
            ),
        ];
        let states: Vec<(&str, GitFileState)> = snapshot
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.state))
            .collect();
        assert_eq!(states, expected);
    }

    #[test]
    fn parses_rename_with_original_and_current_path() {
        let entry = parse_line("R  old.txt -> new.txt").unwrap();
        assert_eq!(entry.path, "new.txt");
        assert_eq!(entry.original.as_deref(), Some("old.txt"));
        assert!(entry.state.is_staged());
    }

    #[test]
    fn parses_quoted_paths_with_c_style_escapes() {
        let entry = parse_line("?? \"with space \\344\\270\\255\\346\\226\\207.txt\"").unwrap();
        assert_eq!(entry.path, "with space 中文.txt");
        let entry = parse_line("M  \"a\\\"b\\\\c.txt\"").unwrap();
        assert_eq!(entry.path, "a\"b\\c.txt");
        let entry = parse_line("M  \"tab\\tnew\\nline.txt\"").unwrap();
        assert_eq!(entry.path, "tab\tnew\nline.txt");
        let entry = parse_line("M  \"short\\12oct.txt\"").unwrap();
        assert_eq!(entry.path, "short\noct.txt");
    }

    #[test]
    fn parses_unquoted_paths_containing_spaces() {
        // Porcelain v1 only quotes special characters; plain spaces are
        // legal in unquoted paths, so the whole remainder is the path.
        let entry = parse_line("M  a file with spaces.txt").unwrap();
        assert_eq!(entry.path, "a file with spaces.txt");
        assert!(entry.original.is_none());
    }

    #[test]
    fn parses_quoted_rename_on_both_sides() {
        let entry = parse_line("R  \"old name.txt\" -> \"new name.txt\"").unwrap();
        assert_eq!(entry.original.as_deref(), Some("old name.txt"));
        assert_eq!(entry.path, "new name.txt");
        let entry = parse_line("R  plain.txt -> \"new name.txt\"").unwrap();
        assert_eq!(entry.original.as_deref(), Some("plain.txt"));
        assert_eq!(entry.path, "new name.txt");
    }

    #[test]
    fn rejects_malformed_lines() {
        for line in [
            "",
            "M",
            "M a.txt",
            "M  ",
            "??",
            "?? \"unterminated",
            "M  \"bad\\q.txt\"",
            "M  \"a.txt\"junk",
            "R  a.txt -> ",
            "R  a.txt -> b.txt -> c.txt",
        ] {
            assert!(
                parse_line(line).is_err(),
                "line {line:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_paths_that_escape_the_repository() {
        for line in [
            "M  /etc/passwd",
            "M  \\\\server\\share\\file.txt",
            "M  C:/windows/file.txt",
            "M  C:\\windows\\file.txt",
            "M  ../outside.txt",
            "M  a/../../outside.txt",
            "R  old.txt -> ../new.txt",
            "M  \"..\\\\quoted-escape.txt\"",
        ] {
            let error = parse_line(line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected as unsafe"
            );
        }
    }

    // ---- ordering ----

    #[test]
    fn snapshot_is_sorted_by_path() {
        let output = "?? z.txt\n M m.txt\n?? a.txt\nR  b.txt -> c.txt\n";
        let snapshot = parse_porcelain(output).unwrap();
        let paths: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, ["a.txt", "c.txt", "m.txt", "z.txt"]);
    }

    #[test]
    fn snapshot_sort_is_stable_for_equal_paths() {
        // Git never emits two lines for the same path, but the snapshot must
        // still preserve input order when sort keys tie.
        let snapshot = parse_porcelain("M  dup.txt\n?? dup.txt\n").unwrap();
        assert_eq!(snapshot.entries[0].state.index, GitStatusCode::Modified);
        assert_eq!(snapshot.entries[1].state.index, GitStatusCode::Untracked);
    }

    // ---- state classification ----

    #[test]
    fn file_state_classifies_staged_unstaged_and_untracked() {
        let staged = GitFileState::new(GitStatusCode::Modified, GitStatusCode::Unmodified);
        assert!(staged.is_staged());
        assert!(!staged.is_unstaged());
        let unstaged = GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified);
        assert!(unstaged.is_unstaged());
        assert!(!unstaged.is_staged());
        let both = GitFileState::new(GitStatusCode::Added, GitStatusCode::Modified);
        assert!(both.is_staged());
        assert!(both.is_unstaged());
        let untracked = GitFileState::new(GitStatusCode::Untracked, GitStatusCode::Untracked);
        assert!(untracked.is_untracked());
        assert!(!untracked.is_staged());
        assert!(!untracked.is_unstaged());
        let clean = GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Unmodified);
        assert!(!clean.is_staged());
        assert!(!clean.is_unstaged());
        assert!(!clean.is_untracked());
        assert!(parse_porcelain("").unwrap().is_clean());
    }

    // ---- status() root validation (no git binary needed) ----

    #[test]
    fn status_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-git-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            status(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn status_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-git-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            status(&file).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_surfaces_as_not_found() {
        let root =
            std::env::temp_dir().join(format!("nextide-git-nobinary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let error = run_status("nextide-no-such-git-binary", &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- status() integration (requires a git binary) ----

    fn git_available() -> bool {
        Command::new("git")
            .args(["--version"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Creates an empty temp directory for a test.
    fn temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nextide-git-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Initializes a fresh Git repository in a temp directory and commits a
    /// baseline file set from `files` (`(path, contents)`).
    fn init_repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = temp_dir(tag);
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.name", "nextide-tests"]);
        run_git(
            &root,
            &["config", "user.email", "nextide-tests@example.com"],
        );
        for (path, contents) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().expect("relative test path")).unwrap();
            fs::write(full, contents).unwrap();
        }
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
        );
        root
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git must be runnable in this environment");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn find<'a>(snapshot: &'a GitStatusSnapshot, path: &str) -> &'a GitStatusEntry {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry for {path:?} in {snapshot:?}"))
    }

    #[test]
    fn status_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("nonrepo");
        // git walks up to the nearest enclosing repository, so this test's
        // premise (the directory is not inside any repo) does not hold when
        // an ancestor — for example a user's home directory — happens to be
        // a git repository. Skip in that environment.
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = status(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Whether git resolves `dir` to an existing enclosing repository (git
    /// walks up parent directories until it finds one).
    fn inside_existing_repo(dir: &Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn status_reports_clean_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("clean", &[("a.txt", "a")]);
        let snapshot = status(&root).unwrap();
        assert!(snapshot.is_clean());
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_parses_real_repository_state() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo(
            "state",
            &[("a.txt", "a"), ("b.txt", "b"), ("sub/c.txt", "c")],
        );
        fs::write(root.join("a.txt"), "CHANGED").unwrap();
        run_git(&root, &["mv", "b.txt", "renamed.txt"]);
        fs::write(root.join("new.txt"), "untracked").unwrap();
        fs::write(root.join("with space 中文.txt"), "x").unwrap();

        let snapshot = status(&root).unwrap();

        let modified = find(&snapshot, "a.txt");
        assert_eq!(
            modified.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        let renamed = find(&snapshot, "renamed.txt");
        assert_eq!(
            renamed.state,
            GitFileState::new(GitStatusCode::Renamed, GitStatusCode::Unmodified)
        );
        assert_eq!(renamed.original.as_deref(), Some("b.txt"));
        let untracked = find(&snapshot, "new.txt");
        assert!(untracked.state.is_untracked());
        let quoted = find(&snapshot, "with space 中文.txt");
        assert!(quoted.state.is_untracked());

        // Entries are reported sorted by path.
        let paths: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);

        // Every path stays inside the root and is relative.
        for entry in &snapshot.entries {
            assert!(Path::new(&entry.path).is_relative());
            assert!(!entry.path.starts_with(".."));
            assert!(root.join(&entry.path).starts_with(&root));
            if let Some(original) = &entry.original {
                assert!(Path::new(original).is_relative());
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_from_nested_root_reports_top_level_relative_paths() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("nested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        fs::write(root.join("sub/b.txt"), "CHANGED").unwrap();
        fs::write(root.join("sub/untracked.txt"), "u").unwrap();

        // Porcelain v1 reports paths relative to the repository top level,
        // even when the queried root is a subdirectory.
        let snapshot = status(&root.join("sub")).unwrap();
        let changed = find(&snapshot, "sub/b.txt");
        assert!(changed.state.is_unstaged());
        let untracked = find(&snapshot, "sub/untracked.txt");
        assert!(untracked.state.is_untracked());

        // Path safety still holds: no entry escapes the repository root.
        for entry in &snapshot.entries {
            assert!(Path::new(&entry.path).is_relative());
            assert!(root.join(&entry.path).starts_with(&root));
        }
        let _ = fs::remove_dir_all(root);
    }
}
