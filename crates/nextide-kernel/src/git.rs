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
//! [`amend(root, message)`](amend) replaces the current HEAD commit with
//! `git -C <root> commit --amend --message=<message>` and returns the same
//! [`GitCommitResult`] as [`commit`]: the fresh HEAD hash and subject. The
//! message validation and the result parsing are shared with [`commit`], so
//! an empty or NUL message is [`io::ErrorKind::InvalidInput`], root and
//! git-binary errors match [`status`], and any git failure (for example a
//! repository with no commits yet: "You have nothing to amend.") is
//! [`io::ErrorKind::Other`] with git's message. The amend never opens an
//! editor — the message is always supplied — and it rewrites the last commit
//! in place: the parent stays the same, staged changes are folded in, and
//! the hash always changes, so like any history rewrite it must only be
//! applied to commits that have not been published.
//!
//! Error behaviour matches [`status`] and [`diff`]: a missing `root` or
//! `git` is [`io::ErrorKind::NotFound`], a non-directory `root` is
//! [`io::ErrorKind::NotADirectory`], and a non-zero git exit (for example a
//! pathspec that matches nothing, an unconfigured identity, or "nothing to
//! commit") is [`io::ErrorKind::Other`] with git's message — taken from
//! stderr, or from stdout when stderr is empty, because `git commit` prints
//! some failures (such as "nothing to commit") on stdout.
//!
//! # History query
//!
//! [`log(root, limit)`](log) lists the repository's commit history with
//! `git -C <root> log --format=%H%x00%P%x00%an%x00%aI%x00%s --date=iso-strict`:
//! no shell, and the fields of every record are separated by NUL bytes
//! (`%x00`, so neither a field nor a record terminator can collide with the
//! data) with records terminated by newlines — git's own machine-oriented
//! format atoms, never a human-format line. Each [`GitCommitEntry`] carries
//! the full `hash`, its `parents` (empty for a root commit, one hash per
//! parent for a merge), the `author` name, the author `date` in strict ISO
//! 8601 (for example `2026-08-29T13:57:43+08:00`), and the `subject`. The
//! [`GitLogSnapshot`] keeps git's own output order — newest commit first, git
//! log's default reverse-chronological order — and is never re-sorted, so the
//! order is deterministic for a given repository.
//!
//! `limit` caps the snapshot: `Some(n)` also limits the query itself with
//! `--max-count=n` (a count beyond git's signed limit is not passed to git;
//! the parsed snapshot is truncated regardless), `Some(0)` short-circuits to
//! an empty snapshot without spawning git, and `None` returns the whole
//! history.
//!
//! Parsed output is validated before it is trusted: the output must be valid
//! UTF-8, every record must have exactly the five fields (hash, parents,
//! author, date, subject) with a non-empty author and date, and every hash —
//! the commit's own and each parent's — must be non-empty hexadecimal;
//! anything else is [`io::ErrorKind::InvalidData`]. A subject may be empty
//! (an `--allow-empty-message` commit). Error behaviour matches [`status`]: a
//! missing `root` or `git` is [`io::ErrorKind::NotFound`], a non-directory
//! `root` is [`io::ErrorKind::NotADirectory`], and a non-zero git exit is
//! [`io::ErrorKind::Other`] with git's message — a repository without any
//! commits makes `git log` itself fail ("your current branch ... does not
//! have any commits yet").
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
//!
//! # Tags
//!
//! [`tags(root)`](tags) lists the repository's tags with
//! `git -C <root> for-each-ref --format=%(objectname)%00%(refname:short)
//! refs/tags` and returns a stably sorted [`GitTagSnapshot`]. Every
//! [`GitTag`] carries the tag's short name (the part after `refs/tags/`)
//! and the `object` id git reports for it. The format is git's own
//! machine-oriented output — NUL-separated fields with newline-terminated
//! records — so no human-format parsing is involved. For a *lightweight*
//! tag `object` is the full id of the object the tag names (typically a
//! commit); for an *annotated* tag it is the id of the tag object itself,
//! recorded without expanding the tag to its target — the snapshot never
//! peels an annotated tag. Every parsed object must be non-empty
//! hexadecimal and every name must pass the same escape-rejecting check as
//! status paths; anything else is [`io::ErrorKind::InvalidData`]. Error
//! behaviour matches [`branches`]: a missing `root` or `git` is
//! [`io::ErrorKind::NotFound`], a non-directory `root` is
//! [`io::ErrorKind::NotADirectory`], and a non-zero git exit (for example
//! `root` is not inside a Git repository) is [`io::ErrorKind::Other`] with
//! git's stderr in the message.
//!
//! [`tag(root, name)`](tag) creates a *lightweight* tag with
//! `git -C <root> tag -- <name>` and returns the fresh [`GitTagSnapshot`]
//! from [`tags`]; [`delete_tag(root, name)`](delete_tag) deletes a tag with
//! `git -C <root> tag --delete <name>` and returns the fresh snapshot the
//! same way. No shell is involved and every name travels as a single
//! argument; the `--` separator shields `tag` from option parsing, and the
//! name is validated before git is spawned regardless — non-empty, free of
//! NUL bytes, not `-`-prefixed, relative and free of `..` and empty
//! components (no leading, trailing or doubled `/`), and free of control
//! characters, whitespace and git's other ref-forbidden characters (`~`,
//! `^`, `:`, `?`, `*`, `[`, `\`, `@{`, leading-dot components,
//! `.lock`-suffixed components, and trailing dots) — mirroring
//! `git check-ref-format` while allowing one-level names such as `v1.2`
//! (the tag lives under `refs/tags/`, so the usual "must contain a slash"
//! rule is waived). Anything else is [`io::ErrorKind::InvalidInput`] before
//! git is spawned. Both operations are local to the repository — the
//! worktree and the index are never touched — and a tag that already exists
//! (for `tag`) or does not exist (for `delete_tag`) surfaces as
//! [`io::ErrorKind::Other`] with git's message.
//!
//! # Remotes
//!
//! [`remotes(root)`](remotes) lists the repository's configured remotes with
//! `git -C <root> remote -v`: no shell, and the query is strictly read-only —
//! it only reads the local Git configuration and never contacts a network or
//! performs any fetch or push. Git prints one line per remote URL as
//! `name<TAB>url (fetch|push)`; each [`GitRemote`] carries the remote's
//! `name`, the `fetch_url` from its `(fetch)` line (the
//! `remote.<name>.url` value — `None` when the remote has no fetch URL, in
//! which case git prints a bare `name<TAB>` line), and the `push_urls` from
//! its `(push)` lines in git's order: a remote without an explicit
//! `remote.<name>.pushurl` reports its fetch URL as the single push URL, and
//! a multi-valued `pushurl` yields one entry per value. URLs are kept exactly
//! as git prints them — they may contain spaces and are never interpreted as
//! paths or passed back to git. The snapshot is stably sorted by name. Parsed
//! output must be valid UTF-8, every line must be `name<TAB>url (fetch)`,
//! `name<TAB>url (push)` or the bare `name<TAB>` form, every name must be
//! non-empty, free of whitespace and pass the same escape-rejecting check as
//! status paths, and a remote may carry at most one fetch line; anything else
//! is [`io::ErrorKind::InvalidData`]. Error behaviour matches [`tags`]: a
//! missing `root` or `git` is [`io::ErrorKind::NotFound`], a non-directory
//! `root` is [`io::ErrorKind::NotADirectory`], and a non-zero git exit (for
//! example `root` is not inside a Git repository) is [`io::ErrorKind::Other`]
//! with git's stderr in the message.
//!
//! # Restore and stash
//!
//! [`restore(root, paths)`](restore) discards the named paths' *worktree*
//! changes with `git -C <root> restore -- <paths>` and returns the fresh
//! [`GitStatusSnapshot`] from [`status`]. The default source of `git restore`
//! is the index, so only the worktree is rewritten: staged changes are kept,
//! and untracked files are never deleted (a pathspec that matches an
//! untracked file is a git error, surfaced as [`io::ErrorKind::Other`]).
//! Every path is validated before git is spawned with the same rules as
//! [`stage`] and [`unstage`]: non-empty, rooted at `root` (relative, free of
//! parent, root and drive components), not `:`-prefixed (git pathspec magic),
//! and passed as its own argument after the `--` separator — so option-like
//! names such as `--help` or `-f` are restored literally — while an empty
//! `paths` slice is rejected with [`io::ErrorKind::InvalidInput`].
//!
//! ## Dangerous-operation boundary (restore)
//!
//! `restore` is destructive and irreversible: worktree edits to the named
//! paths are overwritten from the index and cannot be recovered (unless they
//! were stashed or committed first). In particular:
//!
//! - A `.` pathspec restores every tracked path under `root` — it discards
//!   *all* unstaged changes under `root` in one call (and, from a nested
//!   `root`, nothing outside it).
//! - A path deleted in the worktree (an unstaged deletion) is re-created from
//!   the index; content that only exists in the worktree (untracked files) is
//!   never affected by this call.
//! - Staged changes are never discarded: the worktree is rewritten from the
//!   index, so a path with staged modifications keeps them.
//!
//! [`stash_push(root, message)`](stash_push) moves the repository's tracked
//! changes (staged and unstaged) into a new stash entry with
//! `git -C <root> stash push [--message=<message>]` and returns the fresh
//! [`GitStatusSnapshot`] from [`status`]. The optional message follows the
//! [`commit`] rules — non-empty after trimming and free of NUL bytes,
//! otherwise [`io::ErrorKind::InvalidInput`] before git is spawned — and
//! travels as a single `--message=<message>` argument, so it can never be
//! parsed as an option even when it looks like one. `git stash push` with
//! nothing to stash is a no-op that exits 0 ("No local changes to save"), so
//! the returned snapshot simply reflects the unchanged repository.
//!
//! [`stash_list(root)`](stash_list) lists the repository's stash entries with
//! `git -C <root> stash list` and returns a typed, stably sorted
//! [`GitStashSnapshot`] of [`GitStashEntry`]s — each entry carries the index
//! of its `stash@{N}` ref (0 is the most recent) and its description (the
//! text after the `stash@{N}: ` prefix: the recorded message for entries
//! created by [`stash_push`], otherwise git's default
//! `WIP on <branch>: <abbrev> <subject>` line). Malformed lines are rejected
//! with [`io::ErrorKind::InvalidData`].
//!
//! [`stash_apply(root, index)`](stash_apply) re-applies entry `stash@{index}`
//! to the worktree with `git -C <root> stash apply stash@{index}`: the ref is
//! formatted from the caller-supplied index into a single argument, so it can
//! never be parsed as an option, and the index is validated against a fresh
//! [`stash_list`] before git is spawned — an out-of-range index is
//! [`io::ErrorKind::InvalidInput`] instead of a git error. A successful apply
//! *keeps* the entry (unlike `git stash pop`), and plain `git stash apply`
//! (without `--index`) restores the changes into the worktree without
//! reinstating the original staged/unstaged split. It returns the fresh
//! [`GitStatusSnapshot`] from [`status`].
//!
//! [`stash_drop(root, index)`](stash_drop) deletes entry `stash@{index}` with
//! `git -C <root> stash drop stash@{index}`, validated exactly like
//! [`stash_apply`]; the remaining entries are renumbered by git. It returns
//! the fresh [`GitStatusSnapshot`] from [`status`].
//!
//! [`stash_pop(root, index)`](stash_pop) applies entry `stash@{index}` with
//! `git -C <root> stash pop stash@{index}` and drops it on success: the ref is
//! formatted into a single argument exactly as in [`stash_apply`], and the
//! index is validated the same way. Git only drops the entry *after* a
//! successful apply, so a pop that conflicts with the current worktree fails
//! with [`io::ErrorKind::Other`] carrying git's message and leaves the entry
//! in place (with conflict markers in the worktree) — a failed pop never
//! silently loses an entry.
//!
//! ## Dangerous-operation boundary (stash)
//!
//! `stash_push` rewrites the worktree of the *whole repository*, not just
//! `root`: `git stash push` without pathspecs stashes every tracked change
//! wherever it lives relative to the repository top level, so calling it from
//! a nested `root` also stashes changes outside `root`. Untracked files are
//! never stashed (no `-u`/`-a`), so a repository with only untracked changes
//! is a no-op too. [`stash_apply`] restores an entry's changes with git's
//! merge semantics: an apply that conflicts with the current worktree leaves
//! conflict markers and fails with [`io::ErrorKind::Other`] carrying git's
//! message, and — like `stash_push` — it is scoped to the whole repository,
//! not to `root`. [`stash_drop`] permanently deletes an entry; the stashed
//! changes are gone unless they were applied or committed first. [`stash_pop`]
//! is apply-plus-drop with git's own safety guarantee: the entry is dropped
//! only after a successful apply, so a failed pop never silently deletes an
//! entry.

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
    read_head_commit("git", root)
}

/// Replaces the current HEAD commit — with the staged changes, or just the
/// new message when nothing is staged — and returns its hash and summary.
///
/// Runs `git -C <root> commit --amend --message=<message>`: no shell, and
/// the message is a single `--message=<message>` argument exactly as in
/// [`commit`], so it cannot be parsed as an option even when it looks like
/// one (for example `--amend`). Message validation and the returned
/// [`GitCommitResult`] (the fresh HEAD hash via `git rev-parse HEAD` and the
/// git-defined subject via `git log -1 --format=%s`) are shared with
/// [`commit`].
///
/// Amending rewrites the last commit in place: its parent and tree are
/// reused (staged changes are folded in), but the commit's hash always
/// changes, so this is a history rewrite that must only be applied to
/// commits that have not been published.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `message` is empty or whitespace-only
///   or contains a NUL byte.
/// - [`io::ErrorKind::Other`] if `git commit --amend` exits non-zero (for
///   example a repository with no commits yet — "You have nothing to amend."
///   — or a failing commit hook); git's message is included.
pub fn amend(root: &Path, message: &str) -> io::Result<GitCommitResult> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_commit_message(message)?;
    run_git_amend("git", root, message)?;
    read_head_commit("git", root)
}

/// Runs `program -C root commit --message=<message>`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_commit(program: &str, root: &Path, message: &str) -> io::Result<()> {
    run_git_commit_with(program, root, message, &[], "commit")
}

/// Runs `program -C root commit --amend --message=<message>`; split out so
/// tests can exercise the spawn-failure path with a program that does not
/// exist.
fn run_git_amend(program: &str, root: &Path, message: &str) -> io::Result<()> {
    run_git_commit_with(program, root, message, &["--amend"], "commit --amend")
}

/// Runs `program -C root commit [<extra>...] --message=<message>`; the shared
/// runner behind [`run_git_commit`] and [`run_git_amend`].
fn run_git_commit_with(
    program: &str,
    root: &Path,
    message: &str,
    extra: &[&str],
    label: &str,
) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("commit")
        .args(extra)
        .arg(format!("--message={message}"))
        .output()?;
    ensure_git_success(&output, label)
}

/// Reads back the current HEAD commit's full hash (`git rev-parse HEAD`) and
/// git-defined subject (`git log -1 --format=%s`) into a
/// [`GitCommitResult`]; shared by [`commit`] and [`amend`].
fn read_head_commit(program: &str, root: &Path) -> io::Result<GitCommitResult> {
    let hash = run_git_capture(program, root, &["rev-parse", "HEAD"])?;
    let hash = String::from_utf8(hash).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git rev-parse output is not valid UTF-8",
        )
    })?;
    let summary = run_git_capture(program, root, &["log", "-1", "--format=%s"])?;
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

/// Rejects empty or whitespace-only messages and messages containing NUL
/// bytes (which cannot be passed as an argument) before git is spawned.
/// `what` names the operation in error messages.
fn check_message(message: &str, what: &str) -> io::Result<()> {
    if message.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} must not be empty"),
        ));
    }
    if message.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} must not contain NUL bytes"),
        ));
    }
    Ok(())
}

/// Rejects empty or whitespace-only commit messages and messages containing
/// NUL bytes (which cannot be passed as an argument) before git is spawned.
fn check_commit_message(message: &str) -> io::Result<()> {
    check_message(message, "commit message")
}

/// Rejects empty or whitespace-only stash messages and messages containing
/// NUL bytes (which cannot be passed as an argument) before git is spawned.
fn check_stash_message(message: &str) -> io::Result<()> {
    check_message(message, "stash message")
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
// Git history query
// ---------------------------------------------------------------------------

/// One commit of the repository's history, as parsed from `git log`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitEntry {
    /// The commit's full object id (40 hex digits for SHA-1 repositories, 64
    /// for SHA-256).
    pub hash: String,
    /// The parent hashes in git's order: empty for a root commit, one hash
    /// per parent for a merge commit.
    pub parents: Vec<String>,
    /// The author name.
    pub author: String,
    /// The author date in strict ISO 8601 format (git's `%aI`), for example
    /// `2026-08-29T13:57:43+08:00`.
    pub date: String,
    /// The commit subject as git defines it (`%s`): the first paragraph of
    /// the message, with line breaks collapsed to spaces. May be empty for an
    /// `--allow-empty-message` commit.
    pub subject: String,
}

/// The parsed result of [`log`]: the repository's commit history in git's own
/// output order — newest commit first (git log's default reverse-chronological
/// order) — never re-sorted.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitLogSnapshot {
    /// The entries in git's output order. An empty vec means the query
    /// returned no commits (only possible with a zero `limit`, since git
    /// itself fails on a repository without any commits).
    pub entries: Vec<GitCommitEntry>,
}

impl GitLogSnapshot {
    /// The number of commits in the snapshot.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshot has no commits.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Lists the repository's commit history and returns the typed snapshot.
///
/// Runs `git -C <root> log --format=%H%x00%P%x00%an%x00%aI%x00%s
/// --date=iso-strict`: no shell, and the root is a single argument. Every
/// commit becomes one NUL-separated record of hash, parents, author, date and
/// subject, so no human-format parsing is involved. Entries keep git's output
/// order (newest commit first) and are never re-sorted.
///
/// `limit` caps the returned snapshot: `Some(n)` also limits the query itself
/// with `--max-count=n` (a count above git's signed limit is not passed to
/// git), `Some(0)` returns an empty snapshot without spawning git, and `None`
/// returns the whole history.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository, or the repository has no commits yet); git's
///   message is included.
/// - [`io::ErrorKind::InvalidData`] if git's output is not valid UTF-8 or
///   cannot be parsed (a record with missing or extra fields, a non-empty
///   non-hexadecimal hash or parent, or an empty author or date).
pub fn log(root: &Path, limit: Option<usize>) -> io::Result<GitLogSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    if limit == Some(0) {
        return Ok(GitLogSnapshot::default());
    }
    run_log("git", root, limit)
}

/// Runs `program -C root log --format=... --date=iso-strict` with an optional
/// `--max-count`; split out so tests can exercise the spawn-failure path with
/// a program that does not exist.
fn run_log(program: &str, root: &Path, limit: Option<usize>) -> io::Result<GitLogSnapshot> {
    let mut command = Command::new(program);
    command.arg("-C").arg(root).arg("log");
    // git's --max-count is a signed int; anything beyond it is effectively
    // unbounded, so the count is simply not passed and the parsed snapshot is
    // truncated to `limit` below.
    if let Some(count) = limit.filter(|&count| count <= i32::MAX as usize) {
        command.arg(format!("--max-count={count}"));
    }
    command.args(["--format=%H%x00%P%x00%an%x00%aI%x00%s", "--date=iso-strict"]);
    let output = command.output()?;
    ensure_git_success(&output, "log")?;
    let mut snapshot = parse_log_bytes(output.stdout)?;
    if let Some(count) = limit {
        snapshot.entries.truncate(count);
    }
    Ok(snapshot)
}

/// Converts `git log` stdout to UTF-8 and parses it; split out so the
/// non-UTF-8 path is testable without spawning git.
fn parse_log_bytes(stdout: Vec<u8>) -> io::Result<GitLogSnapshot> {
    let stdout = String::from_utf8(stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git log output is not valid UTF-8",
        )
    })?;
    parse_log(&stdout)
}

/// Parses `hash\0parents\0author\0date\0subject` lines into a snapshot,
/// preserving git's output order.
fn parse_log(output: &str) -> io::Result<GitLogSnapshot> {
    let mut entries = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        entries.push(parse_log_line(line)?);
    }
    Ok(GitLogSnapshot { entries })
}

/// Parses one NUL-separated log record into a [`GitCommitEntry`].
fn parse_log_line(line: &str) -> io::Result<GitCommitEntry> {
    let mut fields = line.split('\0');
    let hash = fields
        .next()
        .ok_or_else(|| malformed("log record missing hash"))?;
    let parents = fields
        .next()
        .ok_or_else(|| malformed("log record missing parents"))?;
    let author = fields
        .next()
        .ok_or_else(|| malformed("log record missing author"))?;
    let date = fields
        .next()
        .ok_or_else(|| malformed("log record missing date"))?;
    let subject = fields
        .next()
        .ok_or_else(|| malformed("log record missing subject"))?;
    if fields.next().is_some() {
        return Err(malformed("log record has more than five fields"));
    }
    check_hash(hash, "commit hash")?;
    let parents = parse_parents(parents)?;
    if author.is_empty() {
        return Err(malformed("log record has an empty author"));
    }
    if date.is_empty() {
        return Err(malformed("log record has an empty date"));
    }
    Ok(GitCommitEntry {
        hash: hash.to_string(),
        parents,
        author: author.to_string(),
        date: date.to_string(),
        subject: subject.to_string(),
    })
}

/// Parses the `%P` field of a log record: space-separated parent hashes,
/// empty for a root commit.
fn parse_parents(field: &str) -> io::Result<Vec<String>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    let mut parents = Vec::new();
    for parent in field.split(' ') {
        check_hash(parent, "parent hash")?;
        parents.push(parent.to_string());
    }
    Ok(parents)
}

/// Rejects empty or non-hexadecimal hashes from git's output. `what` names
/// the hash in error messages.
fn check_hash(hash: &str, what: &str) -> io::Result<()> {
    if hash.is_empty() || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(malformed(&format!("{what} is not hexadecimal")));
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Git tags
// ---------------------------------------------------------------------------

/// One tag of a repository, as `git for-each-ref refs/tags` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTag {
    /// The tag's short name — the part after `refs/tags/`, for example
    /// `v1.0.0`.
    pub name: String,
    /// The full object id git reports for the tag. For a lightweight tag
    /// this is the id of the object the tag names (typically a commit); for
    /// an annotated tag it is the id of the tag object itself — recorded
    /// without expanding the tag to its target.
    pub object: String,
}

/// The parsed result of [`tags`]: every tag of the repository, stably sorted
/// by name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitTagSnapshot {
    /// The tags, stably sorted by [`GitTag::name`] in byte-wise order. An
    /// empty vec means the repository has no tags.
    pub tags: Vec<GitTag>,
}

impl GitTagSnapshot {
    /// The number of tags.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Whether the repository has no tags.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

/// Lists the repository's tags and returns the stably sorted snapshot.
///
/// Runs `git -C <root> for-each-ref --format=%(objectname)%00%(refname:short)
/// refs/tags`: no shell, and the format is git's own machine-oriented output
/// — NUL-separated fields with newline-terminated records — so no
/// human-format parsing is involved. A lightweight tag carries the id of the
/// object it names; an annotated tag is recorded with the id of the tag
/// object itself and is never expanded to its target.
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
///   cannot be parsed, or reports an object or a name that fails validation.
pub fn tags(root: &Path) -> io::Result<GitTagSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    run_tags("git", root)
}

/// Runs `program -C root for-each-ref --format=%(objectname)%00%(refname:short)
/// refs/tags`; split out so tests can exercise the spawn-failure path with a
/// program that does not exist.
fn run_tags(program: &str, root: &Path) -> io::Result<GitTagSnapshot> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args([
            "for-each-ref",
            "--format=%(objectname)%00%(refname:short)",
            "refs/tags",
        ])
        .output()?;
    ensure_git_success(&output, "for-each-ref")?;
    parse_tags_bytes(output.stdout)
}

/// Converts `git for-each-ref` stdout to UTF-8 and parses it; split out so
/// the non-UTF-8 path is testable without spawning git.
fn parse_tags_bytes(stdout: Vec<u8>) -> io::Result<GitTagSnapshot> {
    let stdout = String::from_utf8(stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git for-each-ref output is not valid UTF-8",
        )
    })?;
    parse_tags(&stdout)
}

/// Parses `object\0name` lines into a snapshot, stably sorted by name.
fn parse_tags(output: &str) -> io::Result<GitTagSnapshot> {
    let mut tags = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        tags.push(parse_tag_line(line)?);
    }
    tags.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(GitTagSnapshot { tags })
}

/// Parses one `object\0name` record into a [`GitTag`]: the object must be a
/// non-empty hexadecimal id and the name must be non-empty and safe.
fn parse_tag_line(line: &str) -> io::Result<GitTag> {
    let mut fields = line.split('\0');
    let object = fields
        .next()
        .ok_or_else(|| malformed("tag record missing object"))?;
    let name = fields
        .next()
        .ok_or_else(|| malformed("tag record missing name"))?;
    if fields.next().is_some() {
        return Err(malformed("tag record has more than two fields"));
    }
    check_hash(object, "tag object")?;
    check_safe_path(name)?;
    Ok(GitTag {
        name: name.to_string(),
        object: object.to_string(),
    })
}

/// Creates a lightweight tag and returns the resulting [`GitTagSnapshot`]
/// from [`tags`].
///
/// Runs `git -C <root> tag -- <name>`: no shell, the name is a single
/// argument, and the `--` separator shields it from option parsing. The
/// name is validated before git is spawned with the strict rules of
/// [`check_tag_name`], so a name that passes validation is always a legal
/// ref under `refs/tags/`. The operation is local to the repository: the
/// worktree and the index are never touched.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `name` fails [`check_tag_name`].
/// - [`io::ErrorKind::Other`] if `git tag` exits non-zero (for example the
///   tag already exists, `root` is not inside a Git repository, or HEAD is
///   unborn); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`tags`] output cannot be
///   parsed or reports an object or a name that fails validation.
pub fn tag(root: &Path, name: &str) -> io::Result<GitTagSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_tag_name(name)?;
    run_git_tag("git", root, name)?;
    tags(root)
}

/// Runs `program -C root tag -- <name>`; split out so tests can exercise the
/// spawn-failure path with a program that does not exist.
fn run_git_tag(program: &str, root: &Path, name: &str) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["tag", "--"])
        .arg(name)
        .output()?;
    ensure_git_success(&output, "tag")
}

/// Deletes a tag and returns the resulting [`GitTagSnapshot`] from [`tags`].
///
/// Runs `git -C <root> tag --delete <name>`: no shell, and the name is a
/// single argument — validated with [`check_tag_name`] before git is
/// spawned, so it can never be parsed as an option. The operation is local
/// to the repository and never touches the worktree or the index.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `name` fails [`check_tag_name`].
/// - [`io::ErrorKind::Other`] if `git tag --delete` exits non-zero (for
///   example the tag does not exist or `root` is not inside a Git
///   repository); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`tags`] output cannot be
///   parsed or reports an object or a name that fails validation.
pub fn delete_tag(root: &Path, name: &str) -> io::Result<GitTagSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_tag_name(name)?;
    run_git_delete_tag("git", root, name)?;
    tags(root)
}

/// Runs `program -C root tag --delete <name>`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_delete_tag(program: &str, root: &Path, name: &str) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("tag")
        .arg("--delete")
        .arg(name)
        .output()?;
    ensure_git_success(&output, "tag --delete")
}

/// Validates a caller-supplied tag name before git is spawned: it must be
/// non-empty, free of NUL bytes, not `-`-prefixed (so git never parses it as
/// an option), and relative — no leading `/`, `\` or drive prefix, and no
/// `..` anywhere — with no empty slash-separated components. It must also be
/// free of ASCII control characters (including space and DEL) and of git's
/// other ref-forbidden characters (`~`, `^`, `:`, `?`, `*`, `[`, `\`,
/// `@{`), and no component may start with `.` or end with `.lock`, mirroring
/// `git check-ref-format`. One-level names such as `v1.2` are allowed: the
/// tag lives under `refs/tags/`, so the "must contain a slash" rule does not
/// apply.
fn check_tag_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tag name must not be empty",
        ));
    }
    if name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tag name must not contain NUL bytes",
        ));
    }
    if name.starts_with('-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tag name must not start with '-': {name:?}"),
        ));
    }
    let bytes = name.as_bytes();
    let looks_absolute = bytes[0] == b'/'
        || bytes[0] == b'\\'
        || bytes.starts_with(b"//")
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic());
    let escapes = Path::new(name).is_absolute()
        || looks_absolute
        || Path::new(name)
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if escapes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tag name must be relative and free of '..': {name:?}"),
        ));
    }
    for &byte in bytes {
        if byte <= b' ' || byte == 0x7F {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("tag name must not contain control characters or whitespace: {name:?}"),
            ));
        }
        if matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "tag name must not contain git-ref-forbidden character '{}': {name:?}",
                    byte as char
                ),
            ));
        }
    }
    if name.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tag name must not contain '..': {name:?}"),
        ));
    }
    if name == "@" || name.contains("@{") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tag name must not be '@' or contain '@{{': {name:?}"),
        ));
    }
    for component in name.split('/') {
        if component.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("tag name must not contain empty components: {name:?}"),
            ));
        }
        if component.starts_with('.') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("tag name components must not start with '.': {name:?}"),
            ));
        }
        if component.ends_with(".lock") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("tag name components must not end with '.lock': {name:?}"),
            ));
        }
    }
    if name.ends_with('.') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tag name must not end with '.': {name:?}"),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git remotes
// ---------------------------------------------------------------------------

/// One remote of a repository, as `git remote -v` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRemote {
    /// The remote's name — the part before the tab, for example `origin`.
    pub name: String,
    /// The URL git fetches from, from the remote's `(fetch)` line (the
    /// `remote.<name>.url` value). `None` when the remote has no fetch URL
    /// configured — git then prints a bare `name<TAB>` line.
    pub fetch_url: Option<String>,
    /// The URLs git pushes to, from the remote's `(push)` lines in git's
    /// order. A remote without an explicit `remote.<name>.pushurl` reports
    /// its fetch URL as the single push URL; a multi-valued `pushurl` yields
    /// one entry per value.
    pub push_urls: Vec<String>,
}

/// The parsed result of [`remotes`]: every remote of the repository, stably
/// sorted by name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitRemoteSnapshot {
    /// The remotes, stably sorted by [`GitRemote::name`] in byte-wise order.
    /// An empty vec means the repository has no configured remotes.
    pub remotes: Vec<GitRemote>,
}

impl GitRemoteSnapshot {
    /// The number of remotes.
    pub fn len(&self) -> usize {
        self.remotes.len()
    }

    /// Whether the repository has no configured remotes.
    pub fn is_empty(&self) -> bool {
        self.remotes.is_empty()
    }
}

/// Lists the repository's configured remotes and returns the stably sorted
/// snapshot.
///
/// Runs `git -C <root> remote -v`: no shell, and the query is strictly
/// read-only — it only reads the local Git configuration, never contacts a
/// network and never performs a fetch or push. Every line is git's own
/// machine-oriented `name<TAB>url (fetch|push)` output (or the bare
/// `name<TAB>` form for a remote without a fetch URL), so no human-format
/// parsing is involved.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository); git's stderr is included in the message.
/// - [`io::ErrorKind::InvalidData`] if git's output is not valid UTF-8 or
///   cannot be parsed (a malformed line, a duplicate fetch line, an empty
///   URL, or a name that fails validation).
pub fn remotes(root: &Path) -> io::Result<GitRemoteSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    run_remotes("git", root)
}

/// Runs `program -C root remote -v`; split out so tests can exercise the
/// spawn-failure path with a program that does not exist.
fn run_remotes(program: &str, root: &Path) -> io::Result<GitRemoteSnapshot> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["remote", "-v"])
        .output()?;
    ensure_git_success(&output, "remote -v")?;
    parse_remotes_bytes(output.stdout)
}

/// Converts `git remote -v` stdout to UTF-8 and parses it; split out so the
/// non-UTF-8 path is testable without spawning git.
fn parse_remotes_bytes(stdout: Vec<u8>) -> io::Result<GitRemoteSnapshot> {
    let stdout = String::from_utf8(stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git remote -v output is not valid UTF-8",
        )
    })?;
    parse_remotes(&stdout)
}

/// Parses `name<TAB>url (fetch|push)` lines (or bare `name<TAB>` lines for
/// remotes without a fetch URL) into a snapshot, stably sorted by name.
fn parse_remotes(output: &str) -> io::Result<GitRemoteSnapshot> {
    let mut by_name: Vec<RemoteBuilder> = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (name, rest) = line
            .split_once('\t')
            .ok_or_else(|| malformed("remote line must be name<TAB>url (fetch|push)"))?;
        check_remote_name(name)?;
        let index = match by_name.iter().position(|builder| builder.name == name) {
            Some(index) => index,
            None => {
                by_name.push(RemoteBuilder::new(name.to_string()));
                by_name.len() - 1
            }
        };
        by_name[index].push_line(rest)?;
    }
    let mut remotes: Vec<GitRemote> = by_name.into_iter().map(RemoteBuilder::finish).collect();
    remotes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(GitRemoteSnapshot { remotes })
}

/// How much of a remote's fetch URL has been seen while its lines are being
/// parsed.
#[derive(PartialEq, Eq)]
enum FetchState {
    /// No fetch line seen yet.
    Unset,
    /// A bare `name<TAB>` line: the remote has no fetch URL.
    Bare,
    /// A `name<TAB>url (fetch)` line.
    Set(String),
}

/// Accumulates one remote's lines while `git remote -v` output is parsed.
struct RemoteBuilder {
    name: String,
    fetch: FetchState,
    push_urls: Vec<String>,
}

impl RemoteBuilder {
    fn new(name: String) -> Self {
        Self {
            name,
            fetch: FetchState::Unset,
            push_urls: Vec::new(),
        }
    }

    /// Consumes one line's tab-free remainder: `url (fetch)`, `url (push)`
    /// or the empty remainder of a bare fetch line.
    fn push_line(&mut self, rest: &str) -> io::Result<()> {
        if let Some(url) = rest.strip_suffix(" (fetch)") {
            if self.fetch != FetchState::Unset {
                return Err(malformed("remote has more than one fetch line"));
            }
            if url.is_empty() {
                return Err(malformed("remote fetch URL is empty"));
            }
            self.fetch = FetchState::Set(url.to_string());
        } else if let Some(url) = rest.strip_suffix(" (push)") {
            if url.is_empty() {
                return Err(malformed("remote push URL is empty"));
            }
            self.push_urls.push(url.to_string());
        } else if rest.is_empty() {
            // git's bare `name<TAB>` line for a remote without a fetch URL.
            if self.fetch != FetchState::Unset {
                return Err(malformed("remote fetch line contradicts its URL line"));
            }
            self.fetch = FetchState::Bare;
        } else {
            return Err(malformed(
                "remote line must end in \" (fetch)\", \" (push)\", or be bare",
            ));
        }
        Ok(())
    }

    /// Builds the finished [`GitRemote`].
    fn finish(self) -> GitRemote {
        let fetch_url = match self.fetch {
            FetchState::Set(url) => Some(url),
            FetchState::Unset | FetchState::Bare => None,
        };
        GitRemote {
            name: self.name,
            fetch_url,
            push_urls: self.push_urls,
        }
    }
}

/// Validates a remote name parsed from `git remote -v` output: it must be
/// non-empty, free of whitespace, and pass the same escape-rejecting check
/// as status paths (git's own remote names are restricted ref-like names, so
/// anything else means corrupt configuration).
fn check_remote_name(name: &str) -> io::Result<()> {
    check_safe_path(name)?;
    if name.chars().any(char::is_whitespace) {
        return Err(malformed("remote name contains whitespace"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git restore and stash
// ---------------------------------------------------------------------------

/// Discards the given paths' worktree changes and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> restore -- <paths>`: no shell, and every path is
/// passed as its own argument after the `--` separator, so option-like names
/// such as `--help` or `-f` are restored literally. Paths are validated
/// before git is spawned with the same rules as [`stage`] and [`unstage`]:
/// non-empty, rooted at `root` (relative and free of parent, root and drive
/// components) and not `:`-prefixed (git pathspec magic); an empty `paths`
/// slice is rejected the same way.
///
/// `git restore` writes the *worktree* from the index by default: staged
/// changes are kept, and untracked files are never deleted — see the
/// "Dangerous-operation boundary (restore)" section in the module docs.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `paths` is empty or contains a path
///   that is not rooted at `root` or starts with `:`.
/// - [`io::ErrorKind::Other`] if `git restore` exits non-zero (for example a
///   pathspec that matches no tracked file — such as an untracked or
///   non-existent path — or an unmerged path); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn restore(root: &Path, paths: &[&Path]) -> io::Result<GitStatusSnapshot> {
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
            "at least one path must be restored",
        ));
    }
    for path in paths {
        check_restore_path(path)?;
    }
    run_git_restore("git", root, paths)?;
    status(root)
}

/// Runs `program -C root restore -- <paths>`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_restore(program: &str, root: &Path, paths: &[&Path]) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .arg("restore")
        .arg("--")
        .args(paths)
        .output()?;
    ensure_git_success(&output, "restore")
}

/// Validates a caller-supplied restore path: see [`check_index_path`].
fn check_restore_path(path: &Path) -> io::Result<()> {
    check_index_path(path, "restore path")
}

/// Stashes the repository's tracked changes and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> stash push [--message=<message>]`: no shell. The
/// optional message is a single `--message=<message>` argument, so it can
/// never be parsed as an option even when it looks like one; it must be
/// non-empty after trimming and free of NUL bytes, or the call fails with
/// [`io::ErrorKind::InvalidInput`] before git is spawned. Untracked files are
/// never stashed, and a repository with no tracked changes is a no-op (git
/// exits 0 with "No local changes to save"), so the returned snapshot always
/// reflects the resulting state.
///
/// The stash is scoped to the whole repository, not to `root` — see the
/// "Dangerous-operation boundary (stash)" section in the module docs.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `message` is empty or whitespace-only
///   or contains a NUL byte.
/// - [`io::ErrorKind::Other`] if `git stash push` exits non-zero (for example
///   `root` is not inside a Git repository, or the repository has no commits
///   yet); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn stash_push(root: &Path, message: Option<&str>) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    if let Some(message) = message {
        check_stash_message(message)?;
    }
    run_git_stash_push("git", root, message)?;
    status(root)
}

/// Runs `program -C root stash push [--message=<message>]`; split out so
/// tests can exercise the spawn-failure path with a program that does not
/// exist.
fn run_git_stash_push(program: &str, root: &Path, message: Option<&str>) -> io::Result<()> {
    let mut command = Command::new(program);
    command.arg("-C").arg(root).arg("stash").arg("push");
    if let Some(message) = message {
        command.arg(format!("--message={message}"));
    }
    let output = command.output()?;
    ensure_git_success(&output, "stash push")
}

// ---------------------------------------------------------------------------
// Git stash list, apply, pop and drop
// ---------------------------------------------------------------------------

/// One stash entry of a repository, as `git stash list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStashEntry {
    /// The entry's stash ref number: `stash@{index}`, where 0 is the most
    /// recent entry.
    pub index: usize,
    /// The entry's description — the text after the `stash@{N}: ` prefix. For
    /// entries created by [`stash_push`] this is the recorded message; for
    /// externally created entries it is git's default
    /// `WIP on <branch>: <abbrev> <subject>` line.
    pub description: String,
}

/// The parsed result of [`stash_list`]: every stash entry, stably sorted by
/// index ascending.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitStashSnapshot {
    /// The entries, stably sorted by [`GitStashEntry::index`] in ascending
    /// order. An empty vec means the repository has no stash entries.
    pub entries: Vec<GitStashEntry>,
}

impl GitStashSnapshot {
    /// The number of stash entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the repository has no stash entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Lists the repository's stash entries and returns the stably sorted
/// snapshot.
///
/// Runs `git -C <root> stash list`: no shell, and every parsed line is git's
/// own machine-oriented `stash@{N}: <description>` output, so no human-format
/// parsing is involved. Entries are stably sorted by index ascending, which
/// is also git's own output order.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::Other`] if `git` exits non-zero (for example `root` is
///   not inside a Git repository); git's stderr is included in the message.
/// - [`io::ErrorKind::InvalidData`] if git's output is not valid UTF-8 or
///   cannot be parsed as `stash@{N}: <description>` lines.
pub fn stash_list(root: &Path) -> io::Result<GitStashSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    run_stash_list("git", root)
}

/// Runs `program -C root stash list`; split out so tests can exercise the
/// spawn-failure path with a program that does not exist.
fn run_stash_list(program: &str, root: &Path) -> io::Result<GitStashSnapshot> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["stash", "list"])
        .output()?;
    ensure_git_success(&output, "stash list")?;
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "git stash list output is not valid UTF-8",
        )
    })?;
    parse_stash_list(&stdout)
}

/// Parses `stash@{N}: <description>` lines into a snapshot, stably sorted by
/// index ascending.
fn parse_stash_list(output: &str) -> io::Result<GitStashSnapshot> {
    let mut entries = Vec::new();
    for line in output.lines() {
        // Defensive CRLF tolerance; git itself always emits `\n`.
        let line = line.strip_suffix('\r').unwrap_or(line);
        entries.push(parse_stash_line(line)?);
    }
    entries.sort_by_key(|entry| entry.index);
    Ok(GitStashSnapshot { entries })
}

/// Parses one `stash@{N}: <description>` line. The description may be empty
/// (an externally created stash with no subject), but the index must be a
/// number and the `}:` separator must be present.
fn parse_stash_line(line: &str) -> io::Result<GitStashEntry> {
    let rest = line
        .strip_prefix("stash@{")
        .ok_or_else(|| malformed("stash line must start with \"stash@{\""))?;
    let (index, rest) = rest
        .split_once('}')
        .ok_or_else(|| malformed("stash line must contain a closing brace"))?;
    if index.is_empty() {
        return Err(malformed("stash line has an empty index"));
    }
    let index = index
        .parse::<usize>()
        .map_err(|_| malformed("stash index is not a number"))?;
    let description = rest
        .strip_prefix(": ")
        .or_else(|| rest.strip_prefix(':'))
        .ok_or_else(|| malformed("stash line must separate index and description with ':'"))?;
    Ok(GitStashEntry {
        index,
        description: description.to_string(),
    })
}

/// Applies the stash entry `stash@{index}` to the worktree and returns the
/// resulting [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> stash apply stash@{index}`: no shell, and the ref is
/// formatted from the caller-supplied `index` into a single argument, so it
/// can never be parsed as an option. Before git is spawned the index is
/// validated against a fresh [`stash_list`]: it must name an existing entry,
/// otherwise the call fails with [`io::ErrorKind::InvalidInput`]. A
/// successful apply *keeps* the entry — unlike [`stash_pop`] — and plain
/// `git stash apply` (without `--index`) restores the changes into the
/// worktree, so the original staged/unstaged split is not reinstated. See the
/// "Dangerous-operation boundary (stash)" section in the module docs.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `index` does not name an existing
///   stash entry.
/// - [`io::ErrorKind::Other`] if `git stash apply` exits non-zero (for
///   example an apply that conflicts with the current worktree, a refusal to
///   overwrite local changes, or a `root` that is not inside a Git
///   repository); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn stash_apply(root: &Path, index: usize) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_stash_index(root, index, "stash apply")?;
    run_git_stash_apply("git", root, index)?;
    status(root)
}

/// Runs `program -C root stash apply stash@{index}`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_stash_apply(program: &str, root: &Path, index: usize) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["stash", "apply"])
        .arg(format!("stash@{{{index}}}"))
        .output()?;
    ensure_git_success(&output, "stash apply")
}

/// Deletes the stash entry `stash@{index}` and returns the resulting
/// [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> stash drop stash@{index}`: no shell, and the ref is a
/// single argument exactly as in [`stash_apply`]. The index is validated
/// against a fresh [`stash_list`] before git is spawned: it must name an
/// existing entry, otherwise the call fails with
/// [`io::ErrorKind::InvalidInput`]. The remaining entries are renumbered by
/// git. See the "Dangerous-operation boundary (stash)" section in the module
/// docs.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `index` does not name an existing
///   stash entry.
/// - [`io::ErrorKind::Other`] if `git stash drop` exits non-zero (for example
///   `root` is not inside a Git repository); git's message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn stash_drop(root: &Path, index: usize) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_stash_index(root, index, "stash drop")?;
    run_git_stash_drop("git", root, index)?;
    status(root)
}

/// Runs `program -C root stash drop stash@{index}`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_stash_drop(program: &str, root: &Path, index: usize) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["stash", "drop"])
        .arg(format!("stash@{{{index}}}"))
        .output()?;
    ensure_git_success(&output, "stash drop")
}

/// Applies the stash entry `stash@{index}` to the worktree, drops it on
/// success, and returns the resulting [`GitStatusSnapshot`] from [`status`].
///
/// Runs `git -C <root> stash pop stash@{index}`: no shell, and the ref is
/// formatted from the caller-supplied `index` into a single argument, so it
/// can never be parsed as an option. Before git is spawned the index is
/// validated against a fresh [`stash_list`] exactly like [`stash_apply`] and
/// [`stash_drop`]: it must name an existing entry, otherwise the call fails
/// with [`io::ErrorKind::InvalidInput`].
///
/// `git stash pop` is apply-plus-drop with git's own safety guarantee: the
/// entry is dropped only *after* a successful apply. A pop that conflicts with
/// the current worktree fails with [`io::ErrorKind::Other`] carrying git's
/// message and leaves the stash entry in place (with conflict markers in the
/// worktree), so a failed pop never silently deletes an entry. On success the
/// entry is dropped (git renumbers the rest) and the changes are restored into
/// the worktree; as with plain [`stash_apply`], the original staged/unstaged
/// split is not reinstated.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] if `root` does not exist or `git` is not on
///   `PATH`.
/// - [`io::ErrorKind::NotADirectory`] if `root` exists but is not a
///   directory.
/// - [`io::ErrorKind::InvalidInput`] if `index` does not name an existing
///   stash entry.
/// - [`io::ErrorKind::Other`] if `git stash pop` exits non-zero (for example
///   a pop that conflicts with the current worktree, a refusal to overwrite
///   local changes, or a `root` that is not inside a Git repository); git's
///   message is included.
/// - [`io::ErrorKind::InvalidData`] if the fresh [`status`] output cannot be
///   parsed or reports a path that escapes the repository root.
pub fn stash_pop(root: &Path, index: usize) -> io::Result<GitStatusSnapshot> {
    let metadata = std::fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    check_stash_index(root, index, "stash pop")?;
    run_git_stash_pop("git", root, index)?;
    status(root)
}

/// Runs `program -C root stash pop stash@{index}`; split out so tests can
/// exercise the spawn-failure path with a program that does not exist.
fn run_git_stash_pop(program: &str, root: &Path, index: usize) -> io::Result<()> {
    let output = Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["stash", "pop"])
        .arg(format!("stash@{{{index}}}"))
        .output()?;
    ensure_git_success(&output, "stash pop")
}

/// Rejects a caller-supplied stash index that does not name an existing
/// entry, using a fresh [`stash_list`], before git is spawned. `what` names
/// the operation in error messages.
fn check_stash_index(root: &Path, index: usize, what: &str) -> io::Result<()> {
    let count = stash_list(root)?.len();
    if index >= count {
        let noun = if count == 1 { "entry" } else { "entries" };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} index {index} is out of range: repository has {count} stash {noun}"),
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

    // ---- amend() argument safety (no git binary needed) ----

    #[test]
    fn amend_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-amend-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            amend(&missing, "message").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn amend_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-amend-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            amend(&file, "message").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn amend_empty_message_is_invalid_input() {
        let root = temp_dir("amendempty");
        for message in ["", "   ", "\n\n", "\t"] {
            let error = amend(&root, message).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "message {message:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_message_with_nul_is_invalid_input() {
        let root = temp_dir("amendnul");
        let error = amend(&root, "subject\0nul").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_amend_surfaces_as_not_found() {
        let root = temp_dir("amendnobinary");
        let error = run_git_amend("nextide-no-such-git-binary", &root, "message").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- amend() integration (requires a git binary) ----

    /// The full 40-character hash of the revision named by `rev` (for example
    /// `HEAD`, `HEAD^` or `HEAD^{tree}`).
    fn rev_hash(root: &Path, rev: &str) -> String {
        let bytes = run_git_capture("git", root, &["rev-parse", rev]).unwrap();
        String::from_utf8(bytes).unwrap().trim().to_string()
    }

    /// The number of commits reachable from `HEAD`.
    fn commit_count(root: &Path) -> usize {
        let bytes = run_git_capture("git", root, &["rev-list", "--count", "HEAD"]).unwrap();
        String::from_utf8(bytes).unwrap().trim().parse().unwrap()
    }

    /// `git log -1 <format>` for the current HEAD, with the trailing newline
    /// stripped.
    fn head_log(root: &Path, format: &str) -> String {
        let bytes = run_git_capture("git", root, &["log", "-1", format]).unwrap();
        String::from_utf8(bytes).unwrap().trim_end().to_string()
    }

    #[test]
    fn amend_changes_hash_and_subject_in_place() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("amendok", &[("a.txt", "a\n")]);
        let original = rev_hash(&root, "HEAD");
        assert_eq!(head_log(&root, "--format=%s"), "init");
        assert_eq!(commit_count(&root), 1);

        let result = amend(&root, "amended subject").unwrap();
        assert_eq!(result.summary, "amended subject");
        assert_eq!(result.hash.len(), 40);
        assert!(result.hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(result.hash, original, "amend must create a new commit hash");

        // The amended commit replaced the original one: same count, new
        // subject, and the returned hash is the actual HEAD.
        assert_eq!(commit_count(&root), 1);
        assert_eq!(head_log(&root, "--format=%s"), "amended subject");
        assert_eq!(rev_hash(&root, "HEAD"), result.hash);
        assert!(status(&root).unwrap().is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_keeps_parent_and_tree_when_nothing_staged() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("amendtree", &[("a.txt", "a\n")]);
        fs::write(root.join("a.txt"), "b\n").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        let second = commit(&root, "second").unwrap();
        let parent = rev_hash(&root, "HEAD^");
        let tree = rev_hash(&root, "HEAD^{tree}");

        // Amending with nothing staged rewrites only the message: the parent
        // and the tree are reused, and no commit is added.
        let result = amend(&root, "second amended").unwrap();
        assert_eq!(result.summary, "second amended");
        assert_ne!(result.hash, second.hash);
        assert_eq!(rev_hash(&root, "HEAD^"), parent);
        assert_eq!(rev_hash(&root, "HEAD^{tree}"), tree);
        assert_eq!(commit_count(&root), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_folds_staged_changes_into_the_head_commit() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("amendstage", &[("a.txt", "a\n")]);
        fs::write(root.join("a.txt"), "CHANGED\n").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        let before = rev_hash(&root, "HEAD");

        let result = amend(&root, "amend with staged change").unwrap();
        assert_eq!(result.summary, "amend with staged change");
        assert_ne!(result.hash, before);
        // The staged content is now part of the amended commit...
        assert_eq!(head_log(&root, "--format=%B"), "amend with staged change");
        let content = run_git_capture("git", &root, &["show", "HEAD:a.txt"]).unwrap();
        assert_eq!(String::from_utf8(content).unwrap(), "CHANGED\n");
        // ...and the worktree is clean afterwards.
        assert!(status(&root).unwrap().is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_treats_option_like_messages_as_message_text() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("amendopt", &[("a.txt", "a\n")]);
        // An option-like message is recorded verbatim via --message=<msg>,
        // never parsed as a git option.
        let result = amend(&root, "--amend").unwrap();
        assert_eq!(result.summary, "--amend");
        assert_eq!(head_log(&root, "--format=%s"), "--amend");
        assert_eq!(commit_count(&root), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_multiline_message_reports_git_subject() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = commit_repo("amendmulti", &[("a.txt", "a\n")]);
        let result = amend(&root, "subject line\n\nbody line\nmore body").unwrap();
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
    fn amend_nothing_to_amend_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_clean_repo("amendnothing");
        // A repository with no commits has nothing to amend.
        let error = amend(&root, "message").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("nothing to amend"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn amend_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("amendnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = amend(&root, "message").unwrap_err();
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

    // ---- restore() argument safety (no git binary needed) ----

    #[test]
    fn restore_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-restore-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            restore(&missing, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn restore_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-restore-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            restore(&file, &[Path::new("a.txt")]).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn restore_empty_paths_is_invalid_input() {
        let root = temp_dir("restoreempty");
        let error = restore(&root, &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_rejects_unrooted_and_magic_paths() {
        let root = temp_dir("restorebad");
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
            let error = restore(&root, &[path]).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "path {path:?} should be rejected"
            );
        }
        // `.` (restore everything under root) passes validation.
        check_restore_path(Path::new(".")).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_restore_surfaces_as_not_found() {
        let root = temp_dir("restorenobinary");
        let error = run_git_restore("nextide-no-such-git-binary", &root, &[Path::new("a.txt")])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- restore() integration (requires a git binary) ----

    #[test]
    fn restore_discards_worktree_modifications() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoremod", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        let snapshot = restore(&root, &[Path::new("a.txt")]).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_keeps_staged_changes() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restorestage", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "staged").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        fs::write(root.join("a.txt"), "dirty").unwrap();

        // The worktree returns to the staged content; the staged change
        // itself survives (restore only rewrites the worktree, from the
        // index).
        let snapshot = restore(&root, &[Path::new("a.txt")]).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "staged");
        let entry = find(&snapshot, "a.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Modified, GitStatusCode::Unmodified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_recreates_unstaged_deleted_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoredel", &[("a.txt", "base")]);
        fs::remove_file(root.join("a.txt")).unwrap();
        let snapshot = restore(&root, &[Path::new("a.txt")]).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_leaves_untracked_files_alone() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoreuntracked", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        fs::write(root.join("new.txt"), "untracked").unwrap();
        let snapshot = restore(&root, &[Path::new("a.txt")]).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).unwrap(),
            "untracked"
        );
        let entry = find(&snapshot, "new.txt");
        assert!(entry.state.is_untracked());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_handles_option_like_quoted_and_whitespace_paths() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo(
            "restoreopt",
            &[("--help", "help"), ("with space 中文.txt", "w")],
        );
        fs::write(root.join("--help"), "dirty-help").unwrap();
        fs::write(root.join("with space 中文.txt"), "dirty-w").unwrap();
        let snapshot = restore(
            &root,
            &[Path::new("--help"), Path::new("with space 中文.txt")],
        )
        .unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("--help")).unwrap(), "help");
        assert_eq!(
            fs::read_to_string(root.join("with space 中文.txt")).unwrap(),
            "w"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_with_dot_restores_the_whole_root() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoredot", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        fs::write(root.join("a.txt"), "A").unwrap();
        fs::write(root.join("sub/b.txt"), "B").unwrap();
        let snapshot = restore(&root, &[Path::new(".")]).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("sub/b.txt")).unwrap(), "b");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_from_nested_root_resolves_paths_relative_to_root() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restorenested", &[("sub/a.txt", "a"), ("sub/b.txt", "b")]);
        fs::write(root.join("sub/a.txt"), "A").unwrap();
        fs::write(root.join("sub/b.txt"), "B").unwrap();

        // The pathspec is resolved relative to `root` (the nested directory).
        let snapshot = restore(&root.join("sub"), &[Path::new("a.txt")]).unwrap();
        assert_eq!(fs::read_to_string(root.join("sub/a.txt")).unwrap(), "a");
        // The sibling change was not restored and still reports relative to
        // the repository top level.
        let b = find(&snapshot, "sub/b.txt");
        assert_eq!(
            b.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_nonexistent_path_propagates_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoremiss", &[("a.txt", "a")]);
        let error = restore(&root, &[Path::new("no-such-file.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("did not match any file(s)"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_untracked_path_propagates_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("restoreut", &[("a.txt", "a")]);
        fs::write(root.join("new.txt"), "untracked").unwrap();
        // An untracked path is not "known to git" for a restore, so git
        // fails; the file itself is untouched.
        let error = restore(&root, &[Path::new("new.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("did not match any file(s)"),
            "message was: {message}"
        );
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).unwrap(),
            "untracked"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("restorenonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = restore(&root, &[Path::new("a.txt")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_push() argument safety (no git binary needed) ----

    #[test]
    fn stash_push_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stash-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stash_push(&missing, None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stash_push_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-stash-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stash_push(&file, None).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn stash_push_rejects_bad_messages() {
        let root = temp_dir("stashbadmsg");
        for message in ["", "   ", "\n\n", "\t", "has\0nul"] {
            let error = stash_push(&root, Some(message)).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "message {message:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_stash_push_surfaces_as_not_found() {
        let root = temp_dir("stashnobinary");
        let error = run_git_stash_push("nextide-no-such-git-binary", &root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_push() integration (requires a git binary) ----

    /// The number of stash entries, i.e. `git stash list` lines.
    fn stash_count(root: &Path) -> usize {
        let bytes = run_git_capture("git", root, &["stash", "list"]).unwrap();
        String::from_utf8(bytes).unwrap().lines().count()
    }

    #[test]
    fn stash_push_moves_tracked_changes_and_keeps_untracked_files() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashpush", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        fs::write(root.join("new.txt"), "untracked").unwrap();
        assert_eq!(stash_count(&root), 0);

        let snapshot = stash_push(&root, None).unwrap();
        // The tracked change was stashed away; the untracked file stays.
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).unwrap(),
            "untracked"
        );
        let untracked = find(&snapshot, "new.txt");
        assert!(untracked.state.is_untracked());
        assert_eq!(stash_count(&root), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_records_the_message() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashmsg", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        stash_push(&root, Some("wip feature x")).unwrap();
        let list = run_git_capture("git", &root, &["stash", "list"]).unwrap();
        let list = String::from_utf8(list).unwrap();
        assert!(list.contains("wip feature x"), "stash list was: {list:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_treats_option_like_messages_as_message_text() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashopt", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        // An option-like message is recorded verbatim via --message=<msg>,
        // never parsed as a git option.
        stash_push(&root, Some("--amend")).unwrap();
        let list = run_git_capture("git", &root, &["stash", "list"]).unwrap();
        let list = String::from_utf8(list).unwrap();
        assert!(list.contains("--amend"), "stash list was: {list:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_stashes_staged_changes_too() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashstaged", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "staged").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        let snapshot = stash_push(&root, None).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");
        assert_eq!(stash_count(&root), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_with_nothing_to_stash_is_a_noop() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashclean", &[("a.txt", "base")]);
        // No tracked changes: git exits 0 with "No local changes to save"
        // and creates no entry, so the call succeeds and returns the clean
        // status.
        let snapshot = stash_push(&root, None).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(stash_count(&root), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_with_only_untracked_files_is_a_noop() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashut", &[("a.txt", "base")]);
        fs::write(root.join("new.txt"), "untracked").unwrap();
        // Untracked files are never stashed (no -u/-a), so there is nothing
        // to save and no entry is created.
        let snapshot = stash_push(&root, None).unwrap();
        let untracked = find(&snapshot, "new.txt");
        assert!(untracked.state.is_untracked());
        assert_eq!(stash_count(&root), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_from_nested_root_stashes_the_whole_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashnested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        fs::write(root.join("a.txt"), "dirty-root").unwrap();
        fs::write(root.join("sub/b.txt"), "dirty-sub").unwrap();
        // `git stash push` without pathspecs stashes every tracked change in
        // the repository, including changes outside the nested root.
        let snapshot = stash_push(&root.join("sub"), None).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("sub/b.txt")).unwrap(), "b");
        assert_eq!(stash_count(&root), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_push_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("stashnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = stash_push(&root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash list parsing ----

    #[test]
    fn parses_stash_list_lines_with_descriptions() {
        let snapshot =
            parse_stash_list("stash@{0}: WIP on main: 1a2b3c4 init\nstash@{1}: wip feature x\n")
                .unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.entries[0].index, 0);
        assert_eq!(snapshot.entries[0].description, "WIP on main: 1a2b3c4 init");
        assert_eq!(snapshot.entries[1].index, 1);
        assert_eq!(snapshot.entries[1].description, "wip feature x");
    }

    #[test]
    fn parses_stash_list_sorted_by_index_ascending() {
        // git prints newest first (stash@{0} first); the snapshot must be
        // stably sorted by index ascending regardless of input order.
        let snapshot = parse_stash_list("stash@{1}: older\nstash@{0}: newer\n").unwrap();
        let indexes: Vec<usize> = snapshot.entries.iter().map(|entry| entry.index).collect();
        assert_eq!(indexes, [0, 1]);
        assert_eq!(snapshot.entries[0].description, "newer");
        assert_eq!(snapshot.entries[1].description, "older");
    }

    #[test]
    fn stash_list_sort_is_stable_for_equal_indexes() {
        // git never emits two lines for one stash ref, but the snapshot must
        // still preserve input order when sort keys tie.
        let snapshot = parse_stash_list("stash@{0}: first\nstash@{0}: second\n").unwrap();
        assert_eq!(snapshot.entries[0].description, "first");
        assert_eq!(snapshot.entries[1].description, "second");
    }

    #[test]
    fn parses_empty_stash_list_as_empty_snapshot() {
        let snapshot = parse_stash_list("").unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.entries.is_empty());
    }

    #[test]
    fn allows_empty_stash_descriptions() {
        // An externally created stash with no subject prints `stash@{N}:`
        // with an empty description; the line is still valid.
        let snapshot = parse_stash_list("stash@{0}:\n").unwrap();
        assert_eq!(snapshot.entries[0].index, 0);
        assert_eq!(snapshot.entries[0].description, "");
    }

    #[test]
    fn rejects_malformed_stash_list_lines() {
        for line in [
            "",
            "junk",
            "stash@x: junk",
            "stash@{x}: junk",
            "stash@{0} junk",
            "stash@{0}",
            "stash@{}: junk",
            "stash@{0x1}: junk",
            "@{0}: junk",
            "stash@{99999999999999999999999999}: huge",
        ] {
            let error = parse_stash_line(line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected"
            );
        }
    }

    // ---- stash_list() root validation (no git binary needed) ----

    #[test]
    fn stash_list_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stashlist-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stash_list(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stash_list_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-stashlist-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stash_list(&file).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_stash_list_surfaces_as_not_found() {
        let root = temp_dir("stashlistnobinary");
        let error = run_stash_list("nextide-no-such-git-binary", &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_apply() argument safety (no git binary needed) ----

    #[test]
    fn stash_apply_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stashapply-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stash_apply(&missing, 0).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stash_apply_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-stashapply-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stash_apply(&file, 0).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_stash_apply_surfaces_as_not_found() {
        let root = temp_dir("stashapplynobinary");
        let error = run_git_stash_apply("nextide-no-such-git-binary", &root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_drop() argument safety (no git binary needed) ----

    #[test]
    fn stash_drop_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stashdrop-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stash_drop(&missing, 0).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stash_drop_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-stashdrop-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stash_drop(&file, 0).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_stash_drop_surfaces_as_not_found() {
        let root = temp_dir("stashdropnobinary");
        let error = run_git_stash_drop("nextide-no-such-git-binary", &root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_pop() argument safety (no git binary needed) ----

    #[test]
    fn stash_pop_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-stashpop-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            stash_pop(&missing, 0).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn stash_pop_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-stashpop-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            stash_pop(&file, 0).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_stash_pop_surfaces_as_not_found() {
        let root = temp_dir("stashpopnobinary");
        let error = run_git_stash_pop("nextide-no-such-git-binary", &root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash list/apply/pop/drop integration (requires a git binary) ----

    #[test]
    fn stash_list_returns_typed_entries_for_real_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashlist", &[("a.txt", "base")]);
        assert!(stash_list(&root).unwrap().is_empty());

        fs::write(root.join("a.txt"), "first change").unwrap();
        stash_push(&root, Some("first stash")).unwrap();
        fs::write(root.join("a.txt"), "second change").unwrap();
        stash_push(&root, Some("second stash")).unwrap();

        let snapshot = stash_list(&root).unwrap();
        assert_eq!(snapshot.len(), 2);
        // stash@{0} is the most recent entry; the snapshot is sorted by index
        // ascending. The description is git's full reflog subject, which for
        // a message stash is `On <branch>: <message>` (branch name varies by
        // host configuration, so only the message tail is asserted).
        assert_eq!(snapshot.entries[0].index, 0);
        assert!(
            snapshot.entries[0].description.ends_with("second stash"),
            "description was: {:?}",
            snapshot.entries[0].description
        );
        assert_eq!(snapshot.entries[1].index, 1);
        assert!(
            snapshot.entries[1].description.ends_with("first stash"),
            "description was: {:?}",
            snapshot.entries[1].description
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_apply_restores_changes_and_keeps_the_entry() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashapply", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        stash_push(&root, None).unwrap();
        assert!(status(&root).unwrap().is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");

        let snapshot = stash_apply(&root, 0).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "dirty");
        let entry = find(&snapshot, "a.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        // Unlike `git stash pop`, a successful apply keeps the entry.
        assert_eq!(stash_count(&root), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_apply_does_not_restore_staged_state_without_index() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashapplyindex", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "staged").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        stash_push(&root, None).unwrap();
        assert!(status(&root).unwrap().is_clean());

        // Plain `git stash apply` (no --index) restores the changes into the
        // worktree; the staged/unstaged split is not reinstated.
        let snapshot = stash_apply(&root, 0).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "staged");
        let entry = find(&snapshot, "a.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_apply_out_of_range_index_is_invalid_input() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashapplyrange", &[("a.txt", "base")]);
        // No stashes: any index is out of range.
        let error = stash_apply(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("out of range"), "message was: {message}");

        fs::write(root.join("a.txt"), "dirty").unwrap();
        stash_push(&root, None).unwrap();
        let error = stash_apply(&root, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("out of range"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_apply_conflicting_change_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashapplyconflict", &[("a.txt", "base\n")]);
        fs::write(root.join("a.txt"), "dirty\n").unwrap();
        stash_push(&root, None).unwrap();
        // Commit a divergent change on top of the stash base, so applying the
        // stash is a genuine three-way conflict (git refuses outright while
        // the worktree itself has uncommitted changes).
        fs::write(root.join("a.txt"), "committed\n").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        commit(&root, "divergent change").unwrap();

        let error = stash_apply(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("CONFLICT"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_drop_removes_the_entry_and_returns_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashdrop", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "first").unwrap();
        stash_push(&root, Some("first")).unwrap();
        fs::write(root.join("a.txt"), "second").unwrap();
        stash_push(&root, Some("second")).unwrap();
        assert_eq!(stash_count(&root), 2);

        // Dropping the newest entry renumbers the remaining one to stash@{0}.
        let snapshot = stash_drop(&root, 0).unwrap();
        assert!(snapshot.is_clean());
        assert_eq!(stash_count(&root), 1);
        let remaining = stash_list(&root).unwrap();
        assert_eq!(remaining.entries.len(), 1);
        assert_eq!(remaining.entries[0].index, 0);
        assert!(
            remaining.entries[0].description.ends_with("first"),
            "description was: {:?}",
            remaining.entries[0].description
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_drop_out_of_range_index_is_invalid_input() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashdroprange", &[("a.txt", "base")]);
        let error = stash_drop(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("out of range"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_list_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("stashlistnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = stash_list(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_apply_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("stashapplynonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = stash_apply(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_drop_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("stashdropnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = stash_drop(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- stash_pop() integration (requires a git binary) ----

    #[test]
    fn stash_pop_restores_changes_and_drops_the_entry() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashpop", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "dirty").unwrap();
        stash_push(&root, None).unwrap();
        assert!(status(&root).unwrap().is_clean());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "base");
        assert_eq!(stash_count(&root), 1);

        // pop applies the change and drops the entry (unlike stash_apply).
        let snapshot = stash_pop(&root, 0).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "dirty");
        let entry = find(&snapshot, "a.txt");
        assert_eq!(
            entry.state,
            GitFileState::new(GitStatusCode::Unmodified, GitStatusCode::Modified)
        );
        assert_eq!(stash_count(&root), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_pop_renumbers_remaining_entries() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashpoprenum", &[("a.txt", "base")]);
        fs::write(root.join("a.txt"), "first").unwrap();
        stash_push(&root, Some("first")).unwrap();
        fs::write(root.join("a.txt"), "second").unwrap();
        stash_push(&root, Some("second")).unwrap();
        assert_eq!(stash_count(&root), 2);

        // Popping the newest entry renumbers the remaining one to stash@{0}.
        let snapshot = stash_pop(&root, 0).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "second");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(stash_count(&root), 1);
        let remaining = stash_list(&root).unwrap();
        assert_eq!(remaining.entries.len(), 1);
        assert_eq!(remaining.entries[0].index, 0);
        assert!(
            remaining.entries[0].description.ends_with("first"),
            "description was: {:?}",
            remaining.entries[0].description
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_pop_conflict_keeps_the_entry() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashpopconflict", &[("a.txt", "base\n")]);
        fs::write(root.join("a.txt"), "dirty\n").unwrap();
        stash_push(&root, None).unwrap();
        // Commit a divergent change on top of the stash base, so popping the
        // stash is a genuine three-way conflict.
        fs::write(root.join("a.txt"), "committed\n").unwrap();
        stage(&root, &[Path::new("a.txt")]).unwrap();
        commit(&root, "divergent change").unwrap();

        let error = stash_pop(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("CONFLICT"), "message was: {message}");
        // Git's pop semantics: the entry is dropped only after a successful
        // apply, so a conflicting pop keeps the stash in the list.
        assert_eq!(
            stash_count(&root),
            1,
            "failed pop must keep the stash entry"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_pop_out_of_range_index_is_invalid_input() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("stashpoprange", &[("a.txt", "base")]);
        // No stashes: any index is out of range.
        let error = stash_pop(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("out of range"), "message was: {message}");

        fs::write(root.join("a.txt"), "dirty").unwrap();
        stash_push(&root, None).unwrap();
        let error = stash_pop(&root, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("out of range"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stash_pop_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("stashpopnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = stash_pop(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- log parsing ----

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn parses_log_record_with_root_commit() {
        let entry = parse_log_line(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0Alice\x002024-01-02T03:04:05+00:00\0first commit",
        )
        .unwrap();
        assert_eq!(entry.hash, HASH);
        assert!(entry.parents.is_empty());
        assert_eq!(entry.author, "Alice");
        assert_eq!(entry.date, "2024-01-02T03:04:05+00:00");
        assert_eq!(entry.subject, "first commit");
    }

    #[test]
    fn parses_log_record_with_parents_and_empty_subject() {
        let entry = parse_log_line(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc\0Bob\x002026-08-29T13:57:43+08:00\0",
        )
        .unwrap();
        assert_eq!(
            entry.parents,
            [
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "cccccccccccccccccccccccccccccccccccccccc",
            ]
        );
        assert_eq!(entry.subject, "");
    }

    #[test]
    fn parses_log_records_in_output_order() {
        let output = "1111111111111111111111111111111111111111\0\0Alice\x002026-08-29T13:57:43+08:00\0second\n\
                      2222222222222222222222222222222222222222\0\0Bob\x002026-08-28T13:57:43+08:00\0first\n";
        let snapshot = parse_log(output).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert!(!snapshot.is_empty());
        // git's output order is preserved verbatim — never re-sorted.
        assert_eq!(
            snapshot.entries[0].hash,
            "1111111111111111111111111111111111111111"
        );
        assert_eq!(snapshot.entries[0].author, "Alice");
        assert_eq!(snapshot.entries[0].date, "2026-08-29T13:57:43+08:00");
        assert_eq!(snapshot.entries[0].subject, "second");
        assert_eq!(snapshot.entries[1].subject, "first");
    }

    #[test]
    fn parses_merge_commit_parents_and_special_subject() {
        let entry = parse_log_line(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc\0Alice Example\x002024-01-02T03:04:05+00:00\0Merge \"quoted\" 中文: done",
        )
        .unwrap();
        assert_eq!(entry.parents.len(), 2);
        assert_eq!(entry.parents[0], "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(entry.parents[1], "cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(entry.author, "Alice Example");
        assert_eq!(entry.subject, "Merge \"quoted\" 中文: done");
    }

    #[test]
    fn parses_log_with_crlf_line_endings() {
        let output = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0Alice\x002024-01-02T03:04:05+00:00\0subject\r\n";
        let snapshot = parse_log(output).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.entries[0].subject, "subject");
    }

    #[test]
    fn rejects_malformed_log_records() {
        for line in [
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0Alice\0date",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0Alice\0date\0subject\0extra",
            "not-hex\0\0Alice\0date\0subject",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0not-hex\0Alice\0date\0subject",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0\0date\0subject",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0\0Alice\0\0subject",
        ] {
            let error = parse_log_line(line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_utf8_log_output() {
        let error = parse_log_bytes(vec![b'a', b'0', 0xff]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    // ---- log() root and limit handling (no git binary needed) ----

    #[test]
    fn log_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-git-log-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            log(&missing, None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn log_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-git-log-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            log(&file, None).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn log_zero_limit_short_circuits_without_git() {
        // git is never spawned for a zero limit: any directory works, even a
        // non-repository.
        let root = temp_dir("logzero");
        let snapshot = log(&root, Some(0)).unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_missing_git_binary_surfaces_as_not_found() {
        let root = temp_dir("lognobinary");
        let error = run_log("nextide-no-such-git-binary", &root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- log() integration (requires a git binary) ----

    /// Reads a full revision hash from git, trimmed.
    fn head_rev(root: &Path, revision: &str) -> String {
        let output = run_git_capture("git", root, &["rev-parse", revision]).unwrap();
        String::from_utf8(output).unwrap().trim().to_string()
    }

    #[test]
    fn log_returns_history_newest_first_with_parsed_fields() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("loghistory", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "b").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "second commit",
                "--author=Alice <alice@example.com>",
                "--date=2024-01-02T03:04:05+08:00",
            ],
        );
        fs::write(root.join("a.txt"), "c").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "third commit",
                "--author=Bob <bob@example.com>",
                "--date=2024-02-03T04:05:06+08:00",
            ],
        );

        let head = head_rev(&root, "HEAD");
        let second = head_rev(&root, "HEAD~1");
        let init = head_rev(&root, "HEAD~2");

        let snapshot = log(&root, None).unwrap();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot.entries[0].hash, head);
        assert_eq!(snapshot.entries[0].author, "Bob");
        assert_eq!(snapshot.entries[0].date, "2024-02-03T04:05:06+08:00");
        assert_eq!(snapshot.entries[0].subject, "third commit");
        assert_eq!(snapshot.entries[1].hash, second);
        assert_eq!(snapshot.entries[1].author, "Alice");
        assert_eq!(snapshot.entries[1].date, "2024-01-02T03:04:05+08:00");
        assert_eq!(snapshot.entries[1].subject, "second commit");
        assert_eq!(snapshot.entries[2].hash, init);
        assert_eq!(snapshot.entries[2].author, "nextide-tests");
        assert_eq!(snapshot.entries[2].subject, "init");
        assert!(snapshot.entries[2].parents.is_empty());
        // The order is git's own: newest commit first.
        let subjects: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|entry| entry.subject.as_str())
            .collect();
        assert_eq!(subjects, ["third commit", "second commit", "init"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_respects_limit() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("loglimit", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "b").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "second"],
        );
        fs::write(root.join("a.txt"), "c").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "third"],
        );

        let snapshot = log(&root, Some(2)).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.entries[0].subject, "third");
        assert_eq!(snapshot.entries[1].subject, "second");

        let snapshot = log(&root, Some(1)).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.entries[0].subject, "third");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_oversized_limit_returns_everything() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("loghuge", &[("a.txt", "a")]);
        // Larger than git's signed --max-count: the count is not passed to
        // git, and the snapshot is truncated to the whole history anyway.
        let snapshot = log(&root, Some(usize::MAX)).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.entries[0].subject, "init");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_parses_merge_commit_with_two_parents() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("logmerge");
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.name", "nextide-tests"]);
        run_git(
            &root,
            &["config", "user.email", "nextide-tests@example.com"],
        );
        // Distinct explicit dates make git's output order deterministic.
        fs::write(root.join("a.txt"), "a").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
                "--date=2023-01-01T00:00:00+00:00",
            ],
        );
        run_git(&root, &["checkout", "-q", "-b", "feature"]);
        fs::write(root.join("b.txt"), "feature").unwrap();
        run_git(&root, &["add", "b.txt"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "feature work",
                "--date=2024-01-01T00:00:00+00:00",
            ],
        );
        run_git(&root, &["checkout", "-q", "-"]);
        fs::write(root.join("a.txt"), "master").unwrap();
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-am",
                "master work",
                "--date=2024-01-02T00:00:00+00:00",
            ],
        );
        run_git(
            &root,
            &["merge", "--no-ff", "-q", "-m", "merge feature", "feature"],
        );

        let snapshot = log(&root, None).unwrap();
        assert_eq!(snapshot.len(), 4);
        let merge = &snapshot.entries[0];
        assert_eq!(merge.subject, "merge feature");
        assert_eq!(merge.parents.len(), 2);
        assert_eq!(
            merge.parents,
            [head_rev(&root, "HEAD^1"), head_rev(&root, "HEAD^2")]
        );
        let subjects: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|entry| entry.subject.as_str())
            .collect();
        assert_eq!(
            subjects,
            ["merge feature", "master work", "feature work", "init"]
        );
        // The root commit has no parents.
        assert!(snapshot.entries[3].parents.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_from_nested_root_queries_the_whole_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("lognested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        fs::write(root.join("a.txt"), "c").unwrap();
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-am",
                "second",
            ],
        );
        // `git -C` resolves the nested root; log is not path-scoped, so the
        // whole repository history is returned, newest first.
        let snapshot = log(&root.join("sub"), None).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.entries[0].subject, "second");
        assert_eq!(snapshot.entries[1].subject, "init");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_parses_unicode_and_quoted_subject() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("logunicode", &[("a.txt", "a")]);
        fs::write(root.join("a.txt"), "b").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fix: 中文 \"quoted\" subject",
            ],
        );
        let snapshot = log(&root, None).unwrap();
        assert_eq!(snapshot.entries[0].subject, "fix: 中文 \"quoted\" subject");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_empty_repository_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        // init_repo always commits; build an empty repository manually.
        let root = temp_dir("logempty");
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.name", "nextide-tests"]);
        run_git(
            &root,
            &["config", "user.email", "nextide-tests@example.com"],
        );
        let error = log(&root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("does not have any commits yet"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("lognonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = log(&root, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- tags parsing ----

    const TAG_OBJECT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TAG_OBJECT_2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn parses_tag_lines_with_objects() {
        let snapshot = parse_tags(&format!(
            "{TAG_OBJECT}\0lightweight\n{TAG_OBJECT_2}\0annotated\n"
        ))
        .unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.tags[0].name, "annotated");
        assert_eq!(snapshot.tags[0].object, TAG_OBJECT_2);
        assert_eq!(snapshot.tags[1].name, "lightweight");
        assert_eq!(snapshot.tags[1].object, TAG_OBJECT);
    }

    #[test]
    fn parses_tag_names_with_slashes() {
        // Tag names may contain `/`; only escape-rejecting checks apply.
        let snapshot = parse_tags(&format!("{TAG_OBJECT}\0v1.0/release\n")).unwrap();
        assert_eq!(snapshot.tags[0].name, "v1.0/release");
    }

    #[test]
    fn tag_snapshot_is_sorted_by_name() {
        let output = format!("{TAG_OBJECT}\0zeta\n{TAG_OBJECT_2}\0alpha\n{TAG_OBJECT}\0mid\n");
        let snapshot = parse_tags(&output).unwrap();
        let names: Vec<&str> = snapshot.tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn tag_sort_is_stable_for_equal_names() {
        // for-each-ref never emits two lines for one tag, but the snapshot
        // must still preserve input order when sort keys tie.
        let snapshot = parse_tags(&format!("{TAG_OBJECT}\0dup\n{TAG_OBJECT_2}\0dup\n")).unwrap();
        assert_eq!(snapshot.tags[0].object, TAG_OBJECT);
        assert_eq!(snapshot.tags[1].object, TAG_OBJECT_2);
    }

    #[test]
    fn parses_empty_tag_list_as_empty_snapshot() {
        let snapshot = parse_tags("").unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.tags.is_empty());
    }

    #[test]
    fn parses_tag_lines_with_crlf_line_endings() {
        let snapshot = parse_tags(&format!("{TAG_OBJECT}\0v1.0\r\n")).unwrap();
        assert_eq!(snapshot.tags[0].name, "v1.0");
    }

    #[test]
    fn rejects_malformed_tag_lines() {
        for line in [
            "not-hex\0name".to_string(),
            TAG_OBJECT.to_string(),
            format!("{TAG_OBJECT}\0"),
            "\0name".to_string(),
            format!("{TAG_OBJECT}\0name\0extra"),
        ] {
            let error = parse_tags(&line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_tag_names_that_escape_the_repository() {
        for line in [
            format!("{TAG_OBJECT}\0/etc/passwd"),
            format!("{TAG_OBJECT}\0\\\\server\\share\\x"),
            format!("{TAG_OBJECT}\0C:/windows/x"),
            format!("{TAG_OBJECT}\0../outside"),
            format!("{TAG_OBJECT}\0a/../../outside"),
        ] {
            let error = parse_tags(&line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected as unsafe"
            );
        }
    }

    #[test]
    fn rejects_non_utf8_tag_output() {
        let error = parse_tags_bytes(vec![b'a', b'0', 0xff]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    // ---- tags() root validation (no git binary needed) ----

    #[test]
    fn tags_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-tags-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(tags(&missing).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn tags_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-tags-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            tags(&file).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_tags_surfaces_as_not_found() {
        let root = temp_dir("tagsnobinary");
        let error = run_tags("nextide-no-such-git-binary", &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- tags() integration (requires a git binary) ----

    #[test]
    fn tags_lists_lightweight_and_annotated_tags() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tags", &[("a.txt", "a")]);
        let head = rev_hash(&root, "HEAD");
        run_git(&root, &["tag", "lightweight"]);
        run_git(
            &root,
            &[
                "-c",
                "tag.gpgSign=false",
                "tag",
                "-a",
                "annotated",
                "-m",
                "annotation",
            ],
        );
        // More commits after tagging, so the tags keep pointing at the
        // original commit and the snapshot must not be confused by HEAD.
        fs::write(root.join("a.txt"), "b").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "second"],
        );
        run_git(&root, &["tag", "zeta"]);

        let snapshot = tags(&root).unwrap();
        let names: Vec<&str> = snapshot.tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["annotated", "lightweight", "zeta"]);

        // A lightweight tag's object is the object it names — here the
        // commit it was created on, regardless of later HEAD moves.
        let lightweight = snapshot
            .tags
            .iter()
            .find(|tag| tag.name == "lightweight")
            .unwrap();
        assert_eq!(lightweight.object, head);
        assert_eq!(lightweight.object, rev_hash(&root, "lightweight"));
        let zeta = snapshot.tags.iter().find(|tag| tag.name == "zeta").unwrap();
        assert_eq!(zeta.object, rev_hash(&root, "zeta"));

        // An annotated tag's object is the tag object itself — `rev-parse`
        // of the tag name (no peeling) — never the commit it points at,
        // proving the snapshot records the tag without expanding it.
        let annotated = snapshot
            .tags
            .iter()
            .find(|tag| tag.name == "annotated")
            .unwrap();
        assert_eq!(annotated.object, rev_hash(&root, "annotated"));
        assert_ne!(annotated.object, head);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tags_empty_repository_returns_empty_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagsempty", &[("a.txt", "a")]);
        let snapshot = tags(&root).unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.tags.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tags_from_nested_root_queries_the_whole_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagsnested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        run_git(&root, &["tag", "v1.0"]);
        let snapshot = tags(&root.join("sub")).unwrap();
        assert_eq!(snapshot.tags.len(), 1);
        assert_eq!(snapshot.tags[0].name, "v1.0");
        assert_eq!(snapshot.tags[0].object, rev_hash(&root, "v1.0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tags_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("tagsnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = tags(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- tag() / delete_tag() name validation (no git binary needed) ----

    #[test]
    fn tag_accepts_valid_tag_names() {
        for name in [
            "v1.2",
            "v1.0.0",
            "v1.2-rc.1",
            "release-1.0",
            "build_42",
            "feature/release",
            "feature/v1.2",
            "release/2026-08",
            "a.b/c.d",
        ] {
            assert!(
                check_tag_name(name).is_ok(),
                "tag name {name:?} should be accepted"
            );
        }
    }

    #[test]
    fn tag_rejects_unsafe_tag_names() {
        let root = temp_dir("tagbad");
        for name in [
            "",
            "\0",
            "-f",
            "--delete",
            "/abs/name",
            "\\server\\share\\x",
            "C:/windows/x",
            "C:\\windows\\x",
            "..",
            "../outside",
            "a/../../outside",
            "a..b",
            "foo//bar",
            "foo/",
            "/foo",
            "foo bar",
            "foo\tbar",
            "foo\nbar",
            "foo\rbar",
            "foo\x01bar",
            "foo\x7fbar",
            "foo~bar",
            "foo^bar",
            "foo:bar",
            "foo?bar",
            "foo*bar",
            "foo[bar",
            "foo\\bar",
            "foo@{bar",
            "@",
            ".hidden",
            "foo/.hidden",
            "foo.lock",
            "foo/bar.lock",
            "foo.",
            "v1.0.0.",
        ] {
            let error = tag(&root, name).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "tag name {name:?} should be rejected"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    // ---- tag() / delete_tag() root validation (no git binary needed) ----

    #[test]
    fn tag_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-tag-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            tag(&missing, "v1.0").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn tag_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-tag-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            tag(&file, "v1.0").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn delete_tag_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-delete-tag-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            delete_tag(&missing, "v1.0").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn delete_tag_file_root_is_not_a_directory() {
        let file = std::env::temp_dir().join(format!(
            "nextide-delete-tag-notdir-{}.txt",
            std::process::id()
        ));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            delete_tag(&file, "v1.0").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_tag_surfaces_as_not_found() {
        let root = temp_dir("tagnobinary");
        let error = run_git_tag("nextide-no-such-git-binary", &root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_git_binary_for_delete_tag_surfaces_as_not_found() {
        let root = temp_dir("deletetagnobinary");
        let error = run_git_delete_tag("nextide-no-such-git-binary", &root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- tag() / delete_tag() integration (requires a git binary) ----

    #[test]
    fn tag_creates_lightweight_tag_and_returns_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagcreate", &[("a.txt", "a")]);
        let head = rev_hash(&root, "HEAD");
        let snapshot = tag(&root, "v1.2").unwrap();
        assert_eq!(snapshot.tags.len(), 1);
        assert_eq!(snapshot.tags[0].name, "v1.2");
        assert_eq!(snapshot.tags[0].object, head);
        // The tag is lightweight: it resolves directly to the commit.
        assert_eq!(rev_hash(&root, "v1.2"), head);
        // The worktree and index are untouched.
        assert!(status(&root).unwrap().is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tag_creates_slash_names_and_keeps_existing_tags() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagslash", &[("a.txt", "a")]);
        let head = rev_hash(&root, "HEAD");
        let snapshot = tag(&root, "v1.2").unwrap();
        assert_eq!(snapshot.tags.len(), 1);
        let snapshot = tag(&root, "feature/v1.2").unwrap();
        assert_eq!(snapshot.tags.len(), 2);
        let names: Vec<&str> = snapshot.tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["feature/v1.2", "v1.2"]);
        assert!(snapshot.tags.iter().all(|tag| tag.object == head));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tag_existing_name_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagexists", &[("a.txt", "a")]);
        tag(&root, "v1.0").unwrap();
        let error = tag(&root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("already exists"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tag_in_repository_without_commits_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("tagnocommit");
        run_git(&root, &["init", "-q"]);
        let error = tag(&root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("HEAD"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tag_removes_tag_and_returns_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagdelete", &[("a.txt", "a")]);
        let head = rev_hash(&root, "HEAD");
        tag(&root, "v1.2").unwrap();
        tag(&root, "v1.3").unwrap();
        let snapshot = delete_tag(&root, "v1.2").unwrap();
        assert_eq!(snapshot.tags.len(), 1);
        assert_eq!(snapshot.tags[0].name, "v1.3");
        assert_eq!(snapshot.tags[0].object, head);
        assert_eq!(tags(&root).unwrap().tags.len(), 1);
        // The worktree and index are untouched.
        assert!(status(&root).unwrap().is_clean());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tag_removes_annotated_tags_too() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagdeleteannotated", &[("a.txt", "a")]);
        run_git(
            &root,
            &[
                "-c",
                "tag.gpgSign=false",
                "tag",
                "-a",
                "annotated",
                "-m",
                "annotation",
            ],
        );
        let snapshot = delete_tag(&root, "annotated").unwrap();
        assert!(snapshot.is_empty());
        assert!(tags(&root).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tag_unknown_name_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagnotfound", &[("a.txt", "a")]);
        let error = delete_tag(&root, "nope").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(message.contains("not found"), "message was: {message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tag_from_nested_root_applies_to_the_whole_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("tagnested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        let snapshot = tag(&root.join("sub"), "v1.0").unwrap();
        assert_eq!(snapshot.tags.len(), 1);
        assert_eq!(snapshot.tags[0].name, "v1.0");
        assert_eq!(rev_hash(&root, "v1.0"), rev_hash(&root, "HEAD"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tag_from_nested_root_deletes_the_whole_repository_tag() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("deletetagnested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        tag(&root, "v1.0").unwrap();
        let snapshot = delete_tag(&root.join("sub"), "v1.0").unwrap();
        assert!(snapshot.is_empty());
        assert!(tags(&root).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tag_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("tagnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = tag(&root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tag_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("deletetagnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = delete_tag(&root, "v1.0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- remotes parsing ----

    #[test]
    fn parses_fetch_and_push_lines_into_typed_remotes() {
        let output = concat!(
            "origin\thttps://github.com/foo/bar.git (fetch)\n",
            "origin\thttps://github.com/foo/bar.git (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        assert_eq!(snapshot.remotes.len(), 1);
        let origin = &snapshot.remotes[0];
        assert_eq!(origin.name, "origin");
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        // Without an explicit pushurl, git reports the fetch URL as the
        // single push URL.
        assert_eq!(origin.push_urls, ["https://github.com/foo/bar.git"]);
    }

    #[test]
    fn parses_distinct_pushurl() {
        let output = concat!(
            "origin\thttps://github.com/foo/bar.git (fetch)\n",
            "origin\tgit@github.com:foo/bar.git (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        let origin = &snapshot.remotes[0];
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        assert_eq!(origin.push_urls, ["git@github.com:foo/bar.git"]);
    }

    #[test]
    fn parses_multiple_pushurls_in_git_order() {
        let output = concat!(
            "origin\thttps://github.com/foo/bar.git (fetch)\n",
            "origin\tssh://git@example.com/push1.git (push)\n",
            "origin\tssh://git@example.com/push2.git (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        let origin = &snapshot.remotes[0];
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        assert_eq!(
            origin.push_urls,
            [
                "ssh://git@example.com/push1.git",
                "ssh://git@example.com/push2.git"
            ]
        );
    }

    #[test]
    fn parses_remote_without_fetch_url_from_bare_line() {
        // A remote with a pushurl but no url: git prints a bare `name<TAB>`
        // line for the missing fetch URL.
        let output = concat!(
            "onlypush\t\n",
            "onlypush\tssh://git@example.com/only-push.git (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        assert_eq!(snapshot.remotes.len(), 1);
        let onlypush = &snapshot.remotes[0];
        assert_eq!(onlypush.name, "onlypush");
        assert_eq!(onlypush.fetch_url, None);
        assert_eq!(onlypush.push_urls, ["ssh://git@example.com/only-push.git"]);
    }

    #[test]
    fn parses_urls_containing_spaces() {
        // `git remote -v` prints URLs raw, never quoted, so a URL with
        // spaces is everything between the tab and the trailing suffix.
        let output = concat!(
            "second\t/local/path with spaces (fetch)\n",
            "second\t/local/path with spaces (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        let second = &snapshot.remotes[0];
        assert_eq!(second.fetch_url.as_deref(), Some("/local/path with spaces"));
        assert_eq!(second.push_urls, ["/local/path with spaces"]);
    }

    #[test]
    fn parses_remote_with_fetch_line_only() {
        let output = "origin\thttps://github.com/foo/bar.git (fetch)\n";
        let snapshot = parse_remotes(output).unwrap();
        let origin = &snapshot.remotes[0];
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        assert!(origin.push_urls.is_empty());
    }

    #[test]
    fn parses_remote_lines_with_crlf() {
        let output = "origin\thttps://x.git (fetch)\r\norigin\thttps://x.git (push)\r\n";
        let snapshot = parse_remotes(output).unwrap();
        assert_eq!(snapshot.remotes.len(), 1);
        assert_eq!(snapshot.remotes[0].name, "origin");
        assert_eq!(
            snapshot.remotes[0].fetch_url.as_deref(),
            Some("https://x.git")
        );
    }

    #[test]
    fn parses_remotes_and_sorts_by_name() {
        let output = concat!(
            "zeta\tz-url (fetch)\n",
            "zeta\tz-url (push)\n",
            "alpha\ta-url (fetch)\n",
            "alpha\ta-url (push)\n",
            "beta\tb-url (fetch)\n",
            "beta\tb-url (push)\n",
        );
        let snapshot = parse_remotes(output).unwrap();
        let names: Vec<&str> = snapshot.remotes.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "zeta"]);
    }

    #[test]
    fn empty_remote_output_is_empty_snapshot() {
        let snapshot = parse_remotes("").unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.remotes.is_empty());
    }

    #[test]
    fn rejects_malformed_remote_lines() {
        for line in [
            "origin",                  // no tab separator
            "\turl (fetch)",           // empty name
            "origin\t (fetch)",        // empty fetch URL
            "origin\t (push)",         // empty push URL
            "origin\turl (clone)",     // unknown suffix
            "origin\turl",             // missing suffix
            "origin\turl (fetch)junk", // trailing data
            "bad name\turl (fetch)",   // whitespace in name
            "/abs\turl (fetch)",       // absolute-looking name
            "C:\\win\turl (fetch)",    // drive-looking name
        ] {
            let error = parse_remotes(line).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "line {line:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_conflicting_fetch_lines() {
        for output in [
            // A bare line followed by a real fetch line for the same remote.
            "origin\t\norigin\thttps://x.git (fetch)\n",
            // Two fetch lines for the same remote.
            "origin\thttps://a.git (fetch)\norigin\thttps://b.git (fetch)\n",
        ] {
            let error = parse_remotes(output).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "output {output:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_utf8_remote_output() {
        let error = parse_remotes_bytes(vec![0xFF, 0xFE]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    // ---- remotes() root validation (no git binary needed) ----

    #[test]
    fn remotes_missing_root_is_not_found() {
        let missing =
            std::env::temp_dir().join(format!("nextide-remotes-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert_eq!(
            remotes(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn remotes_file_root_is_not_a_directory() {
        let file =
            std::env::temp_dir().join(format!("nextide-remotes-notdir-{}.txt", std::process::id()));
        fs::write(&file, "x").unwrap();
        assert_eq!(
            remotes(&file).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let _ = fs::remove_file(file);
    }

    #[test]
    fn missing_git_binary_for_remotes_surfaces_as_not_found() {
        let root =
            std::env::temp_dir().join(format!("nextide-remotes-nobinary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let error = run_remotes("nextide-no-such-git-binary", &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    // ---- remotes() integration (requires a git binary) ----

    #[test]
    fn remotes_empty_repository_returns_empty_snapshot() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("remotesempty", &[("a.txt", "a")]);
        let snapshot = remotes(&root).unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.remotes.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remotes_reports_real_configured_remotes() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("remotesreal", &[("a.txt", "a")]);
        // A plain remote: fetch and push share the URL.
        run_git(
            &root,
            &["remote", "add", "origin", "https://github.com/foo/bar.git"],
        );
        // A multi-valued pushurl makes fetch and push differ; git tries the
        // push URLs in order.
        run_git(
            &root,
            &[
                "config",
                "remote.origin.pushurl",
                "ssh://git@example.com/push1.git",
            ],
        );
        run_git(
            &root,
            &[
                "config",
                "--add",
                "remote.origin.pushurl",
                "ssh://git@example.com/push2.git",
            ],
        );
        // A URL containing spaces is stored and printed raw.
        run_git(
            &root,
            &["remote", "add", "spaced", "/local/path with spaces"],
        );
        // A remote whose fetch URL is removed keeps only its push URL: git
        // prints a bare `name<TAB>` line for the missing fetch URL.
        run_git(
            &root,
            &[
                "remote",
                "add",
                "onlypush",
                "ssh://git@example.com/only-push.git",
            ],
        );
        run_git(&root, &["config", "--unset", "remote.onlypush.url"]);
        run_git(
            &root,
            &[
                "config",
                "remote.onlypush.pushurl",
                "ssh://git@example.com/only-push.git",
            ],
        );

        let snapshot = remotes(&root).unwrap();
        let names: Vec<&str> = snapshot.remotes.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["onlypush", "origin", "spaced"]);

        let onlypush = &snapshot.remotes[0];
        assert_eq!(onlypush.fetch_url, None);
        assert_eq!(onlypush.push_urls, ["ssh://git@example.com/only-push.git"]);

        let origin = &snapshot.remotes[1];
        assert_eq!(
            origin.fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        assert_eq!(
            origin.push_urls,
            [
                "ssh://git@example.com/push1.git",
                "ssh://git@example.com/push2.git"
            ]
        );

        let spaced = &snapshot.remotes[2];
        assert_eq!(spaced.fetch_url.as_deref(), Some("/local/path with spaces"));
        assert_eq!(spaced.push_urls, ["/local/path with spaces"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remotes_from_nested_root_queries_the_whole_repository() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = init_repo("remotesnested", &[("a.txt", "a"), ("sub/b.txt", "b")]);
        run_git(
            &root,
            &["remote", "add", "origin", "https://github.com/foo/bar.git"],
        );
        // Remotes are repository-global configuration, so a nested root
        // queries the same set.
        let snapshot = remotes(&root.join("sub")).unwrap();
        assert_eq!(snapshot.remotes.len(), 1);
        assert_eq!(snapshot.remotes[0].name, "origin");
        assert_eq!(
            snapshot.remotes[0].fetch_url.as_deref(),
            Some("https://github.com/foo/bar.git")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remotes_non_repo_directory_fails_with_git_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = temp_dir("remotesnonrepo");
        if inside_existing_repo(&root) {
            eprintln!("skipping: temp dir is nested inside an existing git repository");
            let _ = fs::remove_dir_all(root);
            return;
        }
        let error = remotes(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let message = error.to_string();
        assert!(
            message.contains("not a git repository"),
            "message was: {message}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
