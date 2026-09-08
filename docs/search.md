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
  → report the query-chunk cosine as `score` (absolute, not set-relative)
  → top_k results
  → format_results() → Markdown string
```

## SearchResult

```rust
pub struct SearchResult {
    pub chunk_id:        i64,
    pub content:         String,
    pub heading_context: String,         // e.g. "# Guide > ## Auth"
    pub file_path:       String,
    pub score:           f32,            // Normalised [0, 1]; top result = 1.0
    pub source_db:       Option<String>, // Populated only for multi-DB BankSet results
}
```

RRF decides the **ordering**; the reported `score` is the **query-chunk cosine**. The two are deliberately different: RRF is rank fusion, so a top hit sums to ~0.033 whether or not it answers the query, and it means nothing outside its own result set. Cosine is absolute, which is what lets a caller decide whether a result is worth using at all — see [Confidence](#confidence).

Use `SearchEngine::search_raw` to get both: `(cosine, result)` pairs where `result.score` is still the raw RRF total. `BankSet` uses it to order across banks.

## Scoring

### Reciprocal Rank Fusion (RRF)

Each chunk's score combines its rank in the vector list and BM25 list:

```
score(d) = 1 / (k + rank_vector(d))  +  1 / (k + rank_bm25(d))
```

- `k = 60` by default (`rrf_k` in config)
- If a chunk only appears in one list, it gets one term only
- Higher score = better match
- The raw RRF range is roughly `0.01–0.09`, and it is never displayed — it ranks, it does not score. The `score` a caller sees is the cosine.

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
### Result 1 — score: 1.000
**Section:** Guide > Authentication
**File:** `docs/guide.md`

To configure authentication, set the `AUTH_TOKEN` environment variable...

---
### Result 2 — score: 0.683
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

    /// Like `search`, but returns `(cosine, result)` pairs where `result.score`
    /// is the raw RRF + heading-boost total. Used by `BankSet` to order results
    /// across banks, where RRF is not comparable.
    pub fn search_raw(&self, query: &str, top_k: usize) -> Result<Vec<(f32, SearchResult)>>;

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

## Result ordering

MMR is a selection algorithm, not a presentation one. It decides which chunks
survive — trading a little relevance for diversity, so a result set is not three
paraphrases of the same paragraph — and the engine then orders the survivors by
relevance, because a reader wants the best match first rather than the order a
greedy loop happened to pick them in.

That sort lives in `SearchEngine::search_scored`, which every caller goes
through. It has to live in exactly one place: `BankSet` used to re-sort a single
bank itself while `Bank::search` did not, so `polaris search` on the CLI and the
`search` MCP tool returned the same chunks in different sequences for the same
query — and `polaris eval`, which measures the `Bank` path, was not measuring
what the CLI printed. `tests/bank_set_search.rs` now pins that the two paths
return identical ids and scores.

Multi-bank search is the one case that reorders afterwards, and it has to:
across banks the hybrid score is not comparable — it encodes a chunk's rank
inside its own bank, so merging on it hands roughly half of `top_k` to whichever
other database happens to be mounted. Those merges order on the query-chunk
cosine instead.

Measured with `polaris eval` over 200 corpus sentences, relevance order scores
MRR 0.77 against 0.76 for MMR's own order, with recall@1 and recall@3 identical
at 0.73 and 0.79 — so this is a consistency and readability fix, not a retrieval
win. One caveat on that measurement: eval's queries are sentences lifted from
the corpus, so the correct answer is usually a strong lexical *and* semantic
match, which is a weaker test of diversity ordering than a real ambiguous
question would be.

## Confidence

`score` is the cosine of the query against the chunk embedding, so it means the
same thing across queries, across result sets, and across banks. Scores were
once normalised by the per-call maximum, which pinned the top hit at `1.000` no
matter what — a query the corpus had nothing to say about still came back
looking certain.

Measured on this repo's own index with the default `nomic-embed-text-v1.5`,
by `polaris eval` over 200 corpus sentences plus a hand-built set of off-topic
queries:

| | p10 | median | p95 |
|---|---|---|---|
| on-topic queries | 0.632 | 0.746 | — |
| off-topic queries | — | 0.623 | 0.628 |

The two classes overlap: on-topic queries have been seen as low as 0.607 and
off-topic ones as high as 0.682, so **no threshold separates them cleanly**. The
gate trades false negatives against false positives, and the default sits at the
midpoint `polaris eval` recommends. Off-topic queries that merely *sound*
adjacent to the corpus — deployment, config, auth — are what clears it; plainly
unrelated ones sit at 0.54 and are refused.

Narrowing the index widens the gap: the same measurement over documentation
alone, with design specs excluded, moves on-topic p10 to 0.646 and off-topic p95
to 0.604. Index scope moves the gate, not just the threshold.

The floor is well above zero because the model prefixes queries with
`search_query: `, which gives any two texts a shared baseline. That floor moves
with the model — `all-minilm-l6-v2` uses no prefix and sits far lower — so
`search_min_similarity` (default `0.63`) is config, not a constant, and needs
retuning if you change `model_id`.

Two callers act on it:

- **`polaris.search` (MCP)** returns `No reliable context found` instead of
  results when the best match falls below the threshold. An agent cannot tell a
  weak match from a strong one once the text is in its context, and acting on
  the wrong doc costs more than the search saved.
- **The auto-search hook** (`polaris hook search`) stays silent below it, so an
  unrelated prompt does not get documentation stapled to it.

The MCP tool gates on the **confidence**: the highest query-chunk cosine in the
KNN pool, taken before RRF fusion, the heading boost and MMR have reordered or
truncated anything. That number answers "does this corpus cover the query at
all", and it is a property of the query and the corpus alone.

It has to be, because the obvious alternative is not. Taking the maximum across
the *returned* results makes the verdict depend on how many results the caller
asked for: `candidate_count` is `top_k * mmr_candidate_multiplier`, so a larger
`top_k` draws a wider candidate pool, which changes RRF ranks and changes what
MMR selects. Measured on this repo's own docs, "configure OAuth SSO for the web
dashboard" scored 0.674 at `top_k=2` and 0.646 at `top_k=5` — admitted and
refused by the then-shipped 0.65 gate, same index, same query. KNN is nested, so its
maximum does not move.

The auto-search hook deliberately gates on something else: the cosine of the one
chunk it is about to inject. It hands a single chunk to the model invisibly,
with no scores to weigh, so the question that matters is whether *that chunk* is
relevant — not whether something relevant exists somewhere. Gating it on
confidence would let a strong match elsewhere in the corpus open the gate for a
weak lexical match that happened to rank first. The hook's `top_k` is fixed at
1, so it never had the caller-varying-`top_k` problem in the first place.

`polaris search` on the CLI always shows results with their real scores. Seeing
the near-misses is the point when you are diagnosing retrieval or retuning the
threshold.

`polaris eval` measures the positive and probe distributions on the local
corpus and suggests a `search_min_similarity` for it, replacing the default
measured on this repo.
