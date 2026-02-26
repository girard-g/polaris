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
| `top_k` | integer | no | 5 | Number of results to return |

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
Embedding dim: 512
Last indexed: 2025-02-26T14:23:45Z
```

## Protocol Details

- **Transport:** stdio (JSON-RPC over stdin/stdout)
- **Framework:** rmcp 0.16
- **Server name:** `polaris`
- **Version:** from `CARGO_PKG_VERSION`
- **Server instructions** (sent in `initialize` response):
  > Polaris is a semantic search MCP server for project documentation. Use `search` to find relevant documentation chunks, `index` to add new files, and `status` to check the index health.

## Shared State

All three tools share a single `PolarisState`:

```rust
PolarisState {
    config:           Arc<PolarisConfig>,
    embedding_engine: Arc<EmbeddingEngine>,  // Mutex<TextEmbedding> inside
    db:               Arc<Mutex<Database>>,
}
```

Each tool clones the required `Arc`s and offloads all blocking work (embedding, SQLite, filesystem) to `tokio::task::spawn_blocking`. The DB mutex is acquired inside the blocking closure — never across an `.await` point. Tool calls are serialized through the mutex.

## Error Handling in Tools

Tool errors are returned as formatted error strings rather than MCP error objects. This keeps the integration simple and ensures Claude always receives human-readable feedback.

```
Error: path not found: /nonexistent
Error: <embedding error message>
```

## Implementation Notes

- The `mcp/server.rs` file does **not** import `use crate::error::Result` — it conflicts with rmcp macro-generated code that expects `ErrorData`. Use `std::result::Result` or `PolarisError` directly.
- The `#[tool_router]` attribute on the `impl PolarisServer` block generates the routing table.
- `#[tool_handler(router = self.tool_router)]` wires it into `ServerHandler`.
- `Database` (`rusqlite::Connection`) is `!Send`. It is accessed via `Arc<Mutex<Database>>` — the `Arc` is cloned on the async side, the lock is acquired inside the `spawn_blocking` closure where `Send` is not required.
