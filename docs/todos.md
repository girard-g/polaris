# TODOs & Roadmap

## Known Limitations


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

## Near-Term Improvements


### ~~`spawn_blocking` for embedding and DB calls~~ ✓ Done

### ~~Config validation~~ ✓ Done

`PolarisConfig::validate()` is called after load + CLI overrides. Errors are descriptive and halt startup early.

### Better error messages for missing DB

When `polaris search` is run before any `polaris index`, the DB is empty. The error or output should be a clear hint rather than "No results found."

---

## Medium-Term Ideas

### Watch mode

```bash
polaris watch ./docs
```

Use `notify` crate to re-index files on change automatically.

### Multi-database support

Allow querying across multiple `.db` files without merging them. Useful for keeping project docs separate from library docs.

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

### Configurable models

Support additional fastembed models (e.g. BGE, E5) via the `model_id` config field. Currently only `nomic-embed-text-v1.5` is used regardless of config.

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
