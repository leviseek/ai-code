# NextIDE

NextIDE is a local-first, capability-oriented coding IDE.

## Workspace status

The repository currently contains the first Phase 0 vertical slice:

- Rust workspace
- transactional document model
- undo/redo
- snapshot/recovery primitives
- minimal CLI smoke path
- headless Desktop Shell with window, input, command-palette, notification, and file-tree pipelines
- incremental file-tree rendering with structured change batches and events
- task runtime with lifecycle events, summaries, cancellation, timeout, cleanup, and task queries
- VFS read-only, typed file metadata queries (`exists` returning `io::Result<bool>` plus `is_file`/`is_dir`/`metadata` returning `FileKind` + `FileMetadata` with kind/len/readonly): `exists` reports `Ok(true)` for existing entries and `Ok(false)` for missing ones, and rooted filesystems reject path escapes with `Err(PermissionDenied)` instead of silently reporting `false`; missing entries report `Ok(false)` from `exists`/`is_file`/`is_dir` and `NotFound` from `metadata`
- VFS typed, stable-sorted directory enumeration (`read_dir` returning `io::Result<Vec<FileEntry>>` with name/path/kind/len): entries sort by name in stable order, missing paths report `NotFound`, non-directory paths report `NotADirectory`, and root escapes keep reporting `PermissionDenied`; the trait default returns `Unsupported`, so existing `FileSystem` implementations (`LocalFileSystem`, `MemoryFs`, `TreeFs`, `NoopFs`) compile and behave unchanged, and `LocalFileSystem::list` now delegates to `read_dir`
- dependency-free workspace text search (`search` module: `SearchQuery`/`SearchOptions`/`SearchMatch`/`SearchMatcher`, entry points `search::search(fs, &query)` and `query.run(&fs)`): recursively walks `read_dir` over any rooted `FileSystem`, reports every matching line (relative path, 1-based line, byte column, line text), skips binary (invalid UTF-8 or NUL-containing) and unreadable/denied files and subdirectories silently, propagates only search-root errors, stably sorts results by `(path, line, column)`, and truncates to `max_results`; `case_sensitive` (default on), `max_depth` (default 64) and a normalized `"."` root keep paths inside the workspace root and walks finite even through symlinked cycles; matching is pluggable through the reusable `SearchMatcher` — `SearchQuery::new` keeps the plain literal-substring behavior, while `SearchQuery::glob` (or `matcher: SearchMatcher::Glob`) interprets the needle as a simple glob pattern (`*` = any run of characters, `?` = exactly one character), a deliberately non-regex wildcard matcher with no character classes, alternation, anchors, or escapes

Task state and summary APIs are available through `TaskManager::state`, `summary`, `summaries`, and `ids`; pending tasks can be removed and terminal tasks can be cleaned with lifecycle events.

Capability metadata is observable through `CapabilityRegistry::ids`, `iter`, `summary`, and `summaries`: iteration and projections are returned in stable, sorted id order, and `CapabilitySummary` is a cloneable, UI-safe snapshot of a capability's id, version, and declared permissions. `is_empty`/`len`/`contains` remain available for cheap registry checks.

Shell command metadata is observable through `command_ids`, `get_command`, `command_summary`, and `command_summaries`: ids enumerate in stable declaration order, and `CommandSummary` is a cloneable, UI-safe snapshot of a command's id, title, and search keywords, decoupled from the static `COMMANDS` descriptor table so the command palette, UI, and plugins can observe commands without borrowing. `search_commands`/`command_id`/`COMMANDS` remain unchanged.

Shell command execution is observable through `CommandExecutionResult`, `CommandOutcome`, and `CommandError`: `DesktopShell::execute_observed` runs a `ShellCommand` and returns a typed, cloneable record (stable command id, tri-state outcome, structured error) that UI and plugins can consume without borrowing. Every execution publishes a `shell.command.executed` event with a stable payload (`id=<id>,outcome=<outcome>` or `id=<id>,outcome=failed,error=<kind>,message=<message>`); failures keep publishing `shell.command.failed` plus an error notification. `command_title`, `is_registered`, and `describe_execution` resolve execution results back to registered command metadata, and `CommandError`/`CommandErrorKind` classify failures (`io`, `not-found`, `invalid-state`, `unknown`) with a message, round-tripping through `std::io::Error`. Existing `execute`, `execute_checked`, `execute_selected`, and `CommandResult` signatures and behavior are unchanged; the checked/selected paths now route through the observable one, so every executed command also emits `shell.command.executed`.

## Development

```powershell
cargo test --workspace
cargo run -p nextide-cli -- smoke
cargo run -p nextide-cli -- bootstrap
cargo run -p nextide-cli -- task-smoke
```

`task-smoke` exercises the background task runtime: it registers a cross-platform echo command (`cmd.exe /C echo` on Windows, `sh -c printf` elsewhere), spawns it through `TaskManager::spawn_async`, polls `AsyncTaskHandle::wait_timeout` within a finite deadline, and prints the task's status and output, panicking on failure or timeout.

Background async tasks can also be bridged to a caller-owned `EventBus`: `TaskManager::spawn_async_with_sink` / `spawn_async_timeout_with_sink` accept an `AsyncTaskEventSink`, and the UI thread polls the matching `TaskEventReceiver` (e.g. each frame) and replays what it consumed into the `EventBus` with `publish_into` — so the background worker never holds the non-thread-safe bus, while the async task's lifecycle and output still surface as `task.*` events.

The editor kernel is the single source of truth for document mutations. UI, AI, collaboration, and extensions must eventually submit changes through the same transaction API.
