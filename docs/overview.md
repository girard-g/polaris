# Polaris — Overview

Polaris is a lightweight RAG (Retrieval-Augmented Generation) system built in Rust. It indexes project documentation as markdown files, embeds them using a local ONNX model, stores the vectors in SQLite, and exposes hybrid semantic + keyword search via both a CLI and an MCP (Model Context Protocol) server.

The primary use case is feeding coding agents (e.g. Claude Code) with relevant documentation context during a session, without sending entire doc trees in the context window.

## Goals

- **Single binary, zero runtime deps** — SQLite is bundled; the embedding model is downloaded once and cached locally
- **Incremental indexing** — SHA256 hashes detect changes; only modified files are re-embedded
- **File watching** — `polaris watch` auto re-indexes on change with 500 ms debounce
- **Heading-aware chunking** — Markdown structure is preserved in chunks for better retrieval
- **Hybrid search** — BM25 full-text search fused with vector KNN via RRF; MMR reranking for diversity
- **MCP native** — Runs as a stdio MCP server so any compatible agent can call `search`, `index`, and `status`
- **Setup orchestration** — `polaris setup` writes `.mcp.json`, updates `.gitignore`, and refreshes a marker-delimited Polaris block in `CLAUDE.md` / `AGENTS.md` / `GEMINI.md`
- **Savings telemetry** — `polaris savings` reports how many tokens Polaris saved versus grep+read, via an append-only `search_log` table

## Non-Goals

- Multi-user or networked deployments in the default mode (a multi-tenant server mode with namespace isolation and mTLS is a planned design — see [multi-tenant.md](multi-tenant.md))
- Support for non-markdown formats (PDF, HTML, code files)
- Cloud or remote vector stores

## High-Level Architecture

Two crates: `polaris-core` (library) holds the retrieval pipeline; `polaris-cli` (binary) hosts the CLI and the MCP server.

```
┌────────────────────────────────────────────────────────────────┐
│  CLI (clap)                            │  MCP Server (rmcp)    │
│  index · search · serve · status ·     │  tools: search,       │
│  watch · chunks · setup · savings      │         index, status │
└─────────┬──────────────────────────────┴──────────┬────────────┘
          │                                        │
          ▼                                        ▼
┌────────────────────────────────────────────────────────┐
│  Bank / BankSet (polaris-core)                         │
│      │                                                 │
│  ┌───┴───────────────┐    ┌──────────────────────────┐ │
│  │ SearchEngine ←→ Indexer │  EmbeddingEngine         │ │
│  └─────────┬───────────────┘  (Arc, fastembed inside) │ │
│            ▼                                          │ │
│        Database (db.rs)                               │ │
└────────────────────────────────────────────────────────┘
                              │
                              ▼
       ┌──────────────────────────────────────────┐
       │  SQLite + sqlite-vec + FTS5              │
       │  metadata · documents · chunks ·         │
       │  vec_chunks (KNN) · chunks_fts (BM25) ·  │
       │  search_log (savings telemetry)          │
       └──────────────────────────────────────────┘
```

## Tech Stack

| Layer | Crate | Version |
|-------|-------|---------|
| Embeddings | fastembed | 5 |
| Vector store | sqlite-vec | 0.1 |
| Full-text search | SQLite FTS5 | (bundled) |
| Database | rusqlite (bundled) | 0.32 |
| MCP server | rmcp | 0.16 |
| JSON schema | schemars | 1.x |
| Markdown parsing | pulldown-cmark | 0.12 |
| CLI | clap | 4 |
| Async runtime | tokio | 1 |
| Error types | thiserror | 2 |
| Progress UI | indicatif | 0.17 |
| File watching | notify-debouncer-mini | 0.4 |

## Quick Start

```bash
# Build
cargo build --release

# (Optional) wire Polaris into a project — writes .mcp.json, .gitignore, agent-instruction blocks
polaris setup

# Index a docs folder
polaris index ./docs

# Search
polaris search "how to configure the database"

# Watch and auto-reindex
polaris watch ./docs

# Start MCP server (stdio)
polaris serve

# Inspect token savings vs. grep+read
polaris savings
```

See [cli.md](cli.md) for full command reference.
