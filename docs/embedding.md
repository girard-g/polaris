# Embedding Engine

## Supported Models

| Model ID | Native dim | Default dim | Download |
|----------|-----------|-------------|---------|
| `nomic-embed-text-v1.5` (default) | 768 | 512 | ~137 MB |
| `mxbai-embed-large-v1` | 1024 | 1024 | ~670 MB |
| `all-minilm-l6-v2` | 384 | 384 | ~23 MB |

All models run via ONNX on CPU. Model files are cached in `~/.cache/huggingface/`.

## Matryoshka Truncation

`nomic-embed-text-v1.5` supports Matryoshka Representation Learning — the first N
dimensions of the full 768-dim vector are independently meaningful.
Polaris defaults to 512 dims for nomic (good balance of quality vs. storage).
`mxbai` and `all-minilm` do not support truncation; their native dim is used.

## Task Prefixes

Each model requires specific prefixes to be prepended before encoding:

| Model | Document prefix | Query prefix |
|-------|----------------|--------------|
| `nomic-embed-text-v1.5` | `search_document: ` | `search_query: ` |
| `mxbai-embed-large-v1` | _(none)_ | `Represent this sentence for searching relevant passages: ` |
| `all-minilm-l6-v2` | _(none)_ | _(none)_ |

Polaris applies prefixes automatically — no user action required.

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
}
```

The lock is acquired only for the duration of the `embed()` call and released immediately. The `EmbeddingEngine` itself is `Send + Sync` and can be shared via `Arc<EmbeddingEngine>`.

## Batch Size

During indexing, chunks are embedded in batches of **32** (`EMBED_BATCH_SIZE = 32`). This is a memory-efficiency trade-off: larger batches are faster but require more RAM.
