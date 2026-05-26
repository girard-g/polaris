//! `polaris hook` — internal subcommands invoked by Claude Code hooks.
//!
//! Each subcommand reads its hook payload as JSON on stdin and applies its
//! action. All paths exit 0 unconditionally; failures are reported to stderr
//! so a transient hiccup never interrupts the user's session via a Claude Code
//! warning banner.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use polaris_core::config::PolarisConfig;
use polaris_core::db::Database;
use polaris_core::embedding::EmbeddingEngine;
use polaris_core::error::{PolarisError, Result};
use polaris_core::indexer::Indexer;

/// The slice of a Claude Code hook payload we actually use.
#[derive(Debug)]
pub struct HookPayload {
    pub file_path: PathBuf,
    /// The working directory Claude Code is operating in. We use it to
    /// translate the absolute `file_path` into a project-relative form when
    /// the index stores relative paths (the CLI flow `polaris index docs`).
    /// Absent when the payload omits it; in that case we fall back to
    /// absolute-only matching.
    pub cwd: Option<PathBuf>,
}

/// Parse a Claude Code hook payload (stdin JSON) into the fields we care about.
///
/// Returns `Err` if the JSON is invalid, the top level isn't an object, or
/// `tool_input.file_path` is missing. `cwd` is optional. We don't re-validate
/// `tool_name` here — the gate on which tools trigger the hook is the matcher
/// configured in `.claude/settings.json` (`Write|Edit|MultiEdit`).
pub fn parse_payload(json: &str) -> Result<HookPayload> {
    use serde_json::Value;

    let parsed: Value = serde_json::from_str(json)
        .map_err(|e| PolarisError::Setup(format!("hook payload is not valid JSON: {e}")))?;
    let Value::Object(root) = &parsed else {
        return Err(PolarisError::Setup(
            "hook payload top level is not an object".into(),
        ));
    };
    let file_path = root
        .get("tool_input")
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            PolarisError::Setup("hook payload missing tool_input.file_path".into())
        })?;
    let cwd = root
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    Ok(HookPayload {
        file_path: PathBuf::from(file_path),
        cwd,
    })
}

/// Payload for the `UserPromptSubmit` hook.
#[derive(Debug)]
pub struct SearchPayload {
    pub prompt: String,
    pub cwd: Option<PathBuf>,
}

/// Parse a `UserPromptSubmit` hook payload into the fields we need.
pub fn parse_search_payload(json: &str) -> Result<SearchPayload> {
    use serde_json::Value;

    let parsed: Value = serde_json::from_str(json)
        .map_err(|e| PolarisError::Setup(format!("hook payload is not valid JSON: {e}")))?;
    let Value::Object(root) = &parsed else {
        return Err(PolarisError::Setup(
            "hook payload top level is not an object".into(),
        ));
    };
    let prompt = root
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PolarisError::Setup("hook payload missing prompt".into()))?;
    let cwd = root
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    Ok(SearchPayload {
        prompt: prompt.to_string(),
        cwd,
    })
}

/// Returns true if the path looks like a markdown file we should consider
/// indexing. Strict `ext == "md"` to match
/// `polaris-core::indexer::discover_markdown_files`, which is case-sensitive
/// — accepting `.MD` here would only result in a wasted round-trip when the
/// indexer skips the file. Also rejects extension-only names (e.g. literally
/// `.md` with no stem).
pub fn is_markdown(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    if ext != "md" {
        return false;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// If `target` lives under a directory containing at least one previously
/// indexed document, returns the form of `target` that matches the existing
/// row's path representation:
///   - Absolute `target` when the matching indexed row stores an absolute path
///     (typical when the user ran `polaris index /abs/path`).
///   - `cwd`-relative `target` when the matching indexed row stores a relative
///     path (typical when the user ran `polaris index docs` or `polaris setup`
///     from the project root).
///
/// Returns `None` if no matching root exists. Returning the matched form lets
/// `perform_index` re-index using the same path string the existing row uses,
/// avoiding duplicate rows that would otherwise pollute search results.
///
/// `cwd` is the working directory Claude Code reported in the hook payload.
/// When `cwd` is absent or doesn't contain `target`, we fall back to using
/// `target` itself as the "relative" form — useful for unit tests where both
/// inputs are already relative.
pub fn under_indexed_root(
    target: &Path,
    cwd: Option<&Path>,
    indexed_paths: &[String],
) -> Option<PathBuf> {
    let target_rel: PathBuf = cwd
        .and_then(|c| target.strip_prefix(c).ok())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| target.to_path_buf());

    for p in indexed_paths {
        let p_path = Path::new(p);

        if p_path.is_absolute() {
            // Absolute DB paths: match by immediate parent. We can't infer
            // the original `polaris index <root>` from the stored file path,
            // so walking ancestors would over-match. Files directly under the
            // filesystem root (`/foo.md`) are treated as single-file roots.
            match meaningful_parent(p_path) {
                Some(parent) => {
                    if target.starts_with(parent) {
                        return Some(target.to_path_buf());
                    }
                }
                None => {
                    // Single-file under fs root — exact match only.
                    if target == p_path {
                        return Some(target.to_path_buf());
                    }
                }
            }
        } else {
            // Relative DB paths: walk up to the first directory component
            // (the indexed root). For `docs/sub/seed.md` → `docs`, so
            // `docs/new.md` matches even when the DB has no direct children
            // of `docs/`. Single-component paths (`README.md`) require exact
            // match.
            match topmost_relative_dir(p_path) {
                Some(root) => {
                    if target_rel.starts_with(root) {
                        return Some(target_rel.clone());
                    }
                }
                None => {
                    if target_rel == p_path {
                        return Some(target_rel.clone());
                    }
                }
            }
        }
    }
    None
}

/// For an absolute path, return its parent directory if that parent is above
/// the filesystem root. Returns `None` for files directly under the fs root
/// (`/foo.md`, `C:\foo.md`) — those are single-file roots.
fn meaningful_parent(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if is_fs_root(parent) || parent.as_os_str().is_empty() {
        None
    } else {
        Some(parent)
    }
}

/// For a relative path, walk up to the first (shallowest) directory component.
/// Returns `None` for single-component paths (`README.md`).
///
/// `docs/sub/file.md` → `Some("docs")`
/// `docs/file.md` → `Some("docs")`
/// `README.md` → `None`
fn topmost_relative_dir(path: &Path) -> Option<&Path> {
    let mut current = path;
    while let Some(parent) = current.parent() {
        if parent.as_os_str().is_empty() {
            break;
        }
        current = parent;
    }
    // `current` is now the first component. If it's the same as the input,
    // there was no directory component at all.
    if current == path {
        None
    } else {
        Some(current)
    }
}

fn is_fs_root(path: &Path) -> bool {
    use std::path::Component;
    let mut components = path.components();
    match components.next() {
        Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
        _ => return false,
    }
    // After the root/prefix, there should be nothing left (or just a RootDir
    // following a Prefix on Windows: `C:\`).
    components.all(|c| matches!(c, Component::RootDir))
}

/// Outcome of one `perform_index` call. Used in tests; production code only
/// cares that the call returned `Ok`.
#[derive(Debug)]
pub struct HookIndexReport {
    /// Number of files added or modified by this index call.
    #[allow(dead_code)]
    pub indexed_new_or_modified: usize,
}

/// RAII guard that restores the process working directory on drop. Hook
/// processes are short-lived, but resetting on drop keeps the contract simple
/// in tests and future composition.
struct CwdGuard(PathBuf);
impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// Apply the gates and (if eligible) run a single-file index pass. Pure
/// failures (DB locked, etc.) bubble up as `Err`; the caller decides whether
/// to surface them — `run_index` swallows them into stderr.
///
/// `cwd` is the working directory Claude Code reported in the payload; we
/// use it to compute the relative form of `file_path` for matching against
/// indexed roots that store relative paths. `cfg` is the `PolarisConfig`
/// already loaded by `main.rs` (respecting the user's `polaris.toml` and CLI
/// overrides) — using it here means the hook indexes into the same DB and
/// with the same embedding/model parameters as the rest of the CLI.
pub fn perform_index(
    file_path: &Path,
    cwd: Option<&Path>,
    cfg: &PolarisConfig,
) -> Result<HookIndexReport> {
    if !is_markdown(file_path) {
        return Ok(HookIndexReport { indexed_new_or_modified: 0 });
    }

    // `register_vec_extension` is called by `main.rs::run` before dispatching,
    // so we don't re-register here.
    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;

    // TODO(perf): O(n) on every hook fire — fine at typical scale (hundreds to
    // low thousands of docs) but worth replacing with a dedicated roots table
    // or a process-lifetime cache if very large indexes become common.
    let indexed = db.get_all_document_hashes()?;
    let indexed_paths: Vec<String> = indexed.into_iter().map(|(p, _)| p).collect();
    let Some(target_for_indexer) = under_indexed_root(file_path, cwd, &indexed_paths) else {
        return Ok(HookIndexReport { indexed_new_or_modified: 0 });
    };

    // If the matched form is relative, the indexer's WalkDir will resolve it
    // against the process CWD. Set CWD to the payload's cwd so it points at
    // the project root. The RAII guard restores the prior CWD on return.
    let _cwd_guard = if !target_for_indexer.is_absolute() {
        cwd.and_then(|c| {
            let prev = std::env::current_dir().ok()?;
            std::env::set_current_dir(c).ok()?;
            Some(CwdGuard(prev))
        })
    } else {
        None
    };

    // TODO(perf): EmbeddingEngine::new loads the ONNX model (~140 MB resident,
    // ~hundreds of ms wall time) on every eligible hook fire. Acceptable for
    // occasional markdown edits, but a chatty doc-editing session pays the
    // cost per edit. Phase 2 options: a long-lived background indexer the
    // hook signals, or batching markdown indexing into a Stop hook instead
    // of PostToolUse.
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);
    let indexer = Indexer::new(
        engine,
        cfg.max_chunk_tokens,
        cfg.chunk_overlap_chars,
        cfg.max_file_size,
    );
    let report = indexer.index_path(&db, &target_for_indexer, false, false, false, None)?;
    Ok(HookIndexReport {
        indexed_new_or_modified: report.added.len() + report.modified.len(),
    })
}

/// Entry point for `polaris hook index`. Reads the payload from stdin and
/// delegates to `run_index_for_payload`. Always returns `Ok(())` so the
/// process exits 0; errors are logged to stderr by the inner helper.
///
/// `cfg` is the `PolarisConfig` `main.rs` already loaded — passing it in
/// (rather than re-loading via `PolarisConfig::default()`) means the hook
/// respects the user's `polaris.toml` (`db_path`, `embedding_dim`,
/// `model_id`) so it targets the same DB and embedding setup as the rest of
/// the CLI.
pub fn run_index(cfg: &PolarisConfig) -> Result<()> {
    let mut buf = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
        eprintln!("polaris hook index: failed to read stdin: {e}");
        return Ok(());
    }
    run_index_for_payload(&buf, cfg)
}

/// Pure-ish helper that takes the stdin payload and the loaded config.
/// Swallows every error into a stderr line and returns `Ok(())`. Exposed
/// for tests so we can exercise the silent-failure discipline without
/// spinning up stdin.
pub fn run_index_for_payload(payload: &str, cfg: &PolarisConfig) -> Result<()> {
    let parsed = match parse_payload(payload) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("polaris hook index: {e}");
            return Ok(());
        }
    };

    if let Err(e) = perform_index(&parsed.file_path, parsed.cwd.as_deref(), cfg) {
        eprintln!(
            "polaris hook index: failed to index {}: {e}",
            parsed.file_path.display(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prefix a Unix-style path with a drive letter on Windows so it qualifies
    /// as a true absolute path (Windows requires a drive prefix; a bare
    /// leading `/` is treated as relative there). Used in tests that
    /// exercise the absolute-branch of `under_indexed_root`.
    fn abs(p: &str) -> String {
        if cfg!(unix) {
            p.to_string()
        } else {
            format!("C:{p}")
        }
    }

    /// Build a `PolarisConfig` with an explicit DB path for tests. Other
    /// fields default — sufficient for the silent-failure and integration
    /// tests, which only need `cfg.db_path` to point somewhere specific
    /// (or nowhere, to provoke a DB-open failure).
    fn cfg_with_db(db_path: PathBuf) -> PolarisConfig {
        let mut cfg = PolarisConfig::default();
        cfg.db_path = db_path;
        cfg
    }

    #[test]
    fn parse_payload_extracts_file_path_for_edit() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/abs/path/docs/foo.md", "old_string": "x", "new_string": "y" },
            "cwd": "/abs/path"
        }"#;
        let payload = parse_payload(json).unwrap();
        assert_eq!(payload.file_path.to_string_lossy(), "/abs/path/docs/foo.md");
    }

    #[test]
    fn parse_payload_extracts_file_path_for_write() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": { "file_path": "/p/a.md", "content": "hello" }
        }"#;
        let payload = parse_payload(json).unwrap();
        assert_eq!(payload.file_path.to_string_lossy(), "/p/a.md");
    }

    #[test]
    fn parse_payload_extracts_file_path_for_multiedit() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "MultiEdit",
            "tool_input": { "file_path": "/p/b.md", "edits": [] }
        }"#;
        let payload = parse_payload(json).unwrap();
        assert_eq!(payload.file_path.to_string_lossy(), "/p/b.md");
    }

    #[test]
    fn parse_payload_errors_on_missing_file_path() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "old_string": "x" }
        }"#;
        assert!(parse_payload(json).is_err());
    }

    #[test]
    fn parse_payload_errors_on_invalid_json() {
        assert!(parse_payload("not json {").is_err());
    }

    #[test]
    fn parse_payload_errors_when_top_level_is_not_object() {
        assert!(parse_payload("[1,2,3]").is_err());
    }

    #[test]
    fn parse_payload_extracts_cwd_when_present() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/proj/docs/foo.md" },
            "cwd": "/proj"
        }"#;
        let payload = parse_payload(json).unwrap();
        assert_eq!(payload.cwd, Some(std::path::PathBuf::from("/proj")));
    }

    #[test]
    fn parse_payload_cwd_is_none_when_absent() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/proj/docs/foo.md" }
        }"#;
        let payload = parse_payload(json).unwrap();
        assert_eq!(payload.cwd, None);
    }

    use std::path::Path;

    #[test]
    fn is_markdown_accepts_md_lowercase() {
        assert!(is_markdown(Path::new("/p/foo.md")));
    }

    #[test]
    fn is_markdown_rejects_md_uppercase() {
        // Aligned with polaris-core::indexer::discover_markdown_files which uses
        // strict `ext == "md"`. Hook-side acceptance of `.MD` would only cause a
        // wasted round-trip where the indexer discovers zero files.
        assert!(!is_markdown(Path::new("/p/FOO.MD")));
    }

    #[test]
    fn is_markdown_rejects_other_extensions() {
        assert!(!is_markdown(Path::new("/p/foo.rs")));
        assert!(!is_markdown(Path::new("/p/foo.txt")));
        assert!(!is_markdown(Path::new("/p/foo")));
        assert!(!is_markdown(Path::new("/p/.md")));  // no stem — treat as not-a-doc
    }

    #[test]
    fn under_indexed_root_true_when_indexed_sibling_exists() {
        let indexed = vec!["docs/foo.md".to_string(), "docs/sub/bar.md".to_string()];
        // A new file under the same directory tree should match. Both target
        // and indexed are relative here; cwd=None falls back to target-as-rel.
        assert_eq!(
            under_indexed_root(Path::new("docs/sub/new.md"), None, &indexed),
            Some(PathBuf::from("docs/sub/new.md")),
        );
        assert_eq!(
            under_indexed_root(Path::new("docs/new.md"), None, &indexed),
            Some(PathBuf::from("docs/new.md")),
        );
    }

    #[test]
    fn under_indexed_root_false_when_disjoint() {
        let indexed = vec!["docs/foo.md".to_string()];
        assert_eq!(
            under_indexed_root(Path::new("node_modules/pkg/README.md"), None, &indexed),
            None,
        );
        assert_eq!(
            under_indexed_root(Path::new("other/dir/x.md"), None, &indexed),
            None,
        );
    }

    #[test]
    fn under_indexed_root_false_when_no_indexed_paths() {
        assert_eq!(under_indexed_root(Path::new("docs/foo.md"), None, &[]), None);
    }

    #[test]
    fn under_indexed_root_cwd_translates_absolute_target_to_relative_match() {
        // Production scenario: DB has relative paths from `polaris index docs`,
        // hook payload has absolute file_path + cwd = project root.
        let indexed = vec!["docs/foo.md".to_string()];
        let target = Path::new("/proj/docs/new.md");
        let cwd = Path::new("/proj");
        assert_eq!(
            under_indexed_root(target, Some(cwd), &indexed),
            Some(PathBuf::from("docs/new.md")),
            "should return the cwd-relative form so re-index merges with existing row"
        );
    }

    #[test]
    fn under_indexed_root_absolute_db_matches_absolute_target() {
        // DB has absolute paths (e.g., `polaris index /abs/proj`). Use
        // platform-appropriate absolute paths so the test exercises the
        // absolute branch on Windows too (where /proj is not absolute).
        let indexed_str = abs("/proj/docs/foo.md");
        let target_str = abs("/proj/docs/new.md");
        let cwd_str = abs("/proj");
        let indexed = vec![indexed_str];
        let target = Path::new(&target_str);
        let cwd = Path::new(&cwd_str);
        assert_eq!(
            under_indexed_root(target, Some(cwd), &indexed),
            Some(PathBuf::from(&target_str)),
            "should return the absolute form because the indexed row is absolute"
        );
    }

    #[test]
    fn under_indexed_root_matches_single_file_relative_indexed_root() {
        // User ran `polaris index README.md` from /proj. DB row keyed
        // "README.md" (no parent dir). Hook fires for /proj/README.md with
        // cwd=/proj. Should match exactly on filename — without this case,
        // the empty-parent skip caused single-file indexes to silently no-op.
        let indexed = vec!["README.md".to_string()];
        let target = Path::new("/proj/README.md");
        let cwd = Path::new("/proj");
        assert_eq!(
            under_indexed_root(target, Some(cwd), &indexed),
            Some(PathBuf::from("README.md")),
        );
    }

    #[test]
    fn under_indexed_root_single_file_relative_does_not_match_other_file() {
        // Same DB row, but a different file in the same dir must NOT match —
        // single-file roots are filename-exact, not directory-wide.
        let indexed = vec!["README.md".to_string()];
        let target = Path::new("/proj/CHANGELOG.md");
        let cwd = Path::new("/proj");
        assert_eq!(under_indexed_root(target, Some(cwd), &indexed), None);
    }

    #[test]
    fn under_indexed_root_matches_single_file_absolute_indexed_root() {
        let p = abs("/abs/README.md");
        let indexed = vec![p.clone()];
        let target = Path::new(&p);
        assert_eq!(
            under_indexed_root(target, None, &indexed),
            Some(PathBuf::from(&p)),
        );
    }

    #[test]
    fn under_indexed_root_matches_ancestor_of_deeply_nested_indexed_file() {
        // DB only has docs/sub/seed.md. A new file at docs/new.md should still
        // match because docs/ is an ancestor of the indexed path.
        let indexed = vec!["docs/sub/seed.md".to_string()];
        assert_eq!(
            under_indexed_root(Path::new("docs/new.md"), None, &indexed),
            Some(PathBuf::from("docs/new.md")),
        );
    }

    #[test]
    fn under_indexed_root_ancestor_walk_does_not_overmatch() {
        // Ancestor walk must not match unrelated top-level directories.
        let indexed = vec!["docs/sub/deep/seed.md".to_string()];
        assert_eq!(
            under_indexed_root(Path::new("other/new.md"), None, &indexed),
            None,
        );
    }

    #[test]
    fn under_indexed_root_absolute_uses_immediate_parent() {
        // Absolute paths use immediate parent only (we can't infer the
        // original `polaris index <root>` from stored file paths). So
        // /proj/docs/sub/seed.md matches /proj/docs/sub/new.md but NOT
        // /proj/docs/new.md. The relative branch handles the common case
        // (polaris index docs) via ancestor walk.
        let indexed_str = abs("/proj/docs/sub/seed.md");
        let sibling_str = abs("/proj/docs/sub/new.md");
        let higher_str = abs("/proj/docs/new.md");
        let indexed = vec![indexed_str];

        assert_eq!(
            under_indexed_root(Path::new(&sibling_str), None, &indexed),
            Some(PathBuf::from(&sibling_str)),
            "same-dir sibling should match"
        );
        assert_eq!(
            under_indexed_root(Path::new(&higher_str), None, &indexed),
            None,
            "higher-level file should not match (known limitation for absolute paths)"
        );
    }

    #[test]
    fn under_indexed_root_absolute_root_level_file_does_not_wildcard() {
        // DB has /foo.md (file directly under filesystem root). This must NOT
        // match every absolute path — it should behave like single-file.
        let indexed_str = abs("/foo.md");
        let target_str = abs("/bar.md");
        let indexed = vec![indexed_str];
        let target = Path::new(&target_str);
        assert_eq!(
            under_indexed_root(target, None, &indexed),
            None,
            "root-level absolute file must not become a wildcard"
        );
    }

    #[test]
    fn under_indexed_root_returns_none_without_cwd_for_relative_db() {
        // Without cwd we can't translate an absolute target to a relative form.
        // Returning None is correct — silent no-op is the right hook behavior.
        let indexed = vec!["docs/foo.md".to_string()];
        let target = Path::new("/proj/docs/new.md");
        assert_eq!(under_indexed_root(target, None, &indexed), None);
    }

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn run_index_indexes_new_md_under_indexed_root() {
        use polaris_core::config::PolarisConfig;
        use polaris_core::db::{register_vec_extension, Database};
        use polaris_core::embedding::EmbeddingEngine;
        use polaris_core::indexer::Indexer;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let seed = docs.join("seed.md");
        std::fs::write(&seed, "# Seed\nbody\n").unwrap();

        // Seed the index with the docs/ tree so the indexed-root gate hits.
        let cfg = PolarisConfig::default();
        let db_path = dir.path().join("polaris.db");
        register_vec_extension();
        let db = Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap();
        let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id).unwrap());
        let indexer = Indexer::new(
            engine,
            cfg.max_chunk_tokens,
            cfg.chunk_overlap_chars,
            cfg.max_file_size,
        );
        indexer.index_path(&db, &docs, true, false, false, None).unwrap();
        drop(db);

        // Write a new sibling and run the hook action directly.
        let new_file = docs.join("new.md");
        std::fs::write(&new_file, "# New\nfresh content\n").unwrap();

        // DB stores absolute paths here (tempdir is absolute), so cwd=None
        // is fine — the gate's absolute branch will match.
        let report = perform_index(&new_file, None, &cfg_with_db(db_path.clone()))
            .expect("hook should succeed");
        assert!(
            report.indexed_new_or_modified > 0,
            "expected indexing to record at least one new/modified file; report={report:?}"
        );

        // Verify it landed in the DB.
        let db2 = Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap();
        let docs_after = db2.get_all_document_hashes().unwrap();
        assert!(
            docs_after.iter().any(|(p, _)| p.ends_with("new.md")),
            "new.md should be indexed; got {:?}",
            docs_after
        );
    }

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn run_index_silent_noop_when_file_outside_indexed_roots() {
        use polaris_core::config::PolarisConfig;
        use polaris_core::db::{register_vec_extension, Database};
        use polaris_core::embedding::EmbeddingEngine;
        use polaris_core::indexer::Indexer;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let seed = docs.join("seed.md");
        std::fs::write(&seed, "# Seed\nbody\n").unwrap();

        let cfg = PolarisConfig::default();
        let db_path = dir.path().join("polaris.db");
        register_vec_extension();
        let db = Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap();
        let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id).unwrap());
        let indexer = Indexer::new(
            engine,
            cfg.max_chunk_tokens,
            cfg.chunk_overlap_chars,
            cfg.max_file_size,
        );
        indexer.index_path(&db, &docs, true, false, false, None).unwrap();
        drop(db);

        // Write a file under a disjoint directory.
        let vendor_dir = dir.path().join("vendor").join("pkg");
        std::fs::create_dir_all(&vendor_dir).unwrap();
        let vendor_md = vendor_dir.join("README.md");
        std::fs::write(&vendor_md, "# Vendor\n").unwrap();

        let report = perform_index(&vendor_md, None, &cfg_with_db(db_path.clone()))
            .expect("hook should succeed");
        assert_eq!(report.indexed_new_or_modified, 0);

        // Confirm vendor README did NOT enter the DB.
        let db2 = Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap();
        let docs_after = db2.get_all_document_hashes().unwrap();
        assert!(
            !docs_after.iter().any(|(p, _)| p.ends_with("vendor/pkg/README.md")),
            "vendor README should not be indexed; got {:?}",
            docs_after
        );
    }

    #[test]
    fn run_index_with_payload_returns_ok_even_on_invalid_json() {
        // We use a public helper rather than mocking stdin: same logic path.
        let cfg = cfg_with_db(PathBuf::from("/nonexistent/polaris.db"));
        let result = run_index_for_payload("not json {", &cfg);
        assert!(result.is_ok(), "should swallow errors and return Ok");
    }

    #[test]
    fn run_index_with_payload_returns_ok_when_file_missing() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/this/does/not/exist.md" }
        }"#;
        let cfg = cfg_with_db(PathBuf::from("/nonexistent/polaris.db"));
        let result = run_index_for_payload(json, &cfg);
        assert!(result.is_ok(), "should swallow errors and return Ok");
    }

    #[test]
    fn run_index_with_payload_returns_ok_when_file_not_markdown() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/p/foo.rs" }
        }"#;
        let cfg = cfg_with_db(PathBuf::from("/nonexistent/polaris.db"));
        let result = run_index_for_payload(json, &cfg);
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // parse_search_payload tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_search_payload_extracts_prompt_and_cwd() {
        let json = r#"{
            "hook_event_name": "UserPromptSubmit",
            "prompt": "how does the indexer work?",
            "cwd": "/proj"
        }"#;
        let payload = parse_search_payload(json).unwrap();
        assert_eq!(payload.prompt, "how does the indexer work?");
        assert_eq!(payload.cwd, Some(PathBuf::from("/proj")));
    }

    #[test]
    fn parse_search_payload_cwd_optional() {
        let json = r#"{
            "hook_event_name": "UserPromptSubmit",
            "prompt": "what is polaris?"
        }"#;
        let payload = parse_search_payload(json).unwrap();
        assert_eq!(payload.prompt, "what is polaris?");
        assert_eq!(payload.cwd, None);
    }

    #[test]
    fn parse_search_payload_errors_on_missing_prompt() {
        let json = r#"{
            "hook_event_name": "UserPromptSubmit",
            "cwd": "/proj"
        }"#;
        assert!(parse_search_payload(json).is_err());
    }

    #[test]
    fn parse_search_payload_errors_on_invalid_json() {
        assert!(parse_search_payload("not json {").is_err());
    }

    #[test]
    fn parse_search_payload_errors_on_non_object() {
        assert!(parse_search_payload("[1,2]").is_err());
    }
}
