# Configuration

## Config File

Polaris reads a TOML config file. All fields are optional; unset fields use their defaults.

```toml
# SQLite database file path (relative to CWD or absolute)
db_path = "polaris.db"

# Embedding vector dimension
# Must match the dimension already stored in the DB (checked on open)
# Valid range: 64 to the model's native dimension (see "Supported Models" below)
embedding_dim = 512

# Maximum chunk size in approximate tokens (1 token ≈ 4 chars)
# Chunks that exceed this are split at paragraph/sentence/word boundaries
max_chunk_tokens = 450

# Overlap in characters between adjacent chunks
# Prevents context loss at chunk boundaries
chunk_overlap_chars = 200

# fastembed model identifier
# Validated against the DB on every open — changing this requires deleting
# the database and re-indexing all documents
model_id = "nomic-embed-text-v1.5"

# MMR lambda: 0.0 = pure diversity, 1.0 = pure relevance
mmr_lambda = 0.7

# Fetch top_k × this many candidates before MMR reranking
mmr_candidate_multiplier = 3

# Max additive score boost when query terms appear in the heading context
# Set to 0.0 to disable heading boost
heading_boost = 0.05

# RRF k constant for Reciprocal Rank Fusion
# Higher values smooth the score distribution; 60 is the standard default
rrf_k = 60
```

## Load Priority

Config is resolved in this order (first match wins):

1. `--config <path>` CLI flag — explicit override
2. `./polaris.toml` — project-local config
3. `~/.config/polaris/polaris.toml` — user-global config
4. Built-in defaults (listed above)

## Defaults Reference

| Field | Default | Constraints | Notes |
|-------|---------|-------------|-------|
| `db_path` | `"polaris.db"` | — | Relative to CWD |
| `embedding_dim` | `512` | `[64, native_dim]` | Matryoshka truncation; upper bound depends on model |
| `max_chunk_tokens` | `450` | `> 0` | ≈ 1800 chars |
| `chunk_overlap_chars` | `200` | `< max_chunk_tokens * 4` | Chars of overlap |
| `model_id` | `"nomic-embed-text-v1.5"` | — | Validated on open; changing requires re-index |
| `mmr_lambda` | `0.7` | — | 0 = diversity, 1 = relevance |
| `mmr_candidate_multiplier` | `3` | — | Candidate pool = top_k × 3 |
| `heading_boost` | `0.05` | — | Additive; 0.0 disables it |
| `rrf_k` | `60` | — | RRF rank fusion constant |

## Config Validation

Config values are validated at startup (after loading the file and applying any CLI overrides). Invalid values produce a clear error and halt the process before any DB or model is opened:

```
Error: Config error: embedding_dim must be in [64, 768] for model 'nomic-embed-text-v1.5', got 32
Error: Config error: max_chunk_tokens must be greater than 0
Error: Config error: chunk_overlap_chars (2000) must be less than max_chunk_tokens * 4 (1800)
```

## CLI Overrides

Two config values can be overridden at runtime without editing the config file:

```bash
polaris --dim 384 index ./docs      # Override embedding_dim
polaris --db /tmp/test.db search "query"  # Override db_path
```

These flags are global (accepted before any subcommand).

## Database Constraints

Both `embedding_dim` and `model_id` are written to the database on first index and validated on every subsequent open.

### Dimension mismatch

```
Dimension mismatch: database has dim=256, config has dim=384
```

### Model mismatch

```
Model mismatch: database was indexed with model 'nomic-embed-text-v1.5',
config has 'bge-small-en' — delete the database and re-index to switch models
```

In both cases, resolution is the same: delete (or move) the existing database and re-index.

## Supported Models

| `model_id` | Native dim | Recommended `embedding_dim` | Download size |
|---|---|---|---|
| `nomic-embed-text-v1.5` (default) | 768 | 512 | ~137 MB |
| `mxbai-embed-large-v1` | 1024 | 1024 | ~670 MB |
| `all-minilm-l6-v2` | 384 | 384 | ~23 MB |

`embedding_dim` may be set to any value in `[64, native_dim]`. Matryoshka truncation is applied automatically — lower dimensions trade recall for speed and storage. For `mxbai-embed-large-v1` and `all-minilm-l6-v2`, which do not have Matryoshka training, truncation is still applied but quality may degrade more steeply with smaller dimensions.

Changing `model_id` requires deleting the database and re-indexing.

## Model Caching

The fastembed model is downloaded on first use to `~/.cache/huggingface/`. Subsequent runs reuse the cached ONNX files.

Download progress is shown in the terminal when the model is not yet cached.
