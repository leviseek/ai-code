//! Minimal, dependency-free Git status and diff capability.
//!
//! [`status`] runs `git -C <root> status --porcelain=v1` through
//! [`std::process::Command`] and parses the porcelain output into a typed,
//! stably sorted [`GitStatusSnapshot`]. [`diff`] runs
//! `git -C <root> diff --no-ext-diff --unified=3` and parses the unified
//! output into a typed, stably sorted [`GitDiffSnapshot`]. The module uses
//! only `std` and never invokes a shell: `root` (and the optional diff
//! pathspec) is passed to `git -C` as a single argument, so it is never
//! interpreted as shell syntax.
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
//!
//! # Diff
//!
//! [`diff(root, path)`](diff) compares the worktree against the index with
//! `git -C <root> diff --no-ext-diff --unified=3` (no shell, no external diff
//! driver, 3 lines of context). The optional `path` is passed to git as a
//! pathspec after `--`; it must be rooted at `root` — relative and free of
//! parent, root and drive components — or the call fails with
//! [`io::ErrorKind::InvalidInput`] before git is spawned. The parsed
//! [`GitDiffSnapshot`] holds one [`GitDiffFile`] per changed path: the `b/`
//! side (the current path, reported relative to the repository top level,
//! decoded from git's C-style quoting when needed), a `binary` flag for
//! `Binary files ... differ` entries, and the file's [`GitDiffHunk`]s. Every
//! hunk's [`GitDiffLine`] records its kind (context / addition / removal),
//! the 1-based line numbers on the old and new side, and the line text
//! without the leading marker. Files are stably sorted by path; a clean
//! worktree yields an empty snapshot.
//!
//! Error behaviour matches [`status`]: a missing `root` or `git` is
//! [`io::ErrorKind::NotFound`], a non-directory `root` is
//! [`io::ErrorKind::NotADirectory`], a non-zero git exit is
//! [`io::ErrorKind::Other`] with git's stderr in the message, and output that
//! cannot be parsed or reports an escaping path is
//! [`io::ErrorKind::InvalidData`]. Unlike porcelain, plain `git diff`
//! (without `--exit-code`) exits 0 whether or not differences exist, so a
//! non-zero exit is treated as a genuine failure. Also unlike porcelain,
//! diff *content* is not UTF-8 validated: bytes that are not valid UTF-8 in
//! changed lines are replaced with U+FFFD (git only quotes paths, never line
//! content), while every parsed path still passes the same safety check as
//! `status`.
//!
//! # Stage, unstage and commit
//!
//! [`stage(root, paths)`](stage) stages paths into the index with
//! `git -C <root> add -- <paths>` and returns the fresh
//! [`GitStatusSnapshot`] from [`status`]. No shell is involved and every
//! path is passed to git as its own argument after the `--` separator, so a
//! path such as `--help` or `-f` is treated literally. Every path must be
//! rooted at `root` (relative, non-empty, free of parent, root and drive
//! components) and must not start with `:` — git would interpret a `:`
//! prefix as pathspec magic (for example `:(top)` or `:(exclude)`) — or the
//! call fails with [`io::ErrorKind::InvalidInput`] before git is spawned;
//! an empty `paths` slice is rejected the same way.
//!
//! [`unstage(root, paths)`](unstage) removes paths from the index with
//! `git -C <root> reset -- <paths>` and returns the fresh
//! [`GitStatusSnapshot`] from [`status`]. The same safety rules as
//! [`stage`] apply: no shell, every path is its own argument after the `--`
//! separator, and each path must be rooted at `root` (relative, non-empty,
//! free of parent, root and drive components) and must not start with `:` —
//! an empty `paths` slice is rejected with [`io::ErrorKind::InvalidInput`]
//! before git is spawned. A `.` pathspec un-stages everything under `root`
//! (git resolves it relative to `root`). Unlike `git add`, `git reset --`
//! with a pathspec does not fail when a pathspec matches nothing — the
//! index is simply left untouched for that path — so a successful call
//! always reflects the resulting state.
//!
//! [`commit(root, message)`](commit) creates a commit from the staged
//! changes with `git -C <root> commit --message=<message>`, where the
//! message travels as a single argument — never through a shell — so it
//! cannot be parsed as an option even when it looks like one (for example
//! `--amend`). An empty or whitespace-only message, or one containing a NUL
//! byte, is rejected with [`io::ErrorKind::InvalidInput`] before git is
//! spawned. On success it returns [`GitCommitResult`] with the full
//! 40-character hash of the new commit (read back with `git rev-parse
//! HEAD`) and its subject as git defines it (`git log -1 --format=%s`: the
//! first paragraph, with line breaks collapsed to spaces).
//!
//! Error behaviour matches [`status`] and [`diff`]: a missing `root` or
//! `git` is [`io::ErrorKind::NotFound`], a non-directory `root` is
//! [`io::ErrorKind::NotADirectory`], and a non-zero git exit (for example a
//! pathspec that matches nothing, an unconfigured identity, or "nothing to
//! commit") is [`io::ErrorKind::Other`] with git's message — taken from
//! stderr, or from stdout when stderr is empty, because `git commit` prints
//! some failures (such as "nothing to commit") on stdout.
//!
//! # Branches and checkout
//!
//! [`branches(root)`](branches) lists the repository's local branches with
//! `git -C <root> for-each-ref --format=%(HEAD)|%(refname:short) refs/heads`
//! and returns a stably sorted [`GitBranchSnapshot`]. Every [`GitBranch`]
//! carries the branch's short name (the part after `refs/heads/`) and a
//! `current` flag for the checked-out branch; a detached HEAD marks no branch
//! as current. The `%(HEAD)|name` lines are machine-oriented — git's own
//! `%(HEAD)` atom emits `*` for the checked-out branch and a space otherwise,
//! and the name is everything after the first `|` — so no human-format parsing
//! is involved, and every parsed name still passes the same escape-rejecting
//! check as status paths.
//!
//! [`checkout(root, branch)`](checkout) switches the worktree with
//! `git -C <root> checkout <branch>` and returns the fresh
//! [`GitStatusSnapshot`] from [`status`]. The branch name is validated before
//! git is spawned: it must be non-empty, free of NUL bytes, not absolute and
//! without `..` components, and must not start with `-` — a `-`-prefixed name
//! would be parsed by git as an option, and `--` cannot be used as a shield
//! here because it would turn the argument into a pathspec instead of a
//! branch. Anything else (an unknown branch, a checkout that would overwrite
//! local changes, an invalid reference) surfaces as
//! [`io::ErrorKind::Other`] with git's message. Root and git-binary error
//! behaviour matches [`status`] and [`stage`]: `NotFound` for a missing root
//! or missing `git`, `NotADirectory` for a non-directory root.

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
        format!("malformed git output: {message}"),
    )
}

// ---------------------------------------------------------------------------
// Git diff
// ---------------------------------------------------------------------------

/// The kind of one line in a unified diff hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitDiffLineKind {
    /// A line present in both the old and the new file.
    Context,
    /// A line that exists only in the new file.
    Added,
    /// A line that exists only in the old file.
    Removed,
}

/// One line of a unified diff hunk, with its 1-based line numbers.
///
/// Line numbers are relative to the repository's file content: a context line
/// exists on both sides, an [`GitDiffLineKind::Added`] line only on the new
/// side, a [`GitDiffLineKind::Removed`] line only on the old side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffLine {
    /// Whether this line is context, an addition or a removal.
    pub kind: GitDiffLineKind,
    /// The 1-based line number in the old (before) file; `None` for added
    /// lines.
    pub old_line: Option<u32>,
    /// The 1-based line number in the new (after) file; `None` for removed
    /// lines.
    pub new_line: Option<u32>,
    /// The line content without the leading diff marker or the trailing
    /// newline. Bytes that are not valid UTF-8 were replaced with U+FFFD.
    pub text: String,
}

/// One `@@ -l,s +l,s @@` hunk of a file diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffHunk {
    /// The raw hunk header line as git printed it, for example
    /// `@@ -1,5 +1,6 @@` (with any section heading preserved).
    pub header: String,
    /// The hunk's lines in output order.
    pub lines: Vec<GitDiffLine>,
}

/// The parsed diff of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffFile {
    /// The file's path relative to the repository top level — the `b/` side
    /// of the diff, i.e. the current path. Git's C-style quoting is decoded.
    pub path: String,
    /// Whether git reported this as a binary diff (`Binary files ... differ`);
    /// binary files carry no hunks.
    pub binary: bool,
    /// The hunks in output order; empty for binary files and for changes that
    /// only touch metadata (for example a mode change).
    pub hunks: Vec<GitDiffHunk>,
}

/// The parsed result of [`diff`]: every changed path of the repository,
/// stably sorted by path.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitDiffSnapshot {
    /// The files, stably sorted by [`GitDiffFile::path`] in byte-wise order.
    /// An empty vec means the worktree matches the index.
    pub files: Vec<GitDiffFile>,
}

impl GitDiffSnapshot {
    /// The number of changed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether there are no changed files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Runs `git -C <root> diff --no-ext-diff --unified=3` and returns the
/// parsed, stably sorted snapshot of the worktree-vs-index diff.
///
/// `path`, when given, restricts the diff to that path (passed to git as a
/// pathspec after `--`). It must be rooted at `root`: relative and free of
/// parent, root and drive components. Git still reports paths relative to the
/// repository top level, so when `root` is the repository top level (the
/// normal workspace case) every reported path is relative to `root` exactly.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `path` is given but not rooted at
///   `root` (absolute, empty, or containing `..`, a root or a drive
///   component).
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository); git's stderr is included in the message.
///   Plain `git diff` exits 0 whether or not differences exist, so a non-zero
///   exit always means a real failure.
/// - [`io::ErrorKind::InvalidData`] if git's output cannot be parsed or
///   reports a path that escapes the repository root.
pub fn diff(root: &Path, path: Option<&Path>) -> io::Result<GitDiffSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    if let Some(path) = path {
        check_rooted_path(path)?;
    }
    run_diff("git", root, path)
}

/// Runs `program -C root diff --no-ext-diff --unified=3 [-- path]`; split out
/// so tests can exercise the spawn-failure path with a program that does not
/// exist.
fn run_diff(program: &str, root: &Path, path: Option<&Path>) -> io::Result<GitDiffSnapshot> {
    let mut command = Command::new(program);
    command
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-ext-diff", "--unified=3"]);
    if let Some(path) = path {
        command.arg("--").arg(path);
    }
    let output = command.output()?;
    if !output.status.success() {
        let code = match output.status.code() {
            Some(code) => format!("exit code {code}"),
            None => "terminated by signal".to_string(),
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git diff failed ({code}): {}",
            stderr.trim()
        )));
    }
    // Diff content is arbitrary bytes; git only quotes paths, never line
    // content, so non-UTF-8 bytes become U+FFFD rather than failing the call.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    parse_diff(&stdout)
}

/// Parses unified diff stdout into a snapshot, stably sorted by path.
fn parse_diff(output: &str) -> io::Result<GitDiffSnapshot> {
    let mut files = Vec::new();
    let mut current: Option<DiffFileBuilder> = None;
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(header) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file.finish()?);
            }
            current = Some(DiffFileBuilder::start(header)?);
        } else if let Some(file) = current.as_mut() {
            file.push_line(line)?;
        } else {
            return Err(malformed(
                "diff output before the first \"diff --git\" line",
            ));
        }
    }
    if let Some(file) = current.take() {
        files.push(file.finish()?);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(GitDiffSnapshot { files })
}

/// Parses one whitespace-delimited field of a `diff --git` header, either a
/// plain path or git's C-style quoted form. Unlike [`parse_field`], plain
/// fields stop at whitespace instead of at the ` -> ` rename separator.
fn parse_header_field(input: &str) -> Option<(String, &str)> {
    if input.is_empty() {
        return None;
    }
    if input.as_bytes()[0] == b'"' {
        parse_field(input)
    } else {
        match input.find(char::is_whitespace) {
            Some(position) => Some((input[..position].to_string(), &input[position..])),
            None => Some((input.to_string(), "")),
        }
    }
}

/// Parses a hunk header `@@ -<old>[,<count>] +<new>[,<count>] @@` into
/// `(old_start, old_count, new_start, new_count)`; a missing count means 1.
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let (old_part, rest) = rest.split_once(' ')?;
    let old_part = old_part.strip_prefix('-')?;
    let (new_part, rest) = rest.split_once(' ')?;
    let new_part = new_part.strip_prefix('+')?;
    rest.strip_prefix("@@")?;
    let (old_start, old_count) = parse_start_count(old_part)?;
    let (new_start, new_count) = parse_start_count(new_part)?;
    Some((old_start, old_count, new_start, new_count))
}

/// Parses `start[,count]` from one side of a hunk header; a missing count
/// means 1.
fn parse_start_count(field: &str) -> Option<(u32, u32)> {
    let (start, count) = match field.split_once(',') {
        Some((start, count)) => (start, count),
        None => (field, "1"),
    };
    Some((start.parse().ok()?, count.parse().ok()?))
}

/// Header and metadata lines of a file diff that carry no hunks; all of them
/// are skipped, so git may gain new metadata lines without breaking the
/// parser.
const DIFF_METADATA_PREFIXES: &[&str] = &[
    "--- ",
    "+++ ",
    "index ",
    "new file mode ",
    "deleted file mode ",
    "old mode ",
    "new mode ",
    "similarity index ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "GIT binary patch",
];

/// Accumulates one file's diff while its section is being parsed.
struct DiffFileBuilder {
    path: String,
    binary: bool,
    hunks: Vec<GitDiffHunk>,
    in_hunk: bool,
    old_line: u32,
    new_line: u32,
    old_remaining: u32,
    new_remaining: u32,
}

impl DiffFileBuilder {
    /// Parses the `diff --git` header (without the leading `diff --git `)
    /// into a new builder. The `b/` side is the file's current path.
    fn start(header: &str) -> io::Result<DiffFileBuilder> {
        let (old, rest) =
            parse_header_field(header).ok_or_else(|| malformed("malformed diff --git header"))?;
        let rest = rest.trim_start();
        let (new, rest) =
            parse_header_field(rest).ok_or_else(|| malformed("malformed diff --git header"))?;
        if !rest.trim().is_empty() {
            return Err(malformed("trailing data after diff --git paths"));
        }
        let old = old
            .strip_prefix("a/")
            .ok_or_else(|| malformed("diff --git old path lacks the \"a/\" prefix"))?;
        let path = new
            .strip_prefix("b/")
            .ok_or_else(|| malformed("diff --git new path lacks the \"b/\" prefix"))?;
        check_safe_path(old)?;
        check_safe_path(path)?;
        Ok(DiffFileBuilder {
            path: path.to_string(),
            binary: false,
            hunks: Vec::new(),
            in_hunk: false,
            old_line: 0,
            new_line: 0,
            old_remaining: 0,
            new_remaining: 0,
        })
    }

    /// Consumes one line of the file's section: a hunk header, a hunk body
    /// line, the binary marker or a skipped metadata line.
    fn push_line(&mut self, line: &str) -> io::Result<()> {
        if line.starts_with("@@") {
            if self.in_hunk && (self.old_remaining != 0 || self.new_remaining != 0) {
                return Err(malformed("hunk ends before its line counts are satisfied"));
            }
            let (old_start, old_count, new_start, new_count) =
                parse_hunk_header(line).ok_or_else(|| malformed("malformed hunk header"))?;
            self.hunks.push(GitDiffHunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
            self.old_line = old_start;
            self.new_line = new_start;
            self.old_remaining = old_count;
            self.new_remaining = new_count;
            self.in_hunk = true;
            return Ok(());
        }
        if self.in_hunk {
            let bytes = line.as_bytes();
            let (kind, old_line, new_line, counts_old, counts_new, text_start) = match bytes.first()
            {
                Some(b' ') => (
                    GitDiffLineKind::Context,
                    Some(self.old_line),
                    Some(self.new_line),
                    1,
                    1,
                    1,
                ),
                Some(b'-') => (GitDiffLineKind::Removed, Some(self.old_line), None, 1, 0, 1),
                Some(b'+') => (GitDiffLineKind::Added, None, Some(self.new_line), 0, 1, 1),
                _ => {
                    // git's "no trailing newline" marker after a body line.
                    if line == "\\ No newline at end of file" {
                        return Ok(());
                    }
                    return Err(malformed("unexpected line inside hunk"));
                }
            };
            self.old_remaining = self.old_remaining.checked_sub(counts_old).ok_or_else(|| {
                malformed("hunk has more old-side lines than its header declares")
            })?;
            self.new_remaining = self.new_remaining.checked_sub(counts_new).ok_or_else(|| {
                malformed("hunk has more new-side lines than its header declares")
            })?;
            self.old_line += counts_old;
            self.new_line += counts_new;
            self.hunks
                .last_mut()
                .expect("hunk is pushed before its body lines")
                .lines
                .push(GitDiffLine {
                    kind,
                    old_line,
                    new_line,
                    text: line[text_start..].to_string(),
                });
            return Ok(());
        }
        if line.starts_with("Binary files ") {
            self.binary = true;
            return Ok(());
        }
        if DIFF_METADATA_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return Ok(());
        }
        Err(malformed("unexpected line in git diff output"))
    }

    /// Finalizes the file, verifying the last hunk's line counts were met.
    fn finish(mut self) -> io::Result<GitDiffFile> {
        if self.in_hunk && (self.old_remaining != 0 || self.new_remaining != 0) {
            return Err(malformed("hunk ends before its line counts are satisfied"));
        }
        self.in_hunk = false;
        Ok(GitDiffFile {
            path: self.path,
            binary: self.binary,
            hunks: self.hunks,
        })
    }
}

/// Validates a caller-supplied diff path: it must be non-empty, relative and
/// free of parent, root and drive components, i.e. rooted at `root`.
fn check_rooted_path(path: &Path) -> io::Result<()> {
    check_rooted_path_named(path, "diff path")
}

/// Shared rooted-path validation for caller-supplied paths (the `diff`
/// pathspec and the `stage` paths): the path must be non-empty, relative and
/// free of parent, root and drive components, i.e. rooted at `root`. `what`
/// names the operation in error messages.
fn check_rooted_path_named(path: &Path, what: &str) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} must not be empty"),
        ));
    }
    let unsafe_component = path.components().any(|component| match component {
        Component::Normal(_) | Component::CurDir => false,
        Component::ParentDir | Component::RootDir | Component::Prefix(_) => true,
    });
    if path.is_absolute() || unsafe_component {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{what} must be rooted at the repository root: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git stage, unstage and commit
// ---------------------------------------------------------------------------

/// The result of a successful [`commit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitResult {
    /// The full 40-character object id of the new commit, read back with
    /// `git rev-parse HEAD`.
    pub hash: String,
    /// The commit's subject as git defines it (`git log -1 --format=%s`):
    /// the first paragraph of the message, with line breaks collapsed to
    /// spaces.
    pub summary: String,
}

/// Stages the given paths into the index and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> add -- <paths>`: no shell, and every path is passed
/// as its own argument after the `--` separator, so option-like names such
/// as `--help` or `-f` are staged literally. A `:`-leading path (git
/// pathspec magic) and any path that is empty, absolute, or contains a
/// parent, root or drive component is rejected with
/// [`io::ErrorKind::InvalidInput`] before git is spawned; an empty `paths`
/// slice is rejected the same way.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `paths` is empty or contains a path
///   that is not rooted at `root` or starts with `:`.
/// - [`io::ErrorKind::Other`] if `git add` exits non-zero (for example a
///   pathspec that matches no file); git's message is included.
pub fn stage(root: &Path, paths: &[&Path]) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one path must be staged",
        ));
    }
    for path in paths {
        check_stage_path(path)?;
    }
    run_git_add("git", root, paths)?;
    status(root)
}

/// Runs `program -C root add -- <paths>`; split out so tests can exercise
/// the spawn-failure path with a program that does not exist.
fn run_git_add(program: &str, root: &Path, paths: &[&Path]) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("add")
        .arg("--")
        .args(paths)
        .output()?;
    ensure_git_success(&output, "add")
}

/// Removes the given paths from the index and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> reset -- <paths>`: no shell, and every path is passed
/// as its own argument after the `--` separator, so option-like names such
/// as `--help` or `-f` are un-staged literally. A `.` pathspec un-stages
/// everything under `root` (git resolves it relative to `root`). Unlike
/// `git add`, `git reset --` with a pathspec does not fail when a path
/// matches nothing — the index is simply left untouched for that path — so a
/// successful call means git accepted the pathspecs and the returned
/// snapshot always reflects the resulting state.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `paths` is empty or contains a path
///   that is not rooted at `root` or starts with `:`.
/// - [`io::ErrorKind::Other`] if `git reset` exits non-zero; git's message
///   is included.
pub fn unstage(root: &Path, paths: &[&Path]) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one path must be un-staged",
        ));
    }
    for path in paths {
        check_unstage_path(path)?;
    }
    run_git_reset("git", root, paths)?;
    status(root)
}

/// Runs `program -C root reset -- <paths>`; split out so tests can exercise
/// the spawn-failure path with a program that does not exist.
fn run_git_reset(program: &str, root: &Path, paths: &[&Path]) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("reset")
        .arg("--")
        .args(paths)
        .output()?;
    ensure_git_success(&output, "reset")
}

/// Creates a commit from the staged changes and returns its hash and
/// summary.
///
/// Runs `git -C <root> commit --message=<message>`, where the message is a
/// single argument — never interpreted by a shell — so it cannot be parsed
/// as an option even when it looks like one. The commit's configuration
/// (identity, GPG signing, hooks) is git's own.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `message` is empty or whitespace-only
///   or contains a NUL byte.
/// - [`io::ErrorKind::Other`] if `git commit` exits non-zero (for example an
///   unconfigured identity or an empty index with "nothing to commit"); git's
///   message is included.
pub fn commit(root: &Path, message: &str) -> io::Result<GitCommitResult> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_commit_message(message)?;
    run_git_commit("git", root, message)?;
    let hash = run_git_capture("git", root, &["rev-parse", "HEAD"])?;
    let hash = String::from_utf8(hash).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git rev-parse output is not valid UTF-8",
        )
    })?;
    let summary = run_git_capture("git", root, &["log", "-1", "--format=%s"])?;
    let summary = String::from_utf8(summary).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git log output is not valid UTF-8",
        )
    })?;
    Ok(GitCommitResult {
        hash: hash.trim_end().to_string(),
        summary: summary.trim_end().to_string(),
    })
}

/// Runs `program -C root commit --message=<message>`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_commit(program: &str, root: &Path, message: &str) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("commit")
        .arg(format!("--message={message}"))
        .output()?;
    ensure_git_success(&output, "commit")
}

/// Runs `program -C root <args>` and returns stdout; a non-zero exit is an
/// [`io::ErrorKind::Other`] error carrying git's message.
fn run_git_capture(program: &str, root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    ensure_git_success(&output, args.first().copied().unwrap_or("subcommand"))?;
    Ok(output.stdout)
}

/// Maps a non-zero git exit to an [`io::ErrorKind::Other`] error. The
/// message is git's stderr, falling back to stdout when stderr is empty,
/// because `git commit` prints some failures (such as "nothing to commit")
/// on stdout.
fn ensure_git_success(output: &std::process::Output, subcommand: &str) -> io::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let code = match output.status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(io::Error::other(format!(
        "git {subcommand} failed ({code}): {detail}"
    )))
}

/// Rejects empty or whitespace-only commit messages and messages containing
/// NUL bytes (which cannot be passed as an argument) before git is spawned.
fn check_commit_message(message: &str) -> io::Result<()> {
    if message.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "commit message must not be empty",
        ));
    }
    if message.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "commit message must not contain NUL bytes",
        ));
    }
    Ok(())
}

/// Validates a caller-supplied path that git will see after the `--`
/// separator of an index-changing command (stage or unstage): it must be
/// rooted at `root` and must not start with `:` — after the `--` separator
/// git still interprets a `:` prefix as pathspec magic (for example
/// `:(top)` or `:(exclude)`). `what` names the operation in error messages.
fn check_index_path(path: &Path, what: &str) -> io::Result<()> {
    check_rooted_path_named(path, what)?;
    if path.to_string_lossy().starts_with(':') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{what} must not start with ':' (git pathspec magic): {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Validates a caller-supplied stage path: see [`check_index_path`].
fn check_stage_path(path: &Path) -> io::Result<()> {
    check_index_path(path, "stage path")
}

/// Validates a caller-supplied unstage path: see [`check_index_path`].
fn check_unstage_path(path: &Path) -> io::Result<()> {
    check_index_path(path, "unstage path")
}

// ---------------------------------------------------------------------------
// Git branches and checkout
// ---------------------------------------------------------------------------

/// One local branch of a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBranch {
    /// The branch's short name — the part after `refs/heads/`, for example
    /// `main`.
    pub name: String,
    /// Whether this branch is the currently checked-out branch. A detached
    /// HEAD marks no branch as current.
    pub current: bool,
}

/// The parsed result of [`branches`]: every local branch of the repository,
/// stably sorted by name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitBranchSnapshot {
    /// The branches, stably sorted by [`GitBranch::name`] in byte-wise order.
    /// An empty vec means the repository has no local branches (for example a
    /// fresh repository with no commits yet).
    pub branches: Vec<GitBranch>,
}

impl GitBranchSnapshot {
    /// The number of local branches.
    pub fn len(&self) -> usize {
        self.branches.len()
    }

    /// Whether the repository has no local branches.
    pub fn is_empty(&self) -> bool {
        self.branches.is_empty()
    }
}

/// Lists the repository's local branches and returns the stably sorted
/// snapshot.
///
/// Runs `git -C <root> for-each-ref --format=%(HEAD)|%(refname:short)
/// refs/heads`: no shell, and the format is git's own machine-oriented output
/// (`%(HEAD)` emits `*` for the checked-out branch and a space otherwise), so
/// no human-format line parsing is involved.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository); git's stderr is included in the message.
/// - [`io::ErrorKind::InvalidData`] if git's output is not valid UTF-8,
///   cannot be parsed, or reports a branch name that escapes the repository.
pub fn branches(root: &Path) -> io::Result<GitBranchSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    run_branches("git", root)
}

/// Runs `program -C root for-each-ref --format=%(HEAD)|%(refname:short)
/// refs/heads`; split out so tests can exercise the spawn-failure path with a
/// program that does not exist.
fn run_branches(program: &str, root: &Path) -> io::Result<GitBranchSnapshot> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args([
            "for-each-ref",
            "--format=%(HEAD)|%(refname:short)",
            "refs/heads",
        ])
        .output()?;
    ensure_git_success(&output, "for-each-ref")?;
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git for-each-ref output is not valid UTF-8",
        )
    })?;
    parse_branches(&stdout)
}

/// Parses `HEAD|name` lines into a snapshot, stably sorted by name.
fn parse_branches(output: &str) -> io::Result<GitBranchSnapshot> {
    let mut branches = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (head, name) = line
            .split_once('|')
            .ok_or_else(|| malformed("branch line must be HEAD|name"))?;
        let current = match head {
            "*" => true,
            // git's %(HEAD) atom emits a single space (not empty) for refs
            // that are not the current HEAD.
            " " | "" => false,
            _ => return Err(malformed("branch line has an unknown HEAD marker")),
        };
        check_safe_path(name)?;
        branches.push(GitBranch {
            name: name.to_string(),
            current,
        });
    }
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(GitBranchSnapshot { branches })
}

/// Switches the worktree to `branch` and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> checkout <branch>`: no shell, and the branch name is a
/// single argument. The name is validated before git is spawned — non-empty,
/// free of NUL bytes, not absolute and without `..` components, and not
/// `-`-prefixed — so it can never be parsed by git as an option. A `--`
/// separator cannot be used as an extra shield because `git checkout --` turns
/// its argument into a pathspec instead of a branch.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `branch` is empty, contains a NUL
///   byte, is absolute or contains a `..` component, or starts with `-`.
/// - [`io::ErrorKind::Other`] if `git checkout` exits non-zero (for example
///   an unknown branch, a checkout that would overwrite local changes, or a
///   `root` that is not inside a Git repository); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn checkout(root: &Path, branch: &str) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_branch_name(branch)?;
    run_git_checkout("git", root, branch)?;
    status(root)
}

/// Runs `program -C root checkout <branch>`; split out so tests can exercise
/// the spawn-failure path with a program that does not exist.
fn run_git_checkout(program: &str, root: &Path, branch: &str) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("checkout")
        .arg(branch)
        .output()?;
    ensure_git_success(&output, "checkout")
}

/// Validates a caller-supplied branch name before git is spawned: it must be
/// non-empty, free of NUL bytes, not absolute and without `..` components
/// (under either platform's rules), and must not start with `-` so it can
/// never be parsed by git as an option.
fn check_branch_name(branch: &str) -> io::Result<()> {
    if branch.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name must not be empty",
        ));
    }
    if branch.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name must not contain NUL bytes",
        ));
    }
    if branch.starts_with('-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("branch name must not start with '-': {branch:?}"),
        ));
    }
    let bytes = branch.as_bytes();
    let looks_absolute = bytes[0] == b'/'
        || bytes[0] == b'\\'
        || bytes.starts_with(b"//")
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic());
    let escapes = Path::new(branch).is_absolute()
        || looks_absolute
        || Path::new(branch)
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if escapes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("branch name must not be absolute or contain '..': {branch:?}"),
        ));
    }
    Ok(())
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
        // Keep file content byte-exact so diff tests see exactly what the
        // test wrote, regardless of the host's autocrlf setting.
        run_git(&root, &["config", "core.autocrlf", "false"]);
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

    // ---- diff parsing ----

    /// Projects a hunk's lines into `(kind, old_line, new_line, text)` tuples
    /// for compact assertions.
    fn line_triples(hunk: &GitDiffHunk) -> Vec<(GitDiffLineKind, Option<u32>, Option<u32>, &str)> {
        hunk.lines
            .iter()
            .map(|line| (line.kind, line.old_line, line.new_line, line.text.as_str()))
            .collect()
    }

    #[test]
    fn parses_hunk_with_context_and_changes() {
        let output = concat!(
            "diff --git a/a.txt b/a.txt\n",
            "index 83db48f..ed51eca 100644\n",
            "--- a/a.txt\n",
            "+++ b/a.txt\n",
            "@@ -1,3 +1,3 @@\n",
            " line1\n",
            "-line2\n",
            "+CHANGED\n",
            " line3\n",
        );
        let snapshot = parse_diff(output).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        let file = &snapshot.files[0];
        assert_eq!(file.path, "a.txt");
        assert!(!file.binary);
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.header, "@@ -1,3 +1,3 @@");
        assert_eq!(
            line_triples(hunk),
            [
                (GitDiffLineKind::Context, Some(1), Some(1), "line1"),
                (GitDiffLineKind::Removed, Some(2), None, "line2"),
                (GitDiffLineKind::Added, None, Some(2), "CHANGED"),
                (GitDiffLineKind::Context, Some(3), Some(3), "line3"),
            ]
        );
    }

    #[test]
    fn parses_omitted_hunk_counts_with_section_heading() {
        let output = concat!(
            "diff --git a/x.rs b/x.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -1 +1 @@ fn main() {\n",
            "-let a = 1;\n",
            "+let a = 2;\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let hunk = &snapshot.files[0].hunks[0];
        assert_eq!(hunk.header, "@@ -1 +1 @@ fn main() {");
        assert_eq!(
            line_triples(hunk),
            [
                (GitDiffLineKind::Removed, Some(1), None, "let a = 1;"),
                (GitDiffLineKind::Added, None, Some(1), "let a = 2;"),
            ]
        );
    }

    #[test]
    fn parses_new_file_with_empty_old_side() {
        let output = concat!(
            "diff --git a/new.txt b/new.txt\n",
            "new file mode 100644\n",
            "index 0000000..e69de29\n",
            "--- /dev/null\n",
            "+++ b/new.txt\n",
            "@@ -0,0 +1,2 @@\n",
            "+one\n",
            "+two\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let file = &snapshot.files[0];
        assert_eq!(file.path, "new.txt");
        let hunk = &file.hunks[0];
        assert_eq!(hunk.header, "@@ -0,0 +1,2 @@");
        assert_eq!(
            line_triples(hunk),
            [
                (GitDiffLineKind::Added, None, Some(1), "one"),
                (GitDiffLineKind::Added, None, Some(2), "two"),
            ]
        );
    }

    #[test]
    fn parses_deleted_file_with_empty_new_side() {
        let output = concat!(
            "diff --git a/old.txt b/old.txt\n",
            "deleted file mode 100644\n",
            "index 1234567..0000000\n",
            "--- a/old.txt\n",
            "+++ /dev/null\n",
            "@@ -1 +0,0 @@\n",
            "-gone\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let file = &snapshot.files[0];
        assert_eq!(file.path, "old.txt");
        assert_eq!(
            line_triples(&file.hunks[0]),
            [(GitDiffLineKind::Removed, Some(1), None, "gone")]
        );
    }

    #[test]
    fn parses_multiple_files_and_sorts_stably() {
        let output = concat!(
            "diff --git a/z.txt b/z.txt\n",
            "index a..b 100644\n",
            "--- a/z.txt\n",
            "+++ b/z.txt\n",
            "@@ -1 +1 @@\n",
            "-z\n",
            "+Z\n",
            "diff --git a/a.txt b/a.txt\n",
            "index a..b 100644\n",
            "--- a/a.txt\n",
            "+++ b/a.txt\n",
            "@@ -1 +1 @@\n",
            "-a\n",
            "+A\n",
            "diff --git a/m.txt b/m.txt\n",
            "index a..b 100644\n",
            "--- a/m.txt\n",
            "+++ b/m.txt\n",
            "@@ -1 +1 @@\n",
            "-m\n",
            "+M\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["a.txt", "m.txt", "z.txt"]);

        // Git never emits two sections for one path, but the snapshot must
        // still sort stably when sort keys tie.
        let dup = concat!(
            "diff --git a/d.txt b/d.txt\n",
            "--- a/d.txt\n",
            "+++ b/d.txt\n",
            "@@ -1 +1 @@\n",
            "-x\n",
            "+y\n",
            "diff --git a/d.txt b/d.txt\n",
            "--- a/d.txt\n",
            "+++ b/d.txt\n",
            "@@ -1 +1 @@\n",
            "-p\n",
            "+q\n",
        );
        let snapshot = parse_diff(dup).unwrap();
        assert_eq!(snapshot.files[0].hunks[0].lines[0].text, "x");
        assert_eq!(snapshot.files[1].hunks[0].lines[0].text, "p");
    }

    #[test]
    fn parses_quoted_diff_paths() {
        let output = concat!(
            "diff --git \"a/with space \\344\\270\\255\\346\\226\\207.txt\" \"b/with space \\344\\270\\255\\346\\226\\207.txt\"\n",
            "index a..b 100644\n",
            "--- \"a/with space \\344\\270\\255\\346\\226\\207.txt\"\n",
            "+++ \"b/with space \\344\\270\\255\\346\\226\\207.txt\"\n",
            "@@ -1 +1 @@\n",
            "-x\n",
            "+y\n",
        );
        let snapshot = parse_diff(output).unwrap();
        assert_eq!(snapshot.files[0].path, "with space 中文.txt");
    }

    #[test]
    fn marks_binary_files_without_hunks() {
        let output = concat!(
            "diff --git a/img.png b/img.png\n",
            "index a..b 100644\n",
            "Binary files a/img.png and b/img.png differ\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let file = &snapshot.files[0];
        assert_eq!(file.path, "img.png");
        assert!(file.binary);
        assert!(file.hunks.is_empty());
    }

    #[test]
    fn parses_mode_only_change_without_hunks() {
        let output = concat!(
            "diff --git a/run.sh b/run.sh\n",
            "old mode 100644\n",
            "new mode 100755\n",
        );
        let snapshot = parse_diff(output).unwrap();
        let file = &snapshot.files[0];
        assert_eq!(file.path, "run.sh");
        assert!(!file.binary);
        assert!(file.hunks.is_empty());
    }

    #[test]
    fn skips_no_newline_markers() {
        let output = concat!(
            "diff --git a/a.txt b/a.txt\n",
            "index a..b 100644\n",
            "--- a/a.txt\n",
            "+++ b/a.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "\\ No newline at end of file\n",
            "+new\n",
            "\\ No newline at end of file\n",
        );
        let snapshot = parse_diff(output).unwrap();
        assert_eq!(
            line_triples(&snapshot.files[0].hunks[0]),
            [
                (GitDiffLineKind::Removed, Some(1), None, "old"),
                (GitDiffLineKind::Added, None, Some(1), "new"),
            ]
        );
    }

    #[test]
    fn empty_diff_output_is_clean() {
        let snapshot = parse_diff("").unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.files.is_empty());
    }

    #[test]
    fn rejects_malformed_diff_output() {
        for output in [
            // Junk before the first file header.
            "hello\n",
            // Unterminated quoted path.
            "diff --git \"a/x b/x\n",
            // Trailing data after the two paths.
            "diff --git a/x b/x junk\n",
            // New path without the b/ prefix.
            "diff --git a/x x\n",
            // Malformed hunk header.
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ nonsense\n",
            // Body-like line before any hunk header.
            "diff --git a/x b/x\n line\n",
            // More lines than the hunk header declares.
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-x\n+x\n+y\n",
            // Fewer lines than the hunk header declares.
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n-x\n",
            // Unknown marker inside a hunk.
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n?weird\n",
        ] {
            assert!(
                parse_diff(output).is_err(),
                "output {output:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_diff_paths_that_escape_the_repository() {
        for output in [
            "diff --git a/../escape.txt b/../escape.txt\n--- a/../escape.txt\n+++ b/../escape.txt\n@@ -1 +1 @@\n-x\n+y\n",
            "diff --git a/C:/win.txt b/C:/win.txt\n--- a/C:/win.txt\n+++ b/C:/win.txt\n@@ -1 +1 @@\n-x\n+y\n",
            "diff --git a/\\\\server\\share\\f.txt b/\\\\server\\share\\f.txt\n--- a/\\\\server\\share\\f.txt\n+++ b/\\\\server\\share\\f.txt\n@@ -1 +1 @@\n-x\n+y\n",
        ] {
            let error = parse_diff(output).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "output {output:?} should be rejected as unsafe"
            );
        }
    }

    // ---- diff() root and path validation (no git binary needed) ----

    #[test]
    fn diff_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-diff-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            diff(&missing, None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn diff_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-diff-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            diff(&file, None).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_diff_surfaces_as_not_found() {
        let root =
            std::env::temp_dir().join(format!("nextide-diff-nobinary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let error = run_diff("nextide-no-such-git-binary", &root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_rejects_unrooted_query_paths() {
        let root = temp_dir("diffbad");
        for path in [
            Path::new(""),
            Path::new("/etc/passwd"),
            Path::new("..\\outside.txt"),
            Path::new("a/../../outside.txt"),
            Path::new("C:\\windows\\file.txt"),
            Path::new("C:drive-relative.txt"),
            Path::new("\\\\server\\share\\file.txt"),
        ] {
            let error = diff(&root, Some(path)).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "path {path:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    // ---- diff() integration (requires a git binary) ----

    fn find_diff_file<'a>(snapshot: &'a GitDiffSnapshot, path: &str) -> &'a GitDiffFile {
        snapshot
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no diff for {path:?} in {snapshot:?}"))
    }

    #[test]
    fn diff_clean_repository_is_empty() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("diffclean", &[("a.txt", "a\n")]);
        let snapshot = diff(&root, None).unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_reports_unstaged_modifications_with_line_numbers() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("diffmod", &[("a.txt", "line1\nline2\nline3\n")]);
        fs::write(root.join("a.txt"), "line1\nCHANGED\nline3\n").unwrap();

        let snapshot = diff(&root, None).unwrap();
        assert_eq!(snapshot.len(), 1);
        let file = find_diff_file(&snapshot, "a.txt");
        assert!(!file.binary);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.header, "@@ -1,3 +1,3 @@");
        assert_eq!(
            line_triples(hunk),
            [
                (GitDiffLineKind::Context, Some(1), Some(1), "line1"),
                (GitDiffLineKind::Removed, Some(2), None, "line2"),
                (GitDiffLineKind::Added, None, Some(2), "CHANGED"),
                (GitDiffLineKind::Context, Some(3), Some(3), "line3"),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_splits_distant_changes_into_multiple_hunks() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let content: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        let root = init_repo("diffhunks", &[("a.txt", &content)]);
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[3] = "line4 CHANGED".to_string();
        lines[15] = "line16 CHANGED".to_string();
        fs::write(root.join("a.txt"), lines.join("\n") + "\n").unwrap();

        let snapshot = diff(&root, None).unwrap();
        let file = find_diff_file(&snapshot, "a.txt");
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].header, "@@ -1,7 +1,7 @@");
        // git does not extend the last hunk to the end of the file, and for
        // files without a recognized function pattern the heading is the
        // line preceding the hunk; only the counts are asserted here.
        assert!(
            file.hunks[1].header.starts_with("@@ -13,7 +13,7 @@"),
            "unexpected hunk header: {}",
            file.hunks[1].header
        );
        let changed: Vec<(&str, GitDiffLineKind, Option<u32>, Option<u32>)> = file
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .filter(|line| line.kind != GitDiffLineKind::Context)
            .map(|line| (line.text.as_str(), line.kind, line.old_line, line.new_line))
            .collect();
        assert_eq!(
            changed,
            [
                ("line4", GitDiffLineKind::Removed, Some(4), None),
                ("line4 CHANGED", GitDiffLineKind::Added, None, Some(4)),
                ("line16", GitDiffLineKind::Removed, Some(16), None),
                ("line16 CHANGED", GitDiffLineKind::Added, None, Some(16)),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_reports_new_and_deleted_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo(
            "diffnewdel",
            &[("a.txt", "a\n"), ("gone.txt", "one\ntwo\n")],
        );
        // Intent-to-add makes the new file visible to plain `git diff`.
        fs::write(root.join("new.txt"), "hello\nworld\n").unwrap();
        run_git(&root, &["add", "-N", "new.txt"]);
        // Deleting from the worktree without staging shows a deletion.
        fs::remove_file(root.join("gone.txt")).unwrap();

        let snapshot = diff(&root, None).unwrap();
        let new_file = find_diff_file(&snapshot, "new.txt");
        assert_eq!(new_file.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert_eq!(
            line_triples(&new_file.hunks[0]),
            [
                (GitDiffLineKind::Added, None, Some(1), "hello"),
                (GitDiffLineKind::Added, None, Some(2), "world"),
            ]
        );
        let gone = find_diff_file(&snapshot, "gone.txt");
        assert_eq!(gone.hunks[0].header, "@@ -1,2 +0,0 @@");
        assert_eq!(
            line_triples(&gone.hunks[0]),
            [
                (GitDiffLineKind::Removed, Some(1), None, "one"),
                (GitDiffLineKind::Removed, Some(2), None, "two"),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_limits_to_requested_path() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo(
            "difflimit",
            &[("a.txt", "a\n"), ("b.txt", "b\n"), ("sub/c.txt", "c\n")],
        );
        fs::write(root.join("a.txt"), "A\n").unwrap();
        fs::write(root.join("b.txt"), "B\n").unwrap();
        fs::write(root.join("sub/c.txt"), "C\n").unwrap();

        let snapshot = diff(&root, Some(Path::new("b.txt"))).unwrap();
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["b.txt"]);

        // A pathspec that matches nothing yields an empty snapshot.
        assert!(diff(&root, Some(Path::new("nope.txt"))).unwrap().is_empty());

        // From a nested root the pathspec is resolved relative to `root`,
        // while the reported path stays relative to the repository top level.
        let nested = diff(&root.join("sub"), Some(Path::new("c.txt"))).unwrap();
        let paths: Vec<&str> = nested.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["sub/c.txt"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_parses_quoted_paths_and_sorts_stably() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo(
            "diffquoted",
            &[
                ("with space 中文.txt", "a\n"),
                ("b.txt", "b\n"),
                ("sub/nested.txt", "n\n"),
            ],
        );
        fs::write(root.join("with space 中文.txt"), "CHANGED\n").unwrap();
        fs::write(root.join("b.txt"), "CHANGED\n").unwrap();
        fs::write(root.join("sub/nested.txt"), "CHANGED\n").unwrap();

        let snapshot = diff(&root, None).unwrap();
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["b.txt", "sub/nested.txt", "with space 中文.txt"]);
        for file in &snapshot.files {
            assert!(Path::new(&file.path).is_relative());
            assert!(!file.path.starts_with(".."));
            assert!(root.join(&file.path).starts_with(&root));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_marks_real_binary_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("diffbin", &[("bin.dat", "placeholder")]);
        // Replacing the committed text with NUL bytes makes git report a
        // binary diff.
        fs::write(root.join("bin.dat"), [0x00, 0x01, 0x02]).unwrap();
        let snapshot = diff(&root, None).unwrap();
        let file = find_diff_file(&snapshot, "bin.dat");
        assert!(file.binary);
        assert!(file.hunks.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_replaces_non_utf8_content_with_replacement_char() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("difflossy", &[("raw.dat", "abc\n")]);
        // No NUL byte, so git treats this as text and emits the raw 0xFF in
        // the hunk body; the parser must not fail on it.
        fs::write(root.join("raw.dat"), [0xFF, b'x', b'\n']).unwrap();
        let snapshot = diff(&root, None).unwrap();
        let file = find_diff_file(&snapshot, "raw.dat");
        assert!(!file.binary);
        let texts: Vec<&str> = file.hunks[0]
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains('\u{FFFD}')),
            "expected a replacement character in {texts:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- stage() argument safety (no git binary needed) ----

    #[test]
    fn stage_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stage-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stage(&missing, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stage_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-stage-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stage(&file, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn stage_empty_paths_is_invalid_input() {
        let root = temp_dir("stageempty");
        let error = stage(&root, &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_rejects_unrooted_and_magic_paths() {
        let root = temp_dir("stagebad");
        for path in [
            Path::new(""),
            Path::new("/etc/passwd"),
            Path::new("..\\outside.txt"),
            Path::new("a/../../outside.txt"),
            Path::new("C:\\windows\\file.txt"),
            Path::new("C:drive-relative.txt"),
            Path::new("\\\\server\\share\\file.txt"),
            Path::new(":(top)etc"),
            Path::new(":(exclude)secret"),
        ] {
            let error = stage(&root, &[path]).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "path {path:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_stage_surfaces_as_not_found() {
        let root = temp_dir("stagenobinary");
        let error =
            run_git_add("nextide-no-such-git-binary", &root, &[Path::new("a.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stage() integration (requires a git binary) ----

    #[test]
    fn stage_adds_paths_to_the_index_and_returns_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stageadd", &[("a.txt", "a"), ("b.txt", "b")]);
        fs::write(root.join("a.txt"), "CHANGED").unwrap();
        fs::write(root.join("new.txt"), "untracked").unwrap();

        let snapshot = stage(&root, &[Path::new("a.txt"), Path::new("new.txt")]).unwrap();

        let staged = find(&snapshot, "a.txt");
        assert_eq!(
            staged.state,
            GitFileState::new(GitStatusCode::Modified, GitStatusCode::Unmodified)
        );
        let added = find(&snapshot, "new.txt");
        assert_eq!(
            added.state,
            GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified)
        );
        // b.txt was not requested and must not be staged.
        assert!(!snapshot.entries.iter().any(|entry| entry.path == "b.txt"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_handles_option_like_quoted_and_whitespace_paths() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stageopt", &[("plain.txt", "p")]);
        fs::write(root.join("--help"), "x").unwrap();
        fs::write(root.join("-f"), "z").unwrap();
        fs::write(root.join("with space 中文.txt"), "y").unwrap();

        let snapshot = stage(
            &root,
            &[
                Path::new("--help"),
                Path::new("-f"),
                Path::new("with space 中文.txt"),
            ],
        )
        .unwrap();
        for path in ["--help", "-f", "with space 中文.txt"] {
            let entry = find(&snapshot, path);
            assert_eq!(
                entry.state,
                GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified)
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_with_dot_path_stages_the_whole_root() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stagedot", &[("a.txt", "a")]);
        fs::write(root.join("untracked.txt"), "u").unwrap();
        let snapshot = stage(&root, &[Path::new(".")]).unwrap();
        let entry = find(&snapshot, "untracked.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_records_deleted_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stagedel", &[("gone.txt", "x")]);
        fs::remove_file(root.join("gone.txt")).unwrap();
        let snapshot = stage(&root, &[Path::new("gone.txt")]).unwrap();
        let entry = find(&snapshot, "gone.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Deleted, GitStatusCode::Unmodified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_nonexistent_path_propagates_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stagemiss", &[("a.txt", "a")]);
        let error = stage(&root, &[Path::new("no-such-file.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("did not match any files"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_from_nested_root_resolves_paths_relative_to_root() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stagenested", &[("sub/a.txt", "a")]);
        fs::write(root.join("sub/b.txt"), "b").unwrap();
        let snapshot = stage(&root.join("sub"), &[Path::new("b.txt")]).unwrap();
        // status reports paths relative to the repository top level.
        let entry = find(&snapshot, "sub/b.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified)
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- unstage() argument safety (no git binary needed) ----

    #[test]
    fn unstage_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-unstage-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            unstage(&missing, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn unstage_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-unstage-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            unstage(&file, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn unstage_empty_paths_is_invalid_input() {
        let root = temp_dir("unstageempty");
        let error = unstage(&root, &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_rejects_unrooted_and_magic_paths() {
        let root = temp_dir("unstagebad");
        for path in [
            Path::new(""),
            Path::new("/etc/passwd"),
            Path::new("..\\outside.txt"),
            Path::new("a/../../outside.txt"),
            Path::new("C:\\windows\\file.txt"),
            Path::new("C:drive-relative.txt"),
            Path::new("\\\\server\\share\\file.txt"),
            Path::new(":(top)etc"),
            Path::new(":(exclude)secret"),
        ] {
            let error = unstage(&root, &[path]).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "path {path:?} should be rejected"
            );
        }
        // `.` (un-stage everything under root) passes validation.
        check_unstage_path(Path::new(".")).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_unstage_surfaces_as_not_found() {
        let root = temp_dir("unstagenobinary");
        let error =
            run_git_reset("nextide-no-such-git-binary", &root, &[Path::new("a.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- unstage() integration (requires a git binary) ----

    #[test]
    fn unstage_removes_paths_from_the_index_and_returns_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("unstagepick", &[("a.txt", "a"), ("b.txt", "b")]);
        fs::write(root.join("a.txt"), "A").unwrap();
        fs::write(root.join("b.txt"), "B").unwrap();
        let staged = stage(&root, &[Path::new("a.txt"), Path::new("b.txt")]).unwrap();
        assert_eq!(staged.entries.len(), 2);

        // Only a.txt is un-staged; b.txt stays staged.
        let snapshot = unstage(&root, &[Path::new("a.txt")]).unwrap();
        let a = find(&snapshot, "a.txt");
        assert_eq!(
            a.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        let b = find(&snapshot, "b.txt");
        assert_eq!(
            b.state,
            GitFileState::new(GitStatusCode::Modified, GitStatusCode::Unmodified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_with_dot_unstages_the_whole_root() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("unstagedot", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "CHANGED").unwrap();
        fs::write(root.join("new.txt"), "untracked").unwrap();
        let staged = stage(&root, &[Path::new("a.txt"), Path::new("new.txt")]).unwrap();
        assert_eq!(staged.entries.len(), 2);

        let snapshot = unstage(&root, &[Path::new(".")]).unwrap();
        let a = find(&snapshot, "a.txt");
        assert_eq!(
            a.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        let new = find(&snapshot, "new.txt");
        assert!(new.state.is_untracked());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_restores_staged_deletion_to_unstaged() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("unstagedel", &[("gone.txt", "x")]);
        fs::remove_file(root.join("gone.txt")).unwrap();
        let staged = stage(&root, &[Path::new("gone.txt")]).unwrap();
        assert_eq!(
            find(&staged, "gone.txt").state,
            GitFileState::new(GitStatusCode::Deleted, GitStatusCode::Unmodified)
        );

        let snapshot = unstage(&root, &[Path::new("gone.txt")]).unwrap();
        let entry = find(&snapshot, "gone.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Deleted)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_handles_option_like_and_whitespace_paths() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("unstageopt", &[("plain.txt", "p")]);
        fs::write(root.join("--help"), "x").unwrap();
        fs::write(root.join("-f"), "z").unwrap();
        fs::write(root.join("with space 中文.txt"), "y").unwrap();
        let paths = [
            Path::new("--help"),
            Path::new("-f"),
            Path::new("with space 中文.txt"),
        ];
        let staged = stage(&root, &paths).unwrap();
        for path in ["--help", "-f", "with space 中文.txt"] {
            assert_eq!(
                find(&staged, path).state,
                GitFileState::new(GitStatusCode::Added, GitStatusCode::Unmodified)
            );
        }

        // Option-like names are passed literally after `--`, never parsed as
        // git options, and un-staging drops them back to untracked.
        let snapshot = unstage(&root, &paths).unwrap();
        for path in ["--help", "-f", "with space 中文.txt"] {
            let entry = find(&snapshot, path);
            assert!(
                entry.state.is_untracked(),
                "{path:?} should be untracked again, was {:?}",
                entry.state
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_with_nothing_staged_returns_clean_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("unstageclean", &[("a.txt", "a")]);
        let snapshot = unstage(&root, &[Path::new("a.txt")]).unwrap();
        assert!(snapshot.is_clean());
        // Unlike `git add`, a `git reset` pathspec that matches nothing is a
        // no-op (exit 0), so un-staging a non-existent path also succeeds.
        let snapshot = unstage(&root, &[Path::new("no-such-file.txt")]).unwrap();
        assert!(snapshot.is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstage_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("unstagenonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = unstage(&root, &[Path::new("a.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- commit() argument safety (no git binary needed) ----

    #[test]
    fn commit_empty_message_is_invalid_input() {
        let root = temp_dir("commitempty");
        for message in ["", "   ", "\n\n", "\t"] {
            let error = commit(&root, message).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "message {message:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_message_with_nul_is_invalid_input() {
        let root = temp_dir("commitnul");
        let error = commit(&root, "subject\0nul").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-commit-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            commit(&missing, "message").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn commit_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-commit-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            commit(&file, "message").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_commit_surfaces_as_not_found() {
        let root = temp_dir("commitnobinary");
        let error = run_git_commit("nextide-no-such-git-binary", &root, "message").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- commit() integration (requires a git binary) ----

    /// Like [`init_repo`], plus a repo-local setting that disables GPG
    /// signing so commit tests are immune to the host's global signing
    /// configuration.
    fn commit_repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = init_repo(tag, files);
        run_git(&root, &["config", "commit.gpgsign", "false"]);
        root
    }

    /// A fresh, configured repository with no commits yet.
    fn init_clean_repo(tag: &str) -> PathBuf {
        let root = temp_dir(tag);
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.name", "nextide-tests"]);
        run_git(
            &root,
            &["config", "user.email", "nextide-tests@example.com"],
        );
        run_git(&root, &["config", "commit.gpgsign", "false"]);
        root
    }

    #[test]
    fn commit_creates_commit_and_reports_hash_and_summary() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("commitok", &[("a.txt", "a\n")]);
        fs::write(root.join("a.txt"), "CHANGED\n").unwrap();
        let snapshot = stage(&root, &[Path::new("a.txt")]).unwrap();
        assert_eq!(snapshot.entries.len(), 1);

        let result = commit(&root, "update a.txt").unwrap();
        assert_eq!(result.summary, "update a.txt");
        assert_eq!(result.hash.len(), 40);
        assert!(result.hash.chars().all(|c| c.is_ascii_hexdigit()));

        // The recorded hash is the actual HEAD commit.
        let recorded = run_git_capture("git", &root, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(String::from_utf8(recorded).unwrap().trim_end(), result.hash);

        // The worktree is clean afterwards.
        assert!(status(&root).unwrap().is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_works_for_the_root_commit_of_a_new_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_clean_repo("commitroot");
        fs::write(root.join("first.txt"), "hello").unwrap();
        stage(&root, &[Path::new("first.txt")]).unwrap();
        let result = commit(&root, "first commit").unwrap();
        assert_eq!(result.summary, "first commit");
        assert_eq!(result.hash.len(), 40);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_preserves_multiline_message_and_reports_git_subject() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("commitmulti", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "b").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        let result = commit(&root, "subject line\n\nbody line\nmore body").unwrap();
        assert_eq!(result.summary, "subject line");
        // The full message is stored verbatim (git appends a trailing
        // newline; `%B` emits the raw body).
        let body = run_git_capture("git", &root, &["log", "-1", "--format=%B"]).unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(
            body.starts_with("subject line\n\nbody line\nmore body\n"),
            "body was: {body:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_treats_option_like_messages_as_message_text() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("commitopt", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "b").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        // An option-like message is committed verbatim, never parsed as a
        // git option (for example --amend).
        let result = commit(&root, "--amend").unwrap();
        assert_eq!(result.summary, "--amend");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_with_nothing_staged_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("commitnothing", &[("a.txt", "a")]);
        // No staged changes: git exits non-zero and prints "nothing to
        // commit" on stdout, which the error must surface.
        let error = commit(&root, "no-op").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("nothing to commit"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("commitnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = commit(&root, "message").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- branch parsing ----

    #[test]
    fn parses_branch_lines_with_current_marker() {
        let snapshot = parse_branches("*|main\n|feature\n|dev\n").unwrap();
        let names: Vec<&str> = snapshot
            .branches
            .iter()
            .map(|branch| branch.name.as_str())
            .collect();
        assert_eq!(names, ["dev", "feature", "main"]);
        assert!(snapshot.branches[2].current);
        assert!(!snapshot.branches[0].current);
        assert!(!snapshot.branches[1].current);
    }

    #[test]
    fn branch_sort_is_stable_for_equal_names() {
        // for-each-ref never emits two lines for one branch, but the snapshot
        // must still preserve input order when sort keys tie.
        let snapshot = parse_branches("*|dup\n|dup\n").unwrap();
        assert!(snapshot.branches[0].current);
        assert!(!snapshot.branches[1].current);
    }

    #[test]
    fn parses_empty_branch_list_as_empty_snapshot() {
        let snapshot = parse_branches("").unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.branches.is_empty());
    }

    #[test]
    fn rejects_malformed_branch_lines() {
        for output in [
            "main",    // no `|` separator
            "x|main",  // unknown HEAD marker
            "**|main", // unknown HEAD marker
            " *|main", // unknown HEAD marker
            "|",       // empty name
            "*|",      // empty name
        ] {
            assert!(
                parse_branches(output).is_err(),
                "output {output:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_branch_names_that_escape_the_repository() {
        for output in [
            "|/etc/passwd",
            "|\\\\server\\share\\x",
            "|C:/windows/x",
            "|../outside",
            "|a/../../outside",
        ] {
            let error = parse_branches(output).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "output {output:?} should be rejected as unsafe"
            );
        }
    }

    // ---- branches() root validation (no git binary needed) ----

    #[test]
    fn branches_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-branches-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            branches(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn branches_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-branches-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            branches(&file).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_branches_surfaces_as_not_found() {
        let root = temp_dir("branchesnobinary");
        let error = run_branches("nextide-no-such-git-binary", &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- branches() integration (requires a git binary) ----

    /// The current branch's short name, for example `main`.
    fn current_branch(root: &Path) -> String {
        let bytes = run_git_capture("git", root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        String::from_utf8(bytes).unwrap().trim().to_string()
    }

    #[test]
    fn branches_lists_local_branches_with_current_marker() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("branches", &[("a.txt", "a")]);
        let base = current_branch(&root);
        run_git(&root, &["checkout", "-b", "feature"]);
        run_git(&root, &["branch", "dev"]);
        run_git(&root, &["branch", "zeta"]);

        let snapshot = branches(&root).unwrap();
        let names: Vec<&str> = snapshot
            .branches
            .iter()
            .map(|branch| branch.name.as_str())
            .collect();
        let mut expected = vec![base.as_str(), "dev", "feature", "zeta"];
        expected.sort();
        assert_eq!(names, expected);

        let current = snapshot
            .branches
            .iter()
            .find(|branch| branch.current)
            .unwrap();
        assert_eq!(current.name, "feature");

        // Every name is a short, relative branch name.
        for branch in &snapshot.branches {
            assert!(Path::new(&branch.name).is_relative());
            assert!(!branch.name.starts_with(".."));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn branches_reports_no_current_branch_when_detached() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("branchdetached", &[("a.txt", "a")]);
        run_git(&root, &["checkout", "--detach", "-q"]);
        run_git(&root, &["branch", "feature"]);
        let snapshot = branches(&root).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.branches.iter().all(|branch| !branch.current));
        let _ = fs::remove_dir_all(root);
    }

    // ---- checkout() argument safety (no git binary needed) ----

    #[test]
    fn checkout_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-checkout-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            checkout(&missing, "main").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn checkout_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-checkout-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            checkout(&file, "main").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn checkout_rejects_unsafe_branch_names() {
        let root = temp_dir("checkoutbad");
        for branch in [
            "",
            "\0",
            "/etc/passwd",
            "\\server\\share\\x",
            "C:/windows/x",
            "C:\\windows\\x",
            "..\\..\\outside",
            "a/../../outside",
            "..",
            "-f",
            "--force",
            "-b",
            "--",
        ] {
            let error = checkout(&root, branch).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "branch {branch:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_checkout_surfaces_as_not_found() {
        let root = temp_dir("checkoutnobinary");
        let error = run_git_checkout("nextide-no-such-git-binary", &root, "main").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- checkout() integration (requires a git binary) ----

    #[test]
    fn checkout_switches_branch_and_returns_clean_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("checkoutswitch", &[("a.txt", "a\n")]);
        let base = current_branch(&root);
        run_git(&root, &["checkout", "-b", "feature"]);
        fs::write(root.join("feature.txt"), "f\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "feature",
            ],
        );
        assert_eq!(current_branch(&root), "feature");

        let snapshot = checkout(&root, &base).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(current_branch(&root), base);
        assert!(!root.join("feature.txt").exists());

        let snapshot = checkout(&root, "feature").unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(current_branch(&root), "feature");
        assert!(root.join("feature.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkout_unknown_branch_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("checkoutunknown", &[("a.txt", "a")]);
        let error = checkout(&root, "no-such-branch").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("did not match any file(s)"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkout_refuses_to_overwrite_conflicting_changes() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("checkoutconflict", &[("a.txt", "base\n")]);
        let base = current_branch(&root);
        // feature commits a different a.txt than the base branch.
        run_git(&root, &["checkout", "-b", "feature"]);
        fs::write(root.join("a.txt"), "feature\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "feature",
            ],
        );
        run_git(&root, &["checkout", base.as_str()]);
        fs::write(root.join("a.txt"), "side\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "side"],
        );
        run_git(&root, &["checkout", "-b", "side"]);
        // A dirty worktree change to a.txt would be clobbered by switching to
        // feature, so git must refuse the checkout.
        fs::write(root.join("a.txt"), "local uncommitted\n").unwrap();

        let error = checkout(&root, "feature").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("local changes"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkout_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("checkoutnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = checkout(&root, "main").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
