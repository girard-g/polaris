# Embedding Engine

## Model

Polaris uses the **Nomic Embed Text v1.5** model via [fastembed](https://github.com/Anyscale-AI/fastembed-rs):

| Property | Value |
|----------|-------|
| Model ID | `nomic-embed-text-v1.5` |
| Backend | ONNX (CPU) |
| Native dimension | 768 |
| Default working dim | 512 (Matryoshka truncation) |
| Download size | ~137 MB |
| Cache location | `~/.cache/huggingface/` |

The model supports **Matryoshka Representation Learning**, meaning the first N dimensions of the full 768-dim vector are meaningful on their own. Polaris defaults to 512 dimensions, which gives a good balance between search quality and storage size.

## Task Prefixes

The Nomic model is trained with task-specific prefixes that improve retrieval quality. Polaris always applies them:

| Context | Prefix |
|---------|--------|
| Indexing documents | `"search_document: "` |
| Querying | `"search_query: "` |

Omitting these prefixes degrades retrieval quality. Both the document text and the query must use the correct prefix.

## Embedding Pipeline

```
Input text
  │
  ├─ Prepend prefix ("search_document: " or "search_query: ")
  │
  ├─ fastembed batch encode (ONNX CPU inference)
  │    → raw Vec<f32> of length 768
  │
  ├─ Truncate to target_dim (e.g. 512)
  │    → slice first 512 values
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
let engine = EmbeddingEngine::new(target_dim)?;

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

## Changing the Model

Changing `model_id` in the config requires:
1. Deleting or moving the existing database
2. Re-indexing all documents from scratch

The DB stores the model ID in the `metadata` table but does not currently validate it against the config. A future improvement would be to error on model ID mismatch similarly to how dimension mismatch is handled.
