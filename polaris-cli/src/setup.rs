//! `polaris setup` — create/merge .mcp.json and ensure .gitignore entries.

use std::path::Path;

use polaris_core::error::{PolarisError, Result};

/// Lines we ensure are present in `.gitignore`. Order is the order they appear
/// when written to a fresh file.
const GITIGNORE_ENTRIES: &[&str] = &[
    "polaris.db",
    "polaris.db-shm",
    "polaris.db-wal",
    ".fastembed_cache/",
    ".mcp.json",
];

/// Result of computing a `.gitignore` update.
#[derive(Debug, PartialEq, Eq)]
pub struct GitignoreReport {
    /// New file content to write, or `None` if no rewrite is needed.
    pub new_content: Option<String>,
    /// Entries that were added in this run.
    pub added: Vec<&'static str>,
    /// Entries that were already present before this run.
    pub already_present: Vec<&'static str>,
}

/// What kind of mcp.json change happened.
#[derive(Debug, PartialEq, Eq)]
pub enum McpAction {
    /// File didn't exist; we'll create it.
    Created,
    /// File existed; the polaris entry was added or replaced.
    Updated,
    /// File existed and the polaris entry already matched; no rewrite needed.
    Unchanged,
}

/// Result of computing an `.mcp.json` update.
#[derive(Debug, PartialEq, Eq)]
pub struct McpReport {
    /// New file content to write, or `None` if no rewrite is needed.
    pub new_content: Option<String>,
    /// Action taken.
    pub action: McpAction,
}

/// Compute the .mcp.json update for the polaris entry.
///
/// `existing` is the current file content (or `None` if absent). `binary_path`
/// is the absolute path written into `mcpServers.polaris.command`.
pub fn merge_mcp_json(existing: Option<&str>, binary_path: &Path) -> Result<McpReport> {
    use serde_json::{json, Map, Value};

    let polaris_entry = json!({
        "command": binary_path.to_string_lossy(),
        "args": ["serve"],
    });

    let (mut root, action) = match existing {
        None => (Map::new(), McpAction::Created),
        Some(text) => {
            let parsed: Value = serde_json::from_str(text)
                .map_err(|e| PolarisError::Setup(format!("invalid JSON in .mcp.json: {e}")))?;
            let Value::Object(map) = parsed else {
                return Err(PolarisError::Setup(
                    "expected top-level object in .mcp.json".into(),
                ));
            };
            (map, McpAction::Updated)
        }
    };

    // Ensure `mcpServers` is an object.
    let servers_value = root.entry("mcpServers".to_string()).or_insert_with(|| json!({}));
    let Value::Object(servers) = servers_value else {
        return Err(PolarisError::Setup(
            "expected `mcpServers` to be an object".into(),
        ));
    };
    servers.insert("polaris".to_string(), polaris_entry);

    let new_content_str = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|e| PolarisError::Setup(format!("failed to serialize .mcp.json: {e}")))?;

    // Decide whether anything actually changed.
    if let Some(text) = existing {
        // Normalize both sides via parse → serialize so whitespace/key-order
        // differences don't trigger spurious rewrites.
        let normalized_existing = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok());
        if normalized_existing.as_deref() == Some(new_content_str.as_str()) {
            return Ok(McpReport {
                new_content: None,
                action: McpAction::Unchanged,
            });
        }
    }

    Ok(McpReport {
        new_content: Some(new_content_str + "\n"),
        action,
    })
}

/// Compute the .gitignore update for the polaris entries.
///
/// `existing` is the current file content, or `None` if the file is absent.
pub fn ensure_gitignore_entries(existing: Option<&str>) -> GitignoreReport {
    let existing_content = existing.unwrap_or("");

    // Build the set of entries that are already present, treating each non-comment,
    // non-blank line as a single ignore pattern.
    let present: std::collections::HashSet<&str> = existing_content
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.trim_start().starts_with('#'))
        .collect();

    let mut added: Vec<&'static str> = Vec::new();
    let mut already_present: Vec<&'static str> = Vec::new();
    for entry in GITIGNORE_ENTRIES {
        if present.contains(*entry) {
            already_present.push(*entry);
        } else {
            added.push(*entry);
        }
    }

    if added.is_empty() {
        return GitignoreReport {
            new_content: None,
            added,
            already_present,
        };
    }

    // Build the new content. Preserve the existing file verbatim, then append a
    // `# polaris` header and the missing entries.
    let mut out = String::new();
    if !existing_content.is_empty() {
        out.push_str(existing_content);
        if !existing_content.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str("# polaris\n");
    for entry in &added {
        out.push_str(entry);
        out.push('\n');
    }

    GitignoreReport {
        new_content: Some(out),
        added,
        already_present,
    }
}

/// Entry point for the `setup` command.
pub fn run(_path: &Path) -> Result<()> {
    Err(PolarisError::Setup("not yet implemented".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_creates_when_absent() {
        let report = ensure_gitignore_entries(None);
        let content = report.new_content.expect("should write new file");
        assert!(content.contains("# polaris"));
        for entry in GITIGNORE_ENTRIES {
            assert!(content.contains(entry), "missing {entry}");
        }
        assert_eq!(report.added.len(), GITIGNORE_ENTRIES.len());
        assert!(report.already_present.is_empty());
    }

    #[test]
    fn gitignore_appends_only_missing_entries() {
        let existing = "/target\npolaris.db\n.mcp.json\n";
        let report = ensure_gitignore_entries(Some(existing));
        let content = report.new_content.expect("should rewrite");
        // Original lines are preserved in order at the top.
        assert!(content.starts_with("/target\npolaris.db\n.mcp.json\n"));
        // Missing entries are appended under a header.
        assert!(content.contains("# polaris"));
        assert!(content.contains("polaris.db-shm"));
        assert!(content.contains("polaris.db-wal"));
        assert!(content.contains(".fastembed_cache/"));
        // Entries already present are not duplicated.
        assert_eq!(content.matches("polaris.db\n").count(), 1);
        assert_eq!(content.matches(".mcp.json\n").count(), 1);
        assert_eq!(report.added, vec!["polaris.db-shm", "polaris.db-wal", ".fastembed_cache/"]);
        assert_eq!(report.already_present, vec!["polaris.db", ".mcp.json"]);
    }

    #[test]
    fn gitignore_noop_when_all_present() {
        let existing = "polaris.db\npolaris.db-shm\npolaris.db-wal\n.fastembed_cache/\n.mcp.json\n";
        let report = ensure_gitignore_entries(Some(existing));
        assert!(report.new_content.is_none(), "should not rewrite");
        assert!(report.added.is_empty());
        assert_eq!(report.already_present.len(), GITIGNORE_ENTRIES.len());
    }

    #[test]
    fn gitignore_treats_trailing_whitespace_as_match() {
        // A line with a trailing space should still count as present.
        let existing = "polaris.db   \n";
        let report = ensure_gitignore_entries(Some(existing));
        assert!(report.already_present.contains(&"polaris.db"));
    }

    #[test]
    fn gitignore_ignores_commented_lines() {
        // A commented-out reference should NOT count as present.
        let existing = "# polaris.db\n";
        let report = ensure_gitignore_entries(Some(existing));
        assert!(report.added.contains(&"polaris.db"));
    }

    use std::path::PathBuf;

    fn bin() -> PathBuf {
        PathBuf::from("/usr/local/bin/polaris")
    }

    #[test]
    fn mcp_creates_when_absent() {
        let report = merge_mcp_json(None, &bin()).unwrap();
        assert_eq!(report.action, McpAction::Created);
        let content = report.new_content.expect("should write");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["mcpServers"]["polaris"]["command"],
            "/usr/local/bin/polaris"
        );
        assert_eq!(parsed["mcpServers"]["polaris"]["args"], serde_json::json!(["serve"]));
    }

    #[test]
    fn mcp_preserves_other_servers() {
        let existing = r#"{
  "mcpServers": {
    "other": { "command": "/usr/bin/other", "args": [] }
  }
}"#;
        let report = merge_mcp_json(Some(existing), &bin()).unwrap();
        assert_eq!(report.action, McpAction::Updated);
        let content = report.new_content.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["other"]["command"], "/usr/bin/other");
        assert_eq!(
            parsed["mcpServers"]["polaris"]["command"],
            "/usr/local/bin/polaris"
        );
    }

    #[test]
    fn mcp_replaces_stale_polaris_entry() {
        let existing = r#"{
  "mcpServers": {
    "polaris": {
      "command": "/old/path/polaris",
      "args": ["serve"]
    }
  }
}"#;
        let report = merge_mcp_json(Some(existing), &bin()).unwrap();
        assert_eq!(report.action, McpAction::Updated);
        let parsed: serde_json::Value =
            serde_json::from_str(&report.new_content.unwrap()).unwrap();
        assert_eq!(
            parsed["mcpServers"]["polaris"]["command"],
            "/usr/local/bin/polaris"
        );
    }

    #[test]
    fn mcp_unchanged_when_already_current() {
        // First call to seed the canonical content, then re-run the merge on it.
        let first = merge_mcp_json(None, &bin()).unwrap().new_content.unwrap();
        let report = merge_mcp_json(Some(&first), &bin()).unwrap();
        assert_eq!(report.action, McpAction::Unchanged);
        assert!(report.new_content.is_none());
    }

    #[test]
    fn mcp_errors_on_invalid_json() {
        let result = merge_mcp_json(Some("not json {"), &bin());
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn mcp_errors_when_top_level_is_not_object() {
        let result = merge_mcp_json(Some("[1, 2, 3]"), &bin());
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }
}
