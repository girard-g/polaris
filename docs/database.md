# Database

Polaris uses SQLite with two extensions: [sqlite-vec](https://github.com/asg017/sqlite-vec) for vector KNN search and the built-in **FTS5** module for BM25 full-text search.

## Initialization

The sqlite-vec extension must be registered **before** opening any connection:

```rust
db::register_vec_extension();  // Call once at startup, before Database::open()
let db = Database::open(&config)?;
```

`register_vec_extension()` calls `sqlite3_auto_extension` via the rusqlite FFI.

## Schema (v2)

### `metadata`

Stores database-level configuration. Read on every `open()`; validated against current config.

```sql
CREATE TABLE metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

| Key | Example Value | Notes |
|-----|---------------|-------|
| `schema_version` | `"2"` | Incremented on schema migrations |
| `embedding_dim` | `"512"` | Validated against config on open — mismatch is a hard error |
| `model_id` | `"nomic-embed-text-v1.5"` | Validated against config on open — mismatch is a hard error |

### `documents`

One row per indexed markdown file.

```sql
CREATE TABLE documents (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    path         TEXT NOT NULL UNIQUE,   -- forward-slash normalized
    content_hash TEXT NOT NULL,          -- SHA256 hex string
    title        TEXT,                   -- First H1 heading, or NULL
    indexed_at   TEXT NOT NULL,          -- RFC 3339 (e.g. "2025-02-26T14:23:45Z")
    file_size    INTEGER NOT NULL        -- bytes
);
```

### `chunks`

One row per text chunk derived from a document. Cascades on document deletion.

```sql
CREATE TABLE chunks (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id     INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    content         TEXT NOT NULL,
    heading_context TEXT NOT NULL DEFAULT '',  -- e.g. "# Guide > ## Auth"
    start_byte      INTEGER NOT NULL,
    end_byte        INTEGER NOT NULL,
    chunk_index     INTEGER NOT NULL
);
```

### `vec_chunks` (virtual table)

sqlite-vec virtual table for KNN vector search.

```sql
CREATE VIRTUAL TABLE vec_chunks USING vec0(
    chunk_id  INTEGER PRIMARY KEY,
    embedding float[512] distance_metric=cosine
);
```

The dimension (`512`) is substituted dynamically from `embedding_dim` at schema creation time. The table must be re-created if the dimension changes (requires a new database).

### `chunks_fts` (virtual table)

SQLite FTS5 content-sync table for BM25 full-text search.

```sql
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    content, heading_context,
    content='chunks', content_rowid='id'
);
```

`content='chunks'` means FTS5 reads from the `chunks` table for content (no duplication), but **write operations must be manually synchronized** — FTS5 does not auto-sync with the source table.

## Schema Migrations

`Database::open()` checks `schema_version` on existing databases and applies migrations automatically.

### v1 → v2

Creates the `chunks_fts` table and backfills it from existing chunks:

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content, heading_context,
    content='chunks', content_rowid='id'
);
INSERT INTO chunks_fts(rowid, content, heading_context)
SELECT id, content, heading_context FROM chunks;
UPDATE metadata SET value='2' WHERE key='schema_version';
```

## KNN Search Query

```sql
SELECT
    vc.chunk_id,
    vc.distance,
    c.content,
    c.heading_context,
    d.path
FROM vec_chunks vc
JOIN chunks c ON c.id = vc.chunk_id
JOIN documents d ON d.id = c.document_id
WHERE vc.embedding MATCH ?1
  AND k = ?2
ORDER BY vc.distance ASC;
```

`?1` is the query embedding as a little-endian `f32` byte array. `?2` is the candidate count.

## BM25 Search Query

```sql
SELECT rowid
FROM chunks_fts
WHERE chunks_fts MATCH ?1
ORDER BY rank
LIMIT ?2;
```

`rank` is the built-in FTS5 BM25 score (negative; lower = better). Results are returned in best-first order.

## Vector Encoding

sqlite-vec expects vectors as raw little-endian byte arrays:

```rust
fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
```

## FTS5 Write Synchronization

Because `chunks_fts` is a content table, all writes must be manually synchronized.

### Insert

```sql
INSERT INTO chunks_fts(rowid, content, heading_context) VALUES (?1, ?2, ?3);
```

### Delete (content-sync delete protocol)

The old content must be provided so FTS5 can remove the correct tokens. This must happen **before** deleting the row from `chunks`:

```sql
INSERT INTO chunks_fts(chunks_fts, rowid, content, heading_context)
SELECT 'delete', id, content, heading_context
FROM chunks WHERE document_id = ?1;
```

## Deletion Cascade

Deleting a document cascades to `chunks` via `ON DELETE CASCADE`. However, neither `vec_chunks` nor `chunks_fts` cascade automatically — both must be cleaned up explicitly before the source rows disappear.

Order for `delete_document()` and `delete_chunks_for_document()`:
1. FTS5 delete (using content still present in `chunks`)
2. Delete from `vec_chunks`
3. Delete from `documents` (cascade removes `chunks`) or `DELETE FROM chunks`

## Dimension Validation

On `Database::open()`, the stored `embedding_dim` is compared to the config value. A mismatch returns:

```
Dimension mismatch: database has dim=256, config has dim=384
```

Resolution: delete the database and re-index.

## Transactions

Each document is indexed inside an explicit transaction for atomicity:

```rust
db.begin()?;
// delete old chunks (if re-indexing) — FTS5 sync + vec_chunks + chunks
// insert document row
// insert chunk rows + vec_chunks + chunks_fts rows
db.commit()?;  // or db.rollback() on any error
```

## Timestamps

Polaris does not depend on the `chrono` crate. Timestamps are generated via a custom implementation that:

1. Reads `SystemTime::now()` as Unix seconds
2. Converts to `(year, month, day, hour, minute, second)` with correct leap-year handling
3. Formats as RFC 3339: `"2025-02-26T14:23:45Z"`

## Statistics Query

`db.get_stats()` returns:

```rust
pub struct DbStats {
    pub doc_count: usize,
    pub chunk_count: usize,
    pub last_indexed: Option<String>,  // RFC 3339 or None
    pub db_size_bytes: u64,            // From filesystem metadata
    pub embedding_dim: usize,          // From metadata table
}
```
