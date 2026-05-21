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

When you run `polaris setup` in a project, polaris also writes a `PostToolUse` hook into `.claude/settings.json`. The hook fires after Claude Code's `Write`, `Edit`, or `MultiEdit` tools complete; it re-runs `polaris index` for the touched file if (a) the path ends in `.md` and (b) the file lives under a directory the index already covers. Failures are non-fatal: the hook logs to stderr and always exits 0, so a transient hiccup never surfaces as a warning banner in Claude Code or interrupts your session.

To opt out, run `polaris setup --no-hooks`. To remove the hook from a project that already has it installed, re-run `polaris setup --no-hooks` — it strips the polaris entries from `.claude/settings.json` while leaving any other hooks intact.

The hook is a Claude Code feature; Codex, Cursor, and Gemini CLI users keep using the MCP `search` tool and can run `polaris watch` if they want background auto-indexing.
