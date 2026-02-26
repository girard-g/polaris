# Indexing Pipeline

## Overview

The indexing pipeline takes a file system path and produces semantic vector chunks in the database. It is incremental by default — unchanged files are skipped.

## Pipeline Stages

```
1. File Discovery
   walkdir → all *.md files

2. Load Existing State
   DB → HashMap<path, sha256>

3. Detect Removals
   (DB paths) ∖ (disk paths) → delete from DB

4. Hash & Classify
   for each disk path:
     compute SHA256
     compare to DB hash
     → unchanged | modified | new

5. Skip Unchanged
   (unless --force)

6. Process Changed Files
   for each new/modified file:
     read content
     extract title (first H1)
     chunk markdown (heading-aware)
     embed in batches of 32
     store in transaction

7. Report
   → IndexReport { added, modified, removed, unchanged, errors }
```

## File Discovery

`walkdir` traverses the path recursively (default) or at depth 1 (`--no-recursive`). Only files with the `.md` extension are returned. Paths are forward-slash normalized on all platforms.

## Change Detection

SHA256 is computed from the raw file bytes. The digest is compared against the stored `content_hash` in the `documents` table:

- **Unchanged** → skip (no embedding, no DB write)
- **Modified** → delete old chunks, re-embed, re-store
- **New** → embed and store
- **Missing from disk** → delete from DB (cascades to chunks + vec_chunks)

Use `--force` to bypass hash comparison and re-index everything.

## Markdown Chunking

Chunking is performed by `chunk_markdown()` in `indexer.rs`. It uses `pulldown-cmark` with byte-offset tracking to parse the document structure.

### Step 1 — Section Extraction

The document is split into sections bounded by headings. Each section includes:
- The heading text itself (so heading content is searchable)
- All body content under that heading, up to the next same-or-higher heading

A **heading context** string is built hierarchically:

```
# Guide
## Installation     → "# Guide > ## Installation"
### Linux           → "# Guide > ## Installation > ### Linux"
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

## Storage

For each file, everything is written inside a single DB transaction:

1. If the file existed before: delete its old chunks (+ vec_chunks cascade)
2. Insert/update the `documents` row
3. For each chunk: insert into `chunks` and `vec_chunks`
4. Commit

On any error during this process, the transaction is rolled back and the error is recorded in `IndexReport.errors`. The file is skipped; other files continue processing.

## Progress Display

During indexing, `indicatif` shows a progress bar:

```
[████████████████████] 10/10 | docs/guide.md · 48/160 chunks [embedding…]
```

The message updates per chunk-batch to show the current file and embedding progress.

## IndexReport

```rust
pub struct IndexReport {
    pub added:       Vec<PathBuf>,
    pub modified:    Vec<PathBuf>,
    pub removed:     Vec<PathBuf>,
    pub unchanged:   Vec<PathBuf>,
    pub errors:      Vec<(PathBuf, String)>,
    pub total_chunks: usize,
    pub total_bytes:  u64,
    pub elapsed:      Duration,
}
```

`IndexReport::summary()` formats this as the human-readable string shown in the CLI and returned by the `index` MCP tool.
