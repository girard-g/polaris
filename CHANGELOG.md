# Changelog

All notable changes to Polaris are documented in this file.

## [2.3.0] - 2026-09-08

Search reported a number that could not mean anything outside its own result
set, and the auto-search hook decided what to inject based on it. This release
replaces that number with an absolute one, gates on it, and adds the tooling to
calibrate the gate against a corpus rather than against judgement.

### Added

- **`polaris eval` measures retrieval quality on your own corpus.** It needs no
  question set from you: sentences are sampled from your indexed chunks and
  re-issued as queries, where the chunk a sentence came from is the known-correct
  answer, giving recall@1, recall@3, recall@3 (file) and MRR. Roughly twenty
  built-in off-topic probes establish separately what your corpus scores when it
  genuinely has no answer, and the gap between those two distributions yields a
  recommended `search_min_similarity`. Sampling is content-addressed, so the same
  corpus produces the same questions on every run and re-chunking does not change
  them. Available as `polaris eval`, as an `eval` MCP tool, and as
  `polaris_core::eval::run`.
- **Probe sets for English and French, chosen by corpus language.** Language is
  detected by stopword frequency. A corpus in neither language yields *no*
  threshold recommendation rather than a wrong one — English probes against a
  French corpus are off-topic for the wrong reason and would suggest a threshold
  far too permissive. `[eval] probes` overrides the built-in set for any language;
  writing an off-topic sentence requires no knowledge of your documentation.
- **`search_min_similarity` config key.** The confidence floor described under
  Changed below. Default 0.63.
- **`[eval]` config section** with `sample_size` and `probes`.
- **`polaris setup` writes a starter `polaris.toml`** and gitignores it. One key
  is live — `search_min_similarity` — followed by every other setting commented
  out as an inline reference. Commenting is deliberate: a commented key stays
  inert and keeps tracking its built-in default across upgrades, whereas a key
  written out pins the project to today's value. An existing `polaris.toml` is
  never rewritten or merged into.
- **Update-available notice.** The CLI prints one when a newer release exists,
  and the MCP server surfaces it once per `serve` session. Both the background
  GitHub fetch and the banner are suppressed by `POLARIS_NO_UPDATE_CHECK` or by
  `CI`.

### Changed

- **Search scores are the absolute query-chunk cosine, no longer normalised per
  call.** Previously every score was divided by the result set's maximum, so the
  top hit read `1.000` by construction — "best of this set" wearing the costume
  of "certain". A query the corpus had nothing to say about still came back
  looking certain. **If you parse `polaris search --output json`, the `score`
  field no longer tops out at 1.0 and is comparable across queries.**
- **The MCP `search` tool and the auto-search hook can now decline to answer.**
  Below `search_min_similarity` the tool returns "No reliable context found"
  with both the best match and the threshold, and the hook stays silent. An agent
  cannot tell a weak match from a strong one once the text is in its context, and
  acting on the wrong document costs more than the search saved.
- **The two consumers gate on different quantities, deliberately.** The MCP tool
  asks whether the corpus covers the query at all, using the highest cosine in
  the KNN pool — a number fixed by the query and corpus, not by the caller's
  `top_k`. The hook hands over a single chunk invisibly, with no scores to weigh,
  so it gates on the cosine of *that chunk*.
- **One result ordering everywhere.** MMR selects which chunks survive; the
  engine then presents them best-first. That sort now lives in one place, so
  `polaris search`, the MCP tool and `polaris eval` can no longer disagree about
  the order of identical results.
- **Database schema v3 → v4**, adding the `eval_run` table. Existing databases
  migrate automatically on open; no re-index and no action required.

### Fixed

- **The auto-search hook injected irrelevant documentation.** It gated on the raw
  RRF total, which is rank fusion: a top hit scores about 0.033 whether or not it
  answers the prompt, and `heading_boost` is larger than that entire range. The
  prompt "how do I bake sourdough bread" cleared the gate and injected a README
  refactoring plan. It now gates on cosine similarity.
- **`polaris index .` from a subdirectory could silently wipe the index.** With a
  relative root every stored row matched the prefix, and the removal loop then
  deleted every row indexed from a different working directory. The MCP `index`
  tool took the same route. Removal is now confined to directories the walk
  actually descended into.
- **Hybrid search silently degraded to vector-only on punctuated queries.** The
  FTS5 sanitizer missed `.`, `,` and `/`, so ordinary queries raised syntax
  errors that were swallowed into an empty BM25 list. Every token is now quoted
  as a string literal.
- **`polaris savings` reported zero with a `--db` outside the corpus**, because
  the baseline resolved stored relative paths against the database file's parent
  rather than the working directory.
- **Windows: hook commands were written with POSIX quoting**, which `cmd.exe`
  passes through literally, so an install under a path containing spaces produced
  a hook that could not run. Path absoluteness is also now decided on the stored
  key convention rather than `Path::is_absolute`, which reports the wrong answer
  for `/abs/x.md` on Windows and inverted every purge-gate decision.
- **Config validation was unreachable from the hook and library entry points**,
  so an out-of-range `embedding_dim` created a vector table at that dimension and
  failed every later insert with an opaque SQLite error. A `max_chunk_tokens`
  near `usize::MAX` also overflowed its bound check.
- **`polaris index docs/` with a trailing slash purged nothing**, silently, since
  the prefix built as `docs//` and matched no stored key.

### Internal

Two pre-existing test defects fixed: two setup tests relied on another test
having registered the sqlite-vec extension and failed when run alone, and hook
command assertions compared raw strings rather than parsed tokens, which broke
on Windows. Note that `cargo test` remains unreliable in parallel — several
`setup` tests race on the process-wide working directory — so run it with
`--test-threads=1`.

## 2.2 series

> These entries were written while the work was in progress and were never moved
> out of "Unreleased" when the 2.2.x tags were cut, so they describe features
> that shipped somewhere in the 2.2 series. The exact tag for each is not
> recorded and has not been guessed at; they are kept verbatim.


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
