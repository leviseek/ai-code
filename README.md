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

Task state and summary APIs are available through `TaskManager::state`, `summary`, `summaries`, and `ids`; pending tasks can be removed and terminal tasks can be cleaned with lifecycle events.

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
