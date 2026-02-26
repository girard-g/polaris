# CLI Reference

## Global Flags

These flags are accepted before any subcommand:

| Flag | Type | Description |
|------|------|-------------|
| `--config <PATH>` | PathBuf | Explicit config file path |
| `--dim <N>` | usize | Override `embedding_dim` from config |
| `--db <PATH>` | PathBuf | Override `db_path` from config |

## Commands

### `polaris index <path>`

Index markdown files from a path into the database.

```bash
polaris index ./docs
polaris index ./docs --recursive    # default: recursive
polaris index ./docs --no-recursive # top-level only
polaris index ./docs --force        # re-embed even unchanged files
polaris index README.md             # single file
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--no-recursive` | false | Disable recursive directory traversal |
| `--force` | false | Re-index all files even if hash is unchanged |

**Behaviour:**

1. Discovers all `.md` files under the given path
2. Compares SHA256 hashes against the database
3. Removes records for files that no longer exist
4. Skips files with unchanged hashes (unless `--force`)
5. Chunks, embeds, and stores new/modified files
6. Wraps each file in a transaction (atomic per document)

**Output:**

```
Found 12 markdown file(s)
[████████████████████] 12/12 | setup.md · 8/8 chunks [embedding…]

Added:    3 files (128 chunks)
Modified: 1 file  (32 chunks)
Removed:  0 files
Unchanged: 8 files
Total: 4.7 MB, 2.3 s
```

---

### `polaris search <query>`

Embed a query and return the most semantically similar chunks.

```bash
polaris search "authentication flow"
polaris search "how to configure timeout" --top-k 10
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `-k, --top-k <N>` | 5 | Number of results to return |

**Output:**

```markdown
### Result 1 — score: 0.892
**Section:** Guide > Authentication
**File:** `docs/guide.md`

To configure authentication, set the `AUTH_TOKEN` environment variable...

---
### Result 2 — score: 0.761
...
```

Score is `1.0 - cosine_distance`. Higher is more similar.

**Error cases (stderr, exit 1):**

| Situation | Message |
|-----------|---------|
| DB file doesn't exist | `No index found at 'polaris.db'. Run 'polaris index <path>' first.` |
| DB exists but empty | `Index is empty. Run 'polaris index <path>' to add documents.` |
| DB has docs, no match | `No results found.` (stdout, exit 0) |

---

### `polaris serve`

Start an MCP server over stdio. Used by Claude Code (and other MCP clients) to call Polaris tools.

```bash
polaris serve
```

No additional flags. Reads config from the standard locations.

Logging is written to stderr so stdout remains clean for MCP protocol messages.

---

### `polaris status`

Print statistics about the current index.

```bash
polaris status
```

**Output:**

```
Documents: 24
Chunks:    312
Database size: 1.4 MB
Embedding dim: 512
Last indexed: 2025-02-26T14:23:45Z
```

## Environment Variables

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | Log level filter. Examples: `debug`, `polaris=trace`, `warn` |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Any error (config, IO, embedding, DB, dimension mismatch) |
