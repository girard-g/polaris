# TODOs & Roadmap

## Known Limitations (v1)

These are accepted constraints for the initial release, documented for transparency.

### ~~Blocking async in MCP tool handlers~~ ✓ Done

All three tool handlers now offload blocking work via `tokio::task::spawn_blocking`.

### No concurrent DB access

The `Arc<Mutex<Database>>` design serializes all tool calls through a single mutex. This is fine for single-user local use, but would bottleneck under concurrent MCP sessions.

### No file watching

Polaris does not watch directories for changes. Users must manually call `polaris index` or trigger the `index` MCP tool to pick up new/modified files.

### No non-markdown formats

Only `.md` files are indexed. Plain `.txt`, `.rst`, code files, and PDFs are ignored.

### Chunk byte offsets are approximate

`start_byte` and `end_byte` in `ChunkRecord` track approximate positions in the original file. They are not verified to be accurate after heading extraction and paragraph splitting.

---

## v1 Improvements (all done ✓)

### ~~`spawn_blocking` for embedding and DB calls~~ ✓ Done

### ~~Config validation~~ ✓ Done

`PolarisConfig::validate()` is called after load + CLI overrides. Errors are descriptive and halt startup early.

### ~~Better error messages for missing DB~~ ✓ Done

`polaris search` now distinguishes: DB file missing → actionable hint (exit 1); DB empty → actionable hint; no match → "No results found."

---

## v2 Todo List

Features and fixes targeted for the next release, in rough priority order.

### Watch mode

```bash
polaris watch ./docs
```

Use `notify` crate to re-index files on change automatically. Addresses the "no file watching" known limitation.

### Configurable models

Honor the `model_id` config field at runtime. Currently `nomic-embed-text-v1.5` is always used regardless of config. Support at minimum one alternative (e.g. BGE-small, E5-small) to let users trade speed for quality.

### CLI `--output json`

Return search results as JSON for scripting:

```bash
polaris search "query" --output json | jq '.[0].content'
```

### Chunk viewer

```bash
polaris chunks docs/guide.md
```

Show how a specific file was chunked, with heading contexts and byte offsets. Useful for debugging retrieval quality.

### Multi-database support

Allow querying across multiple `.db` files without merging them. Useful for keeping project docs separate from library docs.

### Progress in MCP `index` tool

The `index` MCP tool currently returns a summary after completion. Real-time progress (via MCP progress notifications) would improve the UX for large indexing runs.

---

## Long-Term / Speculative

### Packaging

- `cargo install polaris` via crates.io
- Pre-built binaries via GitHub Releases
- Homebrew formula

### Cross-encoder reranking

After KNN + BM25 retrieval, re-score with a small cross-encoder model for better precision on ambiguous queries.

### Web UI

Simple local web interface for browsing indexed docs and testing search queries.

### Non-markdown formats

Extend indexing to `.txt`, `.rst`, and source code files. Requires format-specific chunking strategies.

### Concurrent DB access

Replace `Arc<Mutex<Database>>` with a connection pool (e.g. `r2d2` + `rusqlite`) to allow parallel read queries. Write operations (indexing) would still serialize.
