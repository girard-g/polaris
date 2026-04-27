# Changelog

## [0.2.0] - 2026-04-27

### Changed
- Repository restructured into a Cargo workspace:
  - `polaris-core` (library): retrieval pipeline (`Bank`, `BankSet`,
    `SharedEmbedding`, indexer, search, embedding, db).
  - `polaris-cli` (binary, name `polaris`): CLI + MCP server.
- No user-visible CLI or MCP behavior changes.

### Added
- `polaris_core::Bank` and `polaris_core::BankSet` public API.
- `polaris_core::SharedEmbedding` clonable handle so the ONNX embedding
  model is loaded once and reused across multiple `Bank` instances.
- `Bank::index_diff(changed, removed)` for delta indexing without
  filesystem walk — used by git-driven sync.
- `Bank::index_path_with_progress` to plumb progress callbacks through
  the library API (used by the MCP `index` tool to stream progress
  notifications to the client).
- `IndexOpts` and `SearchOpts` types with sensible defaults.
- `BankConfig::default()` so callers can tune only the fields they care
  about: `BankConfig { repo_root, index_path, ..Default::default() }`.

### Internal
- Multi-DB search fusion logic moved from `cmd_search` into
  `BankSet::search`. The fusion now sorts by raw RRF scores (via
  `SearchEngine::search_raw`) before a single cross-bank
  normalization pass — a correctness fix vs. the prior per-bank
  re-normalize-then-sort approach.
- MCP `PolarisState` simplified: a single `Bank` replaces the
  `read_db`/`write_db` pair. Reads and writes now serialize through
  one mutex; in practice MCP tool calls are serial so the impact is
  small. Restoring read/write parallelism is a future concern.
- `Indexer::index_path` factored: a new `pub(crate) index_files(paths)`
  carries Phase A → B → C, preserving cross-file embedding batching;
  `index_path` is now a thin wrapper that does discovery +
  removal-detection and delegates.
