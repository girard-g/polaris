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
- `index_path()` runs the full discover → diff → embed → store pipeline
- `Chunk` struct carries `heading_context` (e.g. `"# Guide > ## Install"`)
- Embedding done in batches of 32 for memory efficiency
- Each document is wrapped in a DB transaction (begin → insert → commit or rollback)

### `search.rs`
- `SearchEngine<'a>` borrows both `EmbeddingEngine` and `Database` by reference
- Hybrid pipeline: vector KNN + BM25 → RRF fusion → heading boost → MMR rerank → `Vec<SearchResult>`
- `compute_rrf_scores()` — pure function, rank-based fusion (scale-invariant)
- `compute_heading_boost()` — additive bonus for heading term matches
- `mmr_rerank()` — greedy Maximal Marginal Relevance selection
- `format_results()` — formats to markdown string; `distance` field holds the final score

### `mcp/server.rs`
- `PolarisState` holds `Arc<PolarisConfig>`, `Arc<EmbeddingEngine>`, `Arc<Mutex<Database>>`
- `PolarisServer` implements `ServerHandler` with three `#[tool]` methods
- Each tool clones required `Arc`s, then offloads all blocking work via `tokio::task::spawn_blocking`
- DB mutex is acquired inside the blocking closure (never across an `.await`)
- Tool errors are returned as formatted strings (not MCP error objects) for simplicity

## Data Flow

### Indexing

```
Path on disk
  → walkdir (discover .md files)
  → SHA256 hash (change detection)
  → pulldown-cmark (parse markdown events + byte offsets)
  → Section extraction (heading-bounded blocks)
  → Chunk splitting (paragraph → sentence → word fallback)
  → Batch embedding (32 chunks/batch via fastembed)
  → SQLite transaction:
      insert documents row
      insert chunks rows
      insert vec_chunks rows (embeddings)
      insert chunks_fts rows (BM25 index)
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
    config:           Arc<PolarisConfig>     (read-only)
    embedding_engine: Arc<EmbeddingEngine>   (Mutex<TextEmbedding> inside)
    db:               Arc<Mutex<Database>>   (lock per tool call)
}
```

The `Arc<Mutex<Database>>` means only one tool call can hold the DB at a time. This is acceptable because:
1. The MCP transport is stdio (inherently sequential per session)
2. Indexing is already CPU-bound on the embedding side

## Threading Model

- tokio runtime runs the MCP server loop
- Tool handlers are `async fn`; all blocking work (embedding, SQLite, filesystem) runs on a dedicated thread pool via `tokio::task::spawn_blocking`
- Each tool clones the relevant `Arc`s before entering `spawn_blocking`; the DB mutex is acquired inside the closure, never across an `.await` point
- `Arc<Mutex<Database>>` serializes concurrent tool calls through the mutex (one active DB holder at a time)
