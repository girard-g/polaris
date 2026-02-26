# Configuration

## Config File

Polaris reads a TOML config file. All fields are optional; unset fields use their defaults.

```toml
# SQLite database file path (relative to CWD or absolute)
db_path = "polaris.db"

# Embedding vector dimension
# Must match the dimension already stored in the DB (checked on open)
# Nomic v1.5 supports Matryoshka truncation: valid range is 64–768
embedding_dim = 512

# Maximum chunk size in approximate tokens (1 token ≈ 4 chars)
# Chunks that exceed this are split at paragraph/sentence/word boundaries
max_chunk_tokens = 450

# Overlap in characters between adjacent chunks
# Prevents context loss at chunk boundaries
chunk_overlap_chars = 200

# fastembed model identifier — changing this requires re-indexing all documents
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

| Field | Default | Notes |
|-------|---------|-------|
| `db_path` | `"polaris.db"` | Relative to CWD |
| `embedding_dim` | `512` | Matryoshka truncation from 768 |
| `max_chunk_tokens` | `450` | ≈ 1800 chars |
| `chunk_overlap_chars` | `200` | Chars of overlap |
| `model_id` | `"nomic-embed-text-v1.5"` | Downloaded once, cached |
| `mmr_lambda` | `0.7` | 0 = diversity, 1 = relevance |
| `mmr_candidate_multiplier` | `3` | Candidate pool = top_k × 3 |
| `heading_boost` | `0.05` | Additive; 0.0 disables it |
| `rrf_k` | `60` | RRF rank fusion constant |

## CLI Overrides

Two config values can be overridden at runtime without editing the config file:

```bash
polaris --dim 384 index ./docs      # Override embedding_dim
polaris --db /tmp/test.db search "query"  # Override db_path
```

These flags are global (accepted before any subcommand).

## Dimension Constraints

`embedding_dim` is validated against the database on every open. If the value in the DB's metadata table does not match the configured value, Polaris exits with a `DimensionMismatch` error:

```
Dimension mismatch: database has dim=256, config has dim=384
```

To switch dimensions you must delete (or move) the existing database and re-index.

## Model Caching

The fastembed model is downloaded on first use to `~/.cache/huggingface/`. Subsequent runs reuse the cached ONNX files. Download size is approximately 137 MB for `nomic-embed-text-v1.5`.

Download progress is shown in the terminal when the model is not yet cached.
