# Embedding Engine

## Supported Models

| Model ID | Native dim | Default dim | Default threshold | Download |
|----------|-----------|-------------|-------------------|---------|
| `nomic-embed-text-v1.5` | 768 | 512 | 0.63 | ~522 MB |
| `nomic-embed-text-v1.5-quantized` | 768 | 512 | 0.63 | ~131 MB |
| `embeddinggemma-300m` | 768 | 768 | 0.42 | ~1.2 GB |
| `mxbai-embed-large-v1` | 1024 | 1024 | uncalibrated | ~670 MB |
| `all-minilm-l6-v2` | 384 | 384 | uncalibrated | ~23 MB |

All models run via ONNX on CPU. Model files are cached in a user-global directory shared across projects (default `~/.cache/polaris/models/`; overridable via `POLARIS_CACHE_DIR`). See [Configuration → Model Caching](configuration.md#model-caching).

## Matryoshka Truncation

`nomic-embed-text-v1.5` (and its quantized variant) and `embeddinggemma-300m` support Matryoshka
Representation Learning — the first N dimensions of the full 768-dim vector are independently
meaningful. Polaris defaults to 512 dims for nomic (good balance of quality vs. storage) and 768
for `embeddinggemma-300m`, the configuration it was measured at.
`mxbai` and `all-minilm` do not support truncation; their native dim is used.

## Task Prefixes

Each model requires specific prefixes to be prepended before encoding:

| Model | Document prefix | Query prefix |
|-------|----------------|--------------|
| `nomic-embed-text-v1.5` | `search_document: ` | `search_query: ` |
| `nomic-embed-text-v1.5-quantized` | `search_document: ` | `search_query: ` |
| `embeddinggemma-300m` | `title: none \| text: ` | `task: search result \| query: ` |
| `mxbai-embed-large-v1` | _(none)_ | `Represent this sentence for searching relevant passages: ` |
| `all-minilm-l6-v2` | _(none)_ | _(none)_ |

Polaris applies prefixes automatically — no user action required.

## Quantized nomic

`nomic-embed-text-v1.5-quantized` is the same model with int8 weights. Measured against
the default on a Ryzen 7 8840HS (CPU, batch size 1):

| | `nomic-embed-text-v1.5` | `-quantized` |
|---|---|---|
| Indexing, English docs (862 chunks) | 9.5 chunks/s, 1.0 GB peak RSS | 21.0 chunks/s, 431 MB |
| Indexing, French docs (1,128 chunks) | 11.7 chunks/s, 1.1 GB | 26.2 chunks/s, 448 MB |
| Cold `polaris search` (process start → result) | 728 ms | 318 ms |
| English, 40 real queries: Recall@1 / @3 / MRR@5 | 75.0% / 87.5% / 0.805 | 72.5% / 80.0% / 0.771 |
| French, `polaris eval`: recall@1 / MRR | 0.85 / 0.88 | 0.87 / 0.89 |

Pick it when indexing time, memory, or the search hook's cold-start latency matter more than
the last few points of recall. The English real-query gap is 3 queries out of 40 — inside the
noise at that sample size, but it is the one test where the quantized model lost. Run
`polaris eval` on your own corpus after switching; the suggested `search_min_similarity`
stayed at 0.63–0.64 in the measurements above.

Its vectors are not interchangeable with the fp32 model's (quantized queries against an fp32
index scored worse than either model alone), so it has its own `model_id` and switching
requires a re-index like any other model change.

## Switching Models

Changing models requires re-indexing from scratch. The database stores the model ID
in the `metadata` table; opening a database with a mismatched model produces a clear
error and suggests deleting the database and re-indexing.

## Embedding Pipeline

```
Input text
  │
  ├─ Prepend prefix (model-specific, applied automatically)
  │
  ├─ fastembed batch encode (ONNX CPU inference)
  │    → raw Vec<f32> of native length
  │
  ├─ Truncate to target_dim (nomic only; others use native dim)
  │    → slice first N values
  │
  └─ L2 normalize
       norm = √(Σ xᵢ²)
       xᵢ  = xᵢ / norm   (if norm > 1e-10)
       → unit-length Vec<f32>
```

After normalization, cosine similarity equals the dot product.

## EmbeddingEngine API

```rust
// Create engine (loads model, validates dim)
let engine = EmbeddingEngine::new(target_dim, model_id)?;

// Embed a batch of document strings (apply doc prefix)
let embeddings: Vec<Vec<f32>> = engine.embed_documents(&texts)?;

// Embed a single query string (apply query prefix)
let embedding: Vec<f32> = engine.embed_query(&query)?;

// Get the configured dimension
let dim: usize = engine.dim();
```

## Thread Safety

`fastembed::TextEmbedding::embed()` takes `&mut self`, so it cannot be called concurrently. The engine wraps the model in a `Mutex`:

```rust
pub struct EmbeddingEngine {
    model: Mutex<TextEmbedding>,
    target_dim: usize,
    doc_prefix: String,    // model-specific document prefix
    query_prefix: String,  // model-specific query prefix
}
```

The lock is acquired only for the duration of the `embed()` call and released immediately. The `EmbeddingEngine` itself is `Send + Sync` and can be shared via `Arc<EmbeddingEngine>`.

## Batch Size

During indexing, chunks are embedded one at a time (`EMBED_BATCH_SIZE = 1`). fastembed pads each batch to its longest sequence and ONNX Runtime already uses every core for a single sequence, so on CPU larger batches are both slower and far heavier on RAM (batch 32: 4.2 chunks/s at 6.5 GB; batch 1: 9.4 chunks/s at 1.0 GB). See [indexing.md](indexing.md#why-batch-size-1).
