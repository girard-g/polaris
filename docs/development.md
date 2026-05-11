# Development Guide

## Prerequisites

**All platforms:** Rust 1.87+ (stable) — install via [rustup](https://rustup.rs)

**Linux (Debian/Ubuntu):**
```bash
sudo apt-get install gcc libssl-dev pkg-config
```

**Linux (Fedora/RHEL):**
```bash
sudo dnf install gcc openssl-devel pkg-config
```

**macOS:** Xcode Command Line Tools:
```bash
xcode-select --install
```

**Windows:** MSVC Build Tools — install via Visual Studio Installer, selecting "Desktop development with C++"

SQLite is bundled and the ONNX runtime ships with fastembed, so no additional runtime dependencies are needed.

## Build

```bash
# Debug build (faster compile, slower runtime)
cargo build

# Release build (recommended for actual use)
cargo build --release
```

The first build downloads and compiles all dependencies including fastembed's ONNX runtime, which may take a few minutes.

## First Run

On first run, the embedding model is downloaded (~137 MB) and cached. Subsequent runs reuse the cache.

| Platform | Model cache location |
|----------|----------------------|
| Linux    | `~/.cache/polaris/models/` (honours `$XDG_CACHE_HOME`) |
| macOS    | `~/Library/Caches/polaris/models/` |
| Windows  | `%LOCALAPPDATA%\polaris\models\` |

The cache is shared across projects. Set `POLARIS_CACHE_DIR` to override the location (e.g. for CI or shared team caches). See [Configuration → Model Caching](configuration.md#model-caching) for full details.

```bash
./target/debug/polaris index ./docs
```

## Running Checks

```bash
# Type check + lint (no binary produced)
cargo check

# Clippy lints
cargo clippy

# Format
cargo fmt

# Run tests
cargo test
```

## Environment Variables

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | Tracing log level. Examples: `debug`, `polaris=trace`, `info` |
| `RUST_LOG_STYLE` | Log color. `never` disables ANSI colors |
| `POLARIS_CACHE_DIR` | Override the user-global model cache root. Models are stored under `$POLARIS_CACHE_DIR/models/`. |

## Project Layout

Polaris is a Cargo workspace with two crates: `polaris-core` (library) and `polaris-cli` (binary).

```
polaris/
├── Cargo.toml             Workspace manifest
├── Cargo.lock             Locked dependency versions (commit this)
├── .mcp.json              MCP server registration for Claude Code
├── polaris-core/          Library: retrieval pipeline
│   └── src/
│       ├── lib.rs
│       ├── config.rs
│       ├── error.rs
│       ├── paths.rs       Global model-cache resolver
│       ├── embedding.rs
│       ├── db.rs
│       ├── bank.rs        Per-project handle + multi-DB BankSet
│       ├── indexer.rs
│       └── search.rs
├── polaris-cli/           Binary: CLI + MCP server + setup + savings
│   └── src/
│       ├── main.rs
│       ├── setup.rs
│       ├── savings.rs
│       ├── tui.rs
│       └── mcp/
│           ├── mod.rs
│           ├── server.rs
│           └── types.rs
└── docs/                  This documentation
```

## Adding a New MCP Tool

1. Add a parameter struct to `polaris-cli/src/mcp/types.rs`:
   ```rust
   #[derive(Debug, Serialize, Deserialize, JsonSchema)]
   pub struct MyToolParams {
       pub some_field: String,
       pub optional: Option<bool>,
   }
   ```

2. Add a `#[tool]` method to `impl PolarisServer` in `polaris-cli/src/mcp/server.rs`:
   ```rust
   #[tool(name = "my_tool", description = "Does something useful.")]
   async fn my_tool(&self, Parameters(params): Parameters<MyToolParams>) -> String {
       // ... implementation
       "result".to_string()
   }
   ```

3. The `#[tool_router]` macro on the `impl` block automatically registers the new tool.

## Adding a New CLI Command

1. Add a variant to the `Command` enum in `polaris-cli/src/main.rs`
2. Add the matching arm in the `match cli.command` block
3. Wire up the appropriate engine components (config, Bank, embedding engine as needed)

## Modifying the Database Schema

The current schema is at version `"3"` (added the `search_log` table). Two migrations already ship: v1→v2 and v2→v3, both applied automatically by `Database::open()`. If you add columns or tables:

1. Bump `SCHEMA_VERSION` constant in `polaris-core/src/db.rs`
2. Add a new `migrate_vN_to_vN+1` function and wire it into `apply_migrations`
3. Update `docs/database.md`

## Debugging MCP Interactions

The MCP server communicates over stdio. To debug, run polaris with verbose logging redirected:

```bash
RUST_LOG=debug polaris serve 2>polaris-debug.log
```

Then watch `polaris-debug.log` while Claude Code interacts with the server.

## Releasing

Pre-built binaries for Linux x86_64, macOS aarch64, macOS x86_64, and Windows x86_64 are built automatically via GitHub Actions on tag push:

```bash
git tag v0.1.0
git push --tags
```

This triggers the release workflow, which builds all platform binaries and creates a GitHub Release with the binaries attached.

**Known limitations:**
- musl/Alpine Linux is not supported (ONNX Runtime pre-built binaries are glibc-only)
- Linux aarch64 is not included in the initial release matrix
