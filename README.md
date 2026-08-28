# NextIDE

NextIDE is a local-first, capability-oriented coding IDE.

## Workspace status

The repository currently contains the first Phase 0 vertical slice:

- Rust workspace
- transactional document model
- undo/redo
- snapshot/recovery primitives
- minimal CLI smoke path

## Development

```powershell
cargo test --workspace
cargo run -p nextide-cli -- smoke
```

The editor kernel is the single source of truth for document mutations. UI, AI, collaboration, and extensions must eventually submit changes through the same transaction API.
