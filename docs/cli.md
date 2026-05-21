# CLI Reference

## Global Flags

These flags are accepted before any subcommand:

| Flag | Type | Description |
|------|------|-------------|
| `--config <PATH>` | PathBuf | Explicit config file path |
| `--dim <N>` | usize | Override `embedding_dim` from config |
| `--db <PATH>` | PathBuf | Override `db_path` from config. Repeat to search multiple databases: `--db primary.db --db extra.db` |
| `--model <ID>` | String | Override embedding model (e.g. `mxbai-embed-large-v1`) |

## Commands

### `polaris setup [path]`

Configure a project to use Polaris as an MCP server. Writes `.mcp.json` (pointing at the running polaris binary) and ensures the right entries exist in `.gitignore`.

```bash
polaris setup            # configure current directory
polaris setup ./my-proj  # configure a specific directory
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--no-agents` | false | Skip writing `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` |
| `--no-hooks` | false | Skip writing `.claude/settings.json` and the initial index pass. Re-run on a previously-set-up project to remove the polaris hook entries (the file itself stays). |

**Behaviour:**

1. Validates the target path exists and is a directory
2. Resolves the running binary via `std::env::current_exe()` and writes its absolute path to `.mcp.json` under `mcpServers.polaris`
3. If `.mcp.json` already exists, parses it and upserts the polaris entry, preserving any other servers
4. Appends missing entries to `.gitignore` under a `# polaris` comment header (`polaris.db`, `polaris.db-shm`, `polaris.db-wal`, `.fastembed_cache/`, `.mcp.json`)
5. Writes a Polaris MCP instruction block into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root, marker-delimited (`<!-- polaris:begin --> … <!-- polaris:end -->`). Preserves existing user content; refreshes only the block on re-run. Skipped if `--no-agents` is passed.
6. Idempotent: re-running prints "already configured" / "already up to date" without rewriting files

**Output (first run):**

```
polaris  · setup

  ✓  Created .mcp.json (polaris → /usr/local/bin/polaris)
  ✓  Created .gitignore (5 entries)
  ✓  Created CLAUDE.md (polaris block)
  ✓  Created AGENTS.md (polaris block)
  ✓  Created GEMINI.md (polaris block)
```

**Output (rerun):**

```
polaris  · setup

  ✓  .mcp.json already configured
  ✓  .gitignore already up to date
  ✓  CLAUDE.md already configured
  ✓  AGENTS.md already configured
  ✓  GEMINI.md already configured
```

---

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
| `--dry-run` | false | Preview changes without writing to the database. Exits 1 if changes are pending. |

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
| `--output <FORMAT>` | plain | Output format: `plain` or `json` |

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

Score is the final hybrid-search score (RRF + heading boost, after MMR rerank) normalised to `[0, 1]` per result set, so the top result is `1.000` and others are fractions of it. See [Search → Score interpretation](search.md).

**Error cases (stderr, exit 1):**

| Situation | Message |
|-----------|---------|
| DB file doesn't exist | `no index at <path>  —  run \`polaris index <path>\` first` |
| DB exists but empty | `index is empty  —  run polaris index <path> to add documents` (stdout, exit 0) |
| DB has docs, no match | `no matches found` plus a `tip:` line (stdout, exit 0) |

---

### `polaris serve`

Start an MCP server over stdio. Used by Claude Code (and other MCP clients) to call Polaris tools.

```bash
polaris serve
```

No additional flags. Reads config from the standard locations.

Logging is written to stderr so stdout remains clean for MCP protocol messages.

---

### `polaris watch <paths...>`

Watch one or more paths and automatically re-index on file changes.

```bash
polaris watch ./docs
polaris watch ./docs ./notes          # multiple paths, each watched independently
polaris watch ./docs --no-recursive   # top-level only
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--no-recursive` | false | Disable recursive directory traversal |

**Behaviour:**

1. Validates all paths exist (exits with code 1 if any are missing)
2. Canonicalizes each path to its absolute form (so relative paths like `./docs` work correctly with inotify, which always emits absolute paths)
3. Loads the embedding model once
4. Runs an initial `index` pass on each path (same as `polaris index`)
5. Registers a debounced file watcher (500 ms debounce) for each path
6. On file change, re-indexes only the affected root path and prints a report
7. Ctrl+C stops the watcher cleanly

**Output:**

```
 polaris  ·  watch  ./docs, ./notes

✓  model ready  nomic-embed-text-v1.5

◆  initial index  ./docs
✓  indexed in 1.2s  ·  47 chunks  38.4 KB
   +  12 added   3 unchanged

◆  initial index  ./notes
✓  no changes  8 unchanged

◆  watching  2 paths  · Ctrl+C to stop

◆  re-indexing  ./docs
✓  indexed in 0.3s  ·  4 chunks  3.1 KB
   ~  1 modified   11 unchanged

^C
◆  stopped
```

---

### `polaris status`

Print statistics about the current index.

```bash
polaris status
polaris status --output json
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--output <FORMAT>` | plain | Output format: `plain` or `json` |

**Output:**

```
Documents: 24
Chunks:    312
Database size: 1.4 MB
Embedding dim: 512
Last indexed: 2025-02-26T14:23:45Z
```

### `polaris savings`

Show cumulative tokens saved by going through Polaris instead of `grep + read`. Reads from the `search_log` table inside `polaris.db`.

```bash
polaris savings              # summary
polaris savings --history    # per-query history (newest first)
polaris savings --history --limit 50
polaris savings --output json
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--history` | false | Print the per-query log instead of the summary |
| `--limit <N>` | 20 | Maximum rows for `--history` |
| `--output <FORMAT>` | plain | Output format: `plain` or `json` |

**Behaviour:**

1. Reads the `search_log` table from `polaris.db`.
2. Aggregates rows into total searches, total result/baseline bytes, and a per-source (`cli` / `mcp`) breakdown.
3. Renders either the summary block or the per-query history table.
4. Tokens are estimated as `bytes / 4`.
5. Empty log: prints `No searches recorded yet. Run a search to start tracking.` and exits 0.

**Multi-DB note.** When `--db` is passed multiple times, only the *primary* (first) DB receives `search_log` rows, and baseline bytes are computed against that DB's `repo_root`. Result chunks that originate from secondary DBs whose files don't exist under the primary repo will contribute 0 bytes to the baseline, so multi-DB savings can under-report.

**Output (summary):**

```
  polaris  ·  savings

  Total searches      127  (mcp 98 / cli 29)
  Tokens delivered   31.2K
  Baseline           412K
  Tokens saved       381K  ~13× cheaper
  Tracking since     2026-04-09

  Tokens estimated at ~4 chars/token.
```

**Error cases (stderr, exit 1):**

| Situation | Message |
|-----------|---------|
| `polaris.db` doesn't exist | `no index at <path>  —  run \`polaris index <path>\` first` |

---

### `polaris chunks <path>`

Show how a file was chunked — heading contexts, byte offsets, and content previews.

```bash
polaris chunks docs/guide.md
```

Useful for diagnosing retrieval quality. Paths are normalised before lookup, so `./docs/guide.md` and `docs/guide.md` both work.

---

### `polaris update`

Self-upgrade the polaris binary from the latest GitHub release.

```
polaris update                       # check, prompt y/N, install
polaris update --check               # read-only: print latest vs current
polaris update --yes                 # skip prompt
polaris update -y                    # short form of --yes
polaris update --version 2.0.10      # install a specific version
polaris update --force               # re-install even when already on latest
```

Flags:

| Flag | Description |
|------|-------------|
| `--check` | Read-only. Prints `polaris is up to date` or `update available: vX.Y.Z → vA.B.C` and exits. |
| `--yes` / `-y` | Skip the confirmation prompt. |
| `--version <X.Y.Z>` | Install a specific version (pin or downgrade) instead of the latest tag. |
| `--force` | Re-install even when already on the target version (recovery from a corrupted binary). |

Behaviour:

- Fetches the latest release (or `--version` when provided) from `github.com/girard-g/polaris`.
- Compares to `CARGO_PKG_VERSION` using semver.
- Downloads the asset matching the running platform (`polaris-linux-x86_64`, `polaris-macos-aarch64`, or `polaris-windows-x86_64.exe`) to a temp file alongside the running binary, then atomically renames it over the current executable.
- On Windows the running binary is first renamed to `polaris.old` so the rename succeeds while the process is live.
- Refuses with exit code 2 on platforms that have no release asset.

Exit codes:

| Code | Meaning |
|------|---------|
| 0 | Update succeeded, already up to date, or `--check` ran cleanly |
| 1 | User declined the prompt, or a network / API / I/O error occurred |
| 2 | Running platform has no release asset |

---

## Environment Variables

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | Log level filter. Examples: `debug`, `polaris=trace`, `warn` |
| `RUST_LOG_STYLE` | Log colour. `never` disables ANSI colours |
| `POLARIS_CACHE_DIR` | Override the user-global model cache root. Models are stored under `$POLARIS_CACHE_DIR/models/`. See [Configuration → Model Caching](configuration.md#model-caching). |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic error (config, IO, embedding, DB, dimension mismatch, user-declined prompt) |
| 2 | `update` only — running platform has no release asset |

## Internal commands

These commands are invoked by Claude Code hooks and are not intended for direct use.

### `polaris hook index`

Reads a `PostToolUse` hook payload (JSON) on stdin and re-indexes the touched file if it passes the markdown extension and indexed-root gates. Always exits 0 — failures go to stderr only.
