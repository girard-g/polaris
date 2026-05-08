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
    std::fs::create_dir_all(&path).map_err(|e| {
        PolarisError::Config(format!(
            "Failed to create cache directory {}: {e}",
            path.display()
        ))
    })?;
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
            // SAFETY: env mutation is not thread-safe (POSIX setenv(3) is not
            // async-signal/thread-safe). The Rust test runner executes tests in
            // parallel threads; this guard is safe here because only ONE test in
            // this module calls EnvGuard. All other tests invoke the pure
            // `resolve_cache_root` and never read POLARIS_CACHE_DIR. Keep it that way.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `EnvGuard::set` — same single-test invariant applies.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
