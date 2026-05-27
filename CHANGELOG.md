# Changelog

All notable changes to Polaris are documented in this file.

## [Unreleased]

### Added
- **Claude Code auto-index hook.** `polaris setup` now installs a
  `PostToolUse` hook into `.claude/settings.json` that fires after
  `Write`, `Edit`, or `MultiEdit`. When the touched file is `.md`
  and lives under an already-indexed root, it is re-indexed
  automatically — no `polaris watch` needed for Claude Code users.
  Pass `--no-hooks` to opt out; re-run `polaris setup --no-hooks`
  to remove an existing hook. Gate check is ~5 ms; actual re-index
  (when triggered) is ~300 ms.
- **Claude Code auto-search hook (opt-in).** `polaris setup --search-hook`
  installs a `UserPromptSubmit` hook that searches the index on every
  user message and injects the top result as context before Claude
  responds. Two gates prevent pollution: a length gate (5–150 words)
  skips confirmations and error pastes, and a raw RRF score threshold
  drops irrelevant hits. Adds ~1 s latency per qualifying prompt
  (ONNX model load); off by default. Re-running `polaris setup`
  without `--search-hook` removes it.
- `--search-hook` flag on `polaris setup`.
- `polaris hook index` internal subcommand (reads hook payload on
  stdin, re-indexes the touched file). Not intended for direct use.
- `polaris hook search` internal subcommand (reads prompt payload on
  stdin, searches the index, prints the top result to stdout for
  Claude Code context injection). Not intended for direct use.
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
- `polaris setup` now writes a marker-delimited Polaris MCP instruction
  block into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root.
  Pass `--no-agents` to skip them. Existing user content is preserved;
  re-runs only refresh the block.

### Changed
- Hook subcommands (`polaris hook index`, `polaris hook search`) are
  now dispatched before config validation in `main.rs`. A broken
  `polaris.toml` falls back to defaults with a stderr warning instead
  of crashing with a non-zero exit — Claude Code never shows a
  warning banner from hook failures.
- `under_indexed_root` now walks ancestors for relative DB paths:
  if the index only contains `docs/sub/seed.md`, a new file at
  `docs/new.md` is correctly recognized as under the indexed tree.
  Absolute paths still use immediate-parent matching (documented
  known limitation).
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
