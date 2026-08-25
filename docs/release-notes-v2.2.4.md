# Polaris v2.2.4

Released 2026-07-14. Changes since **v2.2.3**.

## Features

- **Update-available notice (CLI + MCP).** Polaris now checks GitHub for a
  newer release and tells you about it: a one-line stderr notice on CLI
  commands (e.g. `polaris status`) and a once-per-session banner in the MCP
  `serve` server. The check runs in a detached background `update-refresh`
  child that writes a small cache atomically (temp + rename) and is reaped
  under a ~30 s bound, so a long-lived `serve` never leaks a zombie or blocks
  on a stalled connection. It stays quiet where a notice would be noise —
  machine output (`--output json`), CI, and the `POLARIS_NO_UPDATE_CHECK` /
  `CI` opt-outs (value-aware: `=0` or `=false` keep checks on). Uses the
  platform cache dir via `dirs::cache_dir()`, so it works on Windows and
  doesn't pollute the working directory.

- **Inline context windows in search results (`--context` / `--radius`).**
  `polaris search` can now show the lines surrounding each hit directly in the
  results: `--context` renders a window around the match and `--radius` sizes
  it. This collapses the old search → copy chunk id → fetch-window loop into a
  single command — no id, no second call, no JSON round-trip.

## Bug Fixes

- **Indexing no longer erases rows it simply didn't walk.** The stale-row
  purge pass used to delete any indexed row this run's discovery didn't turn
  up. Discovery is markdown-only and scoped to the root it was given, so a
  `polaris index docs` run erased rows for files that still exist — anything
  ingested by other means, and, with `--recursive` off, everything in the
  subdirectories it never descended into. Removal is now keyed on the source
  file being gone from disk, not on its absence from this run's walk.
  `polaris-core/tests/purge_safety.rs` covers it.

  Known limitation as of this release: that check stats the stored path
  against the *current process* cwd. Rows indexed under a relative root are
  stored relative to the cwd they were indexed from, and nothing records
  which cwd that was — so running `polaris index` from a different directory
  can still resolve a live row's path to nothing and delete it. Absolute
  roots are unaffected. Tracked in `docs/todos.md`.

## Under the Hood

- **polaris-cli exposed as a library, with core seams for external ingestion.**
  Preparatory plumbing for a `polaris-pro` binary that drives the same
  pipeline. `polaris-cli` now has a public `run()` entry point, and several
  core APIs opened up: `Bank::index_documents` accepts already-generated
  Markdown in memory (e.g. a converted PDF or DOCX) and runs it through the
  existing chunk → embed → store → skip-unchanged pipeline without touching
  disk; `Bank::document_hashes` lets a caller skip re-ingesting unchanged
  sources by hash; `Cli::resolve_config` and `Cli::command()` let an external
  binary resolve the same DB and overrides; and `init_tracing` /
  `warn_extra_dbs_ignored` are now public so the same stderr subscriber and
  `--db` warning aren't reimplemented. `Bank::index_documents` also now honors
  `--force`, which it previously ignored by hardcoding skip-unchanged.

Internal: added test coverage for the search-render fallback when no context
window is present.
