# Configuration

## Config File

Polaris reads a TOML config file. All fields are optional; unset fields use their defaults.

`polaris setup` writes a starter `polaris.toml` and gitignores it: it carries per-corpus tuning rather than anything a team shares. Every setting in it is commented out as a reference you can uncomment as needed, including a table of each model's default `search_min_similarity`.

Prefer leaving keys commented. An unset key keeps tracking its built-in default through upgrades, whereas one written out is pinned to whatever it said the day you wrote it. This matters most for `model_id`, `embedding_dim` and `search_min_similarity`: unset, the first index chooses the model from your docs and the other two follow it (see [Model Selection](#model-selection)).

A project-local `polaris.toml` replaces the global one at `~/.config/polaris/polaris.toml` rather than merging with it.

The example below shows every key with its default value; `model_id`, `embedding_dim` and `search_min_similarity` stay commented for the reason above. Uncomment a line only to override it.

```toml
# SQLite database file path (relative to CWD or absolute)
db_path = "polaris.db"

# Embedding vector dimension
# Must match the dimension already stored in the DB (checked on open)
# Valid range: 64 to the model's native dimension (see "Supported Models" below)
# Unset, it follows the index, then the model default (512 for nomic).
# embedding_dim = 512

# Maximum chunk size in approximate tokens (1 token ≈ 4 chars)
# Chunks that exceed this are split at paragraph/sentence/word boundaries
max_chunk_tokens = 450

# Overlap in characters between adjacent chunks
# Prevents context loss at chunk boundaries
chunk_overlap_chars = 200

# fastembed model identifier
# Validated against the DB on every open — changing this requires deleting
# the database and re-indexing all documents
# Unset, the first index that creates the database chooses it from your docs.
# model_id = "nomic-embed-text-v1.5"

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

# Cap on the `top_k` value accepted by search commands (prevents runaway queries)
max_top_k = 50

# Minimum query-to-chunk cosine similarity for the MCP search tool and the
# auto-search hook. Unset, it follows the index's model (see Search Threshold).
# search_min_similarity = 0.63

# Maximum file size (in bytes) the indexer will process; larger files are skipped
max_file_size = 10485760  # 10 MiB

# Additional read-only database paths for multi-DB search (BankSet)
# extra_db_paths = ["/path/to/other/polaris.db", "../shared/docs.db"]
extra_db_paths = []

# polaris eval settings
[eval]
# Corpus sentences sampled as queries per run
sample_size = 200
# Off-topic probes; empty uses the built-in set for the detected language
probes = []
```

## Load Priority

Config is resolved in this order (first match wins):

1. `--config <path>` CLI flag — explicit override
2. `./polaris.toml` — project-local config
3. `~/.config/polaris/polaris.toml` — user-global config
4. Built-in defaults (listed above)

## Model Selection

When `model_id` is not set, the run that creates the index chooses it. That run is `polaris setup` (initial index of `./docs`), `polaris index`, `polaris watch` or the MCP `index` tool, whichever comes first. It reads the Markdown files that run is about to index and measures how much of the prose is English:

- 90% or more English prose, or nothing to measure → `nomic-embed-text-v1.5`
- otherwise → `embeddinggemma-300m`

The choice is printed once, before any download:

```
◆  model: embeddinggemma-300m (38% English prose) — set model_id to override
```

The model and dimension are then recorded in the database. Every later command, including both Claude Code hooks, reads them from there and derives the search threshold from that model's default (see [Search Threshold](#search-threshold)), so nothing needs to be written to `polaris.toml`.

`polaris index --dry-run` against a project with no index yet still runs this choice — a dry run has no other way to show which model it would use — but nothing is recorded, since a dry run creates no database. The line is prefixed to say so:

```
◆  (dry run) model: embeddinggemma-300m (38% English prose) — set model_id to override — nothing recorded
```

How the share is measured:

- Only files with at least 100 words of prose count.
- Fenced code blocks are not prose.
- A file is English when enough of its words are common English function words (*the*, *and*, *of*, …).
- Each file weighs by its size.

No other language is identified; everything that is not English is treated the same way.

**The choice sees only the files of the run that creates the index.** Indexing `docs/en` first and adding `docs/fr` later keeps nomic. To choose again, delete the database and re-index, optionally with an explicit `model_id`.

Setting `model_id` anywhere disables the choice for every index that config applies to. That includes a project `polaris.toml`, `--model`, and the global `~/.config/polaris/polaris.toml`.

## Defaults Reference

| Field | Default | Constraints | Notes |
|-------|---------|-------------|-------|
| `db_path` | `"polaris.db"` | — | Relative to CWD |
| `embedding_dim` | model default: 512 nomic, 768 `embeddinggemma-300m`, native otherwise | `[64, native_dim]` | Unset: taken from the index, then the model |
| `max_chunk_tokens` | `450` | `> 0` | ≈ 1800 chars |
| `chunk_overlap_chars` | `200` | `< max_chunk_tokens * 4` | Chars of overlap |
| `model_id` | chosen from the corpus by the first index | — | See [Model Selection](#model-selection); changing requires re-index |
| `mmr_lambda` | `0.7` | — | 0 = diversity, 1 = relevance |
| `mmr_candidate_multiplier` | `3` | — | Candidate pool = top_k × 3 |
| `heading_boost` | `0.05` | — | Additive; 0.0 disables it |
| `rrf_k` | `60` | — | RRF rank fusion constant |
| `max_top_k` | `50` | — | Maximum `top_k` accepted by search commands |
| `search_min_similarity` | model default: 0.63 nomic, 0.42 `embeddinggemma-300m`, uncalibrated otherwise | `[0.0, 1.0]` | See [Search Threshold](#search-threshold) |
| `max_file_size` | `10485760` | `> 0` | 10 MiB; larger files are skipped during indexing |
| `extra_db_paths` | `[]` | — | Additional read-only DBs fused into search (multi-DB) |
| `eval.sample_size` | `200` | `> 0` | Sentences sampled per `polaris eval` run |
| `eval.probes` | `[]` | — | Overrides built-in probes; empty means auto by language |

## Search Threshold

`search_min_similarity` gates the MCP `search` tool and the auto-search hook. Unset, it follows the model of the index:

| Model | Default | Evidence |
|---|---|---|
| `nomic-embed-text-v1.5` | 0.63 | `polaris eval` midpoint on this repo; also the best threshold on its real-query gold set |
| `nomic-embed-text-v1.5-quantized` | 0.63 | `polaris eval` suggested 0.63–0.64 |
| `embeddinggemma-300m` | 0.42 | Midpoint of the best in-sample thresholds on real queries: 0.40 (this repo, English) and 0.44 (a French/English corpus). In-sample on both; the least certain value here |
| `mxbai-embed-large-v1`, `all-minilm-l6-v2` | uncalibrated | Never measured |

What an uncalibrated model does:

- The MCP `search` tool returns results unfiltered, followed by a note suggesting `polaris eval`.
- The auto-search hook never injects.
- `polaris status` and `polaris eval` show `uncalibrated for <model>`.

Setting `search_min_similarity` enables normal gating for any model.

A pinned value that is 0.10 or more away from the index model's default is flagged by `polaris status`, `polaris eval`, `polaris index` and `polaris setup` (never by the hooks):

```
⚠  search_min_similarity = 0.63 is pinned in polaris.toml; embeddinggemma-300m defaults to 0.42
```

Older versions of `polaris setup` wrote `search_min_similarity = 0.63` into every `polaris.toml`. On an `embeddinggemma-300m` index, delete that line to use the model's default.

`polaris eval` suggests a value for your corpus. Treat it as a hint, not a setting: on `embeddinggemma-300m` its suggestion undershot the best real-query threshold by about 0.12 (0.32 vs 0.44 on a French/English corpus, 0.28 vs 0.40 on this repo).

## Config Validation

Config values are validated at startup (after loading the file and applying any CLI overrides). Invalid values produce a clear error and halt the process before any DB or model is opened:

```
Error: Config error: embedding_dim must be in [64, 768] for model 'nomic-embed-text-v1.5', got 32
Error: Config error: max_chunk_tokens must be greater than 0
Error: Config error: chunk_overlap_chars (2000) must be less than max_chunk_tokens * 4 (1800)
```

## CLI Overrides

Three config values can be overridden at runtime without editing the config file:

```bash
polaris --dim 384 index ./docs              # Override embedding_dim
polaris --db /tmp/test.db search "query"    # Override db_path
polaris --model mxbai-embed-large-v1 index ./docs  # Override model_id
```

These flags are global (accepted before any subcommand). `model_id` and `embedding_dim` are still validated against any existing database on open — switching values typically requires deleting the database and re-indexing.

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

## Upgrading from 2.3

| Your setup | After upgrading |
|---|---|
| An index, and a `polaris.toml` with `search_min_similarity = 0.63` (written by older `polaris setup`) | Unchanged: model and dimension come from the index, the threshold stays 0.63 |
| An index, no `polaris.toml` | Unchanged: model and dimension come from the index, the threshold is that model's default (0.63 for nomic) |
| Database deleted, then re-indexed on mostly non-English docs | `embeddinggemma-300m` is chosen; an old pinned `search_min_similarity = 0.63` now prints a warning — delete the line to use 0.42 |
| `model_id` set and different from the index | `Model mismatch` error, as before |
| `model_id` set in `~/.config/polaris/polaris.toml` | Explicit everywhere: no project chooses its model |
| An older Polaris binary opening an `embeddinggemma-300m` index | `Unknown model 'embeddinggemma-300m'` — upgrade that binary |
| `extra_db_paths` pointing at an index built with another model | Error, now naming that database |
| `polaris serve` in a project with an index | Unchanged: the model loads at startup |
| `polaris serve` before any index exists | No empty `polaris.db` appears any more; the model loads on the first `index` call |

A config file that pins `embedding_dim` now wins over `--model`: an explicit
dimension is no longer clamped to the model's native size, it is validated
against it. `embedding_dim = 768` in `polaris.toml` plus `polaris --model
all-minilm-l6-v2 index ./docs` now fails with `embedding_dim must be in [64,
384] for model 'all-minilm-l6-v2', got 768` instead of silently clamping to
384 — delete or lower the pinned `embedding_dim` to switch models with `--model`.

If a crash interrupts index creation, the database file can exist with no
stored model; the MCP tools report `No index yet` for it and never pick a
model for it, so delete the file and re-index.

## Supported Models

| `model_id` | Native dim | Default `embedding_dim` | Default `search_min_similarity` | Download size |
|---|---|---|---|---|
| `nomic-embed-text-v1.5` | 768 | 512 | 0.63 | ~522 MB |
| `nomic-embed-text-v1.5-quantized` | 768 | 512 | 0.63 | ~131 MB |
| `embeddinggemma-300m` | 768 | 768 | 0.42 | ~1.2 GB |
| `mxbai-embed-large-v1` | 1024 | 1024 | uncalibrated | ~670 MB |
| `all-minilm-l6-v2` | 384 | 384 | uncalibrated | ~23 MB |

`embedding_dim` may be set to any value in `[64, native_dim]`. Matryoshka truncation is applied automatically — lower dimensions trade recall for speed and storage. The quantized nomic variant trades a little recall for ~2× faster indexing and search start-up — see [Embedding → Quantized nomic](embedding.md#quantized-nomic). `embeddinggemma-300m` is Matryoshka-trained like nomic. For `mxbai-embed-large-v1` and `all-minilm-l6-v2`, which do not have Matryoshka training, truncation is still applied but quality may degrade more steeply with smaller dimensions.

Changing `model_id` requires deleting the database and re-indexing.

## Model Caching

The fastembed model is downloaded on first use to a user-global cache shared across all projects, so multiple Polaris installs do not each redownload the same ONNX files.

Resolution order:

1. `POLARIS_CACHE_DIR` environment variable (if set and non-empty) → `$POLARIS_CACHE_DIR/models/`.
2. Otherwise the platform user cache directory + `polaris/models/`:
   - Linux: `~/.cache/polaris/models/` (honours `$XDG_CACHE_HOME`).
   - macOS: `~/Library/Caches/polaris/models/`.
   - Windows: `%LOCALAPPDATA%\polaris\models\`.

Download progress is shown in the terminal when the model is not yet cached.

If you have a leftover `.fastembed_cache/` directory in a project from an older Polaris version, you can safely remove it: `rm -rf .fastembed_cache/`.
