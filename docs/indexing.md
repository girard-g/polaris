# Indexing Pipeline

## Overview

The indexing pipeline takes a file system path and produces semantic vector chunks in the database. It is incremental by default — unchanged files are skipped.

For large corpora (5k+ documents), the pipeline uses a **three-phase design** that eliminates double file reads, maximises embedding batch sizes, and writes all files in a single transaction.

## Pipeline Stages

```
1. File Discovery
   walkdir → all *.md files

2. Load Existing State
   DB → HashMap<path, sha256>

3. Detect Removals
   (DB paths) ∖ (disk paths) → delete from DB

── Phase A: Parallel Collect (rayon) ───────────────────────────────────
4. For each pending file (in parallel across CPU cores):
     read_to_string            ← single read
     SHA256 from content bytes ← no second read
     compare to DB hash        → unchanged | modified | new
     chunk_markdown()          ← parse + split in parallel

── Phase B: Cross-file Embedding ───────────────────────────────────────
5. Flatten all chunks from all pending files into one Vec
   Embed in batches of 32 across the entire corpus
   (batches are always full except the last)

── Phase C: Single-Transaction Write ───────────────────────────────────
6. BEGIN
   For each file: delete old + insert document + insert chunks
   COMMIT  (one write barrier for the entire run)

7. Report
   → IndexReport { added, modified, removed, unchanged, errors }
```

## File Discovery

`walkdir` traverses the path recursively (default) or at depth 1 (`--no-recursive`). Only files with the `.md` extension are returned. Paths are normalized via `normalise_path()`:

- Backslashes are converted to forward slashes (Windows compatibility).
- A leading `./` is stripped so `docs/file.md` and `./docs/file.md` produce the same DB key.

This ensures that `polaris index docs` and `polaris index ./docs` (or `polaris watch ./docs`) always agree on stored paths, preventing spurious re-indexing of unchanged files.

## Change Detection

SHA256 is computed from the file's in-memory bytes (the same buffer used for chunking — no second read). The digest is compared against the stored `content_hash` in the `documents` table:

- **Unchanged** → skip (no embedding, no DB write)
- **Modified** → delete old chunks, re-embed, re-store
- **New** → embed and store
- **Missing from disk** → delete from DB (cascades to chunks + vec_chunks)

Use `--force` to bypass hash comparison and re-index everything.

## Phase A — Parallel Collect

`rayon::par_iter()` processes all pending files concurrently across available CPU cores. Each file is read **once**: SHA256 is computed from the in-memory content bytes, then `chunk_markdown()` runs on the same buffer. The collect phase produces a `Vec<FileData>` ready for embedding.

```rust
struct FileData {
    path: PathBuf,
    norm: String,      // canonical path (forward-slash, no leading "./") — DB key
    hash: String,      // SHA256 hex
    file_size: i64,    // bytes
    title: Option<String>,
    is_new: bool,
    chunks: Vec<Chunk>,
}
```

Files that exceed `max_file_size` (default 10 MB) are skipped with an error recorded in `IndexReport.errors`.

## Phase B — Cross-file Embedding

All chunks from all `FileData` structs are flattened into a single `Vec<String>`, then embedded in a stream of `EMBED_BATCH_SIZE` (32) batches. Because chunks come from many files, batches are almost always exactly 32 — compared to the per-file approach where a 5-chunk file wastes 27 batch slots.

Per-file chunk boundaries are tracked with `(start_idx, chunk_count)` offsets so the flat embedding `Vec` can be sliced back per file during Phase C.

## Phase C — Single-Transaction Write

One `BEGIN` / `COMMIT` covers all files in the run. Each file goes through:

1. `delete_document(norm_path)` — removes old chunks (cascade) if the file existed
2. `insert_document(norm, hash, title, file_size)` — upsert the document row
3. `insert_chunk(...)` × N — one row per chunk with its embedding slice

Files that produce **0 chunks** still receive a document record so they are skipped on the next incremental run.

On any error during Phase C, the transaction is rolled back in full — no partial state is committed.

## Markdown Chunking

Chunking is performed by `chunk_markdown()` in `indexer.rs`. It uses `pulldown-cmark` with byte-offset tracking to parse the document structure.

### Step 1 — Section Extraction

The document is split into sections bounded by headings. Each section includes:
- The heading text itself (so heading content is searchable)
- All body content under that heading, up to the next same-or-higher heading

A **heading context** string is built hierarchically:

```
# Guide
## Installation     → "Guide > Installation"
### Linux           → "Guide > Installation > Linux"
```

### Step 2 — Chunk Formation

Config:
- `max_chunk_tokens = 450` → `max_chars = 450 × 4 = 1800`
- `chunk_overlap_chars = 200`

For each section, the content is split using a **fallback hierarchy**:

```
Level 1: Paragraphs (split on \n\n)
  If section ≤ max_chars → single chunk
  If section < 50 chars → merge into previous chunk
  Otherwise → accumulate paragraphs until max_chars

Level 2: Sentences (split on ". " / "! " / "? ")
  When a paragraph exceeds max_chars
  Accumulate sentences until max_chars, create chunk

Level 3: Words (split on whitespace)
  When a sentence exceeds max_chars
  Accumulate words until max_chars, create chunk
```

### Step 3 — Overlap

After creating each chunk, the last `chunk_overlap_chars` characters are prepended to the next chunk's starting text. If the chunk is shorter than `chunk_overlap_chars`, the entire chunk is used as overlap.

This ensures that context is preserved at chunk boundaries and queries spanning two adjacent chunks can still match.

### Chunk Structure

```rust
pub struct Chunk {
    pub content: String,          // The text to embed and store
    pub heading_context: String,  // Breadcrumb path of headings
    pub start_byte: usize,        // Byte offset in original file
    pub end_byte: usize,          // Byte offset in original file
}
```

## Embedding

Chunks are embedded in batches of **32** using `EmbeddingEngine::embed_documents()`:

1. Prepend `"search_document: "` to each chunk's content
2. Pass the batch to fastembed (ONNX CPU inference)
3. Truncate each embedding to `target_dim`
4. L2-normalize each embedding

Because Phase B flattens chunks across all pending files before embedding, batches in a large run are always full (except the last batch), maximising ONNX throughput.

## Progress Display

Three progress indicators are shown, one per phase:

```
Phase A  ⠹ Reading & chunking files…          (spinner — fast, parallel)
Phase B  [████████████████░░░░] 320/500 chunks | ETA 0:12
Phase C  ⠹ Writing to database…               (spinner — fast, single tx)
```

Phase B is where the wall-clock time is spent. The chunk-level progress bar with ETA gives an accurate view of the slow part.

## IndexReport

```rust
pub struct IndexReport {
    pub added:        Vec<PathBuf>,
    pub modified:     Vec<PathBuf>,
    pub removed:      Vec<PathBuf>,
    pub unchanged:    Vec<PathBuf>,
    pub errors:       Vec<(PathBuf, String)>,
    pub total_chunks: usize,
    pub total_bytes:  u64,
    pub elapsed:      Duration,
}
```

`IndexReport::summary()` formats this as the human-readable string shown in the CLI and returned by the `index` MCP tool.
