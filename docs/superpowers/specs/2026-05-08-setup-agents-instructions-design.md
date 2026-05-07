# Polaris `setup` — Agent Instruction Files — Design

**Date:** 2026-05-08
**Status:** Approved (pending implementation plan)

## Goal

Extend `polaris setup` so that, alongside the existing `.mcp.json` and `.gitignore` updates, it writes (or refreshes) a small marker-delimited instruction block in each of `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` at the project root. The block tells the agent that a Polaris MCP server is configured and asks it to prefer `polaris.search` over `grep`/`read` for documentation queries.

The forcing function is upstream of any single agent: by writing the same block into all three industry-standard agent instruction files, any compatible agent (Claude Code, Codex, Gemini CLI, and the others that read AGENTS.md) inherits the steer the moment a project has been `polaris setup`-prepared.

## Non-goals

- Cursor's `.cursorrules` / `.cursor/rules` — increasingly superseded by AGENTS.md; out of scope for v1.
- GitHub Copilot's `.github/copilot-instructions.md` — different file convention; revisit if requested.
- Auto-running an initial index (still a separate `polaris index` invocation).
- Editing `.mcp.json` for clients other than the project-scoped convention (out of scope of the original setup spec; preserved here).
- Detecting which agent the user actually uses — write all three regardless, since the cost is low and a future move between agents shouldn't require a re-setup.

## Command surface

```
polaris setup [path] [--no-agents]
```

- `path` — unchanged from current behaviour: optional, defaults to current working directory.
- `--no-agents` — new flag. When passed, skips writing/updating `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md`. Default is to write them.

`Command::Setup` in `polaris-cli/src/main.rs` gains the `no_agents: bool` field; the dispatch arm passes it into `setup::run(&target, no_agents)`.

## Block content

The block is identical across all three files. Token-cheap by design (it is loaded into every agent conversation that opens the repo):

```markdown
<!-- polaris:begin -->
## Polaris MCP

This project ships a Polaris MCP server (`polaris serve`) that semantic-searches the docs in this repo. Prefer the `polaris.search` tool over grep/read for any question about the project's documentation, behaviour, or architecture — it returns ranked, section-aware chunks and is typically 10-40× cheaper in tokens. Start with top_k=2; raise only if recall is poor. Use `polaris.index` to add or refresh files, `polaris.status` to check index health.
<!-- polaris:end -->
```

The marker pair `<!-- polaris:begin -->` / `<!-- polaris:end -->` is HTML-comment syntax (renderable by any Markdown viewer without showing the markers). The exact strings are matched verbatim — they are the seam between user content and Polaris-managed content.

## File-merge logic

Marker matching is **substring-based on the literal strings** `<!-- polaris:begin -->` and `<!-- polaris:end -->` (case-sensitive). When a pair is replaced, the replaced byte range runs from the first character of the `:begin` marker through the last character of the `:end` marker, inclusive. Anything on the same line as a marker (other than the marker text itself) is part of the replaced range.

A marker pair is **well-formed** when the file contains exactly one occurrence of `<!-- polaris:begin -->` AND exactly one occurrence of `<!-- polaris:end -->` AND the `:begin` occurrence appears before the `:end` occurrence in byte order.

For each of `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, processed in that order:

1. **Absent** → create file containing only the canonical block. The block itself ends in a newline; no extra leading or trailing blank lines.
2. **Present, no markers at all** → append the block at the end. Concretely: if the existing content does not end with `\n`, append `\n`; then append `\n` (the blank-line separator); then append the canonical block.
3. **Present, well-formed marker pair, content differs from canonical** → replace the marker range (per the rules above) with the canonical block. Content above and below the range is preserved byte-for-byte.
4. **Present, well-formed marker pair, content matches canonical** → no rewrite. Preserve mtime. Report "already configured."
5. **Present, marker malformed** — defined as any of:
    - Two or more `<!-- polaris:begin -->` occurrences
    - Two or more `<!-- polaris:end -->` occurrences
    - Exactly one of each, but `:end` appears before `:begin` in byte order
    - Exactly one `:begin` and zero `:end` (unclosed)
    - Exactly one `:end` and zero `:begin` (orphan)
   → abort with a `PolarisError::Setup` whose message names the file path and the malformation (e.g. `"CLAUDE.md: two polaris:begin markers found; refusing to auto-repair"`).

**Partial-application semantics.** `setup` processes side-effects in the order: `.mcp.json` → `.gitignore` → agent files (in array order). On any error, processing stops at the failing step; previously-applied changes are NOT rolled back. This matches the existing behaviour for `.mcp.json` parse errors. Users fix the offending file and re-run; subsequent runs are no-ops for already-correct files.

## Idempotency

Re-running `polaris setup` on a tree that is already fully set up:
- `.mcp.json` — unchanged (existing behaviour).
- `.gitignore` — unchanged (existing behaviour).
- `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` — each reports "already configured" and is not rewritten.

Re-running after editing user content outside the markers:
- The polaris block is preserved (matches canonical).
- User content is preserved.
- No rewrite happens. mtime is stable.

## `.gitignore` interaction

`CLAUDE.md`, `AGENTS.md`, `GEMINI.md` are deliberately NOT added to the gitignore entry list. They are project documentation and should be committed so collaborators and CI agents inherit the same instruction. The existing five gitignore entries (`polaris.db`, `polaris.db-shm`, `polaris.db-wal`, `.fastembed_cache/`, `.mcp.json`) are unchanged.

## Output

Per file, on its own line, after the existing `.mcp.json` and `.gitignore` output:

```
✓ Created CLAUDE.md (polaris block)
✓ Updated AGENTS.md (polaris block refreshed)
✓ GEMINI.md already configured
```

When `--no-agents` is passed, none of the three lines appear; setup ends after the gitignore line as today.

## Errors handled

In addition to existing setup errors:

- `CLAUDE.md is not a regular file` — reject directories or special files at the target path.
- `CLAUDE.md: two polaris:begin markers found; refusing to auto-repair` — and the matching `:end`-mismatch / orphan-marker variants.
- `failed to write CLAUDE.md: <io error>` — bubble through `PolarisError::Io`.

## Code organization

All new code lives in `polaris-cli/src/setup.rs`. The current file is ~450 lines (with tests) and the new logic adds approximately:

- 1 const: `AGENT_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md", "GEMINI.md"];`
- 1 const: `POLARIS_BLOCK: &str = "<!-- polaris:begin -->\n…\n<!-- polaris:end -->\n";`
- 1 enum: `AgentAction { Created, Updated, Unchanged }`
- 1 struct: `AgentReport { new_content: Option<String>, action: AgentAction }`
- 1 pure function: `pub fn merge_agent_instructions(existing: Option<&str>) -> Result<AgentReport>`
- 6+ unit tests in the existing `mod tests`
- An `if !no_agents { … }` block in `run` that loops over `AGENT_FILES` and calls `merge_agent_instructions` per file

A separate `agents.rs` module would be a one-function pass-through and is not justified at this size. If `setup.rs` grows past ~700 lines later, revisit.

## Testing

New unit tests on `merge_agent_instructions`:

- `agent_block_creates_when_absent` — input `None` → output content equal to the canonical block.
- `agent_block_appends_when_no_marker` — existing content with no markers → block appended after exactly one blank line, original content untouched at the top.
- `agent_block_replaces_stale_marker` — existing block with stale wording → replaced with canonical, surrounding content byte-stable.
- `agent_block_unchanged_when_current` — existing canonical block → returns `AgentAction::Unchanged` and `new_content: None`.
- `agent_block_errors_on_two_begin_markers` — input contains two `polaris:begin` → `PolarisError::Setup`.
- `agent_block_errors_on_orphan_end_marker` — input contains `polaris:end` with no preceding `polaris:begin` → `PolarisError::Setup`.
- `agent_block_errors_on_unclosed_marker` — `polaris:begin` with no following `polaris:end` → `PolarisError::Setup`.

New integration tests on `run`:

- `run_writes_all_three_agent_files` — fresh tempdir → all three files created with canonical block.
- `run_preserves_existing_user_content_in_agent_files` — pre-seeded `CLAUDE.md` with user rules, no marker → block appended; user rules preserved verbatim.
- `run_skips_agent_files_with_no_agents` — pass `no_agents=true`; assert none of the three files were created.
- `run_is_idempotent_with_agent_files` — run twice, assert all three files byte-identical between runs.

## Documentation

After implementation, update:

- `README.md` — `### Setup` subsection (already exists from the previous setup work) gets a one-paragraph note about the new agent-instructions behaviour and the `--no-agents` flag.
- `docs/cli.md` — `### \`polaris setup\`` reference entry gains the `--no-agents` flag in its flags table and a "Files written" subsection listing the three agent files alongside the existing `.mcp.json` / `.gitignore`.
- `CHANGELOG.md` — new line under `[Unreleased] / Added` describing the agent-instruction feature.
