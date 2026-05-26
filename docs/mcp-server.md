# MCP Server

Polaris exposes three tools over the MCP protocol. It communicates via stdio (JSON-RPC 2.0), making it compatible with Claude Code and any other MCP-capable client.

## Starting the Server

```bash
polaris serve
```

## MCP Configuration (`.mcp.json`)

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

The binary path should point to the built `polaris` binary. The project ships with `.mcp.json` pointing to the `target/release/polaris` build.

## Tools

### `search`

Search indexed documentation using semantic similarity.

**Parameters:**

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language search query |
| `top_k` | integer | no | 5 | Number of results to return (clamped to `max_top_k`, default 50) |

**Returns:** Markdown-formatted string with scored results.

**Example response:**

```markdown
### Result 1 — score: 0.892
**Section:** Guide > Authentication
**File:** `docs/guide.md`

To configure authentication, set the AUTH_TOKEN environment...

---
### Result 2 — score: 0.761
...
```

---

### `index`

Index markdown files from a given path. Supports incremental updates.

**Parameters:**

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `path` | string | yes | — | Path to file or directory |
| `recursive` | boolean | no | true | Recurse into subdirectories |
| `force` | boolean | no | false | Re-index even unchanged files |

**Returns:** Plain text summary of the indexing run (added, modified, removed, errors).

**Example response:**

```
Added: 3 files (128 chunks)
Modified: 1 file (32 chunks)
Removed: 0 files
Unchanged: 8 files
```

If the path does not exist, returns: `Error: path not found: <path>`

---

### `status`

Return statistics about the current index.

**Parameters:** None.

**Returns:** Plain text status report.

**Example response:**

```
Documents: 24
Chunks: 312
Database size: 1437696 bytes
Model: nomic-embed-text-v1.5
Embedding dim: 512
Last indexed: 2025-02-26T14:23:45Z
```

The `index` tool also emits progress notifications when the caller passes a `progressToken` in the request `_meta` field.

## Protocol Details

- **Transport:** stdio (JSON-RPC over stdin/stdout)
- **Framework:** rmcp 0.16
- **Server name:** `polaris`
- **Version:** from `CARGO_PKG_VERSION`
- **Server instructions** (sent in `initialize` response):
  > Polaris is a semantic search MCP for project documentation. Prefer `search` over grep/read for documentation questions — it returns ranked, section-aware chunks and is typically 10-40× cheaper in tokens than grepping the docs and reading files. Query with specific domain terms; start with top_k=2 and raise only if recall is poor. Use `index` to add files, `status` to check index health.

## Shared State

All three tools share a single `PolarisState`:

```rust
PolarisState {
    config: Arc<PolarisConfig>,
    bank:   polaris_core::Bank,  // Arc<BankInner> with Mutex<Database> inside
}
```

`Bank` is cheaply cloneable (`Arc<BankInner>` internally) and serialises concurrent access through its own `Mutex<Database>`. MCP tool calls are typically serial, so this single-connection model is acceptable. The underlying SQLite connection runs in WAL mode.

Each tool clones the `config` / `bank` handle and offloads all blocking work to `tokio::task::spawn_blocking`. The DB mutex is acquired inside the blocking closure — never across an `.await` point.

## Error Handling in Tools

Tool errors are returned as formatted error strings rather than MCP error objects. This keeps the integration simple and ensures Claude always receives human-readable feedback.

```
Error: path not found: /nonexistent
Error: <embedding error message>
```

## Implementation Notes

- `polaris-cli/src/mcp/server.rs` does **not** import `polaris_core::Result` — it conflicts with rmcp macro-generated code that expects `ErrorData`. Use `std::result::Result` or `PolarisError` directly.
- The `#[tool_router]` attribute on the `impl PolarisServer` block generates the routing table.
- `#[tool_handler(router = self.tool_router)]` wires it into `ServerHandler`.
- `Database` (`rusqlite::Connection`) is `!Send`. It is accessed through `Bank`, which holds an `Arc<BankInner>` containing a `Mutex<Database>` — the cheap `Bank` handle is cloned on the async side and the lock is acquired inside the `spawn_blocking` closure where `Send` is not required.

## Hook integration (Claude Code)

When you run `polaris setup` in a project, polaris writes two hooks into `.claude/settings.json`:

1. **Auto-index** (`PostToolUse`): fires after Claude Code's `Write`, `Edit`, or `MultiEdit` tools complete. Re-runs `polaris index` for the touched file if (a) the path ends in `.md` and (b) the file lives under a directory the index already covers.

2. **Auto-search** (`UserPromptSubmit`): fires on every user message. Searches the indexed documentation and injects the top result as context before Claude responds. Two gates prevent pollution: prompts shorter than 5 words or longer than 150 words are skipped (confirmations and error pastes make poor queries), and results below the raw relevance threshold are silently dropped.

Both hooks are non-fatal: they log to stderr and always exit 0, so a transient hiccup never surfaces as a warning banner or interrupts your session.

To opt out, run `polaris setup --no-hooks`. To remove the hooks from a project that already has them installed, re-run `polaris setup --no-hooks` — it strips all polaris entries from `.claude/settings.json` while leaving any other hooks intact.

The hooks are a Claude Code feature; Codex, Cursor, and Gemini CLI users keep using the MCP `search` tool and can run `polaris watch` if they want background auto-indexing.

### Known limitations

- **Windows shell quoting (unverified).** `polaris setup` writes the hook command using POSIX single-quoting (`shell-words` crate) so binary paths containing spaces survive Bash parsing on macOS/Linux. Claude Code on Windows likely invokes hooks via `cmd.exe` or PowerShell, which do not honor POSIX single quotes — so a Windows install under a path with spaces (e.g. `C:\Program Files\polaris\polaris.exe`) may not execute the hook correctly. Workaround: install the polaris binary to a path without spaces (e.g. `C:\polaris\polaris.exe`) until this is verified end-to-end against the Windows hook shell.
- **Symlinked `./docs/`.** If `./docs/` is a symlink to a directory elsewhere, `polaris setup` runs the initial index against the symlink path, but Claude Code delivers the *resolved* absolute path in the hook payload. The gate then can't reconcile the two, and edits to those files won't auto-reindex. Workaround: index the real directory directly with `polaris index /path/to/real/docs`, or remove the symlink.
- **Concurrent `polaris setup`.** Running `polaris setup` from two terminals at the same time can race on the same set of files (`.mcp.json`, `.gitignore`, agent-instruction files, `.claude/settings.json`). The last writer wins; partial-state recovery isn't guaranteed. Rare in practice — just don't.
- **Stale hook command after binary move.** The hook entry in `.claude/settings.json` carries the absolute path to the polaris binary as it was resolved at setup time. Moving the binary later (e.g. reinstalling under a different prefix) leaves the hook pointing at the old location, so it fails silently to stderr. Re-run `polaris setup` to refresh the path.
- **Hand-edited malformed hook events.** `merge_claude_settings` and `remove_polaris_hooks_from_settings` walk `hooks.<event>` arrays. If a user has hand-edited a `.claude/settings.json` so an event's value is not an array (e.g. `"UserPromptSubmit": {}`), polaris-owned entries in that event are skipped during reconcile. Degenerate input; fix by hand-editing back to the expected shape.
- **Empty `"PostToolUse": []` after `--no-hooks`.** Removing the polaris hook leaves the surrounding key as an empty array rather than pruning the key. Harmless to Claude Code; cosmetic only.
