# Search

## How It Works

Polaris uses **hybrid search**: vector KNN combined with BM25 full-text search, fused via Reciprocal Rank Fusion (RRF), then reranked with MMR for diversity.

```
Query string
  → EmbeddingEngine::embed_query()
      "search_query: " + query
      → fastembed encode → truncate → L2 normalize
  → Database::search_knn_with_embeddings(embedding, top_k × multiplier)
      → KNN candidates with stored embeddings (for MMR)
  → Database::search_bm25(query, top_k × multiplier)
      → FTS5 MATCH query, ordered by BM25 rank
      → unwrap_or_default() on error (graceful fallback to vector-only)
  → compute_rrf_scores(vector_results, bm25_results, rrf_k)
      → score(d) = 1/(k + rank_vector(d)) + 1/(k + rank_bm25(d))
  → fetch metadata + embeddings for BM25-only results
  → heading boost: additive bonus for heading term matches
  → MMR rerank: greedy diversity selection
  → top_k results, score stored in `distance` field
  → format_results() → Markdown string
```

## SearchResult

```rust
pub struct SearchResult {
    pub chunk_id:        i64,
    pub content:         String,
    pub heading_context: String,  // e.g. "# Guide > ## Auth"
    pub file_path:       String,
    pub distance:        f32,     // RRF + heading boost score (higher = better)
}
```

Note: `distance` no longer stores cosine distance. It holds the final combined score (RRF + heading boost) for display purposes.

## Scoring

### Reciprocal Rank Fusion (RRF)

Each chunk's score combines its rank in the vector list and BM25 list:

```
score(d) = 1 / (k + rank_vector(d))  +  1 / (k + rank_bm25(d))
```

- `k = 60` by default (`rrf_k` in config)
- If a chunk only appears in one list, it gets one term only
- Higher score = better match
- Typical range: `0.01–0.04` for single-list hits, up to ~`0.09` for top hits in both lists

### Heading Boost

An additive bonus is applied when query terms appear in the chunk's heading context:

```
boost = heading_boost * (matching_terms / total_terms)
```

Only terms ≥ 3 characters are counted. Default `heading_boost = 0.05`.

### MMR Reranking

After scoring, Maximal Marginal Relevance selects results that balance relevance and diversity:

```
MMR(d) = λ × score(d)  −  (1 − λ) × max_sim(d, already_selected)
```

Default `mmr_lambda = 0.7` (favours relevance over diversity).

## Result Format

```markdown
### Result 1 — score: 0.041
**Section:** Guide > Authentication
**File:** `docs/guide.md`

To configure authentication, set the `AUTH_TOKEN` environment variable...

---
### Result 2 — score: 0.028
**Section:** Reference > API
**File:** `docs/reference.md`

The `/auth` endpoint accepts a Bearer token...

---
```

Empty result set returns: `"No results found."`

## SearchEngine API

```rust
pub struct SearchEngine<'a> {
    embedding_engine: &'a EmbeddingEngine,
    db: &'a Database,
    mmr_lambda: f32,
    candidate_multiplier: usize,
    heading_boost: f32,
    rrf_k: usize,
}

impl<'a> SearchEngine<'a> {
    pub fn new(
        embedding_engine: &'a EmbeddingEngine,
        db: &'a Database,
        mmr_lambda: f32,
        candidate_multiplier: usize,
        heading_boost: f32,
        rrf_k: usize,
    ) -> Self;

    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>>;

    pub fn format_results(results: &[SearchResult]) -> String;
}
```

`SearchEngine` is a thin facade — construct it per-call.

## Candidate Pool

Both KNN and BM25 retrieve `top_k × mmr_candidate_multiplier` candidates (default: `top_k × 3`). After RRF fusion, the merged pool is heading-boosted and MMR-reranked down to `top_k`.

## Default top_k

- CLI: `5` (configurable with `-k N`)
- MCP `search` tool: `5` (configurable via `top_k` parameter)

## Graceful Fallback

If BM25 fails (e.g. FTS5 query syntax error, empty FTS table), `search_bm25` returns an empty list and search degrades to vector-only. No error is surfaced to the caller.

## Performance

On a typical laptop:
- Query embedding: ~50–100 ms (cold), ~5–10 ms (warm, model cached in RAM)
- KNN + BM25 lookup: <5 ms for databases with fewer than 10,000 chunks
- Total round-trip: under 100 ms for warm searches

The model is loaded once at startup and kept in memory for the lifetime of the process.
