# Architecture

Polaris is a Cargo workspace with two crates:

- **`polaris-core`** — the retrieval pipeline as a library (`bank`, `config`, `db`, `embedding`, `error`, `indexer`, `paths`, `search`).
- **`polaris-cli`** — the binary: CLI entry point, MCP server, setup orchestration, savings reporter, TUI.

## Module Map

```
polaris-core/src/
├── lib.rs          Library entry — re-exports the public API
├── config.rs       PolarisConfig (TOML + CLI overrides), IndexOpts, SearchOpts
├── error.rs        PolarisError enum + Result alias
├── paths.rs        polaris_cache_dir() — global ONNX model cache resolver
├── embedding.rs    EmbeddingEngine / SharedEmbedding — fastembed wrapper
├── db.rs           Database — SQLite schema, CRUD, KNN + BM25, migrations, search_log
├── bank.rs         Bank + BankSet — per-project DB handle, multi-DB fused search
├── indexer.rs      Indexer — file discovery, chunking, incremental sync
└── search.rs       SearchEngine — hybrid: KNN + BM25 → RRF → MMR

polaris-cli/src/
├── main.rs         CLI entry point — parses args, dispatches commands
├── setup.rs        `polaris setup` — gitignore + agent-instruction-file orchestration
├── savings.rs      `polaris savings` — query the search_log table
├── tui.rs          Progress UI helpers for indexing
└── mcp/
    ├── mod.rs      Re-exports
    ├── server.rs   PolarisServer — tool implementations, ServerHandler
    └── types.rs    Tool parameter schemas (serde + schemars)
```

## Component Responsibilities

### `polaris-cli/src/main.rs`
- CLI argument parsing via clap
- Bootstrap: load config → register sqlite-vec extension → open Bank → init embedding engine
- Route to: `index`, `search`, `serve`, `status`, `watch`, `chunks`, `setup`, `savings`
- Logging setup (stderr for `serve`, stdout otherwise)

### `polaris-core/src/config.rs`
- `PolarisConfig` is the central config struct (serde-deserializable from TOML)
- Load priority: explicit `--config` flag → `./polaris.toml` → platform config dir → defaults
- Platform config dir: `~/.config/polaris/` (Linux), `~/Library/Application Support/polaris/` (macOS), `%APPDATA%\polaris\` (Windows)
- `apply_overrides()` handles `--db`, `--dim`, and `--model` CLI flags after loading
- Fields: `db_path`, `embedding_dim`, `max_chunk_tokens`, `chunk_overlap_chars`, `model_id`, `mmr_lambda`, `mmr_candidate_multiplier`, `heading_boost`, `rrf_k`, `max_top_k`, `max_file_size`, `extra_db_paths`
- Also defines `IndexOpts` and `SearchOpts` used by `Bank`

### `polaris-core/src/error.rs`
- `PolarisError` with `thiserror` — variants: Embedding, Database, Io, Indexing, Config, Mcp, Setup, DimensionMismatch, ModelMismatch
- `Result<T>` alias used throughout the codebase
- Exception: `polaris-cli/src/mcp/server.rs` does NOT import this alias (conflicts with rmcp macro-generated code) and uses `PolarisError` directly

### `polaris-core/src/paths.rs`
- `polaris_cache_dir()` resolves the global ONNX model cache directory
- Resolution order: `$POLARIS_CACHE_DIR/models` → `dirs::cache_dir()/polaris/models` → error
- Directory is created on resolve so fastembed can write into it

### `polaris-core/src/embedding.rs`
- `EmbeddingEngine` wraps `TextEmbedding` from fastembed inside a `Mutex` and carries `target_dim` plus the model's `doc_prefix` / `query_prefix`
- Mutex required because `TextEmbedding::embed()` takes `&mut self`
- Exposes `embed_documents(&[String])` and `embed_query(&str)` — both apply task prefix and L2-normalize
- Matryoshka truncation: native-dim output (768 for nomic, 1024 for mxbai, 384 for minilm) is sliced to `target_dim`
- `SharedEmbedding` is an `Arc`-wrapped handle shared by `Bank` / `BankSet`

### `polaris-core/src/db.rs`
- `register_vec_extension()` must be called once before any `Connection` is opened
- `Database::open(path, dim, model_id)` creates schema on first run; validates `embedding_dim` and `model_id` against metadata on subsequent runs; applies migrations automatically
- Schema v3: `documents`, `chunks`, `vec_chunks` (sqlite-vec KNN), `chunks_fts` (FTS5 BM25), `search_log` (savings telemetry)
- `search_knn_with_embeddings()` — KNN with per-result embedding fetch (for MMR)
- `search_bm25()` — FTS5 BM25 ranked search
- `get_chunk_with_metadata()` — hydrate BM25-only results with content + embedding
- `insert_search_log()` / `aggregate_savings()` — append-only telemetry feeding `polaris savings`
- FTS5 writes are manually synchronized on insert and delete
- Central structs: `DocumentRecord`, `ChunkRecord`, `SearchResult`, `SearchResultWithEmbedding`, `Bm25Result`, `DbStats`, `SearchLogRow`, `SavingsAggregate`

### `polaris-core/src/bank.rs`
- `Bank` is the per-project handle bundling a `Database`, embedding engine, and indexing/search config
- Internally `Arc<BankInner>` with a `Mutex<Database>` — `Bank::clone()` is cheap
- `BankSet` mounts multiple `Bank`s for fused multi-DB search (primary + `extra_db_paths`)

### `polaris-core/src/indexer.rs`
- `Indexer` holds `Arc<EmbeddingEngine>` + chunking config
- `index_path()` runs a **three-phase pipeline** optimised for large corpora:
  - **Phase A (parallel collect):** `rayon::par_iter()` reads each file once — SHA256 from in-memory bytes, then `chunk_markdown()`. Concurrent across all CPU cores.
  - **Phase B (cross-file embedding):** All chunks from all pending files are flattened and embedded in batches of 32. Batches are always full (except the last), maximising ONNX throughput.
  - **Phase C (single-transaction write):** One `BEGIN`/`COMMIT` for the entire run.
- `Chunk` struct carries `heading_context` (e.g. `"Guide > Installation"`)
- Files larger than `max_file_size` (default 10 MB) are skipped with an error recorded in `IndexReport.errors`
- `normalise_path()` is exported so callers can pre-normalise paths; CLI commands like `chunks` apply it automatically

### `polaris-core/src/search.rs`
- `SearchEngine<'a>` borrows both `EmbeddingEngine` and `Database` by reference
- Hybrid pipeline: vector KNN + BM25 → RRF fusion → heading boost → MMR rerank → `Vec<SearchResult>`
- `search()` normalises scores to `[0, 1]` per result set (top result = 1.0); `search_raw()` returns the raw RRF score for `BankSet` to fuse across multiple banks before its own normalisation pass
- `compute_rrf_scores()` — pure function, rank-based fusion (scale-invariant)
- `compute_heading_boost()` — additive bonus for heading term matches
- `mmr_rerank()` — greedy Maximal Marginal Relevance selection
- `format_results()` — formats to markdown string; `score` field holds the final normalised [0,1] score

### `polaris-cli/src/mcp/server.rs`
- `PolarisState` holds `config: Arc<PolarisConfig>` and `bank: polaris_core::Bank`
- `Bank` clones cheaply (`Arc<BankInner>` internally) and serialises concurrent access through its own `Mutex<Database>`; MCP tool calls are typically serial so this single-connection model is acceptable
- `PolarisServer` implements `ServerHandler` with three `#[tool]` methods: `search`, `index`, `status`
- Each tool clones `config` / `bank`, then offloads blocking work via `tokio::task::spawn_blocking`
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
  → MMR rerank (greedy, lambda from config)
  → normalise scores to [0, 1] (top result = 1.0)
  → top_k Vec<SearchResult>
  → format_results() → markdown string
```

## Shared State in MCP Mode

```
PolarisState {
    config: Arc<PolarisConfig>     (read-only)
    bank:   polaris_core::Bank     (Arc<BankInner> with Mutex<Database> inside)
}
```

`Bank` owns the single `rusqlite::Connection` (WAL mode) and serialises access through its internal mutex. Tool handlers clone the cheap `Bank` handle before entering `spawn_blocking` and never hold a lock across an `.await` point.

## Threading Model

- tokio runtime runs the MCP server loop
- Tool handlers are `async fn`; all blocking work (embedding, SQLite, filesystem) runs on a dedicated thread pool via `tokio::task::spawn_blocking`
- Each tool clones the relevant `Arc`s / `Bank` handle before entering `spawn_blocking`; the DB mutex is acquired inside the closure, never across an `.await` point
- WAL mode is still enabled on the underlying connection, but a single-connection model means concurrent tool calls serialise on the bank mutex
