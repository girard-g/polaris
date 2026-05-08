# Global Embedding-Model Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Polaris cache the fastembed ONNX model in a single user-global location (`~/.cache/polaris/models/` by default, overridable with `POLARIS_CACHE_DIR`) instead of re-downloading ~137 MB into every project.

**Architecture:** A new tiny `polaris-core/src/paths.rs` module exposes `polaris_cache_dir()`. It splits resolution into a pure function (env value + `dirs::cache_dir()` → `PathBuf`) that is fully unit-testable without touching the filesystem or process env, and a thin public wrapper that performs the env read and `create_dir_all`. `EmbeddingEngine::new` calls the wrapper and threads the path into `InitOptions::with_cache_dir`.

**Tech Stack:** Rust 2024 (workspace edition). Crates already in tree: `dirs = "5"` (runtime), `tempfile = "3"` (dev). No new dependencies.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `polaris-core/src/paths.rs` | Create | `polaris_cache_dir()` public helper + private pure resolver + tests. |
| `polaris-core/src/lib.rs` | Modify | `pub mod paths;` declaration. |
| `polaris-core/src/embedding.rs` | Modify | `EmbeddingEngine::new` calls `paths::polaris_cache_dir()` and chains `.with_cache_dir(...)` on `InitOptions`. |
| `docs/configuration.md` | Modify | Replace the wrong "Model Caching" section content (lines 119–123). |
| `docs/embedding.md` | Modify | Replace the wrong cache-location footer (line 11). |

`polaris-core/Cargo.toml`: no change — both `dirs` and `tempfile` (dev) are already present.

---

## Task 1: Add `polaris_cache_dir` helper with pure resolver (TDD)

**Files:**
- Create: `polaris-core/src/paths.rs`
- Modify: `polaris-core/src/lib.rs`

- [ ] **Step 1: Create `polaris-core/src/paths.rs` with failing tests and stubs**

Write the full file content:

```rust
//! Filesystem path helpers for Polaris.
//!
//! `polaris_cache_dir()` returns the user-global directory used by fastembed
//! to cache downloaded ONNX model files. Resolution order:
//!
//! 1. `POLARIS_CACHE_DIR` env var (if set and non-empty) → `<value>/models`.
//! 2. `dirs::cache_dir()` + `polaris/models`.
//! 3. If `dirs::cache_dir()` returns `None` and no env override is set,
//!    return an error instructing the user to set `POLARIS_CACHE_DIR`.

use std::path::PathBuf;

use crate::error::{PolarisError, Result};

const ENV_VAR: &str = "POLARIS_CACHE_DIR";

/// Pure resolver: takes the optional env value and the optional
/// `dirs::cache_dir()` result, returns the final cache root.
///
/// This function does no I/O and reads no global state, so it is fully
/// unit-testable.
fn resolve_cache_root(env_value: Option<String>, dirs_cache: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(v) = env_value {
        if !v.is_empty() {
            return Ok(PathBuf::from(v).join("models"));
        }
    }
    match dirs_cache {
        Some(p) => Ok(p.join("polaris").join("models")),
        None => Err(PolarisError::Config(format!(
            "Could not determine a user cache directory on this platform. \
             Set {ENV_VAR} to an absolute path."
        ))),
    }
}

/// Resolve the global Polaris model cache directory, creating it if missing.
///
/// Returns the absolute path to the directory that fastembed should use as its
/// `cache_dir`. The directory is created via `std::fs::create_dir_all` before
/// the path is returned.
pub fn polaris_cache_dir() -> Result<PathBuf> {
    let env_value = std::env::var(ENV_VAR).ok();
    let path = resolve_cache_root(env_value, dirs::cache_dir())?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_appends_models_subdir() {
        let resolved = resolve_cache_root(
            Some("/tmp/custom-cache".to_string()),
            Some(PathBuf::from("/home/user/.cache")),
        )
        .expect("resolve");
        assert_eq!(resolved, PathBuf::from("/tmp/custom-cache/models"));
    }

    #[test]
    fn empty_env_value_falls_through_to_dirs_cache() {
        let resolved = resolve_cache_root(
            Some(String::new()),
            Some(PathBuf::from("/home/user/.cache")),
        )
        .expect("resolve");
        assert_eq!(resolved, PathBuf::from("/home/user/.cache/polaris/models"));
    }

    #[test]
    fn no_env_uses_dirs_cache_with_polaris_models() {
        let resolved = resolve_cache_root(None, Some(PathBuf::from("/home/user/.cache")))
            .expect("resolve");
        assert_eq!(resolved, PathBuf::from("/home/user/.cache/polaris/models"));
    }

    #[test]
    fn no_env_and_no_dirs_cache_returns_config_error() {
        let err = resolve_cache_root(None, None).expect_err("should error");
        match err {
            PolarisError::Config(msg) => {
                assert!(msg.contains(ENV_VAR), "error should mention {ENV_VAR}: {msg}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn public_helper_creates_directory_under_env_override() {
        // Single test that touches the process env, scoped via an EnvGuard.
        // We do not run this concurrently with other env-touching tests
        // because there are no others — keep it that way.
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(ENV_VAR, tmp.path().to_str().expect("utf8 tempdir"));

        let resolved = polaris_cache_dir().expect("resolve");
        assert_eq!(resolved, tmp.path().join("models"));
        assert!(resolved.is_dir(), "directory should be created");
    }

    /// RAII guard that sets an env var on construction and restores the prior
    /// value (or removes it) on drop. Rust 2024 made env mutation `unsafe`;
    /// callers must hold this guard for the duration of the scope where the
    /// override should apply.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: tests using this guard do not run concurrently with
            // other code that reads/writes the same env var.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
```

- [ ] **Step 2: Wire the module into `polaris-core/src/lib.rs`**

Edit `polaris-core/src/lib.rs`. Replace:

```rust
pub mod bank;
pub mod config;
pub mod db;
pub mod embedding;
pub mod error;
pub mod indexer;
pub mod search;
```

With:

```rust
pub mod bank;
pub mod config;
pub mod db;
pub mod embedding;
pub mod error;
pub mod indexer;
pub mod paths;
pub mod search;
```

- [ ] **Step 3: Build to make sure the file compiles**

Run: `cargo check -p polaris-core`
Expected: clean build (no warnings, no errors).

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p polaris-core --lib paths::`
Expected: 5 tests pass:
- `paths::tests::env_override_appends_models_subdir`
- `paths::tests::empty_env_value_falls_through_to_dirs_cache`
- `paths::tests::no_env_uses_dirs_cache_with_polaris_models`
- `paths::tests::no_env_and_no_dirs_cache_returns_config_error`
- `paths::tests::public_helper_creates_directory_under_env_override`

If `public_helper_creates_directory_under_env_override` reports a missing `tempfile` import, confirm `tempfile = "3"` is under `[dev-dependencies]` in `polaris-core/Cargo.toml` (it should already be).

- [ ] **Step 5: Run the full polaris-core test suite to ensure no regressions**

Run: `cargo test -p polaris-core --lib`
Expected: all existing tests still pass; the 5 new `paths::tests::*` are added to the count.

- [ ] **Step 6: Commit**

```bash
git add polaris-core/src/paths.rs polaris-core/src/lib.rs
git commit -m "feat(core): add polaris_cache_dir helper for global model cache"
```

---

## Task 2: Wire `EmbeddingEngine::new` into the global cache

**Files:**
- Modify: `polaris-core/src/embedding.rs`

- [ ] **Step 1: Update `EmbeddingEngine::new` to set `cache_dir`**

Edit `polaris-core/src/embedding.rs`. Replace lines 55–70 (the existing `impl EmbeddingEngine { pub fn new ... }` block):

```rust
impl EmbeddingEngine {
    pub fn new(target_dim: usize, model_id: &str) -> Result<Self> {
        let info = resolve_model(model_id)?;
        let model = TextEmbedding::try_new(
            InitOptions::new(info.fastembed_model)
                .with_show_download_progress(true),
        )
        .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Failed to load model: {e}")))?;

        Ok(Self {
            model: Mutex::new(model),
            target_dim,
            doc_prefix: info.document_prefix.to_string(),
            query_prefix: info.query_prefix.to_string(),
        })
    }
```

With:

```rust
impl EmbeddingEngine {
    pub fn new(target_dim: usize, model_id: &str) -> Result<Self> {
        let info = resolve_model(model_id)?;
        let cache_dir = crate::paths::polaris_cache_dir()?;
        let model = TextEmbedding::try_new(
            InitOptions::new(info.fastembed_model)
                .with_show_download_progress(true)
                .with_cache_dir(cache_dir),
        )
        .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Failed to load model: {e}")))?;

        Ok(Self {
            model: Mutex::new(model),
            target_dim,
            doc_prefix: info.document_prefix.to_string(),
            query_prefix: info.query_prefix.to_string(),
        })
    }
```

The two added lines are:
1. `let cache_dir = crate::paths::polaris_cache_dir()?;`
2. `.with_cache_dir(cache_dir)` chained onto the existing `InitOptions::new(...).with_show_download_progress(true)`.

- [ ] **Step 2: Build the workspace**

Run: `cargo check --workspace`
Expected: clean build.

If the build errors with "no method named `with_cache_dir` found for struct `InitOptions`", verify the fastembed v5 API:

```bash
cargo doc -p fastembed --no-deps --open
```

Look up `InitOptions` in the generated docs and use the actual setter name. If it has been renamed (e.g. to `with_model_cache_dir`), update the call site to match. The rest of the plan is unaffected.

- [ ] **Step 3: Run the full polaris-core test suite**

Run: `cargo test -p polaris-core`
Expected: all tests pass. The ignored model-download test (`shared_embedding_clone_does_not_reload`) remains ignored.

- [ ] **Step 4: Run the workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass. No regressions in `polaris-cli`.

- [ ] **Step 5: Smoke-test the wiring manually**

Run from a tempdir to confirm fastembed no longer creates `.fastembed_cache/` in the working directory and instead populates the global cache:

```bash
mkdir -p /tmp/polaris-cache-smoke && cd /tmp/polaris-cache-smoke
POLARIS_CACHE_DIR=/tmp/polaris-cache-smoke/global \
  cargo run -p polaris-cli --quiet -- index README.md 2>&1 | head -5 || true
ls /tmp/polaris-cache-smoke/global/models/ 2>/dev/null && echo "GLOBAL CACHE POPULATED"
test -d /tmp/polaris-cache-smoke/.fastembed_cache && echo "BUG: per-CWD cache still created" || echo "OK: no per-CWD cache"
cd - && rm -rf /tmp/polaris-cache-smoke
```

This step requires network the first time (it actually downloads ~137 MB). If you have already populated `~/.cache/polaris/models/` from a prior run, fastembed will hit the override path freshly, which is the point of the test. If network is unavailable in this environment, skip this step and rely on the unit tests.

Expected output includes `GLOBAL CACHE POPULATED` and `OK: no per-CWD cache`.

- [ ] **Step 6: Commit**

```bash
git add polaris-core/src/embedding.rs
git commit -m "feat(core): cache fastembed model in global polaris_cache_dir"
```

---

## Task 3: Update documentation

**Files:**
- Modify: `docs/configuration.md`
- Modify: `docs/embedding.md`

- [ ] **Step 1: Fix the "Model Caching" section in `docs/configuration.md`**

Edit `docs/configuration.md`. Replace lines 119–123:

```markdown
## Model Caching

The fastembed model is downloaded on first use to `~/.cache/huggingface/`. Subsequent runs reuse the cached ONNX files.

Download progress is shown in the terminal when the model is not yet cached.
```

With:

```markdown
## Model Caching

The fastembed model is downloaded on first use to a user-global cache shared across all projects, so multiple Polaris installs do not each redownload the same ONNX files.

Resolution order:

1. `POLARIS_CACHE_DIR` environment variable (if set and non-empty) → `$POLARIS_CACHE_DIR/models/`.
2. Otherwise the platform user cache directory + `polaris/models/`:
   - Linux: `~/.cache/polaris/models/` (honours `$XDG_CACHE_HOME`).
   - macOS: `~/Library/Caches/polaris/models/`.
   - Windows: `%LOCALAPPDATA%\polaris\models\`.

Download progress is shown in the terminal when the model is not yet cached.

If you have a leftover `.fastembed_cache/` directory in a project from an older Polaris version, you can safely remove it: `rm -rf .fastembed_cache/`.
```

- [ ] **Step 2: Fix the cache-location footer in `docs/embedding.md`**

Edit `docs/embedding.md`. Replace line 11:

```markdown
All models run via ONNX on CPU. Model files are cached in `~/.cache/huggingface/`.
```

With:

```markdown
All models run via ONNX on CPU. Model files are cached in a user-global directory shared across projects (default `~/.cache/polaris/models/`; overridable via `POLARIS_CACHE_DIR`). See [Configuration → Model Caching](configuration.md#model-caching).
```

- [ ] **Step 3: Re-index the docs so polaris.search reflects the change**

Run: `cargo run -p polaris-cli --quiet -- index docs/`
Expected: index reports the two updated files. (If the binary is not built yet, replace with `cargo build -p polaris-cli && ./target/debug/polaris index docs/`.)

- [ ] **Step 4: Commit**

```bash
git add docs/configuration.md docs/embedding.md
git commit -m "docs: describe global model cache and POLARIS_CACHE_DIR override"
```

---

## Verification (manual, post-implementation)

After all three tasks land, the following should hold. None of these are scripted steps — they are signals to confirm the goal of the plan has been met.

- A fresh `cargo run -p polaris-cli -- index <path>` in any directory does **not** create `.fastembed_cache/` in that directory.
- `~/.cache/polaris/models/` (or the platform equivalent) contains the ONNX model files.
- Setting `POLARIS_CACHE_DIR=/some/path` and re-running causes the model to live under `/some/path/models/`.
- `cargo test --workspace` is green.
- `polaris.search` returns the updated wording when queried for "model caching".
