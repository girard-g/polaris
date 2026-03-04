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
- **Web UI (v3 enterprise)** — browser-based admin and user interface, served on the same HTTPS port as the MCP endpoint, bundled into the binary via `rust-embed`

## Non-Goals

- Multi-user or networked deployments in the default mode (an opt-in enterprise/team server mode with namespace isolation and mTLS is planned for v3 — see [multi-tenant.md](multi-tenant.md))
- Support for non-markdown formats (PDF, HTML, code files)
- Cloud or remote vector stores

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│  CLI (clap)         │  MCP Server (rmcp 0.16 / stdio)    │
│  index / search /   │  tools: search, index, status      │
│  serve / status /   │                                    │
│  watch              │                                    │
└─────────┬───────────┴──────────────┬─────────────────────┘
          │                          │
          ▼                          ▼
┌─────────────────────────────────────────┐
│  SearchEngine  ←→  Indexer              │
│      │                  │              │
│  EmbeddingEngine    EmbeddingEngine     │
│      │                  │              │
│  Database (db.rs)   Database (db.rs)    │
└──────────────────────────────────────── ┘
          │
          ▼
┌──────────────────────────────────┐
│  SQLite + sqlite-vec + FTS5      │
│  documents / chunks /            │
│  vec_chunks (KNN) /              │
│  chunks_fts (BM25)               │
└──────────────────────────────────┘
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

# Index a docs folder
polaris index ./docs

# Search
polaris search "how to configure the database"

# Start MCP server (stdio)
polaris serve
```

See [cli.md](cli.md) for full command reference.
