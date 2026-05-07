# Polaris `savings` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `polaris savings` subcommand that reports cumulative tokens saved by going through Polaris instead of `grep + read`. Every CLI and MCP search logs one row to a `search_log` table; the command aggregates and renders.

**Architecture:** Synchronous DB methods in `polaris-core` (`insert_search_log`, `aggregate_savings`, `recent_search_log`) plus a thin `Bank::log_search` wrapper. Logging is fired off the hot path via `tokio::spawn` from `polaris-cli` so `polaris-core` stays tokio-free. Pure formatters (`format_summary`, `format_history`) live in `polaris-cli/src/savings.rs` and are unit-tested directly; the orchestrator gets a tempdir-based integration test.

**Tech Stack:** Rust 2024 (workspace edition), `clap` derive, `rusqlite` (already in tree), `tokio` (already in `polaris-cli`), `tempfile` (dev-dep, already present).

**Spec refinement note.** The spec proposed `Bank::search` accepting `Option<LogSource>`. To avoid (a) changing the README-documented `Bank::search` signature and (b) pulling tokio into `polaris-core`, this plan instead adds `Bank::log_search` as a separate synchronous method. The CLI and MCP layers call `bank.search(...)` first, then `tokio::spawn` a task that computes the baseline (file `stat`s) and calls `bank.log_search(...)`. Behaviour is identical to the spec; the seam is just placed differently.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `polaris-core/src/db.rs` | Modify | Bump `SCHEMA_VERSION` to `"3"`, add `migrate_v2_to_v3`, add `search_log` table to `create_schema`, define `LogSource` enum + `SavingsAggregate`/`SavingsBySource`/`SearchLogRow` structs, add `insert_search_log`/`aggregate_savings`/`recent_search_log` methods + tests. |
| `polaris-core/src/bank.rs` | Modify | Add `Bank::log_search` thin wrapper. |
| `polaris-core/src/lib.rs` | Modify | Re-export `LogSource`, `SavingsAggregate`, `SavingsBySource`, `SearchLogRow`. |
| `polaris-cli/src/savings.rs` | Create | Pure formatters (`format_summary`, `format_history`), `spawn_search_log` helper, `run` orchestrator, unit tests, integration test. |
| `polaris-cli/src/main.rs` | Modify | `mod savings;`, `Command::Savings { history, limit, output }` variant, dispatch arm; capture primary bank in `cmd_search` and call `spawn_search_log` after `set.search`. |
| `polaris-cli/src/mcp/server.rs` | Modify | Call `spawn_search_log` after `bank.search` in the `search` tool handler. |
| `README.md` | Modify | Add `### Savings` subsection under Usage; one-line callout in the Token Savings section. |
| `docs/cli.md` | Modify | Add `polaris savings` reference entry. |

---

## Task 1: Add `LogSource` enum, `search_log` schema, and v2→v3 migration

**Files:**
- Modify: `polaris-core/src/db.rs`

- [ ] **Step 1: Write the failing test for v2→v3 migration**

In `polaris-core/src/db.rs`, in the `mod tests { ... }` block (after the existing `migration_v1_to_v2_backfills_fts` test), add:

```rust
#[test]
fn migration_v2_to_v3_creates_search_log() {
    INIT.call_once(register_vec_extension);
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v2_to_v3.db");

    // Simulate a v2 database: full v2 schema, schema_version='2', no search_log table.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch("
            CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE documents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                content_hash TEXT NOT NULL,
                title TEXT,
                indexed_at TEXT NOT NULL,
                file_size INTEGER NOT NULL
            );
            CREATE TABLE chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                heading_context TEXT NOT NULL DEFAULT '',
                start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                content, heading_context,
                content='chunks', content_rowid='id'
            );
        ").unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE vec_chunks USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[4] distance_metric=cosine
            );"
        ).unwrap();
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES
                ('schema_version', '2'),
                ('embedding_dim',  '4'),
                ('model_id',       'test')",
            [],
        ).unwrap();
    }

    // Open with our Database — should trigger v2→v3 migration.
    let db = Database::open(&db_path, 4, "test").unwrap();

    // search_log table should exist after migration.
    let count: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM search_log", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0);

    // schema_version metadata should now be '3'.
    let version: String = db.conn.query_row(
        "SELECT value FROM metadata WHERE key='schema_version'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(version, "3");
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p polaris-core migration_v2_to_v3_creates_search_log
```

Expected: FAIL — either `search_log` table missing or migration didn't update version.

- [ ] **Step 3: Bump `SCHEMA_VERSION` and add migration**

Edit `polaris-core/src/db.rs`:

Change line 9:

```rust
const SCHEMA_VERSION: &str = "3";
```

In `apply_migrations` (around line 219), chain the new migration. Replace the existing body with:

```rust
fn apply_migrations(&self) -> Result<()> {
    let version: Option<String> = self
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;

    if version.as_deref() == Some("1") {
        self.migrate_v1_to_v2()?;
    }

    let version: Option<String> = self
        .conn
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;

    if version.as_deref() == Some("2") {
        self.migrate_v2_to_v3()?;
    }
    Ok(())
}
```

After `migrate_v1_to_v2` (around line 254), add:

```rust
fn migrate_v2_to_v3(&self) -> Result<()> {
    self.conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS search_log (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            ts              INTEGER NOT NULL,
            source          TEXT    NOT NULL,
            query           TEXT    NOT NULL,
            top_k           INTEGER NOT NULL,
            result_bytes    INTEGER NOT NULL,
            baseline_bytes  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_search_log_ts ON search_log(ts);",
    )?;
    self.conn.execute(
        "UPDATE metadata SET value='3' WHERE key='schema_version'",
        [],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Add `search_log` to fresh-DB `create_schema`**

In `create_schema` (around line 256), inside the `execute_batch` block right after the `chunks_fts` virtual-table creation, append:

```rust
            CREATE TABLE search_log (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                ts              INTEGER NOT NULL,
                source          TEXT    NOT NULL,
                query           TEXT    NOT NULL,
                top_k           INTEGER NOT NULL,
                result_bytes    INTEGER NOT NULL,
                baseline_bytes  INTEGER NOT NULL
            );

            CREATE INDEX idx_search_log_ts ON search_log(ts);
```

(Inside the existing string literal — fits between `chunks_fts` and the closing `");"` on line 286.)

- [ ] **Step 5: Add `LogSource` enum near the top of `db.rs`**

Just below the `pub struct Database { ... }` block (around line 16), add:

```rust
/// Source tag for a `search_log` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    Cli,
    Mcp,
}

impl LogSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogSource::Cli => "cli",
            LogSource::Mcp => "mcp",
        }
    }
}

impl std::str::FromStr for LogSource {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s {
            "cli" => Ok(LogSource::Cli),
            "mcp" => Ok(LogSource::Mcp),
            _ => Err(()),
        }
    }
}
```

- [ ] **Step 6: Run test, verify it passes**

```bash
cargo test -p polaris-core migration_v2_to_v3_creates_search_log
```

Expected: PASS. Also re-run the v1 test to confirm chained migrations still work:

```bash
cargo test -p polaris-core migration_v1_to_v2_backfills_fts
```

Expected: PASS.

- [ ] **Step 7: Confirm full crate builds and tests pass**

```bash
cargo check -p polaris-core
cargo test -p polaris-core
```

Expected: clean build, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add polaris-core/src/db.rs
git commit -m "feat(savings): add search_log schema and v2->v3 migration"
```

---

## Task 2: `Database::insert_search_log`

**Files:**
- Modify: `polaris-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

In the `mod tests` block of `polaris-core/src/db.rs`, add:

```rust
#[test]
fn insert_search_log_round_trip() {
    INIT.call_once(register_vec_extension);
    let db = Database::open_in_memory(4, "test-model").unwrap();

    db.insert_search_log(
        1_700_000_000,
        LogSource::Cli,
        "how does chunking work",
        5,
        300,
        9_400,
    ).unwrap();

    let rows = db.recent_search_log(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, LogSource::Cli);
    assert_eq!(rows[0].query, "how does chunking work");
    assert_eq!(rows[0].top_k, 5);
    assert_eq!(rows[0].result_bytes, 300);
    assert_eq!(rows[0].baseline_bytes, 9_400);
    assert_eq!(rows[0].ts, 1_700_000_000);
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p polaris-core insert_search_log_round_trip
```

Expected: FAIL — `insert_search_log`, `recent_search_log`, and `SearchLogRow` not defined.

- [ ] **Step 3: Add `SearchLogRow` struct**

In `polaris-core/src/db.rs`, near the other DB record structs (after `Bm25Result` around line 81), add:

```rust
/// A single row from the `search_log` table.
#[derive(Debug, Clone)]
pub struct SearchLogRow {
    pub id: i64,
    pub ts: i64,
    pub source: LogSource,
    pub query: String,
    pub top_k: usize,
    pub result_bytes: usize,
    pub baseline_bytes: usize,
}
```

- [ ] **Step 4: Implement `insert_search_log` and a stub `recent_search_log`**

Anywhere inside `impl Database { ... }` (e.g., right after `get_stats`), add:

```rust
/// Append one row to the `search_log` table.
pub fn insert_search_log(
    &self,
    ts: i64,
    source: LogSource,
    query: &str,
    top_k: usize,
    result_bytes: usize,
    baseline_bytes: usize,
) -> Result<()> {
    self.conn.execute(
        "INSERT INTO search_log (ts, source, query, top_k, result_bytes, baseline_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ts,
            source.as_str(),
            query,
            top_k as i64,
            result_bytes as i64,
            baseline_bytes as i64,
        ],
    )?;
    Ok(())
}

/// Return the most recent `limit` rows from `search_log`, newest first.
pub fn recent_search_log(&self, limit: usize) -> Result<Vec<SearchLogRow>> {
    let mut stmt = self.conn.prepare(
        "SELECT id, ts, source, query, top_k, result_bytes, baseline_bytes
         FROM search_log
         ORDER BY ts DESC, id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| {
        let source_str: String = r.get(2)?;
        let source = source_str.parse::<LogSource>().unwrap_or(LogSource::Cli);
        Ok(SearchLogRow {
            id: r.get(0)?,
            ts: r.get(1)?,
            source,
            query: r.get(3)?,
            top_k: r.get::<_, i64>(4)? as usize,
            result_bytes: r.get::<_, i64>(5)? as usize,
            baseline_bytes: r.get::<_, i64>(6)? as usize,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
```

- [ ] **Step 5: Run test, verify it passes**

```bash
cargo test -p polaris-core insert_search_log_round_trip
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add polaris-core/src/db.rs
git commit -m "feat(savings): add Database::insert_search_log + recent_search_log"
```

---

## Task 3: `Database::aggregate_savings`

**Files:**
- Modify: `polaris-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

In the `mod tests` block of `polaris-core/src/db.rs`, add:

```rust
#[test]
fn aggregate_savings_empty_db() {
    INIT.call_once(register_vec_extension);
    let db = Database::open_in_memory(4, "test-model").unwrap();
    let agg = db.aggregate_savings().unwrap();
    assert_eq!(agg.total_searches, 0);
    assert_eq!(agg.total_result_bytes, 0);
    assert_eq!(agg.total_baseline_bytes, 0);
    assert_eq!(agg.tracking_since_ts, None);
    assert_eq!(agg.by_source.cli.searches, 0);
    assert_eq!(agg.by_source.mcp.searches, 0);
}

#[test]
fn aggregate_savings_mixed_sources() {
    INIT.call_once(register_vec_extension);
    let db = Database::open_in_memory(4, "test-model").unwrap();

    db.insert_search_log(100, LogSource::Cli, "q1", 5, 200, 5_000).unwrap();
    db.insert_search_log(200, LogSource::Mcp, "q2", 2, 100, 3_000).unwrap();
    db.insert_search_log(300, LogSource::Mcp, "q3", 3, 150, 4_000).unwrap();

    let agg = db.aggregate_savings().unwrap();
    assert_eq!(agg.total_searches, 3);
    assert_eq!(agg.total_result_bytes, 450);
    assert_eq!(agg.total_baseline_bytes, 12_000);
    assert_eq!(agg.tracking_since_ts, Some(100));
    assert_eq!(agg.by_source.cli.searches, 1);
    assert_eq!(agg.by_source.cli.result_bytes, 200);
    assert_eq!(agg.by_source.cli.baseline_bytes, 5_000);
    assert_eq!(agg.by_source.mcp.searches, 2);
    assert_eq!(agg.by_source.mcp.result_bytes, 250);
    assert_eq!(agg.by_source.mcp.baseline_bytes, 7_000);
}
```

- [ ] **Step 2: Run tests, verify they fail**

```bash
cargo test -p polaris-core aggregate_savings_
```

Expected: FAIL — `aggregate_savings`, `SavingsAggregate`, `SavingsBySource` not defined.

- [ ] **Step 3: Add aggregate structs**

In `polaris-core/src/db.rs`, after `SearchLogRow` (added in Task 2), add:

```rust
/// Per-source counters in a `SavingsAggregate`.
#[derive(Debug, Clone, Default)]
pub struct SavingsCounters {
    pub searches: usize,
    pub result_bytes: usize,
    pub baseline_bytes: usize,
}

/// Per-source breakdown of `SavingsAggregate`.
#[derive(Debug, Clone, Default)]
pub struct SavingsBySource {
    pub cli: SavingsCounters,
    pub mcp: SavingsCounters,
}

/// Cumulative savings counters returned by `Database::aggregate_savings`.
#[derive(Debug, Clone, Default)]
pub struct SavingsAggregate {
    pub total_searches: usize,
    pub total_result_bytes: usize,
    pub total_baseline_bytes: usize,
    /// Unix-seconds timestamp of the earliest logged row, or `None` if empty.
    pub tracking_since_ts: Option<i64>,
    pub by_source: SavingsBySource,
}
```

- [ ] **Step 4: Implement `aggregate_savings`**

Anywhere inside `impl Database { ... }` (e.g., right after `recent_search_log`), add:

```rust
/// Aggregate `search_log` into a `SavingsAggregate`.
pub fn aggregate_savings(&self) -> Result<SavingsAggregate> {
    let mut agg = SavingsAggregate::default();

    let mut stmt = self.conn.prepare(
        "SELECT source, COUNT(*), COALESCE(SUM(result_bytes), 0), COALESCE(SUM(baseline_bytes), 0)
         FROM search_log GROUP BY source",
    )?;
    let rows = stmt.query_map([], |r| {
        let source: String = r.get(0)?;
        let count: i64 = r.get(1)?;
        let res: i64 = r.get(2)?;
        let base: i64 = r.get(3)?;
        Ok((source, count as usize, res as usize, base as usize))
    })?;

    for row in rows {
        let (source, count, res, base) = row?;
        agg.total_searches += count;
        agg.total_result_bytes += res;
        agg.total_baseline_bytes += base;
        let bucket = match source.parse::<LogSource>() {
            Ok(LogSource::Cli) => &mut agg.by_source.cli,
            Ok(LogSource::Mcp) => &mut agg.by_source.mcp,
            Err(()) => continue, // unknown source — skip
        };
        bucket.searches = count;
        bucket.result_bytes = res;
        bucket.baseline_bytes = base;
    }

    agg.tracking_since_ts = self
        .conn
        .query_row("SELECT MIN(ts) FROM search_log", [], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten();

    Ok(agg)
}
```

- [ ] **Step 5: Run tests, verify they pass**

```bash
cargo test -p polaris-core aggregate_savings_
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add polaris-core/src/db.rs
git commit -m "feat(savings): add Database::aggregate_savings"
```

---

## Task 4: `recent_search_log` ordering + limit test

**Files:**
- Modify: `polaris-core/src/db.rs`

The method itself was implemented in Task 2. This task adds explicit ordering and limit coverage.

- [ ] **Step 1: Write the failing test**

In `mod tests`:

```rust
#[test]
fn recent_search_log_newest_first_with_limit() {
    INIT.call_once(register_vec_extension);
    let db = Database::open_in_memory(4, "test-model").unwrap();

    db.insert_search_log(100, LogSource::Cli, "first", 5, 100, 1_000).unwrap();
    db.insert_search_log(200, LogSource::Cli, "second", 5, 100, 1_000).unwrap();
    db.insert_search_log(300, LogSource::Mcp, "third", 5, 100, 1_000).unwrap();

    let rows = db.recent_search_log(2).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].query, "third");
    assert_eq!(rows[1].query, "second");
}
```

- [ ] **Step 2: Run, verify it passes**

```bash
cargo test -p polaris-core recent_search_log_newest_first_with_limit
```

Expected: PASS (Task 2 implementation already orders by `ts DESC, id DESC`).

- [ ] **Step 3: Commit**

```bash
git add polaris-core/src/db.rs
git commit -m "test(savings): cover recent_search_log ordering and limit"
```

---

## Task 5: `Bank::log_search` thin wrapper + re-exports

**Files:**
- Modify: `polaris-core/src/bank.rs`
- Modify: `polaris-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`Bank::open` requires a real `SharedEmbedding` which downloads a ~137 MB ONNX model. The codebase already gates such tests with `#[ignore]` (see `polaris-core/src/embedding.rs::shared_embedding_clone_does_not_reload`). Follow the same convention: a single `#[ignore]`-gated end-to-end test on the wrapper.

In `polaris-core/src/bank.rs`, scroll to the bottom. If a `mod tests` block exists, add to it; otherwise create one. Append:

```rust
#[cfg(test)]
mod tests_log_search {
    use super::*;
    use crate::db::LogSource;

    #[test]
    #[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
    fn bank_log_search_inserts_row_visible_via_aggregate() {
        crate::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("bank.db");

        let embed = crate::embedding::SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
        let bank = Bank::open(
            BankConfig {
                repo_root: dir.path().to_path_buf(),
                index_path: index_path.clone(),
                embedding_dim: 64,
                model_id: "nomic-embed-text-v1.5".into(),
                ..Default::default()
            },
            embed,
        ).unwrap();

        bank.log_search(LogSource::Cli, "q", 5, 200, 5_000, 1_700_000_000).unwrap();

        // Re-open the underlying DB to check the row landed.
        let db = crate::db::Database::open(&index_path, 64, "nomic-embed-text-v1.5").unwrap();
        let agg = db.aggregate_savings().unwrap();
        assert_eq!(agg.total_searches, 1);
        assert_eq!(agg.total_result_bytes, 200);
        assert_eq!(agg.total_baseline_bytes, 5_000);
    }
}
```

The test is `#[ignore]`-gated; CI runs `cargo test` without `--include-ignored`. The Database-layer tests in Tasks 2–4 already cover the underlying logic; this test exists to verify the one-line `Bank::log_search` wrapper compiles and forwards correctly when actually exercised. Run it manually before declaring the task done:

```bash
cargo test -p polaris-core bank_log_search_inserts_row_visible_via_aggregate -- --ignored
```

- [ ] **Step 2: Run, verify it fails to compile**

```bash
cargo test -p polaris-core --no-run
```

Expected: FAIL — `Bank::log_search` not defined.

- [ ] **Step 3: Implement `Bank::log_search`**

In `polaris-core/src/bank.rs`, inside `impl Bank { ... }` (e.g., after `search_raw` around line 173), add:

```rust
/// Append one row to the search log.
///
/// Used by the CLI and MCP server to record per-search counters.
/// Library users who do not need savings tracking can ignore this method.
pub fn log_search(
    &self,
    source: crate::db::LogSource,
    query: &str,
    top_k: usize,
    result_bytes: usize,
    baseline_bytes: usize,
    ts: i64,
) -> Result<()> {
    let db = self.inner.db.lock().expect("bank db poisoned");
    db.insert_search_log(ts, source, query, top_k, result_bytes, baseline_bytes)
}
```

- [ ] **Step 4: Re-export new types from `lib.rs`**

Edit `polaris-core/src/lib.rs`. Replace line 13 with:

```rust
pub use db::{
    ChunkRecord, Database, DbStats, LogSource, SavingsAggregate, SavingsBySource,
    SavingsCounters, SearchLogRow, SearchResult, register_vec_extension,
};
```

- [ ] **Step 5: Run, verify it builds and the gated test passes when run explicitly**

```bash
cargo check -p polaris-core
cargo test -p polaris-core                               # default tests still pass; the gated one is skipped
cargo test -p polaris-core bank_log_search_inserts_row_visible_via_aggregate -- --ignored
```

Expected: clean build; default tests PASS; the `--ignored` invocation also PASSes (the model download succeeds — required at least once locally to confirm).

- [ ] **Step 6: Commit**

```bash
git add polaris-core/src/bank.rs polaris-core/src/lib.rs
git commit -m "feat(savings): add Bank::log_search and re-export savings types"
```

---

## Task 6: `polaris-cli/src/savings.rs` — pure formatters and types

**Files:**
- Create: `polaris-cli/src/savings.rs`

This task creates the module with formatters and report structs, but no command wiring yet. The orchestrator stub returns `Ok(())` after consuming its inputs. Tests cover the formatters directly.

- [ ] **Step 1: Create the file with the report structs and stub formatters**

Write `polaris-cli/src/savings.rs`:

```rust
//! `polaris savings` — render cumulative tokens-saved analytics from `search_log`.

use std::path::Path;

use console::style;
use polaris_core::db::{Database, SavingsAggregate, SearchLogRow};
use polaris_core::error::{PolarisError, Result};

/// Heuristic: ~4 chars per token (matches README's existing claim).
pub const BYTES_PER_TOKEN: f64 = 4.0;

/// Render `SavingsAggregate` as the plain-text summary block.
pub fn format_summary(agg: &SavingsAggregate) -> String {
    if agg.total_searches == 0 {
        return "No searches recorded yet. Run a search to start tracking.\n".to_string();
    }

    let delivered_tok = bytes_to_tokens(agg.total_result_bytes);
    let baseline_tok = bytes_to_tokens(agg.total_baseline_bytes);
    let saved_tok = baseline_tok.saturating_sub(delivered_tok);
    let multiplier = if delivered_tok > 0 {
        baseline_tok as f64 / delivered_tok as f64
    } else {
        0.0
    };

    let mut out = String::new();
    out.push_str(&format!("\n  {}  ·  savings\n\n", style("polaris").bold()));
    out.push_str(&format!(
        "  Total searches      {}  (mcp {} / cli {})\n",
        agg.total_searches, agg.by_source.mcp.searches, agg.by_source.cli.searches,
    ));
    out.push_str(&format!("  Tokens delivered   {}\n", fmt_count(delivered_tok)));
    out.push_str(&format!("  Baseline           {}\n", fmt_count(baseline_tok)));
    out.push_str(&format!(
        "  Tokens saved       {}  ~{:.1}× cheaper\n",
        fmt_count(saved_tok),
        multiplier,
    ));
    if let Some(ts) = agg.tracking_since_ts {
        out.push_str(&format!("  Tracking since     {}\n", fmt_iso_date(ts)));
    }
    out.push_str("\n  Tokens estimated at ~4 chars/token.\n");
    out
}

/// Render the most recent rows as the plain-text history table.
pub fn format_history(rows: &[SearchLogRow]) -> String {
    if rows.is_empty() {
        return "No searches recorded yet. Run a search to start tracking.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "\n  {}  ·  savings  ·  history (last {})\n\n",
        style("polaris").bold(),
        rows.len(),
    ));
    out.push_str("  ts                    src   top_k  delivered  saved  query\n");
    for r in rows {
        let delivered = bytes_to_tokens(r.result_bytes);
        let saved = bytes_to_tokens(r.baseline_bytes).saturating_sub(delivered);
        out.push_str(&format!(
            "  {}  {:<4}  {:<5}  {:<9}  {:<5}  {}\n",
            fmt_iso_seconds(r.ts),
            r.source.as_str(),
            r.top_k,
            fmt_count(delivered),
            fmt_count(saved),
            truncate(&r.query, 50),
        ));
    }
    out
}

fn bytes_to_tokens(bytes: usize) -> usize {
    ((bytes as f64) / BYTES_PER_TOKEN).round() as usize
}

fn fmt_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_iso_date(ts: i64) -> String {
    fmt_iso_seconds(ts).split('T').next().unwrap_or("").to_string()
}

fn fmt_iso_seconds(ts: i64) -> String {
    // RFC 3339 / ISO-8601 in UTC, second precision. Handles ts ≥ 0.
    let secs = ts.max(0) as u64;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let (y, m, d) = days_to_ymd(days as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hour, minute, second)
}

/// Convert "days since 1970-01-01" to a (year, month, day) tuple. Civil-from-days
/// algorithm by Howard Hinnant, public domain.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Entry point for the `savings` command.
///
/// Reads the savings log from `db_path` and prints the rendered output.
pub fn run(
    db_path: &Path,
    embedding_dim: usize,
    model_id: &str,
    history: bool,
    limit: usize,
    json: bool,
) -> Result<()> {
    if !db_path.exists() {
        return Err(PolarisError::Indexing(format!(
            "no index at {}  —  run `polaris index <path>` first",
            db_path.display()
        )));
    }

    let db = Database::open(db_path, embedding_dim, model_id)?;

    if history {
        let rows = db.recent_search_log(limit)?;
        if json {
            let val = serde_json::to_string_pretty(&history_json(&rows))
                .map_err(|e| PolarisError::Indexing(format!("json encode failed: {e}")))?;
            println!("{val}");
        } else {
            print!("{}", format_history(&rows));
        }
    } else {
        let agg = db.aggregate_savings()?;
        if json {
            let val = serde_json::to_string_pretty(&summary_json(&agg))
                .map_err(|e| PolarisError::Indexing(format!("json encode failed: {e}")))?;
            println!("{val}");
        } else {
            print!("{}", format_summary(&agg));
        }
    }
    Ok(())
}

fn summary_json(agg: &SavingsAggregate) -> serde_json::Value {
    serde_json::json!({
        "total_searches": agg.total_searches,
        "total_result_bytes": agg.total_result_bytes,
        "total_baseline_bytes": agg.total_baseline_bytes,
        "tracking_since_ts": agg.tracking_since_ts,
        "source_breakdown": {
            "mcp": {
                "searches": agg.by_source.mcp.searches,
                "result_bytes": agg.by_source.mcp.result_bytes,
                "baseline_bytes": agg.by_source.mcp.baseline_bytes,
            },
            "cli": {
                "searches": agg.by_source.cli.searches,
                "result_bytes": agg.by_source.cli.result_bytes,
                "baseline_bytes": agg.by_source.cli.baseline_bytes,
            },
        },
    })
}

fn history_json(rows: &[SearchLogRow]) -> serde_json::Value {
    let arr: Vec<_> = rows.iter().map(|r| {
        serde_json::json!({
            "id": r.id,
            "ts": fmt_iso_seconds(r.ts),
            "source": r.source.as_str(),
            "query": r.query,
            "top_k": r.top_k,
            "result_bytes": r.result_bytes,
            "baseline_bytes": r.baseline_bytes,
        })
    }).collect();
    serde_json::Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use polaris_core::db::{LogSource, SavingsAggregate, SavingsBySource, SavingsCounters, SearchLogRow};

    fn agg_with(cli: (usize, usize, usize), mcp: (usize, usize, usize), since: Option<i64>) -> SavingsAggregate {
        SavingsAggregate {
            total_searches: cli.0 + mcp.0,
            total_result_bytes: cli.1 + mcp.1,
            total_baseline_bytes: cli.2 + mcp.2,
            tracking_since_ts: since,
            by_source: SavingsBySource {
                cli: SavingsCounters { searches: cli.0, result_bytes: cli.1, baseline_bytes: cli.2 },
                mcp: SavingsCounters { searches: mcp.0, result_bytes: mcp.1, baseline_bytes: mcp.2 },
            },
        }
    }

    #[test]
    fn format_summary_empty() {
        let agg = SavingsAggregate::default();
        let out = format_summary(&agg);
        assert!(out.contains("No searches recorded yet"));
    }

    #[test]
    fn format_summary_populated() {
        let agg = agg_with((1, 4_000, 100_000), (2, 8_000, 200_000), Some(1_700_000_000));
        let out = format_summary(&agg);
        assert!(out.contains("Total searches      3"));
        assert!(out.contains("(mcp 2 / cli 1)"));
        assert!(out.contains("Tokens delivered   3.0K"));
        assert!(out.contains("Baseline           75.0K"));
        // 75K - 3K = 72K saved; multiplier = 75/3 = 25.0
        assert!(out.contains("Tokens saved       72.0K"));
        assert!(out.contains("~25.0× cheaper"));
        assert!(out.contains("Tracking since"));
        assert!(out.contains("Tokens estimated at ~4 chars/token"));
    }

    #[test]
    fn format_history_empty() {
        let out = format_history(&[]);
        assert!(out.contains("No searches recorded yet"));
    }

    #[test]
    fn format_history_truncates_long_queries() {
        let row = SearchLogRow {
            id: 1,
            ts: 1_700_000_000,
            source: LogSource::Cli,
            query: "a".repeat(80),
            top_k: 5,
            result_bytes: 400,
            baseline_bytes: 8_000,
        };
        let out = format_history(&[row]);
        assert!(out.contains("…"));
        // Truncated string itself should be present (49 a's + ellipsis).
        let line_with_query = out.lines().find(|l| l.contains("a")).unwrap();
        assert!(line_with_query.contains(&format!("{}…", "a".repeat(49))));
    }

    #[test]
    fn fmt_count_thresholds() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1.0K");
        assert_eq!(fmt_count(31_200), "31.2K");
        assert_eq!(fmt_count(1_500_000), "1.5M");
    }

    #[test]
    fn fmt_iso_seconds_known_value() {
        // 2023-11-14T22:13:20Z (well-known 1700000000 epoch).
        assert_eq!(fmt_iso_seconds(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn summary_json_shape() {
        let agg = agg_with((1, 100, 1_000), (2, 200, 2_000), Some(42));
        let v = summary_json(&agg);
        assert_eq!(v["total_searches"], 3);
        assert_eq!(v["source_breakdown"]["cli"]["searches"], 1);
        assert_eq!(v["source_breakdown"]["mcp"]["searches"], 2);
        assert_eq!(v["tracking_since_ts"], 42);
    }
}
```

- [ ] **Step 2: Run unit tests, verify they pass**

```bash
cargo test -p polaris-cli --lib savings::
```

Expected: PASS.

- [ ] **Step 3: Run, verify clean**

```bash
cargo check -p polaris-core -p polaris-cli
cargo test -p polaris-cli --lib savings::
```

Expected: clean build, all savings tests PASS.

- [ ] **Step 4: Commit**

```bash
git add polaris-cli/src/savings.rs
git commit -m "feat(savings): add savings module formatters and JSON encoders"
```

---

## Task 7: Wire `polaris savings` command surface

**Files:**
- Modify: `polaris-cli/src/main.rs`
- Modify: `polaris-cli/src/savings.rs` (add integration test)

- [ ] **Step 1: Write the failing integration test**

Add to the bottom of `polaris-cli/src/savings.rs`, inside the existing `mod tests`:

```rust
#[test]
fn savings_run_summary_against_seeded_db() {
    polaris_core::db::register_vec_extension();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("savings.db");

    {
        let db = Database::open(&db_path, 4, "test-model").unwrap();
        db.insert_search_log(1_700_000_000, LogSource::Cli, "q1", 5, 400, 8_000).unwrap();
        db.insert_search_log(1_700_000_100, LogSource::Mcp, "q2", 2, 200, 4_000).unwrap();
    }

    // Capture stdout via a pipe-style helper. For simplicity we just call run()
    // and verify the underlying aggregate; the rendered formatting is covered by
    // format_summary tests above.
    run(&db_path, 4, "test-model", false, 20, false).unwrap();

    // Re-open and assert the data is what we expect.
    let db = Database::open(&db_path, 4, "test-model").unwrap();
    let agg = db.aggregate_savings().unwrap();
    assert_eq!(agg.total_searches, 2);
    assert_eq!(agg.by_source.cli.searches, 1);
    assert_eq!(agg.by_source.mcp.searches, 1);
}

#[test]
fn savings_run_errors_when_db_missing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.db");
    let err = run(&missing, 512, "nomic-embed-text-v1.5", false, 20, false).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no index at"));
}
```

- [ ] **Step 2: Run, verify they fail**

```bash
cargo test -p polaris-cli --lib savings::tests::savings_run_summary_against_seeded_db
cargo test -p polaris-cli --lib savings::tests::savings_run_errors_when_db_missing
```

Expected: at minimum the second one fails with a different error message until `run` is plumbed correctly.

- [ ] **Step 3: Add `mod savings;` and `Command::Savings`**

Edit `polaris-cli/src/main.rs`.

After line 2 (`mod setup;`), add:

```rust
mod savings;
```

In the `Command` enum (after `Command::Setup`, around line 115), add:

```rust
    /// Show cumulative tokens saved using polaris vs grep+read
    Savings {
        /// Show per-query history instead of the summary
        #[arg(long)]
        history: bool,
        /// Maximum rows for --history (default 20)
        #[arg(long)]
        limit: Option<usize>,
        /// Output format
        #[arg(long, value_enum, default_value = "plain")]
        output: OutputFormat,
    },
```

In the `match cli.command` block in `run()` (after the `Command::Setup { path }` arm), add:

```rust
        Command::Savings { history, limit, output } => {
            savings::run(
                &cfg.db_path,
                cfg.embedding_dim,
                &cfg.model_id,
                history,
                limit.unwrap_or(20),
                output == OutputFormat::Json,
            )
        }
```

- [ ] **Step 4: Run, verify integration tests pass**

```bash
cargo test -p polaris-cli --lib savings::
cargo build -p polaris-cli
```

Expected: PASS, clean build.

- [ ] **Step 5: Smoke test the CLI**

```bash
cargo run -p polaris-cli -- savings 2>&1 | head -5
```

Expected: error mentioning "no index at" (since no DB exists in cwd).

- [ ] **Step 6: Commit**

```bash
git add polaris-cli/src/main.rs polaris-cli/src/savings.rs
git commit -m "feat(savings): wire polaris savings command and integration test"
```

---

## Task 8: `spawn_search_log` helper + wire CLI search

**Files:**
- Modify: `polaris-cli/src/savings.rs`
- Modify: `polaris-cli/src/main.rs`

- [ ] **Step 1: Add `spawn_search_log` to `savings.rs`**

Append to `polaris-cli/src/savings.rs`:

```rust
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use polaris_core::db::{LogSource, SearchResult};
use polaris_core::{Bank, BankConfig, SharedEmbedding};
use tokio::task::JoinHandle;

/// Compute `result_bytes` (sum of `content` bytes) and the unique result file paths.
fn measure_result(results: &[SearchResult]) -> (usize, Vec<PathBuf>) {
    let result_bytes: usize = results.iter().map(|r| r.content.len()).sum();
    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    for r in results {
        paths.insert(PathBuf::from(&r.file_path));
    }
    (result_bytes, paths.into_iter().collect())
}

fn baseline_from_paths(repo_root: &Path, paths: &[PathBuf]) -> usize {
    paths.iter().filter_map(|p| {
        let absolute = if p.is_absolute() { p.clone() } else { repo_root.join(p) };
        std::fs::metadata(&absolute).ok().map(|m| m.len() as usize)
    }).sum()
}

/// Fire-and-forget: compute baseline + insert one row into `search_log`.
///
/// Returns the JoinHandle so tests can `.await` for determinism. Production
/// callers drop it.
pub fn spawn_search_log(
    bank: Bank,
    repo_root: PathBuf,
    source: LogSource,
    query: String,
    top_k: usize,
    results: &[SearchResult],
) -> JoinHandle<()> {
    let (result_bytes, paths) = measure_result(results);
    tokio::spawn(async move {
        let baseline_bytes = baseline_from_paths(&repo_root, &paths);
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        if let Err(e) = bank.log_search(source, &query, top_k, result_bytes, baseline_bytes, ts) {
            tracing::warn!("search log write failed: {e}");
        }
    })
}
```

(`Bank`, `BankConfig`, `SharedEmbedding` imports may not all be needed in `savings.rs` — only what is used. Trim unused imports before committing.)

- [ ] **Step 2: Add `measure_result` unit test**

In the `mod tests` block of `polaris-cli/src/savings.rs`:

```rust
#[test]
fn measure_result_dedups_paths_and_sums_content_bytes() {
    let results = vec![
        SearchResult {
            chunk_id: 1, content: "hello".into(), heading_context: "".into(),
            file_path: "docs/a.md".into(), score: 1.0, source_db: None,
        },
        SearchResult {
            chunk_id: 2, content: "world".into(), heading_context: "".into(),
            file_path: "docs/a.md".into(), score: 0.9, source_db: None,
        },
        SearchResult {
            chunk_id: 3, content: "!".into(), heading_context: "".into(),
            file_path: "docs/b.md".into(), score: 0.8, source_db: None,
        },
    ];
    let (bytes, paths) = measure_result(&results);
    assert_eq!(bytes, 11);
    assert_eq!(paths, vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/b.md")]);
}
```

(Add `use polaris_core::db::SearchResult;` to the test module if not already present.)

- [ ] **Step 3: Wire `spawn_search_log` into `cmd_search`**

In `polaris-cli/src/main.rs`, modify `cmd_search` (around line 335). After the for-loop that opens banks and mounts them, capture the primary bank for logging. Replace:

```rust
    for db_path in &all_db_paths {
        let bank_cfg = ...;
        let bank = polaris_core::Bank::open(bank_cfg, embed.clone())?;
        let label = ...;
        set.mount(bank, label);
    }

    let results = set.search(query, polaris_core::SearchOpts { top_k })?;
```

with:

```rust
    let mut primary_bank: Option<polaris_core::Bank> = None;
    for (i, db_path) in all_db_paths.iter().enumerate() {
        let bank_cfg = polaris_core::BankConfig {
            repo_root: db_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf(),
            index_path: db_path.clone(),
            embedding_dim: cfg.embedding_dim,
            model_id: cfg.model_id.clone(),
            max_chunk_tokens: cfg.max_chunk_tokens,
            chunk_overlap_chars: cfg.chunk_overlap_chars,
            max_file_size: cfg.max_file_size,
            mmr_lambda: cfg.mmr_lambda,
            mmr_candidate_multiplier: cfg.mmr_candidate_multiplier,
            heading_boost: cfg.heading_boost,
            rrf_k: cfg.rrf_k,
        };
        let bank = polaris_core::Bank::open(bank_cfg, embed.clone())?;
        let label = db_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if i == 0 {
            primary_bank = Some(bank.clone());
        }
        set.mount(bank, label);
    }

    let results = set.search(query, polaris_core::SearchOpts { top_k })?;

    if let Some(bank) = primary_bank {
        let repo_root = bank.repo_root().to_path_buf();
        let _handle = savings::spawn_search_log(
            bank,
            repo_root,
            polaris_core::db::LogSource::Cli,
            query.to_string(),
            top_k,
            &results,
        );
    }
```

- [ ] **Step 4: Add an `#[ignore]`-gated end-to-end integration test**

`Bank::open` requires a real `SharedEmbedding` (downloads the model). Gate the test the same way as Task 5; default `cargo test` skips it. The pure unit tests on `measure_result`, `format_summary`, `format_history` plus Task 7's `savings_run_summary_against_seeded_db` already give the load-bearing coverage; this test exists for end-to-end verification.

In `polaris-cli/src/savings.rs`, in `mod tests`, add:

```rust
#[tokio::test]
#[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
async fn spawn_search_log_inserts_row_for_cli_source() {
    polaris_core::db::register_vec_extension();
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    let doc_a = docs.join("a.md");
    std::fs::write(&doc_a, "Lorem ipsum dolor sit amet, consectetur adipiscing elit.").unwrap();

    let index_path = dir.path().join("polaris.db");
    let embed = polaris_core::SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
    let bank = polaris_core::Bank::open(
        polaris_core::BankConfig {
            repo_root: dir.path().to_path_buf(),
            index_path: index_path.clone(),
            embedding_dim: 64,
            model_id: "nomic-embed-text-v1.5".into(),
            ..Default::default()
        },
        embed,
    ).unwrap();

    let fake_results = vec![
        SearchResult {
            chunk_id: 1, content: "Lorem ipsum".into(), heading_context: "".into(),
            file_path: "docs/a.md".into(), score: 1.0, source_db: None,
        },
    ];

    let handle = spawn_search_log(
        bank.clone(),
        dir.path().to_path_buf(),
        LogSource::Cli,
        "test query".into(),
        5,
        &fake_results,
    );
    handle.await.unwrap();

    let db = Database::open(&index_path, 64, "nomic-embed-text-v1.5").unwrap();
    let agg = db.aggregate_savings().unwrap();
    assert_eq!(agg.total_searches, 1);
    assert_eq!(agg.by_source.cli.searches, 1);
    assert_eq!(agg.by_source.cli.result_bytes, 11);
    assert!(agg.by_source.cli.baseline_bytes >= 50, "baseline should reflect the file size");
}
```

Verify with `cargo test -p polaris-cli spawn_search_log_inserts_row_for_cli_source -- --ignored` once locally before declaring the task done.

- [ ] **Step 5: Build and run all tests**

```bash
cargo check -p polaris-cli
cargo test -p polaris-cli
```

Expected: clean build, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add polaris-cli/src/savings.rs polaris-cli/src/main.rs
git commit -m "feat(savings): log every CLI search via spawn_search_log"
```

---

## Task 9: Wire `spawn_search_log` into MCP search handler

**Files:**
- Modify: `polaris-cli/src/mcp/server.rs`

- [ ] **Step 1: Modify the `search` handler**

Edit `polaris-cli/src/mcp/server.rs`. Locate the `async fn search` handler (around line 88). Replace its body:

```rust
async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
    let config = Arc::clone(&self.state.config);
    let top_k = (params.top_k.unwrap_or(5) as usize).min(config.max_top_k);
    let query = params.query;
    let bank = self.state.bank.clone();

    let result = tokio::task::spawn_blocking(move || {
        match bank.search(&query, polaris_core::SearchOpts { top_k }) {
            Ok(results) => SearchEngine::format_results(&results),
            Err(e) => format!("Error: {e}"),
        }
    }).await;

    result.unwrap_or_else(|e| format!("Error: task failed: {e}"))
}
```

with:

```rust
async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
    let config = Arc::clone(&self.state.config);
    let top_k = (params.top_k.unwrap_or(5) as usize).min(config.max_top_k);
    let query = params.query.clone();
    let bank = self.state.bank.clone();
    let repo_root = bank.repo_root().to_path_buf();

    // Run the synchronous search on a blocking thread; capture the raw result
    // set so we can feed it both into the formatter (returned to the client)
    // and into the savings log writer.
    let bank_for_search = bank.clone();
    let query_for_search = query.clone();
    let search_outcome = tokio::task::spawn_blocking(move || {
        bank_for_search.search(&query_for_search, polaris_core::SearchOpts { top_k })
    }).await;

    let results = match search_outcome {
        Ok(Ok(results)) => results,
        Ok(Err(e)) => return format!("Error: {e}"),
        Err(e) => return format!("Error: task failed: {e}"),
    };

    let formatted = SearchEngine::format_results(&results);

    let _handle = crate::savings::spawn_search_log(
        bank,
        repo_root,
        polaris_core::db::LogSource::Mcp,
        query,
        top_k,
        &results,
    );

    formatted
}
```

- [ ] **Step 2: Source the `repo_root` from `Bank::repo_root()`**

`Bank` exposes `pub fn repo_root(&self) -> &Path` (defined in `polaris-core/src/bank.rs`). Use it directly instead of threading a separate field through `PolarisState`. Replace the `let repo_root = self.state.repo_root.clone();` line in Step 1 with:

```rust
let repo_root = bank.repo_root().to_path_buf();
```

(Take the `repo_root` snapshot before the `tokio::task::spawn_blocking` move so the borrow doesn't escape into the closure.)

This avoids touching `PolarisState`'s shape.

- [ ] **Step 3: Build and verify**

```bash
cargo check -p polaris-cli
cargo build -p polaris-cli
cargo test -p polaris-cli
```

Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add polaris-cli/src/mcp/server.rs
git commit -m "feat(savings): log MCP search calls via spawn_search_log"
```

---

## Task 10: Documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/cli.md`

- [ ] **Step 1: Add `### Savings` to README**

Edit `README.md`. After the `### Status` subsection (around line 110), and before `### MCP Server`, insert:

```markdown
### Savings

```bash
polaris savings              # cumulative summary
polaris savings --history    # per-query log (newest first)
polaris savings --output json
```

Reports the cumulative tokens you've saved by going through Polaris instead of `grep + read`. The baseline for each query is the total content of the unique files in the result set — i.e., what an agent without Polaris would have opened after grepping. Tokens are estimated at ~4 chars/token.
```

- [ ] **Step 2: Update the README's token-savings callout**

In the `## Why Polaris — Token Savings vs. Grep + Read` section, after the existing table (around the `Estimates use ~4 chars/token` paragraph), add a one-line callout:

```markdown
Run `polaris savings` to see your own cumulative number once you've made some queries.
```

- [ ] **Step 3: Add `polaris savings` to `docs/cli.md`**

Edit `docs/cli.md`. After the `### `polaris status`` section and before `### `polaris chunks <path>``, insert:

```markdown
### `polaris savings`

Show cumulative tokens saved by going through Polaris instead of `grep + read`. Reads from the `search_log` table inside `polaris.db`.

```bash
polaris savings              # summary
polaris savings --history    # per-query history (newest first)
polaris savings --history --limit 50
polaris savings --output json
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--history` | false | Print the per-query log instead of the summary |
| `--limit <N>` | 20 | Maximum rows for `--history` |
| `--output <FORMAT>` | plain | Output format: `plain` or `json` |

**Behaviour:**

1. Reads the `search_log` table from `polaris.db`.
2. Aggregates rows into total searches, total result/baseline bytes, and a per-source (`cli` / `mcp`) breakdown.
3. Renders either the summary block or the per-query history table.
4. Tokens are estimated as `bytes / 4`.
5. Empty log: prints `No searches recorded yet. Run a search to start tracking.` and exits 0.

**Output (summary):**

```
  polaris  ·  savings

  Total searches      127  (mcp 98 / cli 29)
  Tokens delivered   31.2K
  Baseline           412K
  Tokens saved       381K  ~13× cheaper
  Tracking since     2026-04-09

  Tokens estimated at ~4 chars/token.
```

**Error cases (stderr, exit 1):**

| Situation | Message |
|-----------|---------|
| `polaris.db` doesn't exist | `no index at 'polaris.db'  —  run polaris index <path> first` |

---
```

- [ ] **Step 4: Smoke-test the docs render**

```bash
grep -n "polaris savings" README.md docs/cli.md
```

Expected: matches in both files.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/cli.md
git commit -m "docs: document polaris savings command"
```

---

## Final Verification

After all tasks are complete, run the full workspace check:

```bash
cargo check
cargo test
cargo clippy --workspace --all-targets
```

Expected: all green, no new clippy warnings.

Dispatch a final code-reviewer subagent over the full branch range:

```bash
git log --oneline d6d4559..HEAD
```

Expected: 10 atomic commits, one per task, each with a clean message.
