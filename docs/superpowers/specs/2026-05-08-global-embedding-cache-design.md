# Global Embedding-Model Cache — Design

**Date:** 2026-05-08
**Status:** Approved (pending implementation plan)

## Goal

Stop re-downloading the ~137 MB ONNX embedding model into every repo where Polaris runs. Cache it once in a user-global location so all Polaris installs share a single copy.

## Problem

`EmbeddingEngine::new` (`polaris-core/src/embedding.rs:58`) constructs:

```rust
TextEmbedding::try_new(
    InitOptions::new(info.fastembed_model)
        .with_show_download_progress(true),
)
```

It never sets `cache_dir`. fastembed defaults to `.fastembed_cache/` relative to the **current working directory**, so each repo accumulates its own copy of the model. The user-facing docs (`docs/configuration.md`, `docs/embedding.md`) describe the cache as living at `~/.cache/huggingface/`, which is incorrect.

## Non-goals

- **No auto-migration.** Existing per-repo `.fastembed_cache/` directories are left alone. Users delete them manually once the new global cache is populated.
- **No `polaris.toml` `cache_dir` field.** The env var override is the only knob.
- **No removal of `.fastembed_cache/` from the `polaris setup` gitignore template.** It stays as a safety net (older binaries, env overrides pointing into the project).
- **No new model-storage abstraction.** Pass the path straight to fastembed.

## Resolution order

The new helper resolves the cache root in this order:

1. **`POLARIS_CACHE_DIR` env var** (if set and non-empty) → `<value>/models/`.
2. **`dirs::cache_dir()`** + `polaris/models/`. Per-platform:
   - Linux: `~/.cache/polaris/models/` (honoring `XDG_CACHE_HOME` via `dirs`).
   - macOS: `~/Library/Caches/polaris/models/`.
   - Windows: `%LOCALAPPDATA%\polaris\models\`.
3. **`dirs::cache_dir()` returns `None`** (extremely unusual) → return `PolarisError::Config` with a message instructing the user to set `POLARIS_CACHE_DIR`. Do **not** silently fall back to the current working directory; that re-introduces today's bug.

The helper creates the directory (`std::fs::create_dir_all`) before returning the path, since fastembed expects it to exist.

## Surface

A single new public helper, in a new tiny module:

```rust
// polaris-core/src/paths.rs
pub fn polaris_cache_dir() -> crate::Result<std::path::PathBuf>;
```

Wired in `polaris-core/src/lib.rs` (`pub mod paths;`). The only in-tree consumer is `embedding::EmbeddingEngine::new` within the same crate, so no re-export at the crate root is needed; the `pub` on the module is for external callers (tests, future tools) that want to inspect the resolved path.

`EmbeddingEngine::new` calls `paths::polaris_cache_dir()?` and chains `.with_cache_dir(path)` onto `InitOptions`:

```rust
let cache_dir = crate::paths::polaris_cache_dir()?;
let model = TextEmbedding::try_new(
    InitOptions::new(info.fastembed_model)
        .with_show_download_progress(true)
        .with_cache_dir(cache_dir),
)
```

No other public API changes. `dirs = "5"` is already a polaris-core dependency — no new crates.

## Errors handled

| Condition | Behavior |
|---|---|
| `POLARIS_CACHE_DIR` set but the path is unwritable | Surface the underlying `std::io::Error` from `create_dir_all`, wrapped in `PolarisError::Config`. |
| `dirs::cache_dir()` returns `None` and no env override | `PolarisError::Config` with a clear remediation hint. |
| fastembed download itself fails (network, disk full, etc.) | Existing behavior — bubbles up as `PolarisError::Embedding`. Unchanged. |

## Testing

Unit tests in `polaris-core/src/paths.rs`:

1. `POLARIS_CACHE_DIR` set to a `tempfile::tempdir()` path → returned path is `<override>/models`, the directory exists on disk after the call.
2. `POLARIS_CACHE_DIR` unset → returned path ends with `polaris/models` and starts with `dirs::cache_dir().unwrap()`.
3. `POLARIS_CACHE_DIR` set to empty string → treated as unset; falls through to `dirs::cache_dir()`.

Env vars are process-global. To avoid flakiness when tests run in parallel, use scoped guards (`temp_env::with_var` is the simplest option — small dev-dep) or a tiny RAII wrapper. The latter is preferable to keep the dep set lean; a ~15-line `EnvGuard` struct that saves/restores in `Drop` is enough. Whichever route, the env-touching tests are marked `#[serial]` (via `serial_test`) or grouped under one `#[test]` that exercises the variants in sequence.

The existing ignored end-to-end test `shared_embedding_clone_does_not_reload` continues to pass and now writes into the global cache (no behavior change for the test itself).

## Documentation

Update after implementation:

- `docs/configuration.md` — "Model Caching" section: replace the `~/.cache/huggingface/` claim with the new resolution order, document `POLARIS_CACHE_DIR`.
- `docs/embedding.md` — "Supported Models" footer: same correction.
- `README.md` — if there is a quick-start mention of the cache, update it. (No mention as of writing, so likely a no-op.)

## Open question (resolved during implementation, not now)

Confirm `InitOptions::with_cache_dir(impl Into<PathBuf>)` exists on fastembed v5. This is the documented public API and has been stable since v3, but verify against `cargo doc -p fastembed --open` before writing the call site. If the method has been renamed, adjust accordingly — the rest of the design is unaffected.

## Out-of-band cleanup

Once this lands, users with leftover `.fastembed_cache/` directories can:

```bash
rm -rf .fastembed_cache/
```

A note in the changelog and in `docs/configuration.md` is sufficient — no automation.
