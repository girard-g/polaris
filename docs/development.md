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
| Linux    | `~/.cache/huggingface/` |
| macOS    | `~/Library/Caches/huggingface/` |
| Windows  | `%LOCALAPPDATA%\huggingface\` |

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

## Project Layout

```
polaris/
├── Cargo.toml          Dependency manifest
├── Cargo.lock          Locked dependency versions (commit this)
├── .mcp.json           MCP server registration for Claude Code
├── src/                All source code
│   ├── main.rs
│   ├── config.rs
│   ├── error.rs
│   ├── embedding.rs
│   ├── db.rs
│   ├── indexer.rs
│   ├── search.rs
│   └── mcp/
│       ├── mod.rs
│       ├── server.rs
│       └── types.rs
├── docs/               This documentation
└── .fastembed_cache/   Local model cache (gitignored)
```

## Adding a New MCP Tool

1. Add a parameter struct to `mcp/types.rs`:
   ```rust
   #[derive(Debug, Serialize, Deserialize, JsonSchema)]
   pub struct MyToolParams {
       pub some_field: String,
       pub optional: Option<bool>,
   }
   ```

2. Add a `#[tool]` method to `impl PolarisServer` in `mcp/server.rs`:
   ```rust
   #[tool(name = "my_tool", description = "Does something useful.")]
   async fn my_tool(&self, Parameters(params): Parameters<MyToolParams>) -> String {
       // ... implementation
       "result".to_string()
   }
   ```

3. The `#[tool_router]` macro on the `impl` block automatically registers the new tool.

## Adding a New CLI Command

1. Add a variant to the `Command` enum in `main.rs`
2. Add the matching arm in the `match cli.command` block
3. Wire up the appropriate engine components (config, DB, embedding engine as needed)

## Modifying the Database Schema

Schema changes require a migration strategy. Currently there are no migrations — schema version `"1"` is the only version. If you add columns or tables:

1. Bump `SCHEMA_VERSION` constant in `db.rs`
2. Add a migration branch in `Database::open()` that detects version `"1"` and upgrades to `"2"`
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
