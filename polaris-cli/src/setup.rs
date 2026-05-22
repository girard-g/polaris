//! `polaris setup` — create/merge .mcp.json and ensure .gitignore entries.

use std::path::Path;

use polaris_core::config::PolarisConfig;
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

/// Filenames in the project root that receive the Polaris instruction block,
/// in the order `setup` processes them.
const AGENT_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md", "GEMINI.md"];

/// The canonical Polaris MCP instruction block, including its marker pair.
/// The block ends with a single trailing newline so callers can append it
/// directly without further normalisation.
const POLARIS_BLOCK: &str = "\
<!-- polaris:begin -->
## Polaris MCP

This project ships a Polaris MCP server (`polaris serve`) that semantic-searches the docs in this repo. Prefer the `polaris.search` tool over grep/read for any question about the project's documentation, behaviour, or architecture — it returns ranked, section-aware chunks and is typically 10-40× cheaper in tokens. Start with top_k=2; raise only if recall is poor. Use `polaris.index` to add or refresh files, `polaris.status` to check index health.
<!-- polaris:end -->
";

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

/// What kind of agent-instruction-file change happened.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentAction {
    /// File didn't exist; we'll create it.
    Created,
    /// File existed; the polaris block was added or replaced.
    Updated,
    /// File existed and the polaris block already matched; no rewrite needed.
    Unchanged,
}

/// Result of computing an agent-instruction-file update.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentReport {
    /// New file content to write, or `None` if no rewrite is needed.
    pub new_content: Option<String>,
    /// Action taken.
    pub action: AgentAction,
}

/// What kind of `.claude/settings.json` change happened.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaudeSettingsAction {
    /// File didn't exist; we'll create it.
    Created,
    /// File existed; the polaris hook entry was added or replaced.
    Updated,
    /// File existed and the polaris hook entry already matched; no rewrite needed.
    Unchanged,
}

/// Result of computing a `.claude/settings.json` update.
#[derive(Debug, PartialEq, Eq)]
pub struct ClaudeSettingsReport {
    /// New file content to write, or `None` if no rewrite is needed.
    pub new_content: Option<String>,
    /// Action taken.
    pub action: ClaudeSettingsAction,
}

/// Matcher string for the `PostToolUse` hook block polaris installs for
/// auto-indexing.
const POLARIS_POST_TOOL_USE_MATCHER: &str = "Write|Edit|MultiEdit";

/// Compute the `.claude/settings.json` update for the polaris hook entries.
///
/// `existing` is the current file content (or `None` if absent). `binary_path`
/// is the absolute path to the polaris binary; the canonical hook command is
/// `<binary_path> hook index`.
///
/// Strategy: drop every polaris-owned hook entry from every matcher block under
/// every event (currently just `PostToolUse`; we walk the whole `hooks.*` map
/// to be Phase 2-ready), prune any matcher block whose `hooks[]` becomes empty,
/// then append our canonical matcher block to `hooks.PostToolUse`.
pub fn merge_claude_settings(
    existing: Option<&str>,
    binary_path: &Path,
) -> Result<ClaudeSettingsReport> {
    use serde_json::{json, Map, Value};

    // Quote the binary path so spaces in the installation path (common on
    // Windows: `C:\Program Files\...`) survive Claude Code's shell parsing.
    // `is_polaris_owned` uses the matching `shell_words::split` to recover
    // the original tokens.
    let bin_str = binary_path.to_string_lossy();
    let polaris_command = format!("{} hook index", shell_words::quote(&bin_str));
    let canonical_block = json!({
        "matcher": POLARIS_POST_TOOL_USE_MATCHER,
        "hooks": [
            { "type": "command", "command": polaris_command }
        ]
    });

    let (mut root, action) = match existing {
        None => (Map::new(), ClaudeSettingsAction::Created),
        Some(text) => {
            let parsed: Value = serde_json::from_str(text).map_err(|e| {
                PolarisError::Setup(format!("invalid JSON in .claude/settings.json: {e}"))
            })?;
            let Value::Object(map) = parsed else {
                return Err(PolarisError::Setup(
                    "expected top-level object in .claude/settings.json".into(),
                ));
            };
            (map, ClaudeSettingsAction::Updated)
        }
    };

    // Ensure `hooks` is an object.
    let hooks_value = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let Value::Object(hooks_map) = hooks_value else {
        return Err(PolarisError::Setup(
            "expected `hooks` to be an object in .claude/settings.json".into(),
        ));
    };

    // Drop stale polaris-owned entries across every event. Prune empty matcher
    // blocks.
    for event_value in hooks_map.values_mut() {
        let Value::Array(blocks) = event_value else {
            continue;
        };
        for block in blocks.iter_mut() {
            let Value::Object(block_obj) = block else {
                continue;
            };
            let Some(Value::Array(hook_entries)) = block_obj.get_mut("hooks") else {
                continue;
            };
            hook_entries.retain(|entry| !is_polaris_owned(entry));
        }
        // Prune any block whose hooks array is now empty.
        blocks.retain(|block| {
            let Value::Object(block_obj) = block else {
                return true;
            };
            match block_obj.get("hooks") {
                Some(Value::Array(arr)) => !arr.is_empty(),
                _ => true,
            }
        });
    }

    // Append our canonical matcher block to PostToolUse.
    let post_tool_use = hooks_map
        .entry("PostToolUse".to_string())
        .or_insert_with(|| json!([]));
    let Value::Array(blocks) = post_tool_use else {
        return Err(PolarisError::Setup(
            "expected `hooks.PostToolUse` to be an array in .claude/settings.json".into(),
        ));
    };
    blocks.push(canonical_block);

    let new_content_str = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|e| PolarisError::Setup(format!("failed to serialize .claude/settings.json: {e}")))?;

    if let Some(text) = existing {
        let normalized_existing = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok());
        if normalized_existing.as_deref() == Some(new_content_str.as_str()) {
            return Ok(ClaudeSettingsReport {
                new_content: None,
                action: ClaudeSettingsAction::Unchanged,
            });
        }
    }

    Ok(ClaudeSettingsReport {
        new_content: Some(new_content_str + "\n"),
        action,
    })
}

/// Strip every polaris-owned hook entry from a `.claude/settings.json` file,
/// pruning matcher blocks whose `hooks[]` becomes empty.
///
/// Returns `Some(new_content)` if the file changed, `None` if nothing
/// polaris-owned was present (no rewrite needed).
pub fn remove_polaris_hooks_from_settings(existing: &str) -> Result<Option<String>> {
    use serde_json::Value;

    let parsed: Value = serde_json::from_str(existing).map_err(|e| {
        PolarisError::Setup(format!("invalid JSON in .claude/settings.json: {e}"))
    })?;
    let Value::Object(mut root) = parsed else {
        return Err(PolarisError::Setup(
            "expected top-level object in .claude/settings.json".into(),
        ));
    };

    // If `hooks` is missing or not an object, polaris cannot own anything in
    // it; treat as no-op. `merge_claude_settings` errors on the same input
    // (because it must install something); the uninstall path is best-effort.
    let Some(Value::Object(hooks_map)) = root.get_mut("hooks") else {
        return Ok(None);
    };

    let mut removed_any = false;
    for event_value in hooks_map.values_mut() {
        let Value::Array(blocks) = event_value else {
            continue;
        };
        for block in blocks.iter_mut() {
            let Value::Object(block_obj) = block else {
                continue;
            };
            let Some(Value::Array(hook_entries)) = block_obj.get_mut("hooks") else {
                continue;
            };
            let before = hook_entries.len();
            hook_entries.retain(|entry| !is_polaris_owned(entry));
            if hook_entries.len() != before {
                removed_any = true;
            }
        }
        blocks.retain(|block| {
            let Value::Object(block_obj) = block else {
                return true;
            };
            match block_obj.get("hooks") {
                Some(Value::Array(arr)) => !arr.is_empty(),
                _ => true,
            }
        });
    }

    if !removed_any {
        return Ok(None);
    }

    let new_content_str = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|e| PolarisError::Setup(format!("failed to serialize .claude/settings.json: {e}")))?;
    Ok(Some(new_content_str + "\n"))
}

/// Returns true if the given hook entry's `command` is a polaris-owned
/// `polaris hook ...` invocation. Two requirements:
///   1. The first shell-parsed token's basename is `polaris` or
///      `polaris.exe` (Windows binaries carry the `.exe` suffix).
///   2. The second token is `hook` — the entire `polaris hook` subcommand
///      surface is internal to polaris, so any `polaris hook <subcmd>` is
///      ours (current `index`, future `search` for Phase 2, etc.). Other
///      polaris invocations (e.g. `polaris status` or `polaris search`)
///      are user-owned and stay untouched.
///
/// Uses `shell_words::split` to honor the same quoting we apply in
/// `merge_claude_settings`. A command string we can't shell-parse is treated
/// as not-ours (conservative: leave the user's odd entry alone).
fn is_polaris_owned(entry: &serde_json::Value) -> bool {
    let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    let Ok(tokens) = shell_words::split(cmd) else {
        return false;
    };
    let mut iter = tokens.into_iter();
    let Some(first) = iter.next() else {
        return false;
    };
    let basename_matches = std::path::Path::new(&first)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s == "polaris" || s == "polaris.exe")
        .unwrap_or(false);
    if !basename_matches {
        return false;
    }
    iter.next().as_deref() == Some("hook")
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

/// Compute the agent-instruction-file update for one of `CLAUDE.md` /
/// `AGENTS.md` / `GEMINI.md`.
///
/// `existing` is the current file content, or `None` if the file is absent.
///
/// Marker matching is substring-based on the literal strings
/// `<!-- polaris:begin -->` and `<!-- polaris:end -->` (case-sensitive).
/// On a malformed marker layout the function returns `Err(PolarisError::Setup(_))`
/// — the caller is expected to abort the run and let the user fix the file.
pub fn merge_agent_instructions(existing: Option<&str>) -> Result<AgentReport> {
    const BEGIN: &str = "<!-- polaris:begin -->";
    const END: &str = "<!-- polaris:end -->";

    let Some(text) = existing else {
        return Ok(AgentReport {
            new_content: Some(POLARIS_BLOCK.to_string()),
            action: AgentAction::Created,
        });
    };

    let begin_count = text.matches(BEGIN).count();
    let end_count = text.matches(END).count();

    match (begin_count, end_count) {
        (0, 0) => {
            // No markers — append the block after exactly one blank line.
            let mut new_content = String::with_capacity(text.len() + POLARIS_BLOCK.len() + 2);
            new_content.push_str(text);
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push('\n');
            new_content.push_str(POLARIS_BLOCK);
            Ok(AgentReport {
                new_content: Some(new_content),
                action: AgentAction::Updated,
            })
        }
        (1, 1) => {
            let begin = text.find(BEGIN).expect("count == 1");
            let end = text.find(END).expect("count == 1");
            if end < begin {
                return Err(PolarisError::Setup(
                    "polaris:end appears before polaris:begin; refusing to auto-repair".into(),
                ));
            }
            // Replace [begin .. after-end-marker) with POLARIS_BLOCK. Consume one
            // trailing newline if present so the replacement's own trailing
            // newline doesn't double up — keeps the round-trip Unchanged-stable.
            let mut block_end = end + END.len();
            if text[block_end..].starts_with('\n') {
                block_end += 1;
            }
            let mut new_content = String::with_capacity(text.len() + POLARIS_BLOCK.len());
            new_content.push_str(&text[..begin]);
            new_content.push_str(POLARIS_BLOCK);
            new_content.push_str(&text[block_end..]);
            if new_content == text {
                return Ok(AgentReport {
                    new_content: None,
                    action: AgentAction::Unchanged,
                });
            }
            Ok(AgentReport {
                new_content: Some(new_content),
                action: AgentAction::Updated,
            })
        }
        (b, _) if b > 1 => Err(PolarisError::Setup(format!(
            "found {b} polaris:begin markers; refusing to auto-repair"
        ))),
        (_, e) if e > 1 => Err(PolarisError::Setup(format!(
            "found {e} polaris:end markers; refusing to auto-repair"
        ))),
        (1, 0) => Err(PolarisError::Setup(
            "polaris:begin marker is unclosed; refusing to auto-repair".into(),
        )),
        (0, 1) => Err(PolarisError::Setup(
            "polaris:end marker has no matching polaris:begin; refusing to auto-repair".into(),
        )),
        _ => unreachable!("all (begin_count, end_count) cases covered above"),
    }
}

/// Entry point for the `setup` command.
///
/// `cfg` is the `PolarisConfig` `main.rs` already loaded (respecting the
/// user's `polaris.toml` + CLI overrides). The initial-index step uses
/// `cfg.embedding_dim`/`cfg.model_id`/`cfg.db_path` so a re-run of
/// `polaris setup` after the user customized their config indexes into the
/// same DB and with the same embedding parameters as `polaris index docs`.
pub fn run(cfg: &PolarisConfig, path: &Path, no_agents: bool, no_hooks: bool) -> Result<()> {
    use console::style;

    if !path.exists() {
        return Err(PolarisError::Setup(format!(
            "path not found: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(PolarisError::Setup(format!(
            "path is not a directory: {}",
            path.display()
        )));
    }

    let binary_path = std::env::current_exe().map_err(|e| {
        PolarisError::Setup(format!("could not resolve polaris binary path: {e}"))
    })?;

    println!();
    println!(
        "{}  {}",
        style("polaris").cyan().bold(),
        style("· setup").dim(),
    );
    println!();

    // .mcp.json
    let mcp_path = path.join(".mcp.json");
    let existing_mcp = read_optional(&mcp_path)?;
    let mcp_report = merge_mcp_json(existing_mcp.as_deref(), &binary_path)?;
    match (&mcp_report.new_content, &mcp_report.action) {
        (Some(content), McpAction::Created) => {
            std::fs::write(&mcp_path, content)?;
            println!(
                "  {}  Created .mcp.json (polaris → {})",
                style("✓").green(),
                binary_path.display(),
            );
        }
        (Some(content), McpAction::Updated) => {
            std::fs::write(&mcp_path, content)?;
            println!(
                "  {}  Updated .mcp.json (polaris → {})",
                style("✓").green(),
                binary_path.display(),
            );
        }
        // `Unchanged` is only produced with `new_content: None`, but the
        // compiler can't prove that — fold the impossible (Some, Unchanged)
        // case into the no-op arm rather than panicking with unreachable!().
        (None, _) | (Some(_), McpAction::Unchanged) => {
            println!("  {}  .mcp.json already configured", style("✓").green());
        }
    }

    // .gitignore
    let gitignore_path = path.join(".gitignore");
    let existing_gitignore = read_optional(&gitignore_path)?;
    let gi_report = ensure_gitignore_entries(existing_gitignore.as_deref());
    match gi_report.new_content {
        Some(ref content) if existing_gitignore.is_none() => {
            std::fs::write(&gitignore_path, content)?;
            println!(
                "  {}  Created .gitignore ({} entries)",
                style("✓").green(),
                gi_report.added.len(),
            );
        }
        Some(ref content) => {
            std::fs::write(&gitignore_path, content)?;
            println!(
                "  {}  Updated .gitignore (added {}, {} already present)",
                style("✓").green(),
                gi_report.added.len(),
                gi_report.already_present.len(),
            );
        }
        None => {
            println!(
                "  {}  .gitignore already up to date",
                style("✓").green(),
            );
        }
    }

    // Agent instruction files
    if !no_agents {
        for filename in AGENT_FILES {
            let agent_path = path.join(filename);
            if agent_path.exists() && !agent_path.is_file() {
                return Err(PolarisError::Setup(format!(
                    "{} is not a regular file",
                    agent_path.display()
                )));
            }
            let existing = read_optional(&agent_path)?;
            let report = merge_agent_instructions(existing.as_deref()).map_err(|e| {
                if let PolarisError::Setup(msg) = e {
                    PolarisError::Setup(format!("{filename}: {msg}"))
                } else {
                    e
                }
            })?;
            match (&report.new_content, &report.action) {
                (Some(content), AgentAction::Created) => {
                    std::fs::write(&agent_path, content)?;
                    println!(
                        "  {}  Created {filename} (polaris block)",
                        style("✓").green(),
                    );
                }
                (Some(content), AgentAction::Updated) => {
                    std::fs::write(&agent_path, content)?;
                    println!(
                        "  {}  Updated {filename} (polaris block refreshed)",
                        style("✓").green(),
                    );
                }
                (None, _) | (Some(_), AgentAction::Unchanged) => {
                    println!("  {}  {filename} already configured", style("✓").green());
                }
            }
        }
    }

    // .claude/settings.json — install the auto-index hook unless --no-hooks
    if !no_hooks {
        let claude_dir = path.join(".claude");
        if claude_dir.exists() && !claude_dir.is_dir() {
            return Err(PolarisError::Setup(format!(
                "{} exists but is not a directory",
                claude_dir.display()
            )));
        }
        if !claude_dir.exists() {
            std::fs::create_dir_all(&claude_dir)?;
        }
        let settings_path = claude_dir.join("settings.json");
        let existing_settings = read_optional(&settings_path)?;
        let report = merge_claude_settings(existing_settings.as_deref(), &binary_path)?;
        match (&report.new_content, &report.action) {
            (Some(content), ClaudeSettingsAction::Created) => {
                std::fs::write(&settings_path, content)?;
                println!(
                    "  {}  Created .claude/settings.json (auto-index hook)",
                    style("✓").green(),
                );
            }
            (Some(content), ClaudeSettingsAction::Updated) => {
                std::fs::write(&settings_path, content)?;
                println!(
                    "  {}  Updated .claude/settings.json (auto-index hook)",
                    style("✓").green(),
                );
            }
            // `Unchanged` is only produced with `new_content: None`, but the
            // compiler can't prove that — fold the impossible (Some, Unchanged)
            // case into the no-op arm rather than panicking with unreachable!().
            (None, _) | (Some(_), ClaudeSettingsAction::Unchanged) => {
                println!(
                    "  {}  .claude/settings.json already configured",
                    style("✓").green(),
                );
            }
        }
        run_initial_index(cfg, path)?;
    } else {
        // --no-hooks: remove any polaris-owned hook entries if the file exists.
        // Mirror the install branch's guard: refuse to operate on a `.claude`
        // that exists but isn't a directory, rather than crashing with an
        // opaque NotADirectory error from `read_optional`.
        let claude_dir = path.join(".claude");
        if claude_dir.exists() && !claude_dir.is_dir() {
            return Err(PolarisError::Setup(format!(
                "{} exists but is not a directory",
                claude_dir.display()
            )));
        }
        let settings_path = claude_dir.join("settings.json");
        if let Some(existing) = read_optional(&settings_path)? {
            match remove_polaris_hooks_from_settings(&existing)? {
                Some(new_content) => {
                    std::fs::write(&settings_path, new_content)?;
                    println!(
                        "  {}  Removed polaris hook from .claude/settings.json",
                        style("✓").green(),
                    );
                }
                None => {
                    println!(
                        "  {}  .claude/settings.json has no polaris hook to remove",
                        style("✓").green(),
                    );
                }
            }
        }
    }

    println!();
    Ok(())
}

/// Run an initial index on the project's `./docs/` directory so the auto-index
/// hook has an established root to reconcile against. Non-fatal: failures print
/// to stderr and return Ok(()) so setup completes.
///
/// If `./docs/` doesn't exist we explicitly do NOT fall back to indexing the
/// whole setup path — the core indexer uses a plain `WalkDir` with no
/// gitignore/`node_modules`/`.git` awareness, so a fallback would freeze
/// `polaris setup` for minutes on typical fullstack repos. Instead we print
/// a hint and let the user run `polaris index <path>` against the directory
/// they actually want indexed.
///
/// We deliberately do NOT canonicalize the target path. The CLI flow
/// (`polaris index docs` invoked from inside the project) stores paths as
/// `docs/foo.md`. Canonicalizing here would create absolute rows that
/// DUPLICATE existing relative rows for the same files. The hook side
/// translates Claude Code's absolute payloads to whichever form matches
/// the DB (see `hook.rs`).
///
/// To produce that same `docs/foo.md` form regardless of which directory
/// the user invoked `polaris setup` from (e.g. `polaris setup ./my-proj`
/// from a parent dir), we switch the process CWD to `setup_path` for the
/// indexing call and pass the indexer a plain `docs` relative path. Without
/// this, `polaris setup ./my-proj` would store `my-proj/docs/foo.md` and
/// the hook's cwd-relative gate (looking for `docs/foo.md`) would silently
/// no-op. The RAII guard restores the prior CWD on return.
fn run_initial_index(cfg: &PolarisConfig, setup_path: &Path) -> Result<()> {
    use polaris_core::db::Database;
    use polaris_core::embedding::EmbeddingEngine;
    use polaris_core::indexer::Indexer;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    let docs_dir = setup_path.join("docs");
    if !docs_dir.is_dir() {
        println!(
            "  {}  No ./docs/ directory; skipping initial index",
            console::style("ℹ").cyan(),
        );
        eprintln!(
            "      Run `polaris index <path>` to establish an indexed root for the auto-index hook."
        );
        return Ok(());
    }

    let Some(prev_cwd) = std::env::current_dir().ok() else {
        eprintln!(
            "  {}  initial index: could not capture current dir; skipping",
            console::style("⚠").yellow(),
        );
        return Ok(());
    };
    if let Err(e) = std::env::set_current_dir(setup_path) {
        eprintln!(
            "  {}  initial index: could not enter {}: {e}",
            console::style("⚠").yellow(),
            setup_path.display(),
        );
        return Ok(());
    }
    let _cwd_guard = CwdGuard(prev_cwd);

    let target = Path::new("docs");
    // `register_vec_extension` is called by `main.rs::run` before dispatching,
    // so we don't re-register here. Use the passed-in cfg directly so the
    // user's polaris.toml (db_path, embedding_dim, model_id) is respected.

    let attempt = || -> Result<()> {
        let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
        let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);
        let indexer = Indexer::new(
            engine,
            cfg.max_chunk_tokens,
            cfg.chunk_overlap_chars,
            cfg.max_file_size,
        );
        let recursive = true;
        let force = false;
        let dry_run = false;
        let _report = indexer.index_path(&db, &target, recursive, force, dry_run, None)?;
        Ok(())
    };

    match attempt() {
        Ok(()) => {
            println!(
                "  {}  Indexed {}",
                console::style("✓").green(),
                target.display(),
            );
        }
        Err(e) => {
            eprintln!(
                "  {}  initial index failed: {e}",
                console::style("⚠").yellow(),
            );
            eprintln!("      You can retry with: polaris index {}", target.display());
        }
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PolarisError::Io(e)),
    }
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

    #[test]
    fn agent_block_creates_when_absent() {
        let report = merge_agent_instructions(None).unwrap();
        assert_eq!(report.action, AgentAction::Created);
        let content = report.new_content.expect("should write new file");
        assert_eq!(content, POLARIS_BLOCK);
    }

    #[test]
    fn agent_block_appends_when_no_marker() {
        let existing = "# Project rules\n\nWe use Rust 2024 edition.\n";
        let report = merge_agent_instructions(Some(existing)).unwrap();
        assert_eq!(report.action, AgentAction::Updated);
        let content = report.new_content.expect("should rewrite");
        // Original content preserved verbatim at the top.
        assert!(content.starts_with(existing));
        // Exactly one blank line between original content and the block.
        let suffix = &content[existing.len()..];
        assert_eq!(suffix, format!("\n{POLARIS_BLOCK}"));
    }

    #[test]
    fn agent_block_appends_when_no_marker_and_no_trailing_newline() {
        let existing = "# Rules"; // no trailing newline
        let report = merge_agent_instructions(Some(existing)).unwrap();
        let content = report.new_content.expect("should rewrite");
        // Normalised to end in newline before the blank-line separator.
        assert_eq!(content, format!("# Rules\n\n{POLARIS_BLOCK}"));
    }

    #[test]
    fn agent_block_replaces_stale_marker() {
        let existing = "\
# Project rules

<!-- polaris:begin -->
old stale instructions
<!-- polaris:end -->

## More rules
end of file
";
        let report = merge_agent_instructions(Some(existing)).unwrap();
        assert_eq!(report.action, AgentAction::Updated);
        let content = report.new_content.expect("should rewrite");
        // Content above the markers is preserved.
        assert!(content.starts_with("# Project rules\n\n"));
        // Canonical block replaces the marker range.
        assert!(content.contains(POLARIS_BLOCK.trim_end_matches('\n')));
        // Content below the markers is preserved.
        assert!(content.ends_with("## More rules\nend of file\n"));
        // Stale text is gone.
        assert!(!content.contains("old stale instructions"));
    }

    #[test]
    fn agent_block_unchanged_when_current() {
        let existing = format!("# Header\n\n{POLARIS_BLOCK}");
        let report = merge_agent_instructions(Some(&existing)).unwrap();
        assert_eq!(report.action, AgentAction::Unchanged);
        assert!(report.new_content.is_none());
    }

    #[test]
    fn agent_block_errors_on_two_begin_markers() {
        let existing = "\
<!-- polaris:begin -->
first
<!-- polaris:end -->

<!-- polaris:begin -->
second
<!-- polaris:end -->
";
        let result = merge_agent_instructions(Some(existing));
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn agent_block_errors_on_orphan_end_marker() {
        let existing = "trailing junk\n<!-- polaris:end -->\n";
        let result = merge_agent_instructions(Some(existing));
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn agent_block_errors_on_unclosed_marker() {
        let existing = "<!-- polaris:begin -->\noops no end\n";
        let result = merge_agent_instructions(Some(existing));
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn agent_block_errors_when_end_appears_before_begin() {
        let existing = "<!-- polaris:end -->\nstuff\n<!-- polaris:begin -->\n";
        let result = merge_agent_instructions(Some(existing));
        assert!(matches!(result, Err(PolarisError::Setup(_))));
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

    use tempfile::TempDir;

    #[test]
    fn run_creates_files_in_empty_dir() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        let mcp = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&mcp).unwrap();
        assert!(parsed["mcpServers"]["polaris"]["command"].is_string());

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        for entry in GITIGNORE_ENTRIES {
            assert!(gitignore.contains(entry), "missing {entry} in gitignore");
        }
    }

    #[test]
    fn run_is_idempotent() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();
        let mcp_first = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let gi_first = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();

        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();
        let mcp_second = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let gi_second = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();

        assert_eq!(mcp_first, mcp_second, ".mcp.json should be unchanged on rerun");
        assert_eq!(gi_first, gi_second, ".gitignore should be unchanged on rerun");
    }

    #[test]
    fn run_errors_when_path_missing() {
        let result = run(&PolarisConfig::default(), Path::new("/this/path/should/not/exist/polaris-test-zzz"), false, true);
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn run_errors_when_path_is_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let result = run(&PolarisConfig::default(), &file, false, true);
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn run_writes_all_three_agent_files() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        for filename in AGENT_FILES {
            let path = dir.path().join(filename);
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("expected {filename} to exist"));
            assert!(
                content.contains("<!-- polaris:begin -->"),
                "{filename} missing polaris:begin"
            );
            assert!(
                content.contains("<!-- polaris:end -->"),
                "{filename} missing polaris:end"
            );
            assert!(
                content.contains("## Polaris MCP"),
                "{filename} missing block header"
            );
        }
    }

    #[test]
    fn run_preserves_existing_user_content_in_agent_files() {
        let dir = TempDir::new().unwrap();
        let existing_user_rules = "# My project rules\n\nUse Rust 2024 edition.\nNo unsafe blocks.\n";
        std::fs::write(dir.path().join("CLAUDE.md"), existing_user_rules).unwrap();

        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        // Original content preserved at the top.
        assert!(content.starts_with(existing_user_rules));
        // Polaris block appended after a blank line.
        assert!(content.contains("\n\n<!-- polaris:begin -->"));
    }

    #[test]
    fn run_skips_agent_files_with_no_agents() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), true, true).unwrap();

        for filename in AGENT_FILES {
            let path = dir.path().join(filename);
            assert!(
                !path.exists(),
                "{filename} should not exist when --no-agents is set"
            );
        }
        // .mcp.json and .gitignore should still be written.
        assert!(dir.path().join(".mcp.json").exists());
        assert!(dir.path().join(".gitignore").exists());
    }

    #[test]
    fn run_is_idempotent_with_agent_files() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        let mut first: Vec<(String, String)> = Vec::new();
        for filename in AGENT_FILES {
            first.push((
                (*filename).to_string(),
                std::fs::read_to_string(dir.path().join(filename)).unwrap(),
            ));
        }

        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        for (filename, before) in &first {
            let after = std::fs::read_to_string(dir.path().join(filename)).unwrap();
            assert_eq!(*before, after, "{filename} should be unchanged on rerun");
        }
    }

    #[test]
    fn claude_settings_creates_when_absent() {
        let report = merge_claude_settings(None, &bin()).unwrap();
        assert_eq!(report.action, ClaudeSettingsAction::Created);
        let content = report.new_content.expect("should write");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let arr = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["matcher"], "Write|Edit|MultiEdit");
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.starts_with("/usr/local/bin/polaris"));
        assert!(cmd.ends_with("hook index"));
    }

    #[test]
    fn claude_settings_preserves_unrelated_top_level_keys() {
        let existing = r#"{
  "permissions": { "allow": ["Bash"] },
  "env": { "FOO": "bar" }
}"#;
        let report = merge_claude_settings(Some(existing), &bin()).unwrap();
        assert_eq!(report.action, ClaudeSettingsAction::Updated);
        let parsed: serde_json::Value =
            serde_json::from_str(&report.new_content.unwrap()).unwrap();
        assert_eq!(parsed["permissions"]["allow"][0], "Bash");
        assert_eq!(parsed["env"]["FOO"], "bar");
        assert!(parsed["hooks"]["PostToolUse"].is_array());
    }

    #[test]
    fn claude_settings_preserves_sibling_hooks_in_same_matcher() {
        // A non-polaris hook lives in the same matcher block we'll touch.
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit",
        "hooks": [
          { "type": "command", "command": "/usr/bin/my-formatter" }
        ]
      }
    ]
  }
}"#;
        let report = merge_claude_settings(Some(existing), &bin()).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&report.new_content.unwrap()).unwrap();
        let blocks = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        // Two matcher blocks: the user's (preserved verbatim) and ours (canonical matcher).
        assert_eq!(blocks.len(), 2);
        let user_block = blocks.iter().find(|b| b["matcher"] == "Write|Edit").unwrap();
        assert_eq!(
            user_block["hooks"][0]["command"],
            "/usr/bin/my-formatter"
        );
        let polaris_block = blocks.iter().find(|b| b["matcher"] == "Write|Edit|MultiEdit").unwrap();
        let cmd = polaris_block["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("polaris"));
    }

    #[test]
    fn claude_settings_drops_stale_polaris_entries_and_appends_canonical() {
        // Two stale polaris entries under different matchers — both should be dropped,
        // one canonical entry appended.
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "/old/path/polaris hook index" }
        ]
      },
      {
        "matcher": "Edit",
        "hooks": [
          { "type": "command", "command": "/other/old/polaris hook index" }
        ]
      }
    ]
  }
}"#;
        let report = merge_claude_settings(Some(existing), &bin()).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&report.new_content.unwrap()).unwrap();
        let blocks = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        // Both stale matcher blocks were empty after polaris removal → pruned.
        // Only our canonical block remains.
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["matcher"], "Write|Edit|MultiEdit");
        let cmd = blocks[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.starts_with("/usr/local/bin/polaris"));
    }

    #[test]
    fn claude_settings_unchanged_when_already_current() {
        let first = merge_claude_settings(None, &bin())
            .unwrap()
            .new_content
            .unwrap();
        let report = merge_claude_settings(Some(&first), &bin()).unwrap();
        assert_eq!(report.action, ClaudeSettingsAction::Unchanged);
        assert!(report.new_content.is_none());
    }

    #[test]
    fn claude_settings_errors_on_invalid_json() {
        let result = merge_claude_settings(Some("not json {"), &bin());
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn claude_settings_errors_when_top_level_is_not_object() {
        let result = merge_claude_settings(Some("[1, 2, 3]"), &bin());
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn run_writes_claude_settings_by_default() {
        let dir = TempDir::new().unwrap();
        // Use no_hooks=false to exercise the new path.
        run(&PolarisConfig::default(), dir.path(), false, false).unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        assert!(settings_path.exists(), ".claude/settings.json should be created");

        let content = std::fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let block = &parsed["hooks"]["PostToolUse"][0];
        assert_eq!(block["matcher"], "Write|Edit|MultiEdit");
        let cmd = block["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with("hook index"));
    }

    #[test]
    fn run_skips_claude_settings_with_no_hooks() {
        let dir = TempDir::new().unwrap();
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        assert!(
            !settings_path.exists(),
            ".claude/settings.json should not be created when --no-hooks is set"
        );
    }

    #[test]
    fn remove_polaris_hooks_strips_polaris_entries_across_events() {
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/polaris hook index" }
        ]
      },
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "/usr/bin/my-formatter" }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/polaris hook search" }
        ]
      }
    ]
  },
  "permissions": { "allow": ["Bash"] }
}"#;
        let new_content = remove_polaris_hooks_from_settings(existing).unwrap();
        let content = new_content.expect("file should be rewritten");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // PostToolUse: polaris matcher block was the only entry and was pruned;
        // the formatter block survives.
        let post = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["matcher"], "Write");
        assert_eq!(
            post[0]["hooks"][0]["command"],
            "/usr/bin/my-formatter"
        );

        // UserPromptSubmit: the only entry was polaris-owned and got removed,
        // pruning the surrounding matcher block. The event key remains as an
        // empty array; assert that explicitly so a regression that deletes the
        // event key would be caught.
        let submit = parsed["hooks"]["UserPromptSubmit"]
            .as_array()
            .expect("UserPromptSubmit key must still exist");
        assert!(submit.is_empty(), "UserPromptSubmit should have no polaris entries");

        // Unrelated top-level keys are preserved.
        assert_eq!(parsed["permissions"]["allow"][0], "Bash");
    }

    #[test]
    fn remove_polaris_hooks_returns_none_when_nothing_to_remove() {
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "/usr/bin/my-formatter" }
        ]
      }
    ]
  }
}"#;
        let result = remove_polaris_hooks_from_settings(existing).unwrap();
        assert!(
            result.is_none(),
            "should return None when no polaris entries are present"
        );
    }

    #[test]
    fn remove_polaris_hooks_returns_none_on_empty_hooks_section() {
        let existing = r#"{ "permissions": { "allow": [] } }"#;
        let result = remove_polaris_hooks_from_settings(existing).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn remove_polaris_hooks_errors_on_invalid_json() {
        let result = remove_polaris_hooks_from_settings("not json {");
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    fn remove_polaris_hooks_strips_polaris_but_keeps_sibling_in_same_block() {
        // A matcher block has both a polaris hook and a sibling hook. The polaris
        // entry should be stripped; the sibling should survive in the same block.
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/polaris hook index" },
          { "type": "command", "command": "/usr/bin/my-formatter" }
        ]
      }
    ]
  }
}"#;
        let content = remove_polaris_hooks_from_settings(existing)
            .unwrap()
            .expect("file should be rewritten");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let blocks = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(blocks.len(), 1, "matcher block should be preserved");
        let hooks = blocks[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1, "exactly one hook should remain");
        assert_eq!(
            hooks[0]["command"],
            "/usr/bin/my-formatter",
            "sibling hook should be the survivor"
        );
    }

    #[test]
    fn remove_polaris_hooks_recognizes_windows_exe_basename() {
        // On Windows, `current_exe()` returns `polaris.exe`. The install path
        // writes that basename; the uninstall path must recognize it too.
        // Use forward slashes so `Path::file_name()` parses the basename on
        // both Linux and Windows (Windows accepts either separator).
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          { "type": "command", "command": "/c/Users/u/bin/polaris.exe hook index" }
        ]
      }
    ]
  }
}"#;
        let new_content = remove_polaris_hooks_from_settings(existing).unwrap();
        let content = new_content.expect("polaris.exe entry should be recognized and removed");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let post = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert!(post.is_empty(), "matcher block should be pruned, got {:?}", post);
    }

    #[test]
    fn merge_claude_settings_quotes_binary_path_with_spaces() {
        // On Windows the binary commonly lives under `C:\Program Files\...`.
        // The written command must be shell-parseable and round-trip through
        // `is_polaris_owned` so re-runs and uninstall keep working.
        let bin = std::path::PathBuf::from("/opt/some path/polaris");
        let report = merge_claude_settings(None, &bin).unwrap();
        let content = report.new_content.expect("should write");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let cmd = parsed["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        // The command must be shell-quoted in some way (single-quoted token
        // form is what shell_words::quote emits for paths with spaces).
        assert!(
            cmd.contains("'") || cmd.contains("\""),
            "expected command to be shell-quoted; got: {cmd}"
        );
        // Round trip: re-merging should detect this as already-current and
        // return Unchanged. That only works if `is_polaris_owned` parses the
        // quoted form correctly.
        let report2 = merge_claude_settings(Some(&content), &bin).unwrap();
        assert_eq!(report2.action, ClaudeSettingsAction::Unchanged);
        assert!(report2.new_content.is_none());
    }

    #[test]
    fn remove_polaris_hooks_preserves_user_owned_polaris_status_hook() {
        // A user hand-wrote a hook that runs `polaris status` for some other
        // purpose. Our ownership check is scoped to `polaris hook ...` only,
        // so this hook must survive.
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/polaris status" }
        ]
      }
    ]
  }
}"#;
        let result = remove_polaris_hooks_from_settings(existing).unwrap();
        assert!(
            result.is_none(),
            "user `polaris status` hook is not polaris-owned and must not be removed; got {:?}",
            result
        );
    }

    #[test]
    fn run_no_hooks_removes_existing_polaris_entries() {
        let dir = TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");

        // Seed the file with a canonical polaris hook entry (don't go through
        // the install path, because cargo's test binary is named `polaris-<hash>`
        // and would not be recognized as polaris-owned by the strict-equality
        // `is_polaris_owned` rule).
        let seeded = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/polaris hook index" }
        ]
      }
    ]
  }
}"#;
        std::fs::write(&settings_path, seeded).unwrap();

        // Run with --no-hooks; the polaris hook entry should be removed.
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();

        // File still exists (we don't delete the file, only our entries).
        assert!(settings_path.exists());
        let after = std::fs::read_to_string(&settings_path).unwrap();
        assert!(
            !after.contains("polaris"),
            "polaris entries should be removed from .claude/settings.json"
        );
    }

    #[test]
    fn run_no_hooks_is_noop_when_settings_absent() {
        let dir = TempDir::new().unwrap();
        // Run --no-hooks on a fresh project (no .claude/ at all).
        run(&PolarisConfig::default(), dir.path(), false, true).unwrap();
        let settings_path = dir.path().join(".claude").join("settings.json");
        assert!(
            !settings_path.exists(),
            ".claude/settings.json should not be created by --no-hooks"
        );
    }

    #[test]
    fn run_no_hooks_errors_when_claude_is_a_regular_file() {
        // Mirror coverage for the install-branch guard: if `.claude` exists
        // but is a regular file, the uninstall branch must surface a clear
        // PolarisError::Setup rather than the opaque NotADirectory error
        // that `read_optional(.claude/settings.json)` would otherwise hit.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".claude"), "this is a file, not a dir\n").unwrap();
        let result = run(&PolarisConfig::default(), dir.path(), false, true);
        assert!(matches!(result, Err(PolarisError::Setup(_))));
    }

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn run_runs_initial_index_when_hooks_installed() {
        use polaris_core::config::PolarisConfig;
        use polaris_core::db::Database;

        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs").join("foo.md"),
            "# Foo\n\nThis is some content for initial indexing.\n",
        )
        .unwrap();

        run(&PolarisConfig::default(), dir.path(), false, false).unwrap();

        // The DB lives at <setup-path>/polaris.db (run_initial_index sets it
        // explicitly to that location).
        let db_path = dir.path().join("polaris.db");
        assert!(db_path.exists(), "initial index should produce polaris.db");

        let cfg = PolarisConfig::default();
        let db = Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap();
        let docs = db.get_all_document_hashes().unwrap();
        assert!(
            docs.iter().any(|(p, _)| p.ends_with("foo.md")),
            "docs/foo.md should be indexed; got {:?}",
            docs
        );
    }

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn run_initial_index_stores_paths_matching_hook_cwd_relative_form() {
        // Regression: `polaris setup ./my-proj` invoked from a parent dir
        // must produce DB paths `docs/foo.md` (cwd-relative from the project
        // root) rather than `my-proj/docs/foo.md`. The hook gate translates
        // Claude Code's absolute payload using `cwd` and compares against
        // `docs/foo.md` — any other prefix would silently no-op.
        //
        // NOTE: mutates process CWD via set_current_dir. Don't run in
        // parallel with other CWD-mutating tests.
        use polaris_core::config::PolarisConfig;
        use polaris_core::db::Database;

        let parent = TempDir::new().unwrap();
        let proj_name = "myproj";
        let proj = parent.path().join(proj_name);
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        std::fs::write(proj.join("docs").join("foo.md"), "# Foo\nbody\n").unwrap();

        // Mimic `polaris setup ./myproj` from `parent`.
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(parent.path()).unwrap();
        let result = run(&PolarisConfig::default(), Path::new(proj_name), false, false);
        std::env::set_current_dir(prev_cwd).unwrap();
        result.unwrap();

        let cfg = PolarisConfig::default();
        let db = Database::open(
            &proj.join("polaris.db"),
            cfg.embedding_dim,
            &cfg.model_id,
        )
        .unwrap();
        let docs = db.get_all_document_hashes().unwrap();
        assert!(
            docs.iter().any(|(p, _)| p == "docs/foo.md"),
            "expected stored path \"docs/foo.md\" (cwd-relative form the hook \
             will look for); got {:?} — setup invoked with an explicit path \
             argument must not bake the project-name prefix into stored paths",
            docs,
        );
    }
}
