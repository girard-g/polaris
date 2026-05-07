# Polaris `savings` Command — Design

**Date:** 2026-05-07
**Status:** Approved (pending implementation plan)

## Goal

Provide a `polaris savings` command that shows the cumulative tokens an agent (or CLI user) saved by going through Polaris instead of `grep + read`. Replaces the README's hand-measured marketing numbers with a real, per-user counter.

## Non-goals

- A one-shot benchmark or synthetic estimator. The metric is what *actually* happened, not what *could* happen.
- Tiktoken-accurate token counts. We use the same `~4 chars/token` heuristic the README already uses; the conversion is centralised so a more accurate tokenizer can drop in later.
- Aggregation by query text ("top queries by savings"). Slight phrasing variations make exact-match grouping noisy and prefix grouping fragile; per-query rows live in `--history` instead.
- A separate stats database. The log lives in `polaris.db` next to the index. Wiping the index resets savings — acceptable.
- Backfilling savings for searches that pre-date the upgrade.

## Command surface

```
polaris savings                      # summary
polaris savings --history            # per-query log (newest first)
polaris savings --history --limit 50
polaris savings --output json        # works with both views
```

Exit code 0 on success (including empty log). Non-zero only if `polaris.db` is missing entirely, mirroring `polaris status`.

## Data flow

```
                                     ┌──────────────────────┐
 polaris search ──┐                  │ search_log table     │
                  │   Bank::search   │                      │
 MCP search call ─┼──>  (existing) ──┼─> result + paths     │
                  │                  │                      │
 (library users) ─┘                  └──────────┬───────────┘
                                                │
                                                │ tokio::spawn (fire-and-forget)
                                                ▼
                                         baseline = sum of stat().len()
                                         over unique result paths
                                                │
                                                ▼
                                         INSERT INTO search_log
```

The hot path returns to the caller immediately. Logging is best-effort: a failed insert or stat is `tracing::warn!`-logged and the row is dropped; user-facing search is never affected.

## Schema

New table in `polaris.db`:

```sql
CREATE TABLE IF NOT EXISTS search_log (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  ts              INTEGER NOT NULL,        -- unix seconds, UTC
  source          TEXT    NOT NULL,        -- 'mcp' | 'cli'
  query           TEXT    NOT NULL,
  top_k           INTEGER NOT NULL,
  result_bytes    INTEGER NOT NULL,        -- bytes returned to caller
  baseline_bytes  INTEGER NOT NULL         -- sum of unique result-file sizes
);
CREATE INDEX IF NOT EXISTS idx_search_log_ts ON search_log(ts);
```

Created via the existing `db.rs` schema-version mechanism (one new step: create-if-not-exists). No data migration. `query` is stored verbatim; if redaction becomes necessary later, a `track_queries = false` config flag or a `--redact` flag is a localised change.

## Logging hook

Hooked at every path that produces a user-facing search result: the tail of `Bank::search` covers the common case; if `BankSet::search` (or any future entrypoint) returns results without going through `Bank::search`, that path adds the same hook so no source escapes the log. New helper:

```rust
fn spawn_search_log(
    db: Arc<Db>,
    source: LogSource,                  // Mcp | Cli
    query: String,
    top_k: usize,
    result_bytes: usize,
    result_paths: Vec<PathBuf>,
) {
    tokio::spawn(async move {
        let baseline_bytes: usize = result_paths.iter().filter_map(|p| {
            std::fs::metadata(p).ok().map(|m| m.len() as usize)
        }).sum();
        if let Err(e) = db.insert_search_log(...) {
            tracing::warn!("search log write failed: {e}");
        }
    });
}
```

The CLI path passes `LogSource::Cli`; the MCP server path passes `LogSource::Mcp`. To preserve library ergonomics for non-binary consumers of `polaris-core`, `Bank::search` accepts an `Option<LogSource>` and skips logging when `None`.

## Baseline algorithm

`baseline_bytes` per query = the sum of byte-sizes of the **unique** files appearing in the result set, obtained via `std::fs::metadata().len()` on each path. One stat per unique file; `top_k` files at most.

Rationale: an agent without Polaris would `grep` for the query terms, see those file paths, and `cat` them in full. The result set already gives us the paths — comparing "what we delivered" vs "the full content of those same files" is the most honest framing of the README's claim and avoids the expense of re-running grep on every search.

If a result file is missing (e.g., the user deleted it after indexing), its contribution to the baseline is 0 and a warning is logged once per writer task.

## Token estimation

Stored values are bytes. Display converts via the constant `BYTES_PER_TOKEN: f64 = 4.0`, defined once in `savings.rs`. Numbers ≥ 1000 render with a `K` suffix, ≥ 1_000_000 with `M`. The summary footer reads `Tokens estimated at ~4 chars/token.` so the heuristic is visible.

## Output

**Plain summary:**

```
  polaris  ·  savings

  Total searches      127  (mcp 98 / cli 29)
  Tokens delivered   31.2K
  Baseline           412K
  Tokens saved       381K  ~13× cheaper
  Tracking since     2026-04-09

  Tokens estimated at ~4 chars/token.
```

**Plain history (default limit 20, newest first):**

```
  polaris  ·  savings  ·  history (last 20)

  ts                    src   top_k  delivered  saved  query
  2026-05-07T14:22:11Z  mcp   2      0.3K       3.1K   embedding pipeline
  2026-05-07T14:21:48Z  cli   5      1.1K       9.4K   how does chunking work
  ...
```

Queries are truncated at ~50 chars with an ellipsis in plain mode; full text is available via `--output json`.

**JSON summary:** flat object including a `source_breakdown: { mcp: {...}, cli: {...} }`.

**JSON history:** array of row objects with full untruncated `query`, ISO-8601 `ts`, and integer counts.

**Empty log:** both views print `No searches recorded yet. Run a search to start tracking.` and exit 0.

## Error handling

| Condition | Behaviour |
|---|---|
| `polaris.db` does not exist | `No index found at 'polaris.db'. Run 'polaris index <path>' first.` Exit 1. (Same UX as `polaris status`.) |
| `polaris.db` exists with pre-savings schema | Migration auto-creates the table on open; report works and likely renders the empty-log message. |
| Spawned writer fails (disk full, lock contention, panic, stat error) | `tracing::warn!` with the error; the row is lost; user-facing search is unaffected. |

## Code organisation

- New module `polaris-cli/src/savings.rs` containing pure formatters (`format_summary`, `format_history`) plus a thin `run` orchestrator.
- New `Command::Savings { history: bool, limit: Option<usize>, output: OutputFormat }` variant in `main.rs`.
- New `LogSource` enum and `Db::insert_search_log` / `Db::aggregate_savings` / `Db::recent_search_log` methods in `polaris-core/src/db.rs`.
- New `spawn_search_log` helper, called from the `Bank::search` tail. `Bank::search` accepts `Option<LogSource>` so library consumers opt out by passing `None`.
- No new workspace dependencies (`tokio` and `tracing` are already in tree).

## Testing

`polaris-cli/src/savings.rs`:

- `format_summary` with empty data → "no searches recorded yet" string.
- `format_summary` with a synthetic aggregate (2 mcp + 1 cli, known bytes) → exact rendered output, including the `~Nx cheaper` multiplier.
- `format_history` with synthetic rows → correct header, ordering (newest first), and 50-char query truncation.
- JSON outputs (both views) parsed back via `serde_json::from_str` and asserted on structure.

`polaris-core/src/db.rs` (matching existing test patterns):

- Migration creates `search_log` on a pre-savings DB.
- `insert_search_log` + `aggregate_savings` round-trip with mixed `mcp` / `cli` rows.
- `recent_search_log(limit)` returns newest-first up to `limit`.

Integration test in `polaris-cli/tests/`:

- Spawn a tempdir, run `polaris index`, run two `polaris search` invocations, run `polaris savings`, assert the summary shows 2 CLI searches and `tokens_saved > 0`.
- The integration test needs a deterministic way to know the spawned write has landed before asserting. Two acceptable strategies: (a) `spawn_search_log` returns the `JoinHandle` and the binary drops it while tests `.await` it, or (b) the test polls `aggregate_savings` until the expected row count appears with a short timeout. Either is fine; the implementation plan picks one.

## Documentation

After implementation:

- Add a `### Savings` subsection under "Usage" in `README.md`, briefly describing the command and linking to `polaris savings --history`.
- Add `polaris savings` to `docs/cli.md` with the same flag table style as the other commands.
- Update the README's "Why Polaris — Token Savings" section with a one-line callout: *"Run `polaris savings` to see your own usage."*
