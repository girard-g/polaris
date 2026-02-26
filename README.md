# Polaris

Polaris is the North Star — the fixed point sailors and navigators have used for centuries to orient themselves in the dark. This tool aims to do the same for coding agents: give them a reliable reference point in an unfamiliar codebase, so they always know where they are.

Lightweight RAG system for coding agents. Index your project docs, search them semantically, serve results over MCP — single binary, no runtime dependencies.

```bash
polaris index ./docs
polaris search "how does chunking work"
polaris serve   # stdio MCP server for Claude Code
```

## How It Works

Polaris indexes markdown files as vector embeddings (via a local ONNX model) and full-text search entries (FTS5). At query time it runs both vector KNN and BM25, fuses the results with Reciprocal Rank Fusion, applies a heading boost, and reranks with MMR for diversity.

```
query → vector KNN + BM25 → RRF fusion → heading boost → MMR rerank → top-k results
```

## Install

```bash
cargo build --release
# binary at: ./target/release/polaris
```

First search will download the embedding model (~137 MB) to `~/.cache/huggingface/`.

## Usage

### Index

```bash
polaris index ./docs                # recursive, incremental
polaris index ./docs --force        # re-embed all files
polaris index ./docs --no-recursive # top-level only
```

Only `.md` files are indexed. Unchanged files (same SHA256) are skipped automatically.

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

### MCP Server

```bash
polaris serve
```

Starts a stdio MCP server. Add to `.mcp.json`:

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
- **Manual re-indexing** — no file watching; run `polaris index` again to pick up changes
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
