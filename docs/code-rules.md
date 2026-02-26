# Code Rules

Conventions and constraints that must be followed when modifying or extending Polaris.

## Error Handling

### Use the `Result` alias everywhere — except `mcp/server.rs`

```rust
// error.rs defines:
pub type Result<T> = std::result::Result<T, PolarisError>;

// Use it in all modules:
use crate::error::Result;
```

**Exception:** `mcp/server.rs` must NOT import `use crate::error::Result`. The rmcp `#[tool]` and `#[tool_router]` macros generate code that expects `ErrorData` in scope, and importing the custom `Result` alias breaks compilation. Use bare `std::result::Result<_, PolarisError>` or `PolarisError` directly.

### Tool errors are strings, not MCP errors

MCP tool handlers return `String`, never `Err(...)`. Wrap all errors into human-readable messages:

```rust
match engine.search(...) {
    Ok(results) => SearchEngine::format_results(&results),
    Err(e) => format!("Error: {e}"),
}
```

### Add specific `PolarisError` variants instead of `Indexing(String)` for new categories

Reserve `Indexing(String)` for unexpected/ad-hoc errors. Named variants with context are preferred for conditions that can be acted upon.

## Threading and Ownership

### `EmbeddingEngine` is `Arc`-shared, never cloned deeply

```rust
Arc<EmbeddingEngine>  // pass this, not a cloned EmbeddingEngine
```

### `Database` is behind `Arc<Mutex<Database>>` in MCP mode

Lock the mutex per operation, not per session. Do not hold the lock across `await` points.

### `TextEmbedding` requires `&mut self` — always goes through `Mutex`

Never expose `TextEmbedding` directly. All access must go through `EmbeddingEngine::embed_documents()` or `embed_query()`.

## Embedding

### Always use the task prefix

Documents: `"search_document: " + text`
Queries: `"search_query: " + text`

Omitting the prefix silently degrades retrieval quality. Both are applied inside `EmbeddingEngine` — don't add them at the call site.

### Always L2-normalize after truncation

Normalization happens inside `EmbeddingEngine`. The DB stores unit-length vectors. Do not store unnormalized vectors.

## Database

### Register the sqlite-vec extension before any connection

```rust
db::register_vec_extension();  // must be first
let db = Database::open(...)?;
```

Calling `Database::open()` before registration will silently create a DB without the `vec0` virtual table, causing errors later.

### Use transactions for multi-row writes

Every document insertion must be wrapped in `begin()` / `commit()` / `rollback()`. Partial writes leave the DB in an inconsistent state.

### vec_chunks and chunks_fts deletions are manual

`ON DELETE CASCADE` on `chunks` does not cascade to virtual tables. Always explicitly clean up both `vec_chunks` and `chunks_fts` **before** deleting from `chunks` or `documents`.

For `chunks_fts`, use the FTS5 content-sync delete protocol (the old content must still be present in `chunks` when this runs):

```sql
INSERT INTO chunks_fts(chunks_fts, rowid, content, heading_context)
SELECT 'delete', id, content, heading_context FROM chunks WHERE document_id = ?1;
```

Required deletion order:
1. `chunks_fts` delete (SELECT from `chunks` while it still exists)
2. `DELETE FROM vec_chunks`
3. `DELETE FROM documents` (cascade removes `chunks`) or `DELETE FROM chunks`

### Dimension is fixed at DB creation time

The `embedding_dim` stored in metadata is checked on every `Database::open()`. A mismatch is a hard error. Do not silently truncate or pad embeddings to fit.

## Chunking

### Maintain heading context for every chunk

Every chunk must carry a non-empty `heading_context` derived from the heading hierarchy. This is used in search result display and improves retrieval quality.

### Minimum chunk size is 50 chars

Chunks shorter than 50 characters are merged into the previous chunk. Do not create sub-50-char chunks — they carry insufficient semantic content for embedding.

### Overlap is applied in characters, not tokens

`chunk_overlap_chars` is measured in `String::len()` (bytes for ASCII, chars for UTF-8 slicing via `char_indices`). Do not interpret it as a token count.

## schemars Version

rmcp 0.16 requires **schemars 1.x** (not 0.8). The two versions have incompatible APIs. Do not add `schemars = "0.8"` anywhere in `Cargo.toml`.

## Cargo Edition

The project uses `edition = "2024"`. Use `use` imports, `let ... else`, and other edition-2024 idioms freely. Do not use deprecated patterns from older editions.

## No `chrono` Dependency

Timestamps are generated with a custom Unix → RFC 3339 formatter in `db.rs`. Do not add `chrono` or `time` as a dependency — they pull in large dependency trees and are not needed.

## Logging

- Use `tracing::info!`, `tracing::warn!`, `tracing::debug!` — not `println!` or `eprintln!`
- The `serve` command routes logs to stderr (keeps stdout clean for MCP)
- All other commands log to stdout
- Do not log sensitive data (file contents, embeddings)

## Progress UI

- Use `indicatif` for all user-facing progress feedback
- Spinner for open-ended waits (model loading, file discovery)
- Progress bar with ETA for bounded loops (file indexing)
- Do not use `println!` for progress in the indexing pipeline — it will interleave with the progress bar output
