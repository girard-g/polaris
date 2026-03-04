# TODOs & Roadmap

## Known Limitations (v1)

These are accepted constraints for the initial release, documented for transparency.

### ~~Blocking async in MCP tool handlers~~ ✓ Done

All three tool handlers now offload blocking work via `tokio::task::spawn_blocking`.

### No concurrent DB access

The `Arc<Mutex<Database>>` design serializes all tool calls through a single mutex. This is fine for single-user local use, but would bottleneck under concurrent MCP sessions.

### ~~No file watching~~ ✓ Done

`polaris watch` monitors paths with a 500 ms debounce and re-indexes automatically on change. Paths are canonicalized to absolute form at startup so that inotify event paths (always absolute on Linux) match correctly — relative paths like `./docs` work as expected.

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

## v2 Improvements (done ✓)

### ~~Large corpus indexing optimization~~ ✓ Done

Restructured `index_path` into a three-phase pipeline:
- **Phase A:** `rayon::par_iter()` reads each file once (hash + chunk in parallel) — eliminates double reads and parallelises I/O + CPU.
- **Phase B:** All chunks across all pending files are flattened and embedded in batches of 32, keeping ONNX batches full.
- **Phase C:** A single `BEGIN`/`COMMIT` covers the entire run, replacing per-file transactions.

Result: for a 5k-doc corpus (~50k chunks), this eliminates 5k redundant file reads, raises batch utilisation from ~30% to ~97%, and cuts 5k transaction round-trips to 1.

---

## v2 Todo List

Features and fixes targeted for the next release, in rough priority order.

### ~~Path normalisation (`./` stripping)~~ ✓ Done

`normalise_path()` now strips a leading `./` in addition to converting backslashes.
This means `docs/file.md` and `./docs/file.md` map to the same DB key, so
`polaris index docs` and `polaris index ./docs` (and `polaris watch`) no longer
re-index unchanged files after the first run.

**Side effect:** existing databases whose paths were stored with a `./` prefix will see a
one-time full re-index on the first run after the update; subsequent runs skip unchanged
files correctly.

### ~~Watch mode~~ ✓ Done

`polaris watch ./docs` — uses `notify-debouncer-mini` (500 ms debounce) to re-index on file changes. Supports multiple paths and `--no-recursive`. Initial index runs on start.

### ~~Configurable models~~ ✓ Done

`model_id` is now wired through to fastembed model selection. Supported: `nomic-embed-text-v1.5` (768-dim), `mxbai-embed-large-v1` (1024-dim), `all-minilm-l6-v2` (384-dim). Config validation enforces the correct `embedding_dim` range per model.

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

## v3 Todo List

Features planned for v3, in rough priority order.

### Multi-tenant deployment

Optional company/team server mode with document segregation by user and group. Full design in [multi-tenant.md](multi-tenant.md).

Summary of work items:
- `[multi_tenant]` and `[tls]` config sections
- Per-namespace SQLite files under a configurable `data_dir`
- mTLS server (`polaris serve-https`) with client-cert authentication
- CN/SAN extraction to derive username and group membership
- `namespaces.toml` permission config with hot-reload
- `polaris namespace create/list/delete` subcommands
- `index` MCP tool gains optional `namespace` parameter
- `search` fans out to all accessible namespaces, merges via RRF, adds `provenance` field
- `status` reports per-namespace counts
- Path traversal protection on namespace names
- Audit logging for authenticated requests

### Automated user enrollment

Two-command flow so users never touch openssl after the initial admin bootstrap:

- Pending enrollment store (in-memory or SQLite table) for one-time tokens
- `/enroll/<token>` HTTP endpoint (unauthenticated, token-gated)
- `polaris user invite <cn> --groups <g1,g2> [--expires 24h]` — creates token, prints invite command
- `polaris connect --setup <url> --token <token>` — generates key locally, posts CSR, saves cert files, prints Claude Code snippet; OS-aware config dir
- `polaris connect <url>` — subsequent MCP-over-HTTPS client mode (reads certs from config dir)
- `polaris cert export --format p12 --out <file>` — PKCS#12 wrapper for browser import; no openssl required
- Cross-platform config dir resolution: Linux `~/.config/polaris/`, macOS `~/Library/Application Support/polaris/`, Windows `%APPDATA%\polaris\`

### Web UI

Browser-based admin and user interface, served on the same HTTPS port, bundled into the binary:

- `rust-embed` static asset bundling (compile-time embedding of HTML/CSS/JS)
- Route handler for `/ui/`, `/ui/admin/`, `/enroll/<token>`, `/health` with SAN-based access control
- Admin UI: namespaces page (list, create, delete, namespaces.toml editor); users/certs page (list by CN+groups, invite button, revoke); monitoring page (per-namespace counts, embedding model info, recent request log)
- User UI: search tester page (query box → results with scores, provenance, source file); namespace browser page (accessible namespaces, file count, expandable file list)
- `[web_ui]` config section (`enabled`, `path_prefix` for reverse-proxy deployments)

### Service / deployment

- Linux: systemd unit (already in docs)
- macOS: launchd plist example
- Windows: PowerShell `New-Service` example

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
