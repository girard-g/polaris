# Architecture

## Module Map

```
src/
├── main.rs         CLI entry point — parses args, dispatches commands
├── config.rs       Config loading (TOML + CLI overrides)
├── error.rs        PolarisError enum + Result alias
├── embedding.rs    EmbeddingEngine — wraps fastembed, handles prefix + normalization
├── db.rs           Database — SQLite schema, CRUD, KNN + BM25 search, migrations
├── indexer.rs      Indexer — file discovery, chunking, incremental sync
├── search.rs       SearchEngine — hybrid search: KNN + BM25 → RRF → MMR
└── mcp/
    ├── mod.rs      Re-exports
    ├── server.rs   PolarisServer — tool implementations, ServerHandler
    └── types.rs    Tool parameter schemas (serde + schemars)
```

## Component Responsibilities

### `main.rs`
- CLI argument parsing via clap
- Bootstrap: load config → register sqlite-vec extension → open DB → init embedding engine
- Route to: `index`, `search`, `serve`, or `status`
- Logging setup (stderr for `serve`, stdout otherwise)

### `config.rs`
- `PolarisConfig` is the central config struct (serde-deserializable from TOML)
- Load priority: explicit `--config` flag → `./polaris.toml` → platform config dir → defaults
- Platform config dir: `~/.config/polaris/` (Linux), `~/Library/Application Support/polaris/` (macOS), `%APPDATA%\polaris\` (Windows)
- `apply_overrides()` handles `--dim` and `--db` CLI flags after loading
- Fields: `db_path`, `embedding_dim`, `max_chunk_tokens`, `chunk_overlap_chars`, `model_id`, `mmr_lambda`, `mmr_candidate_multiplier`, `heading_boost`, `rrf_k`

### `error.rs`
- `PolarisError` with `thiserror` — distinct variants for Embedding, Database, IO, Config, MCP, DimensionMismatch, ModelMismatch
- `Result<T>` alias used throughout the codebase
- Exception: `mcp/server.rs` does NOT import this alias (conflicts with rmcp macro-generated code)

### `embedding.rs`
- `EmbeddingEngine` wraps `TextEmbedding` from fastembed inside a `Mutex`
- Mutex required because `TextEmbedding::embed()` takes `&mut self`
- Exposes `embed_documents(&[String])` and `embed_query(&str)` — both apply task prefix and L2-normalize
- Matryoshka truncation: full 768-dim output is sliced to `target_dim`

### `db.rs`
- `register_vec_extension()` must be called once before any `Connection` is opened
- `Database::open(path, dim, model_id)` creates schema on first run; validates `embedding_dim` and `model_id` against metadata on subsequent runs; applies migrations automatically
- Schema v2: `documents`, `chunks`, `vec_chunks` (sqlite-vec KNN), `chunks_fts` (FTS5 BM25)
- `search_knn_with_embeddings()` — KNN with per-result embedding fetch (for MMR)
- `search_bm25()` — FTS5 BM25 ranked search
- `get_chunk_with_metadata()` — hydrate BM25-only results with content + embedding
- FTS5 writes are manually synchronized on insert and delete
- Central structs: `DocumentRecord`, `ChunkRecord`, `SearchResult`, `SearchResultWithEmbedding`, `Bm25Result`, `DbStats`

### `indexer.rs`
- `Indexer` holds `Arc<EmbeddingEngine>` + chunking config
- `index_path()` runs a **three-phase pipeline** optimised for large corpora:
  - **Phase A (parallel collect):** `rayon::par_iter()` reads each file once — SHA256 from in-memory bytes, then `chunk_markdown()`. Concurrent across all CPU cores.
  - **Phase B (cross-file embedding):** All chunks from all pending files are flattened and embedded in batches of 32. Batches are always full (except the last), maximising ONNX throughput.
  - **Phase C (single-transaction write):** One `BEGIN`/`COMMIT` for the entire run.
- `Chunk` struct carries `heading_context` (e.g. `"Guide > Installation"`)
- `FileData` struct is the intermediate representation produced by Phase A

### `search.rs`
- `SearchEngine<'a>` borrows both `EmbeddingEngine` and `Database` by reference
- Hybrid pipeline: vector KNN + BM25 → RRF fusion → heading boost → MMR rerank → `Vec<SearchResult>`
- `compute_rrf_scores()` — pure function, rank-based fusion (scale-invariant)
- `compute_heading_boost()` — additive bonus for heading term matches
- `mmr_rerank()` — greedy Maximal Marginal Relevance selection
- `format_results()` — formats to markdown string; `score` field holds the final normalized [0,1] score

### `mcp/server.rs`
- `PolarisState` holds `Arc<PolarisConfig>`, `Arc<EmbeddingEngine>`, `read_db: Arc<Mutex<Database>>`, `write_db: Arc<Mutex<Database>>`
- Two separate DB connections to the same file so reads (search, status) and writes (index) don't mutually serialize under WAL mode
- `lock_db()` helper acquires a `Mutex` guard or returns a formatted error string — used by all three tool closures
- `PolarisServer` implements `ServerHandler` with three `#[tool]` methods
- Each tool clones required `Arc`s, then offloads all blocking work via `tokio::task::spawn_blocking`
- DB mutex is acquired inside the blocking closure (never across an `.await`)
- Tool errors are returned as formatted strings (not MCP error objects) for simplicity

## Data Flow

### Indexing

```
Path on disk
  → walkdir (discover .md files)
  → DB hash load (existing SHA256 per path)

── Phase A (rayon par_iter) ──────────────────────────────────────────────
  → read_to_string           (single read per file)
  → SHA256 from content bytes (no second read)
  → skip if hash unchanged   (unless --force)
  → pulldown-cmark (parse markdown events + byte offsets)
  → Section extraction (heading-bounded blocks)
  → Chunk splitting (paragraph → sentence → word fallback)
  → Vec<FileData>

── Phase B (sequential, full batches) ────────────────────────────────────
  → Flatten all chunks across all files → Vec<String>
  → Batch embedding (32 chunks/batch via fastembed)
  → Vec<Vec<f32>> (indexed by global chunk position)

── Phase C (single transaction) ──────────────────────────────────────────
  → BEGIN
  → For each file:
      delete old document + chunks (cascade)
      insert documents row
      insert chunks rows
      insert vec_chunks rows (embeddings)
      insert chunks_fts rows (BM25 index)
  → COMMIT
```

### Searching

```
Query string
  → EmbeddingEngine::embed_query()
      → "search_query: " prefix → fastembed → truncate → L2-normalize
  → Database::search_knn_with_embeddings()
      → f32 slice → bytes → SQL KNN MATCH → join chunks + documents
      → top_k × multiplier candidates with stored embeddings
  → Database::search_bm25()
      → FTS5 MATCH query → BM25 rank ORDER BY → limit candidates
      → unwrap_or_default() for graceful fallback
  → compute_rrf_scores(vector_results, bm25_results, rrf_k)
      → score(d) = 1/(k + rank_v) + 1/(k + rank_bm25)
      → returns (scores_map, bm25_only_ids)
  → fetch metadata + embeddings for BM25-only chunks
  → heading boost on RRF scores
  → MMR rerank (greedy, lambda=0.7)
  → top_k results with score in distance field
  → format_results() → markdown string
```

## Shared State in MCP Mode

```
PolarisState {
    config:           Arc<PolarisConfig>       (read-only)
    embedding_engine: Arc<EmbeddingEngine>     (Mutex<TextEmbedding> inside)
    read_db:          Arc<Mutex<Database>>     (search + status)
    write_db:         Arc<Mutex<Database>>     (index)
}
```

Two separate `rusqlite::Connection`s open the same WAL-mode database file. Read operations (search, status) use `read_db`; write operations (index) use `write_db`. Under WAL mode a writer never blocks readers, so a background index call won't stall concurrent search queries.

## Threading Model

- tokio runtime runs the MCP server loop
- Tool handlers are `async fn`; all blocking work (embedding, SQLite, filesystem) runs on a dedicated thread pool via `tokio::task::spawn_blocking`
- Each tool clones the relevant `Arc`s before entering `spawn_blocking`; the DB mutex is acquired inside the closure, never across an `.await` point
- `read_db` and `write_db` each serialize their respective callers through their own mutex; a search and an index can proceed concurrently under WAL mode
