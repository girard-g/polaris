//! `polaris hook` — internal subcommands invoked by Claude Code hooks.
//!
//! Each subcommand reads its hook payload as JSON on stdin and applies its
//! action. All paths exit 0 unconditionally; failures are reported to stderr
//! so a transient hiccup never interrupts the user's session via a Claude Code
//! warning banner.

use std::path::PathBuf;

use polaris_core::error::{PolarisError, Result};

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
pub fn is_markdown(path: &std::path::Path) -> bool {
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
}
