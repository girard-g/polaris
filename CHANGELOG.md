# Changelog

## [Unreleased]

### Added
- `polaris savings` subcommand reporting cumulative tokens saved by
  going through Polaris instead of `grep + read`. Renders a summary
  block by default, a per-query history with `--history`, and JSON
  with `--output json`.
- `search_log` table (schema v3) records one row per CLI and MCP
  search: timestamp, source, query, top_k, delivered bytes, and
  baseline bytes (sum of unique result-file sizes).
- `polaris_core::Bank::log_search` thin wrapper, plus `LogSource`,
  `SavingsAggregate`, `SavingsBySource`, `SavingsCounters`, and
  `SearchLogRow` types re-exported from `polaris_core`.
- `Database::insert_search_log`, `recent_search_log`, and
  `aggregate_savings` query methods.

### Changed
- SQLite schema bumped from v2 to v3. Existing databases migrate
  automatically on first open; the v1→v2→v3 chain is exercised in
  tests.
- CLI and MCP `search` paths now log one `search_log` row per query
  via `tokio::spawn` (off the hot path). The CLI awaits the log
  task before returning so rows aren't lost when `#[tokio::main]`
  drops the runtime at process exit.

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
