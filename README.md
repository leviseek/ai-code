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
```

The editor kernel is the single source of truth for document mutations. UI, AI, collaboration, and extensions must eventually submit changes through the same transaction API.
