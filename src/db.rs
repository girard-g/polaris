#![allow(dead_code)]
use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{PolarisError, Result};

const SCHEMA_VERSION: &str = "2";

pub struct Database {
    conn: Connection,
    embedding_dim: usize,
    pub model_id: String,
}

/// A document row as stored in the `documents` table.
#[allow(dead_code)]
pub struct DocumentRecord {
    pub id: i64,
    pub path: String,
    pub content_hash: String,
    pub title: Option<String>,
    pub indexed_at: String,
    pub file_size: i64,
}

/// A chunk row with its parent document path and heading context.
#[allow(dead_code)]
pub struct ChunkRecord {
    pub id: i64,
    pub document_id: i64,
    pub content: String,
    pub heading_context: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub chunk_index: usize,
}

/// Returned by `search_knn`.
pub struct SearchResult {
    pub chunk_id: i64,
    pub content: String,
    pub heading_context: String,
    pub file_path: String,
    pub score: f32,
}

/// Like `SearchResult` but also carries the stored embedding (for MMR reranking).
pub struct SearchResultWithEmbedding {
    pub chunk_id: i64,
    pub content: String,
    pub heading_context: String,
    pub file_path: String,
    pub score: f32,
    pub embedding: Vec<f32>,
}

impl SearchResultWithEmbedding {
    pub fn into_search_result(self) -> SearchResult {
        SearchResult {
            chunk_id: self.chunk_id,
            content: self.content,
            heading_context: self.heading_context,
            file_path: self.file_path,
            score: self.score,
        }
    }
}

/// Returned by `search_bm25`.
pub struct Bm25Result {
    pub chunk_id: i64,
    /// 1-based rank from BM25 ordering (1 = best match).
    pub bm25_rank: usize,
}

/// Database statistics.
pub struct DbStats {
    pub doc_count: usize,
    pub chunk_count: usize,
    pub empty_doc_count: usize,
    pub total_source_bytes: u64,
    pub last_indexed: Option<String>,
    pub db_size_bytes: u64,
    pub embedding_dim: usize,
}

// ---------------------------------------------------------------------------
// Pragma helpers
// ---------------------------------------------------------------------------

/// Apply all connection-level pragmas. Called from both `open()` and `open_in_memory()`.
///
/// WAL is a no-op on `:memory:` databases (safe for tests).
fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous  = NORMAL;
         PRAGMA cache_size   = -64000;
         PRAGMA mmap_size    = 268435456;
         PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;",
    )
}

// ---------------------------------------------------------------------------
// Extension registration — must be called once before any Connection opens.
// ---------------------------------------------------------------------------

/// Register sqlite-vec as an auto-extension. Call once at program start.
///
/// # Safety
/// This calls `sqlite3_auto_extension` which is inherently unsafe.
pub fn register_vec_extension() {
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

impl Database {
    /// Open (or create) the database at `path` with the given embedding dimension and model ID.
    ///
    /// On first open the schema is created. On subsequent opens the stored
    /// `embedding_dim` and `model_id` are validated against the config values.
    pub fn open(path: &Path, config_dim: usize, config_model_id: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        let mut db = Self { conn, embedding_dim: config_dim, model_id: config_model_id.to_string() };
        db.init_schema(config_dim, config_model_id)?;
        Ok(db)
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory(embedding_dim: usize, model_id: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        apply_pragmas(&conn)?;
        let mut db = Self { conn, embedding_dim, model_id: model_id.to_string() };
        db.init_schema(embedding_dim, model_id)?;
        Ok(db)
    }

    fn init_schema(&mut self, config_dim: usize, config_model_id: &str) -> Result<()> {
        // Check whether schema already exists.
        let table_count: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='metadata'",
            [],
            |r| r.get(0),
        )?;

        if table_count == 0 {
            // Fresh database — create everything.
            self.create_schema(config_dim, config_model_id)?;
        } else {
            // Existing database — validate dimension.
            let stored_dim: Option<String> = self
                .conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='embedding_dim'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;

            if let Some(dim_str) = stored_dim {
                let db_dim: usize = dim_str.parse().map_err(|_| {
                    PolarisError::Config("Invalid embedding_dim in metadata".to_string())
                })?;
                if db_dim != config_dim {
                    return Err(PolarisError::DimensionMismatch {
                        db_dim,
                        config_dim,
                    });
                }
                self.embedding_dim = db_dim;
            }

            // Validate model ID.
            let stored_model: Option<String> = self
                .conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='model_id'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;

            if let Some(db_model) = stored_model {
                if db_model != config_model_id {
                    return Err(PolarisError::ModelMismatch {
                        db_model,
                        config_model: config_model_id.to_string(),
                    });
                }
            }

            // Apply any pending schema migrations.
            self.apply_migrations()?;
        }
        Ok(())
    }

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
        Ok(())
    }

    fn migrate_v1_to_v2(&self) -> Result<()> {
        // Create the FTS5 content-sync table.
        self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                content, heading_context,
                content='chunks', content_rowid='id'
            );",
        )?;
        // Backfill existing chunks into the FTS index.
        self.conn.execute_batch(
            "INSERT INTO chunks_fts(rowid, content, heading_context)
             SELECT id, content, heading_context FROM chunks;",
        )?;
        // Update schema version.
        self.conn.execute(
            "UPDATE metadata SET value='2' WHERE key='schema_version'",
            [],
        )?;
        Ok(())
    }

    fn create_schema(&self, dim: usize, model_id: &str) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE metadata (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE documents (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                path         TEXT NOT NULL UNIQUE,
                content_hash TEXT NOT NULL,
                title        TEXT,
                indexed_at   TEXT NOT NULL,
                file_size    INTEGER NOT NULL
            );

            CREATE TABLE chunks (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id     INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                content         TEXT NOT NULL,
                heading_context TEXT NOT NULL DEFAULT '',
                start_byte      INTEGER NOT NULL,
                end_byte        INTEGER NOT NULL,
                chunk_index     INTEGER NOT NULL
            );

            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                content, heading_context,
                content='chunks', content_rowid='id'
            );
        ")?;

        // The vec0 table dimension is baked in, so we create it dynamically.
        self.conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE vec_chunks USING vec0(
                chunk_id  INTEGER PRIMARY KEY,
                embedding float[{dim}] distance_metric=cosine
            );"
        ))?;

        // Store metadata.
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES
                ('schema_version', ?1),
                ('embedding_dim',  ?2),
                ('model_id',       ?3)",
            params![SCHEMA_VERSION, dim.to_string(), model_id],
        )?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Document CRUD
    // -----------------------------------------------------------------------

    pub fn get_document_by_path(&self, path: &str) -> Result<Option<DocumentRecord>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, path, content_hash, title, indexed_at, file_size
                 FROM documents WHERE path = ?1",
                params![path],
                |r| {
                    Ok(DocumentRecord {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        content_hash: r.get(2)?,
                        title: r.get(3)?,
                        indexed_at: r.get(4)?,
                        file_size: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    pub fn get_all_document_hashes(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash FROM documents")?;
        let pairs = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(pairs)
    }

    pub fn insert_document(
        &self,
        path: &str,
        content_hash: &str,
        title: Option<&str>,
        file_size: i64,
    ) -> Result<i64> {
        let now = chrono_now();
        self.conn.execute(
            "INSERT INTO documents (path, content_hash, title, indexed_at, file_size)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, content_hash, title, now, file_size],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_document_hash(
        &self,
        path: &str,
        content_hash: &str,
        file_size: i64,
    ) -> Result<()> {
        let now = chrono_now();
        self.conn.execute(
            "UPDATE documents SET content_hash=?1, indexed_at=?2, file_size=?3 WHERE path=?4",
            params![content_hash, now, file_size, path],
        )?;
        Ok(())
    }

    pub fn delete_document(&self, path: &str) -> Result<()> {
        // Get ID first for vec_chunks cleanup (no FK cascade on virtual table).
        let doc_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM documents WHERE path=?1",
                params![path],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(id) = doc_id {
            // Remove from FTS5 index (must happen before chunks cascade-delete).
            self.conn.execute(
                "INSERT INTO chunks_fts(chunks_fts, rowid, content, heading_context)
                 SELECT 'delete', id, content, heading_context FROM chunks WHERE document_id = ?1",
                params![id],
            )?;
            // Delete embeddings for all chunks of this document.
            self.conn.execute(
                "DELETE FROM vec_chunks WHERE chunk_id IN (
                    SELECT id FROM chunks WHERE document_id = ?1
                )",
                params![id],
            )?;
            // Chunk rows are deleted by FK cascade when document is deleted.
            self.conn
                .execute("DELETE FROM documents WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Chunk CRUD
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn insert_chunk(
        &self,
        document_id: i64,
        content: &str,
        heading_context: &str,
        start_byte: usize,
        end_byte: usize,
        chunk_index: usize,
        embedding: &[f32],
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO chunks
                (document_id, content, heading_context, start_byte, end_byte, chunk_index)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                document_id,
                content,
                heading_context,
                start_byte as i64,
                end_byte as i64,
                chunk_index as i64
            ],
        )?;
        let chunk_id = self.conn.last_insert_rowid();

        // Insert the embedding vector.
        let bytes = f32_slice_to_bytes(embedding);
        self.conn.execute(
            "INSERT INTO vec_chunks (chunk_id, embedding) VALUES (?1, ?2)",
            params![chunk_id, bytes],
        )?;

        // Sync to FTS5 index.
        self.conn.execute(
            "INSERT INTO chunks_fts(rowid, content, heading_context) VALUES (?1, ?2, ?3)",
            params![chunk_id, content, heading_context],
        )?;

        Ok(chunk_id)
    }

    pub fn delete_chunks_for_document(&self, document_id: i64) -> Result<()> {
        // Remove from FTS5 index first (must happen before chunks are deleted).
        self.conn.execute(
            "INSERT INTO chunks_fts(chunks_fts, rowid, content, heading_context)
             SELECT 'delete', id, content, heading_context FROM chunks WHERE document_id = ?1",
            params![document_id],
        )?;
        // Remove from vec index.
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE chunk_id IN (
                SELECT id FROM chunks WHERE document_id = ?1
            )",
            params![document_id],
        )?;
        self.conn.execute(
            "DELETE FROM chunks WHERE document_id = ?1",
            params![document_id],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // KNN search
    // -----------------------------------------------------------------------

    pub fn search_knn(&self, query_embedding: &[f32], top_k: usize) -> Result<Vec<SearchResult>> {
        let bytes = f32_slice_to_bytes(query_embedding);

        let mut stmt = self.conn.prepare(
            "SELECT
                vc.chunk_id,
                vc.distance,
                c.content,
                c.heading_context,
                d.path
             FROM vec_chunks vc
             JOIN chunks   c ON c.id = vc.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE vc.embedding MATCH ?1
               AND k = ?2
             ORDER BY vc.distance",
        )?;

        let results = stmt
            .query_map(params![bytes, top_k as i64], |r| {
                Ok(SearchResult {
                    chunk_id: r.get(0)?,
                    score: r.get(1)?,
                    content: r.get(2)?,
                    heading_context: r.get(3)?,
                    file_path: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(results)
    }

    /// Like `search_knn` but also fetches each result's stored embedding for MMR.
    ///
    /// Uses exactly 2 queries: one KNN query + one batch embedding fetch (no N+1).
    pub fn search_knn_with_embeddings(
        &self,
        query_embedding: &[f32],
        candidate_count: usize,
    ) -> Result<Vec<SearchResultWithEmbedding>> {
        let bytes = f32_slice_to_bytes(query_embedding);

        let mut stmt = self.conn.prepare(
            "SELECT
                vc.chunk_id,
                vc.distance,
                c.content,
                c.heading_context,
                d.path
             FROM vec_chunks vc
             JOIN chunks   c ON c.id = vc.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE vc.embedding MATCH ?1
               AND k = ?2
             ORDER BY vc.distance",
        )?;

        // Collect KNN results first (releases the statement borrow).
        let rows_data: Vec<(i64, f32, String, String, String)> = stmt
            .query_map(params![bytes, candidate_count as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;

        if rows_data.is_empty() {
            return Ok(vec![]);
        }

        // Batch-fetch all embeddings in one query (eliminates N+1).
        let chunk_ids: Vec<i64> = rows_data.iter().map(|r| r.0).collect();
        let placeholders = (1..=chunk_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let batch_sql = format!(
            "SELECT chunk_id, embedding FROM vec_chunks WHERE chunk_id IN ({placeholders})"
        );
        let mut emb_stmt = self.conn.prepare(&batch_sql)?;
        let id_params: Vec<&dyn rusqlite::ToSql> =
            chunk_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let mut embeddings: HashMap<i64, Vec<f32>> = HashMap::new();
        let mut emb_rows = emb_stmt.query(rusqlite::params_from_iter(id_params))?;
        while let Some(row) = emb_rows.next()? {
            let cid: i64 = row.get(0)?;
            let emb_bytes: Vec<u8> = row.get(1)?;
            embeddings.insert(cid, bytes_to_f32_slice(&emb_bytes));
        }

        // Assemble final results.
        let mut results = Vec::with_capacity(rows_data.len());
        for (chunk_id, score, content, heading_context, file_path) in rows_data {
            let embedding = embeddings.remove(&chunk_id).unwrap_or_default();
            results.push(SearchResultWithEmbedding {
                chunk_id,
                score,
                content,
                heading_context,
                file_path,
                embedding,
            });
        }
        Ok(results)
    }

    // -----------------------------------------------------------------------
    // BM25 / FTS5 search
    // -----------------------------------------------------------------------

    /// Full-text search using SQLite FTS5 BM25 ranking.
    ///
    /// Returns results ordered by BM25 rank (best first). On FTS5 query syntax
    /// errors the caller should use `unwrap_or_default()` to degrade gracefully.
    pub fn search_bm25(&self, query: &str, limit: usize) -> Result<Vec<Bm25Result>> {
        let mut stmt = self.conn.prepare(
            "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let results = stmt
            .query_map(params![query, limit as i64], |r| r.get::<_, i64>(0))?
            .enumerate()
            .map(|(rank, row)| row.map(|chunk_id| Bm25Result { chunk_id, bm25_rank: rank + 1 }))
            .collect::<rusqlite::Result<_>>()?;
        Ok(results)
    }

    /// Fetch content, heading context, file path, and embedding for a chunk by ID.
    ///
    /// Used to hydrate BM25-only results that were not returned by KNN.
    pub fn get_chunk_with_metadata(&self, chunk_id: i64) -> Result<Option<SearchResultWithEmbedding>> {
        let row = self
            .conn
            .query_row(
                "SELECT c.content, c.heading_context, d.path
                 FROM chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.id = ?1",
                params![chunk_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let Some((content, heading_context, file_path)) = row else {
            return Ok(None);
        };

        let emb_bytes: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT embedding FROM vec_chunks WHERE chunk_id = ?1",
                params![chunk_id],
                |r| r.get(0),
            )
            .optional()?;

        let embedding = emb_bytes.map(|b| bytes_to_f32_slice(&b)).unwrap_or_default();

        Ok(Some(SearchResultWithEmbedding {
            chunk_id,
            content,
            heading_context,
            file_path,
            score: 0.0,
            embedding,
        }))
    }

    // -----------------------------------------------------------------------
    // Transaction helpers
    // -----------------------------------------------------------------------

    pub fn begin(&self) -> Result<()> {
        Ok(self.conn.execute_batch("BEGIN")?)
    }

    pub fn commit(&self) -> Result<()> {
        Ok(self.conn.execute_batch("COMMIT")?)
    }

    pub fn rollback(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    pub fn get_stats(&self, db_path: &Path) -> Result<DbStats> {
        let doc_count: i64 =
            self.conn
                .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))?;
        let chunk_count: i64 =
            self.conn
                .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
        let empty_doc_count: i64 = self.conn.query_row(
            "SELECT count(*) FROM documents WHERE id NOT IN (SELECT DISTINCT document_id FROM chunks)",
            [],
            |r| r.get(0),
        )?;
        let total_source_bytes: i64 = self
            .conn
            .query_row("SELECT coalesce(sum(file_size), 0) FROM documents", [], |r| r.get(0))?;
        let last_indexed: Option<String> = self
            .conn
            .query_row(
                "SELECT max(indexed_at) FROM documents",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let db_size_bytes = std::fs::metadata(db_path)
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(DbStats {
            doc_count: doc_count as usize,
            chunk_count: chunk_count as usize,
            empty_doc_count: empty_doc_count as usize,
            total_source_bytes: total_source_bytes as u64,
            last_indexed,
            db_size_bytes,
            embedding_dim: self.embedding_dim,
        })
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl Database {
    /// Query `PRAGMA journal_mode` (for WAL tests).
    fn journal_mode(&self) -> String {
        self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn bytes_to_f32_slice(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().expect("chunks_exact(4) guarantees 4 bytes")))
        .collect()
}

fn chrono_now() -> String {
    // RFC 3339-ish timestamp without pulling in the `chrono` crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format as YYYY-MM-DDTHH:MM:SSZ (approximate, no timezone math needed)
    let s = secs;
    let (y, mo, d, h, m, sec) = unix_to_ymd_hms(s);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{sec:02}Z")
}

/// Minimal Unix timestamp → calendar conversion (no external crate).
fn unix_to_ymd_hms(ts: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec = (ts % 60) as u32;
    let min = ((ts / 60) % 60) as u32;
    let hour = ((ts / 3600) % 24) as u32;
    let days = ts / 86400;

    // Days since 1970-01-01
    let mut year = 1970u32;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }

    const DAYS_IN_MONTH: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u32;
    for (m, &dim_base) in DAYS_IN_MONTH.iter().enumerate() {
        let dim: u64 = if m == 1 && is_leap(year) { 29 } else { dim_base };
        if remaining < dim {
            break;
        }
        remaining -= dim;
        month += 1;
    }
    let day = remaining as u32 + 1;

    (year, month, day, hour, min, sec)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::error::PolarisError;

    static INIT: std::sync::Once = std::sync::Once::new();

    /// Registers the sqlite-vec extension once and opens a fresh in-memory DB with dim=4.
    fn setup() -> Database {
        INIT.call_once(register_vec_extension);
        Database::open_in_memory(4, "test-model").unwrap()
    }

    /// Shorthand: insert a chunk with a 4-element embedding.
    fn insert_chunk(db: &Database, doc_id: i64, content: &str, emb: [f32; 4]) -> i64 {
        db.insert_chunk(doc_id, content, "", 0, content.len(), 0, &emb).unwrap()
    }

    // -----------------------------------------------------------------------
    // Basic document CRUD
    // -----------------------------------------------------------------------

    #[test]
    fn fresh_db_is_empty() {
        let db = setup();
        let stats = db.get_stats(Path::new("nonexistent")).unwrap();
        assert_eq!(stats.doc_count, 0);
        assert_eq!(stats.chunk_count, 0);
    }

    #[test]
    fn insert_and_get_document_by_path() {
        let db = setup();
        db.insert_document("docs/readme.md", "abc123", Some("Readme"), 512).unwrap();
        let doc = db.get_document_by_path("docs/readme.md").unwrap().unwrap();
        assert_eq!(doc.path, "docs/readme.md");
        assert_eq!(doc.content_hash, "abc123");
        assert_eq!(doc.title, Some("Readme".to_string()));
        assert_eq!(doc.file_size, 512);
    }

    #[test]
    fn get_nonexistent_document_returns_none() {
        let db = setup();
        let result = db.get_document_by_path("no/such/file.md").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_document_hash() {
        let db = setup();
        db.insert_document("file.md", "old_hash", None, 100).unwrap();
        db.update_document_hash("file.md", "new_hash", 200).unwrap();
        let doc = db.get_document_by_path("file.md").unwrap().unwrap();
        assert_eq!(doc.content_hash, "new_hash");
        assert_eq!(doc.file_size, 200);
    }

    #[test]
    fn get_all_document_hashes() {
        let db = setup();
        db.insert_document("a.md", "hash_a", None, 0).unwrap();
        db.insert_document("b.md", "hash_b", None, 0).unwrap();
        let hashes = db.get_all_document_hashes().unwrap();
        assert_eq!(hashes.len(), 2);
        let paths: Vec<&str> = hashes.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.md"));
        assert!(paths.contains(&"b.md"));
    }

    #[test]
    fn insert_duplicate_path_returns_error() {
        let db = setup();
        db.insert_document("dupe.md", "hash1", None, 0).unwrap();
        let result = db.insert_document("dupe.md", "hash2", None, 0);
        assert!(result.is_err(), "inserting duplicate path should fail");
    }

    // -----------------------------------------------------------------------
    // Delete / cascade
    // -----------------------------------------------------------------------

    #[test]
    fn delete_document_cascades_to_chunks() {
        let db = setup();
        let doc_id = db.insert_document("delete_me.md", "hash", None, 0).unwrap();
        insert_chunk(&db, doc_id, "chunk content", [1.0, 0.0, 0.0, 0.0]);

        db.delete_document("delete_me.md").unwrap();

        assert!(db.get_document_by_path("delete_me.md").unwrap().is_none());
        let stats = db.get_stats(Path::new("nonexistent")).unwrap();
        assert_eq!(stats.doc_count, 0);
        assert_eq!(stats.chunk_count, 0);
    }

    #[test]
    fn delete_chunks_for_document_keeps_doc_row() {
        let db = setup();
        let doc_id = db.insert_document("keep_doc.md", "hash", None, 0).unwrap();
        insert_chunk(&db, doc_id, "chunk 1", [1.0, 0.0, 0.0, 0.0]);

        db.delete_chunks_for_document(doc_id).unwrap();

        assert!(db.get_document_by_path("keep_doc.md").unwrap().is_some());
        let stats = db.get_stats(Path::new("nonexistent")).unwrap();
        assert_eq!(stats.doc_count, 1);
        assert_eq!(stats.chunk_count, 0);
    }

    // -----------------------------------------------------------------------
    // KNN search
    // -----------------------------------------------------------------------

    #[test]
    fn knn_on_empty_db_returns_empty() {
        let db = setup();
        let results = db.search_knn(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn knn_returns_nearest_chunk_first() {
        let db = setup();
        let doc_id = db.insert_document("a.md", "hash", None, 0).unwrap();
        insert_chunk(&db, doc_id, "chunk X", [1.0, 0.0, 0.0, 0.0]);
        insert_chunk(&db, doc_id, "chunk Y", [0.0, 1.0, 0.0, 0.0]);

        // Query aligned with X → X should be closer (distance ~0), Y farther (distance ~1)
        let results = db.search_knn(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "chunk X");
        assert!(
            results[0].score < results[1].score,
            "nearest chunk should have smaller score (raw cosine distance)"
        );
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    #[test]
    fn get_stats_correct_counts() {
        let db = setup();
        let doc_id = db.insert_document("stats.md", "hash", None, 0).unwrap();
        insert_chunk(&db, doc_id, "first chunk", [1.0, 0.0, 0.0, 0.0]);
        insert_chunk(&db, doc_id, "second chunk", [0.0, 1.0, 0.0, 0.0]);

        let stats = db.get_stats(Path::new("nonexistent")).unwrap();
        assert_eq!(stats.doc_count, 1);
        assert_eq!(stats.chunk_count, 2);
        assert_eq!(stats.embedding_dim, 4);
    }

    // -----------------------------------------------------------------------
    // Transaction helpers
    // -----------------------------------------------------------------------

    #[test]
    fn transaction_commit_persists_data() {
        let db = setup();
        db.begin().unwrap();
        db.insert_document("committed.md", "hash", None, 0).unwrap();
        db.commit().unwrap();

        assert!(db.get_document_by_path("committed.md").unwrap().is_some());
    }

    #[test]
    fn transaction_rollback_discards_changes() {
        let db = setup();
        // Insert one doc outside any explicit transaction (auto-committed).
        db.insert_document("persistent.md", "hash1", None, 0).unwrap();

        // Begin, insert second doc, then rollback.
        db.begin().unwrap();
        db.insert_document("rolled_back.md", "hash2", None, 0).unwrap();
        db.rollback();

        assert!(db.get_document_by_path("persistent.md").unwrap().is_some());
        assert!(db.get_document_by_path("rolled_back.md").unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Dimension mismatch (file-based DB required)
    // -----------------------------------------------------------------------

    #[test]
    fn dimension_mismatch_returns_error() {
        INIT.call_once(register_vec_extension);
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("mismatch.db");

        // Create DB with dim=4.
        { let _db = Database::open(&db_path, 4, "test-model").unwrap(); }

        // Re-open with a different dimension → must error.
        let result = Database::open(&db_path, 8, "test-model");
        match result {
            Err(PolarisError::DimensionMismatch { db_dim, config_dim }) => {
                assert_eq!(db_dim, 4);
                assert_eq!(config_dim, 8);
            }
            _ => panic!("expected DimensionMismatch error"),
        }
    }

    #[test]
    fn model_mismatch_returns_error() {
        INIT.call_once(register_vec_extension);
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("model_mismatch.db");

        // Create DB with model-a.
        { let _db = Database::open(&db_path, 4, "model-a").unwrap(); }

        // Re-open with a different model → must error.
        let result = Database::open(&db_path, 4, "model-b");
        match result {
            Err(PolarisError::ModelMismatch { db_model, config_model }) => {
                assert_eq!(db_model, "model-a");
                assert_eq!(config_model, "model-b");
            }
            _ => panic!("expected ModelMismatch error"),
        }
    }

    #[test]
    fn same_model_id_opens_successfully() {
        INIT.call_once(register_vec_extension);
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("model_ok.db");

        { let _db = Database::open(&db_path, 4, "nomic-embed-text-v1.5").unwrap(); }

        // Same model → no error.
        let db = Database::open(&db_path, 4, "nomic-embed-text-v1.5").unwrap();
        assert_eq!(db.model_id, "nomic-embed-text-v1.5");
    }

    // -----------------------------------------------------------------------
    // BM25 / FTS5 search
    // -----------------------------------------------------------------------

    #[test]
    fn bm25_on_empty_db_returns_empty() {
        let db = setup();
        let results = db.search_bm25("anything", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn bm25_matches_inserted_content() {
        let db = setup();
        let doc_id = db.insert_document("bm25.md", "hash", None, 0).unwrap();
        db.insert_chunk(doc_id, "Rust programming language", "", 0, 25, 0, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        db.insert_chunk(doc_id, "Python scripting", "", 0, 16, 1, &[0.0, 1.0, 0.0, 0.0]).unwrap();

        let results = db.search_bm25("Rust", 5).unwrap();
        assert_eq!(results.len(), 1, "only the Rust chunk should match");
        assert_eq!(results[0].bm25_rank, 1);
    }

    #[test]
    fn bm25_respects_limit() {
        let db = setup();
        let doc_id = db.insert_document("limit.md", "hash", None, 0).unwrap();
        for i in 0..5 {
            let content = format!("keyword content item {i}");
            db.insert_chunk(doc_id, &content, "", 0, content.len(), i, &[1.0, 0.0, 0.0, 0.0])
                .unwrap();
        }
        let results = db.search_bm25("keyword", 2).unwrap();
        assert_eq!(results.len(), 2, "limit of 2 should be respected");
    }

    #[test]
    fn bm25_delete_removes_fts_entries() {
        let db = setup();
        let doc_id = db.insert_document("deletefts.md", "hash", None, 0).unwrap();
        db.insert_chunk(doc_id, "unique_token_xyz content", "", 0, 24, 0, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();

        // Verify it's findable.
        assert_eq!(db.search_bm25("unique_token_xyz", 5).unwrap().len(), 1);

        // Delete the document (cascade) and verify FTS is clean.
        db.delete_document("deletefts.md").unwrap();
        assert_eq!(
            db.search_bm25("unique_token_xyz", 5).unwrap().len(),
            0,
            "deleted chunk should not appear in FTS"
        );
    }

    #[test]
    fn bm25_delete_chunks_removes_fts_entries() {
        let db = setup();
        let doc_id = db.insert_document("deletechunks.md", "hash", None, 0).unwrap();
        db.insert_chunk(doc_id, "another_unique_token content", "", 0, 28, 0, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();

        assert_eq!(db.search_bm25("another_unique_token", 5).unwrap().len(), 1);

        db.delete_chunks_for_document(doc_id).unwrap();
        assert_eq!(
            db.search_bm25("another_unique_token", 5).unwrap().len(),
            0,
            "deleted chunks should not appear in FTS"
        );
    }

    // -----------------------------------------------------------------------
    // Migration v1 → v2
    // -----------------------------------------------------------------------

    #[test]
    fn migration_v1_to_v2_backfills_fts() {
        INIT.call_once(register_vec_extension);
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("migrate.db");

        // Simulate a v1 database: create schema manually without FTS5 table.
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
            ").unwrap();
            conn.execute_batch(
                "CREATE VIRTUAL TABLE vec_chunks USING vec0(
                    chunk_id INTEGER PRIMARY KEY,
                    embedding float[4] distance_metric=cosine
                );"
            ).unwrap();
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES
                    ('schema_version', '1'),
                    ('embedding_dim',  '4'),
                    ('model_id',       'test')",
                [],
            ).unwrap();

            // Insert a document + chunk directly (bypassing our insert_chunk which syncs FTS).
            let now = "2024-01-01T00:00:00Z";
            conn.execute(
                "INSERT INTO documents (path, content_hash, title, indexed_at, file_size)
                 VALUES ('migrate_test.md', 'hash', NULL, ?1, 0)",
                rusqlite::params![now],
            ).unwrap();
            conn.execute(
                "INSERT INTO chunks (document_id, content, heading_context, start_byte, end_byte, chunk_index)
                 VALUES (1, 'migration_test_token content', '', 0, 28, 0)",
                [],
            ).unwrap();
            let emb_bytes: Vec<u8> = [1.0_f32, 0.0, 0.0, 0.0]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            conn.execute(
                "INSERT INTO vec_chunks (chunk_id, embedding) VALUES (1, ?1)",
                rusqlite::params![emb_bytes],
            ).unwrap();
        }

        // Open with our Database — should trigger migration.
        let db = Database::open(&db_path, 4, "test").unwrap();

        // After migration, the pre-existing chunk should be findable via BM25.
        let results = db.search_bm25("migration_test_token", 5).unwrap();
        assert_eq!(results.len(), 1, "backfilled chunk should be found after migration");
    }

    // -----------------------------------------------------------------------
    // WAL mode (Phase 1)
    // -----------------------------------------------------------------------

    #[test]
    fn wal_mode_enabled_on_file_db() {
        INIT.call_once(register_vec_extension);
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("wal_test.db");
        let db = Database::open(&db_path, 4, "test-model").unwrap();
        assert_eq!(db.journal_mode(), "wal", "WAL mode should be active on file-based DB");
    }

    #[test]
    fn in_memory_db_opens_successfully_with_pragmas() {
        // WAL is a no-op on :memory: but open_in_memory should not fail.
        let db = setup();
        let stats = db.get_stats(Path::new("nonexistent")).unwrap();
        assert_eq!(stats.doc_count, 0);
    }

    // -----------------------------------------------------------------------
    // Batch embedding fetch — no N+1 (Phase 2)
    // -----------------------------------------------------------------------

    #[test]
    fn search_knn_with_embeddings_batch_fetch_returns_correct_results() {
        let db = setup();
        let doc_id = db.insert_document("batch.md", "hash", None, 0).unwrap();
        db.insert_chunk(doc_id, "chunk alpha", "", 0, 11, 0, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        db.insert_chunk(doc_id, "chunk beta",  "", 0, 10, 1, &[0.0, 1.0, 0.0, 0.0]).unwrap();
        db.insert_chunk(doc_id, "chunk gamma", "", 0, 11, 2, &[0.0, 0.0, 1.0, 0.0]).unwrap();

        let results = db.search_knn_with_embeddings(&[1.0, 0.0, 0.0, 0.0], 3).unwrap();
        assert_eq!(results.len(), 3, "should return all 3 chunks");
        // Each result must carry a non-empty embedding.
        for r in &results {
            assert_eq!(r.embedding.len(), 4, "embedding length must equal dim");
        }
        // The chunk most aligned with [1,0,0,0] should come first.
        assert_eq!(results[0].content, "chunk alpha");
    }
}
