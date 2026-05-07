# Polaris `setup` — Agent Instruction Files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `polaris setup` so it writes/refreshes a marker-delimited Polaris MCP instruction block in `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root, with a `--no-agents` opt-out.

**Architecture:** A single pure function `merge_agent_instructions(existing) -> AgentReport` mirrors the existing `merge_mcp_json` shape. The `run` orchestrator loops over the three filenames after handling `.mcp.json` and `.gitignore`. Marker matching is substring-based on literal `<!-- polaris:begin -->` / `<!-- polaris:end -->` strings. The `Command::Setup` enum gains a `no_agents: bool` field; the dispatch passes it through.

**Tech Stack:** Rust 2024 (workspace edition), `clap` derive, `tempfile` (dev-dep, already present), no new crate dependencies.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `polaris-cli/src/setup.rs` | Modify | Add `AgentAction`, `AgentReport`, `AGENT_FILES`, `POLARIS_BLOCK`, `merge_agent_instructions`. Extend `run` to take a `no_agents: bool` and process the three agent files. New unit + integration tests. |
| `polaris-cli/src/main.rs` | Modify | Add `no_agents: bool` field to `Command::Setup`. Pass it into `setup::run`. |
| `README.md` | Modify | Update the existing `### Setup` subsection with one paragraph about agent instructions and the `--no-agents` flag. |
| `docs/cli.md` | Modify | Update the `### \`polaris setup\`` reference entry: add `--no-agents` to the flags table, list the three agent files in the side-effects section. |
| `CHANGELOG.md` | Modify | Add one line under `[Unreleased] / Added` describing the agent-instruction feature. |

---

## Task 1: Add `AgentAction`, `AgentReport`, and the `POLARIS_BLOCK` constant

**Files:**
- Modify: `polaris-cli/src/setup.rs`

This task only adds the data types and constants. The `merge_agent_instructions` function is added in Task 2.

- [ ] **Step 1: Add the constants and types**

In `polaris-cli/src/setup.rs`, immediately after the `GITIGNORE_ENTRIES` constant (around line 15), add:

```rust
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
```

Then, immediately after the existing `McpReport` struct (around line 46), add:

```rust
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
```

- [ ] **Step 2: Verify the crate still compiles**

```bash
cargo check -p polaris-cli
```

Expected: clean. The new items are not yet used; expect 1–2 dead-code warnings on `AgentAction`, `AgentReport`, `AGENT_FILES`, `POLARIS_BLOCK`. They will be consumed in Task 2 and Task 3 — do NOT silence them.

- [ ] **Step 3: Commit**

```bash
git add polaris-cli/src/setup.rs
git commit -m "feat(setup): add AgentAction, AgentReport, and POLARIS_BLOCK"
```

---

## Task 2: Implement `merge_agent_instructions` (TDD)

**Files:**
- Modify: `polaris-cli/src/setup.rs`

This task implements the pure merge function and its 7 unit tests. No orchestrator wiring yet.

- [ ] **Step 1: Write the failing test for the absent-file case**

In `polaris-cli/src/setup.rs`, in `mod tests` (after the existing setup-related tests, before the `use std::path::PathBuf;` block at line 322), add:

```rust
#[test]
fn agent_block_creates_when_absent() {
    let report = merge_agent_instructions(None).unwrap();
    assert_eq!(report.action, AgentAction::Created);
    let content = report.new_content.expect("should write new file");
    assert_eq!(content, POLARIS_BLOCK);
}
```

- [ ] **Step 2: Run, verify FAIL**

```bash
cargo test -p polaris-cli setup::tests::agent_block_creates_when_absent
```

Expected: FAIL — `merge_agent_instructions` not defined.

- [ ] **Step 3: Implement the absent-file path**

In `polaris-cli/src/setup.rs`, immediately after `ensure_gitignore_entries` (around line 160), add:

```rust
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
```

- [ ] **Step 4: Run, verify PASS**

```bash
cargo test -p polaris-cli setup::tests::agent_block_creates_when_absent
```

Expected: PASS.

- [ ] **Step 5: Add the remaining 6 unit tests**

Append these to the same `mod tests` block, after `agent_block_creates_when_absent`:

```rust
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
```

- [ ] **Step 6: Run, verify all PASS**

```bash
cargo test -p polaris-cli setup::tests::agent_block_
cargo check -p polaris-cli
```

Expected: 8 tests pass. Clean build (the dead-code warnings from Task 1 on `AgentAction`/`AgentReport`/`POLARIS_BLOCK` should now be gone — `AGENT_FILES` is still unused until Task 3).

- [ ] **Step 7: Commit**

```bash
git add polaris-cli/src/setup.rs
git commit -m "feat(setup): implement merge_agent_instructions with marker semantics"
```

---

## Task 3: Wire `--no-agents` flag and orchestrate the three files

**Files:**
- Modify: `polaris-cli/src/main.rs`
- Modify: `polaris-cli/src/setup.rs`

This task adds the CLI flag, threads it into `run`, and processes the three agent files in order.

- [ ] **Step 1: Add `no_agents` to `Command::Setup`**

Edit `polaris-cli/src/main.rs`. The current variant (around line 113) is:

```rust
    /// Configure the current project to use polaris (writes .mcp.json, updates .gitignore)
    Setup {
        /// Project directory (defaults to current working directory)
        path: Option<PathBuf>,
    },
```

Replace with:

```rust
    /// Configure the current project to use polaris (writes .mcp.json, updates .gitignore, writes agent instructions)
    Setup {
        /// Project directory (defaults to current working directory)
        path: Option<PathBuf>,
        /// Do not write CLAUDE.md, AGENTS.md, GEMINI.md
        #[arg(long)]
        no_agents: bool,
    },
```

In the dispatch `match cli.command` block (around line 174), the existing arm is:

```rust
        Command::Setup { path } => {
            let target = path.unwrap_or_else(|| std::path::PathBuf::from("."));
            setup::run(&target)
        }
```

Replace with:

```rust
        Command::Setup { path, no_agents } => {
            let target = path.unwrap_or_else(|| std::path::PathBuf::from("."));
            setup::run(&target, no_agents)
        }
```

- [ ] **Step 2: Update `setup::run` signature and add the agent-files block**

Edit `polaris-cli/src/setup.rs`. The current signature (around line 163) is:

```rust
pub fn run(path: &Path) -> Result<()> {
```

Change to:

```rust
pub fn run(path: &Path, no_agents: bool) -> Result<()> {
```

After the existing `.gitignore` handling block ends (the closing `}` of the `match gi_report.new_content` block, currently around line 248) and BEFORE the trailing `println!()` + `Ok(())` (currently around line 250-251), insert:

```rust
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
```

- [ ] **Step 3: Update existing `run_*` integration tests for the new signature**

Still in `polaris-cli/src/setup.rs`, find each existing call to `run(...)` in `mod tests` (there are four: `run_creates_files_in_empty_dir`, `run_is_idempotent`, `run_errors_when_path_missing`, `run_errors_when_path_is_file`). Update each to pass `false` as the second argument so the existing tests cover the default (`--no-agents` not set) behaviour.

For example, `run_creates_files_in_empty_dir` currently has `run(dir.path()).unwrap();` — change it to `run(dir.path(), false).unwrap();`. Apply the same change to all four sites.

- [ ] **Step 4: Run, verify the existing tests still pass**

```bash
cargo test -p polaris-cli setup::tests::run_
```

Expected: all four pre-existing `run_*` tests pass with the new signature. The two `run_errors_*` tests pass because the agent-files block is only reached after the path validation passes.

Note: `run_creates_files_in_empty_dir` and `run_is_idempotent` will now also create `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` in the tempdir. The existing assertions only check `.mcp.json` and `.gitignore`, so they continue to pass — Task 4 adds the new agent-file integration tests.

- [ ] **Step 5: Commit**

```bash
git add polaris-cli/src/main.rs polaris-cli/src/setup.rs
git commit -m "feat(setup): wire --no-agents flag and orchestrate agent files"
```

---

## Task 4: Add integration tests for the orchestrator's agent-file behaviour

**Files:**
- Modify: `polaris-cli/src/setup.rs`

- [ ] **Step 1: Add the four integration tests**

In `polaris-cli/src/setup.rs` `mod tests`, after the existing `run_errors_when_path_is_file` test, append:

```rust
#[test]
fn run_writes_all_three_agent_files() {
    let dir = TempDir::new().unwrap();
    run(dir.path(), false).unwrap();

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

    run(dir.path(), false).unwrap();

    let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
    // Original content preserved at the top.
    assert!(content.starts_with(existing_user_rules));
    // Polaris block appended after a blank line.
    assert!(content.contains("\n\n<!-- polaris:begin -->"));
}

#[test]
fn run_skips_agent_files_with_no_agents() {
    let dir = TempDir::new().unwrap();
    run(dir.path(), true).unwrap();

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
    run(dir.path(), false).unwrap();

    let mut first: Vec<(String, String)> = Vec::new();
    for filename in AGENT_FILES {
        first.push((
            (*filename).to_string(),
            std::fs::read_to_string(dir.path().join(filename)).unwrap(),
        ));
    }

    run(dir.path(), false).unwrap();

    for (filename, before) in &first {
        let after = std::fs::read_to_string(dir.path().join(filename)).unwrap();
        assert_eq!(*before, after, "{filename} should be unchanged on rerun");
    }
}
```

- [ ] **Step 2: Run, verify all four PASS**

```bash
cargo test -p polaris-cli setup::tests::run_writes_all_three_agent_files
cargo test -p polaris-cli setup::tests::run_preserves_existing_user_content_in_agent_files
cargo test -p polaris-cli setup::tests::run_skips_agent_files_with_no_agents
cargo test -p polaris-cli setup::tests::run_is_idempotent_with_agent_files
cargo test -p polaris-cli setup::
```

Expected: all four new tests pass. Full setup-test suite (the 4 pre-existing `run_*` tests + 5 pre-existing `gitignore_*` tests + 6 pre-existing `mcp_*` tests + 8 new `agent_block_*` tests + 4 new `run_*_agent*` tests = 27 tests) all green.

- [ ] **Step 3: Smoke test the CLI**

```bash
cargo build -p polaris-cli
TMP_DIR=$(mktemp -d)
./target/debug/polaris setup "$TMP_DIR"
ls "$TMP_DIR"
grep -l "polaris:begin" "$TMP_DIR"/CLAUDE.md "$TMP_DIR"/AGENTS.md "$TMP_DIR"/GEMINI.md
./target/debug/polaris setup "$TMP_DIR"   # idempotent re-run
rm -rf "$TMP_DIR"
```

Expected: first run prints six `✓` lines (mcp.json, gitignore, three agent files, …). Second run prints "already configured" for everything. All three agent files contain the polaris block.

Run the same with `--no-agents`:

```bash
TMP_DIR=$(mktemp -d)
./target/debug/polaris setup --no-agents "$TMP_DIR"
ls "$TMP_DIR"   # should NOT contain CLAUDE.md / AGENTS.md / GEMINI.md
rm -rf "$TMP_DIR"
```

Expected: only `.mcp.json` and `.gitignore` in the tempdir.

- [ ] **Step 4: Commit**

```bash
git add polaris-cli/src/setup.rs
git commit -m "test(setup): cover orchestrator agent-file behaviour"
```

---

## Task 5: Documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/cli.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Update README.md `### Setup` subsection**

In `README.md`, find the existing `### Setup` subsection. After the existing description of what `polaris setup` writes, append a paragraph describing the new behaviour. Locate the existing subsection with:

```bash
grep -n "^### Setup" README.md
```

(Expect a line number near 95–110.) Read the surrounding 20 lines so you understand the local style. Then add this paragraph at the end of the subsection (just before the next `### …` header):

```markdown
`polaris setup` also writes a marker-delimited Polaris MCP block into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root, steering compatible coding agents toward `polaris.search` for documentation queries. Existing user content in those files is preserved (the block is delimited by `<!-- polaris:begin --> … <!-- polaris:end -->` markers and only that range is rewritten on re-run). Pass `--no-agents` to skip these three files.
```

- [ ] **Step 2: Update `docs/cli.md` `### \`polaris setup\`` reference entry**

In `docs/cli.md`, find the existing `### \`polaris setup\`` section:

```bash
grep -n "^### \`polaris setup\`" docs/cli.md
```

Read the surrounding 30 lines to understand the existing flag-table style. Then:

a) **Add `--no-agents` to the flags table.** If a flags table already exists in that section, add a new row:

| `--no-agents` | false | Skip writing `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` |

If no flags table exists yet, add one with this row and a header that fits the local style.

b) **Update the side-effects list** to include the three agent files. The existing section likely lists `.mcp.json` and `.gitignore`. Add a third bullet/entry along the lines of:

```markdown
- `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` — Polaris MCP instruction block, marker-delimited (`<!-- polaris:begin --> … <!-- polaris:end -->`). Preserves existing user content; refreshes only the block on re-run. Skipped if `--no-agents` is passed.
```

Match the bullet style used by the existing `.mcp.json` / `.gitignore` entries — copy their structure rather than inventing a new one.

- [ ] **Step 3: Update CHANGELOG.md**

In `CHANGELOG.md`, find the `## [Unreleased] / ### Added` section (added in the previous savings work). Append one bullet:

```markdown
- `polaris setup` now writes a marker-delimited Polaris MCP instruction
  block into `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root.
  Pass `--no-agents` to skip them. Existing user content is preserved;
  re-runs only refresh the block.
```

Place this bullet at the end of the existing `### Added` list, before the `### Changed` heading.

- [ ] **Step 4: Smoke-test the docs render**

```bash
grep -n "no-agents" README.md docs/cli.md
grep -n "polaris:begin" README.md docs/cli.md
grep -n "no-agents" CHANGELOG.md
```

Expected: matches in all three files.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/cli.md CHANGELOG.md
git commit -m "docs: document setup --no-agents and agent instruction files"
```

---

## Final Verification

After all tasks complete, run the full workspace check:

```bash
cargo check --workspace
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets
```

Expected: all green, no new clippy warnings (pre-existing ones in `polaris-core/{config,bank,indexer,search,embedding}.rs` and `polaris-cli/src/main.rs:625` are not introduced by this work).

Verify five atomic commits:

```bash
git log --oneline c18b2ba..HEAD   # adjust the base SHA to the commit before Task 1
```

Expected: one commit per task, each with a clean message.
