//! `polaris hook` — internal subcommands invoked by Claude Code hooks.
//!
//! Each subcommand reads its hook payload as JSON on stdin and applies its
//! action. All paths exit 0 unconditionally; failures are reported to stderr
//! so a transient hiccup never interrupts the user's session via a Claude Code
//! warning banner.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use polaris_core::config::PolarisConfig;
use polaris_core::db::{register_vec_extension, Database};
use polaris_core::embedding::EmbeddingEngine;
use polaris_core::error::{PolarisError, Result};
use polaris_core::indexer::Indexer;

/// The slice of a Claude Code hook payload we actually use.
#[derive(Debug)]
pub struct HookPayload {
    pub file_path: PathBuf,
}

/// Parse a Claude Code hook payload (stdin JSON) into the fields we care about.
///
/// `Write`, `Edit`, and `MultiEdit` all set `tool_input.file_path` to the
/// target. Anything else is treated as a parse error so the caller can decide
/// (we currently exit 0 silently — see `run_index`).
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
    Ok(HookPayload {
        file_path: PathBuf::from(file_path),
    })
}

/// Returns true if the path looks like a markdown file we should consider
/// indexing. Case-insensitive on the extension; rejects extension-only names.
pub fn is_markdown(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    if !ext.eq_ignore_ascii_case("md") {
        return false;
    }
    // Require a non-empty stem so files literally named `.md` are rejected.
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// Returns true if `target` lives under the parent directory of at least one
/// previously-indexed document. Checks whether any indexed path's parent is an
/// ancestor of (or equal to the parent of) `target`.
pub fn under_indexed_root(target: &Path, indexed_paths: &[String]) -> bool {
    for p in indexed_paths {
        if let Some(parent) = Path::new(p).parent() {
            if !parent.as_os_str().is_empty() && target.starts_with(parent) {
                return true;
            }
        }
    }
    false
}

/// Outcome of one `perform_index` call. Used in tests; production code only
/// cares that the call returned `Ok`.
#[derive(Debug)]
pub struct HookIndexReport {
    /// Number of files added or modified by this index call.
    pub indexed_new_or_modified: usize,
}

/// Apply the gates and (if eligible) run a single-file index pass. Pure
/// failures (DB locked, etc.) bubble up as `Err`; the caller decides whether
/// to surface them — `run_index` swallows them into stderr.
pub fn perform_index(file_path: &Path, db_path: &Path) -> Result<HookIndexReport> {
    if !is_markdown(file_path) {
        return Ok(HookIndexReport { indexed_new_or_modified: 0 });
    }

    let cfg = PolarisConfig::default();
    register_vec_extension();
    let db = Database::open(db_path, cfg.embedding_dim, &cfg.model_id)?;

    let indexed = db.get_all_document_hashes()?;
    let indexed_paths: Vec<String> = indexed.into_iter().map(|(p, _)| p).collect();
    if !under_indexed_root(file_path, &indexed_paths) {
        return Ok(HookIndexReport { indexed_new_or_modified: 0 });
    }

    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);
    let indexer = Indexer::new(
        engine,
        cfg.max_chunk_tokens,
        cfg.chunk_overlap_chars,
        cfg.max_file_size,
    );
    let report = indexer.index_path(&db, file_path, false, false, false, None)?;
    Ok(HookIndexReport {
        indexed_new_or_modified: report.added.len() + report.modified.len(),
    })
}

/// Entry point for `polaris hook index` — re-index a single file the agent
/// just edited.
pub fn run_index() -> Result<()> {
    // Implementation lands in Task 8–11.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    use std::path::Path;

    #[test]
    fn is_markdown_accepts_md_lowercase() {
        assert!(is_markdown(Path::new("/p/foo.md")));
    }

    #[test]
    fn is_markdown_accepts_md_uppercase() {
        assert!(is_markdown(Path::new("/p/FOO.MD")));
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
        // A new file under the same directory tree should match.
        assert!(under_indexed_root(Path::new("docs/sub/new.md"), &indexed));
        assert!(under_indexed_root(Path::new("docs/new.md"), &indexed));
    }

    #[test]
    fn under_indexed_root_false_when_disjoint() {
        let indexed = vec!["docs/foo.md".to_string()];
        assert!(!under_indexed_root(Path::new("node_modules/pkg/README.md"), &indexed));
        assert!(!under_indexed_root(Path::new("other/dir/x.md"), &indexed));
    }

    #[test]
    fn under_indexed_root_false_when_no_indexed_paths() {
        assert!(!under_indexed_root(Path::new("docs/foo.md"), &[]));
    }

    #[test]
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

        let report = perform_index(&new_file, &db_path).expect("hook should succeed");
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

        let report = perform_index(&vendor_md, &db_path).expect("hook should succeed");
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
}
