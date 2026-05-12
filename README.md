# Polaris

Polaris is the North Star — the fixed point sailors and navigators have used for centuries to orient themselves in the dark. This tool aims to do the same for coding agents: give them a reliable reference point in an unfamiliar codebase, so they always know where they are.

Lightweight RAG system for coding agents. Index your project docs, search them semantically, serve results over MCP — single binary, no runtime dependencies.

## Why Polaris — Token Savings vs. Grep + Read

For documentation questions, calling the Polaris MCP `search` tool is dramatically cheaper than the usual agent loop of grepping the docs tree and reading the matching files. Measured on a real query (*"how does Polaris embed documents?"*) against this repo's `docs/`:

| Approach | Tokens returned | vs. grep + read |
|---|---:|---:|
| `grep -rn "embed" docs/` + read `embedding.md` + read `architecture.md` | ~12,700 | 1× |
| MCP `search`, vague query, `top_k=5` | ~925 | **~14× cheaper** |
| MCP `search`, focused query (2-4 domain nouns), `top_k=2` | ~285 | **~45× cheaper** |

Estimates use ~4 chars/token. Accuracy actually improves with the focused query: the top-ranked chunk is the canonical *Embedding Pipeline* section instead of the generic overview.

Run `polaris savings` to see your own cumulative number once you've made some queries.

The MCP server's tool descriptions and server `instructions` brief calling agents on these patterns (use specific domain terms, start at `top_k=2`), so a well-behaved client picks them up automatically.

```bash
polaris index ./docs
polaris search "how does chunking work"
polaris watch ./docs        # auto re-index on file changes
polaris serve               # stdio MCP server for Claude Code
```

## How It Works

Polaris indexes markdown files as vector embeddings (via a local ONNX model) and full-text search entries (FTS5). At query time it runs both vector KNN and BM25, fuses the results with Reciprocal Rank Fusion, applies a heading boost, and reranks with MMR for diversity.

```
query → vector KNN + BM25 → RRF fusion → heading boost → MMR rerank → top-k results
```

## Library Use

Polaris's retrieval pipeline is also available as a Rust library (`polaris-core`):

```rust
use polaris_core::{Bank, BankConfig, IndexOpts, SearchOpts, SharedEmbedding};

let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 512)?;
let bank = Bank::open(BankConfig {
    repo_root: "./docs".into(),
    index_path: "./docs/.polaris/index.db".into(),
    embedding_dim: 512,
    model_id: "nomic-embed-text-v1.5".into(),
    ..Default::default()
}, embed)?;

bank.index_path(std::path::Path::new("./docs"), IndexOpts::default())?;
let results = bank.search("how does chunking work", SearchOpts { top_k: 5 })?;
```

For multi-bank searches with score fusion, use `BankSet`. For incremental
updates after a git pull, use `Bank::index_diff(&changed, &removed)`.

## Install

```bash
cargo build --release
# binary at: ./target/release/polaris
```

Already installed? Run `polaris update` to upgrade in place.

First search will download the embedding model (~137 MB) to a user-global cache shared across projects (default `~/.cache/polaris/models/` on Linux). See [Configuration → Model Caching](docs/configuration.md#model-caching) for the full resolution order and the `POLARIS_CACHE_DIR` override.

## Usage

### Setup

```bash
polaris setup            # configure current directory
polaris setup ./my-proj  # configure a specific directory
```

Creates `.mcp.json` (pointing at the running polaris binary) and adds polaris-related entries to `.gitignore` (`polaris.db`, `polaris.db-shm`, `polaris.db-wal`, `.fastembed_cache/`, `.mcp.json` itself). Idempotent: re-running is safe.

`polaris setup` also writes a marker-delimited Polaris MCP block into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root, steering compatible coding agents toward `polaris.search` for documentation queries. Existing user content in those files is preserved (the block is delimited by `<!-- polaris:begin --> … <!-- polaris:end -->` markers and only that range is rewritten on re-run). Pass `--no-agents` to skip these three files.

### Index

```bash
polaris index ./docs                # recursive, incremental
polaris index ./docs --force        # re-embed all files
polaris index ./docs --no-recursive # top-level only
```

Only `.md` files are indexed. Unchanged files (same SHA256) are skipped automatically.

### Watch

```bash
polaris watch ./docs                # watch and auto re-index on changes
polaris watch ./docs ./notes        # multiple paths
polaris watch ./docs --no-recursive # top-level only
```

Runs an initial index on start, then re-indexes affected paths automatically within ~500 ms of a file change. Ctrl+C to stop.

### Search

```bash
polaris search "authentication flow"
polaris search "database schema" -k 10
```

### Status

```bash
polaris status
```

```
Database   : polaris.db
Documents  : 13
Chunks     : 145
DB size    : 2.2 KB
Embed dim  : 512
Last index : 2026-02-26T14:41:28Z
```

### Savings

```bash
polaris savings              # cumulative summary
polaris savings --history    # per-query log (newest first)
polaris savings --output json
```

Reports the cumulative tokens you've saved by going through Polaris instead of `grep + read`. The baseline for each query is the total content of the unique files in the result set — i.e., what an agent without Polaris would have opened after grepping. Tokens are estimated at ~4 chars/token.

### MCP Server

```bash
polaris serve
```

Starts a stdio MCP server. Run `polaris setup` once per project to write `.mcp.json` automatically, or add it manually:

```json
{
  "mcpServers": {
    "polaris": {
      "command": "/path/to/polaris",
      "args": ["serve"]
    }
  }
}
```

Tools exposed: `search`, `index`, `status`.

## Configuration

Create `polaris.toml` in the working directory (all fields optional):

```toml
db_path = "polaris.db"
embedding_dim = 512          # 64–768, Matryoshka truncation
max_chunk_tokens = 450
chunk_overlap_chars = 200
model_id = "nomic-embed-text-v1.5"
mmr_lambda = 0.7             # 0 = diversity, 1 = relevance
mmr_candidate_multiplier = 3
heading_boost = 0.05
rrf_k = 60
```

Config is resolved in order: `--config <path>` → `./polaris.toml` → `~/.config/polaris/polaris.toml` → defaults.

CLI overrides: `polaris --dim 384 --db /tmp/test.db search "query"`

## Known Limitations

- **Markdown only** — only `.md` files are indexed; `.txt`, `.rst`, code files, and PDFs are ignored
- **Single-user** — all tool calls are serialized through a single DB mutex; not designed for concurrent MCP sessions
- **Approximate chunk offsets** — `start_byte`/`end_byte` metadata is approximate and not verified post-split

## Tech Stack

| | |
|-|-|
| Embeddings | fastembed 5 (nomic-embed-text-v1.5, ONNX, CPU) |
| Vector search | sqlite-vec 0.1 (cosine KNN) |
| Full-text search | SQLite FTS5 (BM25) |
| Database | rusqlite 0.32 (bundled SQLite) |
| MCP server | rmcp 0.16 (stdio transport) |

## Documentation

| | |
|-|-|
| [docs/overview.md](docs/overview.md) | Goals, architecture diagram, tech stack |
| [docs/search.md](docs/search.md) | Hybrid search pipeline, scoring, MMR |
| [docs/database.md](docs/database.md) | Schema, FTS5 sync, migrations |
| [docs/configuration.md](docs/configuration.md) | All config fields and defaults |
| [docs/cli.md](docs/cli.md) | Full CLI reference |
| [docs/mcp-server.md](docs/mcp-server.md) | MCP tools and integration |
| [docs/architecture.md](docs/architecture.md) | Module map and data flow |
| [docs/todos.md](docs/todos.md) | Known limitations and roadmap |
