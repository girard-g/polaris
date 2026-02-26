use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

const EMBED_BATCH_SIZE: usize = 32; // mirrors EmbeddingEngine's internal BATCH_SIZE
use pulldown_cmark::{Event, HeadingLevel, Options as CmarkOptions, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::db::Database;
use crate::embedding::EmbeddingEngine;
use crate::error::Result;

const MIN_CHUNK_CHARS: usize = 50;

pub struct IndexReport {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
    pub total_chunks: usize,
    pub total_bytes: u64,
    pub elapsed: Duration,
}

impl IndexReport {
    fn new() -> Self {
        Self {
            added: Vec::new(),
            modified: Vec::new(),
            removed: Vec::new(),
            unchanged: Vec::new(),
            errors: Vec::new(),
            total_chunks: 0,
            total_bytes: 0,
            elapsed: Duration::ZERO,
        }
    }

    pub fn summary(&self) -> String {
        let changed = self.added.len() + self.modified.len();
        let mut lines = vec![format!(
            "Added: {}, Modified: {}, Removed: {}, Unchanged: {}, Errors: {}",
            self.added.len(),
            self.modified.len(),
            self.removed.len(),
            self.unchanged.len(),
            self.errors.len(),
        )];
        if changed > 0 {
            lines.push(format!(
                "Chunks: {} | Data: {:.1} KB | Time: {:.1}s",
                self.total_chunks,
                self.total_bytes as f64 / 1024.0,
                self.elapsed.as_secs_f64(),
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Chunk produced by the markdown parser
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Chunk {
    pub content: String,
    pub heading_context: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

// ---------------------------------------------------------------------------
// Public indexer entry point
// ---------------------------------------------------------------------------

pub struct Indexer {
    embedding_engine: Arc<EmbeddingEngine>,
    max_chunk_tokens: usize,
    chunk_overlap_chars: usize,
}

impl Indexer {
    pub fn new(
        embedding_engine: Arc<EmbeddingEngine>,
        max_chunk_tokens: usize,
        chunk_overlap_chars: usize,
    ) -> Self {
        Self {
            embedding_engine,
            max_chunk_tokens,
            chunk_overlap_chars,
        }
    }

    /// Index all `.md` files under `root` (recursively unless `recursive==false`).
    ///
    /// When `force` is true, all files are re-indexed regardless of hash.
    pub fn index_path(
        &self,
        db: &Database,
        root: &Path,
        recursive: bool,
        force: bool,
    ) -> Result<IndexReport> {
        let mut report = IndexReport::new();
        let start = Instant::now();

        // 1. Discover .md files (with a spinner).
        let discover_spinner = ProgressBar::new_spinner();
        discover_spinner.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        discover_spinner.set_message("Discovering markdown files…");
        discover_spinner.enable_steady_tick(Duration::from_millis(80));

        let discovered = discover_markdown_files(root, recursive);
        discover_spinner.finish_and_clear();
        eprintln!("  Found {} markdown file(s)", discovered.len());

        // 2. Load existing hashes from DB.
        let existing: HashMap<String, String> = db
            .get_all_document_hashes()?
            .into_iter()
            .collect();

        // 3. Build normalised-path set for detecting removals.
        let discovered_paths: std::collections::HashSet<String> = discovered
            .iter()
            .filter_map(|p| normalise_path(p))
            .collect();

        // 4. Handle removals.
        for db_path in existing.keys() {
            if !discovered_paths.contains(db_path.as_str()) {
                match db.delete_document(db_path) {
                    Ok(_) => report.removed.push(PathBuf::from(db_path)),
                    Err(e) => report.errors.push((PathBuf::from(db_path), e.to_string())),
                }
            }
        }

        // 5. Classify each discovered file: unchanged vs. needs indexing.
        let mut to_index: Vec<(PathBuf, String, String, bool)> = Vec::new();

        for path in &discovered {
            let Some(norm) = normalise_path(path) else {
                report.errors.push((path.clone(), "Non-UTF-8 path".to_string()));
                continue;
            };

            let hash = match file_sha256(path) {
                Ok(h) => h,
                Err(e) => {
                    report.errors.push((path.clone(), e.to_string()));
                    continue;
                }
            };

            let needs_index = force
                || existing.get(&norm).map(|h| h != &hash).unwrap_or(true);

            if !needs_index {
                report.unchanged.push(path.clone());
                continue;
            }

            let is_new = !existing.contains_key(&norm);
            to_index.push((path.clone(), norm, hash, is_new));
        }

        // 6. Process files with a progress bar; chunk progress lives in the message field.
        if to_index.is_empty() {
            eprintln!("  Nothing to index ({} file(s) unchanged)", report.unchanged.len());
        } else {
            let pb = ProgressBar::new(to_index.len() as u64);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} files | {wide_msg} | ETA {eta}",
                )
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
                .progress_chars("█▓░"),
            );
            pb.enable_steady_tick(Duration::from_millis(80));

            for (path, norm, hash, is_new) in to_index {
                let display = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                pb.set_message(display.clone());

                match self.index_file(db, &path, &norm, &hash, &display, &pb) {
                    Ok(chunk_count) => {
                        report.total_chunks += chunk_count;
                        if let Ok(meta) = path.metadata() {
                            report.total_bytes += meta.len();
                        }
                        if is_new {
                            report.added.push(path);
                        } else {
                            report.modified.push(path);
                        }
                    }
                    Err(e) => report.errors.push((path, e.to_string())),
                }
                pb.inc(1);
            }

            pb.finish_and_clear();
        }

        report.elapsed = start.elapsed();
        Ok(report)
    }

    fn index_file(
        &self,
        db: &Database,
        path: &Path,
        norm_path: &str,
        hash: &str,
        display: &str,
        pb: &ProgressBar,
    ) -> Result<usize> {
        let content = std::fs::read_to_string(path)?;
        let file_size = content.len() as i64;
        let title = extract_title(&content);

        // Chunk first — no DB writes yet.
        let chunks = chunk_markdown(
            &content,
            self.max_chunk_tokens * 4,
            self.chunk_overlap_chars,
        );
        let total_chunks = chunks.len();

        if total_chunks == 0 {
            // Document is too small to produce chunks; record it so we skip it next run.
            db.delete_document(norm_path)?;
            db.insert_document(norm_path, hash, title.as_deref(), file_size)?;
            return Ok(0);
        }

        // Show total chunk count immediately — before the slow embedding starts.
        pb.set_message(format!("{display}  ·  0/{total_chunks} chunks"));

        // Embed all chunks in batches (the slow part — no DB writes here).
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(total_chunks);

        for batch_start in (0..total_chunks).step_by(EMBED_BATCH_SIZE) {
            let batch_end = (batch_start + EMBED_BATCH_SIZE).min(total_chunks);
            pb.set_message(format!(
                "{display}  ·  {}/{total_chunks} chunks  [embedding…]",
                all_embeddings.len()
            ));
            let batch_emb =
                self.embedding_engine.embed_documents(&texts[batch_start..batch_end])?;
            all_embeddings.extend(batch_emb);
            pb.set_message(format!(
                "{display}  ·  {}/{total_chunks} chunks",
                all_embeddings.len()
            ));
        }

        // Persist everything in one transaction: delete old, insert doc + all chunks.
        // If anything fails the whole operation rolls back — no orphaned records.
        db.begin()?;
        let write_result = (|| -> Result<()> {
            db.delete_document(norm_path)?;
            let doc_id = db.insert_document(norm_path, hash, title.as_deref(), file_size)?;
            for (i, (chunk, embedding)) in
                chunks.iter().zip(all_embeddings.iter()).enumerate()
            {
                db.insert_chunk(
                    doc_id,
                    &chunk.content,
                    &chunk.heading_context,
                    chunk.start_byte,
                    chunk.end_byte,
                    i,
                    embedding,
                )?;
            }
            Ok(())
        })();
        match write_result {
            Ok(()) => db.commit()?,
            Err(e) => {
                db.rollback();
                return Err(e);
            }
        }

        pb.set_message(display.to_string());
        Ok(total_chunks)
    }
}

// ---------------------------------------------------------------------------
// Markdown chunker
// ---------------------------------------------------------------------------

/// Chunk a markdown document using heading-aware splitting.
pub fn chunk_markdown(content: &str, max_chars: usize, overlap_chars: usize) -> Vec<Chunk> {
    // Build heading-aware sections using pulldown-cmark.
    let sections = extract_sections(content);

    let mut chunks: Vec<Chunk> = Vec::new();

    for section in sections {
        let text = section.text.trim().to_string();
        if text.is_empty() {
            continue;
        }

        if text.len() <= max_chars {
            // Section fits in one chunk.
            if text.len() < MIN_CHUNK_CHARS && !chunks.is_empty() {
                // Merge tiny section into previous chunk.
                if let Some(last) = chunks.last_mut() {
                    last.content.push('\n');
                    last.content.push_str(&text);
                    last.end_byte = section.end_byte;
                }
            } else {
                chunks.push(Chunk {
                    content: text,
                    heading_context: section.heading_context.clone(),
                    start_byte: section.start_byte,
                    end_byte: section.end_byte,
                });
            }
        } else {
            // Split oversized section.
            let sub_chunks = split_text(
                &text,
                max_chars,
                overlap_chars,
                section.start_byte,
                &section.heading_context,
            );
            chunks.extend(sub_chunks);
        }
    }

    chunks
}

// ---------------------------------------------------------------------------
// Section extraction via pulldown-cmark
// ---------------------------------------------------------------------------

struct Section {
    text: String,
    heading_context: String,
    start_byte: usize,
    end_byte: usize,
}

fn extract_sections(content: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new(); // (level, text)
    let mut current_text = String::new();
    let mut current_heading_ctx = String::new();
    let mut section_start = 0usize;

    // pulldown-cmark events with byte offsets via into_offset_iter().
    let parser_with_offsets = Parser::new_ext(content, CmarkOptions::empty())
        .into_offset_iter();

    let mut in_heading = false;
    let mut heading_level: u8 = 0;
    let mut heading_text_buf = String::new();

    for (event, range) in parser_with_offsets {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                // Flush current section.
                let trimmed = current_text.trim().to_string();
                if !trimmed.is_empty() {
                    sections.push(Section {
                        text: trimmed,
                        heading_context: current_heading_ctx.clone(),
                        start_byte: section_start,
                        end_byte: range.start,
                    });
                }
                current_text.clear();
                section_start = range.start;
                in_heading = true;
                heading_level = heading_level_to_u8(level);
                heading_text_buf.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if in_heading {
                    in_heading = false;
                    let htext = heading_text_buf.trim().to_string();

                    // Update heading stack: pop headings at same or deeper level.
                    while heading_stack.last().map(|(l, _)| *l >= heading_level).unwrap_or(false)
                    {
                        heading_stack.pop();
                    }
                    heading_stack.push((heading_level, htext.clone()));

                    // Rebuild heading context from stack.
                    current_heading_ctx = heading_stack
                        .iter()
                        .map(|(_, t)| t.as_str())
                        .collect::<Vec<_>>()
                        .join(" > ");

                    // Include heading text in the section body so it is searchable
                    // and so heading-only documents still produce chunks.
                    if !htext.is_empty() {
                        if !current_text.is_empty() {
                            current_text.push('\n');
                        }
                        current_text.push_str(&htext);
                    }
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if in_heading {
                    heading_text_buf.push_str(&text);
                } else {
                    if !current_text.is_empty() {
                        current_text.push(' ');
                    }
                    current_text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if !in_heading {
                    current_text.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !in_heading {
                    current_text.push_str("\n\n");
                }
            }
            _ => {}
        }
    }

    // Flush last section.
    let trimmed = current_text.trim().to_string();
    if !trimmed.is_empty() {
        sections.push(Section {
            text: trimmed,
            heading_context: current_heading_ctx,
            start_byte: section_start,
            end_byte: content.len(),
        });
    }

    sections
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ---------------------------------------------------------------------------
// Text splitting (paragraph → sentence → word fallback)
// ---------------------------------------------------------------------------

fn split_text(
    text: &str,
    max_chars: usize,
    overlap_chars: usize,
    base_byte_offset: usize,
    heading_context: &str,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    // Try paragraph boundaries first.
    let paragraphs: Vec<&str> = text.split("\n\n").filter(|s| !s.trim().is_empty()).collect();

    let mut current = String::new();
    let mut current_start = base_byte_offset;

    for para in &paragraphs {
        let sep_len = if current.is_empty() { 0 } else { 2 };
        if current.len() + sep_len + para.len() <= max_chars {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        } else {
            if !current.is_empty() {
                let end_byte = current_start + current.len();
                chunks.push(Chunk {
                    content: current.trim().to_string(),
                    heading_context: heading_context.to_string(),
                    start_byte: current_start,
                    end_byte,
                });

                // Overlap: take last `overlap_chars` chars from current.
                let overlap = if current.len() > overlap_chars {
                    current[current.floor_char_boundary(current.len() - overlap_chars)..].to_string()
                } else {
                    current.clone()
                };
                current_start = end_byte.saturating_sub(overlap_chars);
                current = overlap;
            }

            if para.len() > max_chars {
                // Paragraph itself is too long — split by sentences.
                let sub = split_by_sentence(para, max_chars, overlap_chars, current_start, heading_context);
                chunks.extend(sub);
                current.clear();
            } else {
                current = para.to_string();
            }
        }
    }

    if !current.trim().is_empty() {
        chunks.push(Chunk {
            content: current.trim().to_string(),
            heading_context: heading_context.to_string(),
            start_byte: current_start,
            end_byte: base_byte_offset + text.len(),
        });
    }

    chunks
}

fn split_by_sentence(
    text: &str,
    max_chars: usize,
    overlap_chars: usize,
    base_byte_offset: usize,
    heading_context: &str,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_start = base_byte_offset;

    // Naive sentence split on ". ", "! ", "? ".
    let mut remaining = text;
    while !remaining.is_empty() {
        let end = find_sentence_boundary(remaining, max_chars);
        let sentence = &remaining[..end];
        remaining = remaining[end..].trim_start();

        let sep_len = if current.is_empty() { 0 } else { 1 };
        if current.len() + sep_len + sentence.len() <= max_chars {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(sentence);
        } else {
            if !current.is_empty() {
                chunks.push(Chunk {
                    content: current.trim().to_string(),
                    heading_context: heading_context.to_string(),
                    start_byte: current_start,
                    end_byte: current_start + current.len(),
                });
                let overlap = if current.len() > overlap_chars {
                    current[current.floor_char_boundary(current.len() - overlap_chars)..].to_string()
                } else {
                    current.clone()
                };
                current_start += current.len().saturating_sub(overlap_chars);
                current = overlap;
            }

            if sentence.len() > max_chars {
                // Sentence too long — word split.
                let sub = split_by_word(sentence, max_chars, overlap_chars, current_start, heading_context);
                chunks.extend(sub);
                current.clear();
            } else {
                current = sentence.to_string();
            }
        }
    }

    if !current.trim().is_empty() {
        chunks.push(Chunk {
            content: current.trim().to_string(),
            heading_context: heading_context.to_string(),
            start_byte: current_start,
            end_byte: base_byte_offset + text.len(),
        });
    }

    chunks
}

fn find_sentence_boundary(text: &str, max_chars: usize) -> usize {
    // Clamp to a valid char boundary so we never slice mid-codepoint.
    let cap = text.floor_char_boundary(max_chars.min(text.len()));
    let search_in = &text[..cap];
    // Find the last '. ', '! ', '? ' within cap.
    for pattern in ["! ", "? ", ". "] {
        if let Some(pos) = search_in.rfind(pattern) {
            return pos + pattern.len();
        }
    }
    // No sentence boundary — use the whole cap.
    cap
}

fn split_by_word(
    text: &str,
    max_chars: usize,
    overlap_chars: usize,
    base_byte_offset: usize,
    heading_context: &str,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut current = String::new();
    let mut current_start = base_byte_offset;

    for word in words {
        let sep_len = if current.is_empty() { 0 } else { 1 };
        if current.len() + sep_len + word.len() <= max_chars {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        } else {
            if !current.is_empty() {
                chunks.push(Chunk {
                    content: current.trim().to_string(),
                    heading_context: heading_context.to_string(),
                    start_byte: current_start,
                    end_byte: current_start + current.len(),
                });
                let overlap = if current.len() > overlap_chars {
                    current[current.floor_char_boundary(current.len() - overlap_chars)..].to_string()
                } else {
                    current.clone()
                };
                current_start += current.len().saturating_sub(overlap_chars);
                current = overlap;
            }
            // Add a separator if there's overlap text already in current.
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
    }

    if !current.trim().is_empty() {
        chunks.push(Chunk {
            content: current.trim().to_string(),
            heading_context: heading_context.to_string(),
            start_byte: current_start,
            end_byte: base_byte_offset + text.len(),
        });
    }

    chunks
}

// ---------------------------------------------------------------------------
// File utilities
// ---------------------------------------------------------------------------

fn discover_markdown_files(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = if recursive {
        WalkDir::new(root)
    } else {
        WalkDir::new(root).max_depth(1)
    };

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let content = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    Ok(hex::encode(hasher.finalize()))
}

/// Normalise to a forward-slash path string for stable DB storage.
fn normalise_path(path: &Path) -> Option<String> {
    path.to_str().map(|s| s.replace('\\', "/"))
}

/// Extract the first H1 title from markdown content.
fn extract_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // extract_title
    // -----------------------------------------------------------------------

    #[test]
    fn extract_title_h1_present() {
        let content = "# Hello World\n\nSome text.";
        assert_eq!(extract_title(content), Some("Hello World".to_string()));
    }

    #[test]
    fn extract_title_no_heading() {
        let content = "Some plain text\nwithout any headings.";
        assert_eq!(extract_title(content), None);
    }

    #[test]
    fn extract_title_h2_only() {
        let content = "## Secondary Heading\n\nText here.";
        assert_eq!(extract_title(content), None);
    }

    #[test]
    fn extract_title_multiple_h1_returns_first() {
        let content = "# First\n\n# Second\n\nText.";
        assert_eq!(extract_title(content), Some("First".to_string()));
    }

    #[test]
    fn extract_title_indented_h1() {
        // line.trim() strips leading whitespace before the prefix check
        let content = "  # Indented Title\n\nBody.";
        assert_eq!(extract_title(content), Some("Indented Title".to_string()));
    }

    // -----------------------------------------------------------------------
    // normalise_path
    // -----------------------------------------------------------------------

    #[test]
    fn normalise_path_forward_slashes_unchanged() {
        let path = std::path::Path::new("docs/guide/intro.md");
        assert_eq!(normalise_path(path), Some("docs/guide/intro.md".to_string()));
    }

    #[test]
    fn normalise_path_backslash_converted() {
        // On Linux '\\' is a valid filename character; normalise_path replaces it.
        let path = std::path::Path::new("docs\\guide\\intro.md");
        assert_eq!(normalise_path(path), Some("docs/guide/intro.md".to_string()));
    }

    // -----------------------------------------------------------------------
    // chunk_markdown
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_markdown_empty_document() {
        assert!(chunk_markdown("", 1000, 100).is_empty());
    }

    #[test]
    fn chunk_markdown_whitespace_only() {
        assert!(chunk_markdown("   \n\n\t\n  ", 1000, 100).is_empty());
    }

    #[test]
    fn chunk_markdown_single_short_paragraph() {
        // Content is longer than MIN_CHUNK_CHARS so it becomes its own chunk.
        let content =
            "This is a paragraph that has more than fifty characters in total.";
        let chunks = chunk_markdown(content, 1000, 100);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("paragraph"));
    }

    #[test]
    fn chunk_markdown_heading_only_no_body() {
        let content = "# My Title\n";
        let chunks = chunk_markdown(content, 1000, 100);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("My Title"));
        assert_eq!(chunks[0].heading_context, "My Title");
    }

    #[test]
    fn chunk_markdown_multiple_headings_correct_heading_context() {
        let content = concat!(
            "# Section A\n\n",
            "Content for section A that is long enough to be its own standalone chunk here.\n\n",
            "## Section B\n\n",
            "Content for section B that is also long enough to be its own standalone chunk.",
        );
        let chunks = chunk_markdown(content, 1000, 100);
        assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());
        assert!(
            chunks.iter().any(|c| c.heading_context == "Section A"),
            "expected a chunk with heading_context 'Section A'"
        );
        assert!(
            chunks.iter().any(|c| c.heading_context.contains("Section B")),
            "expected a chunk with 'Section B' in heading_context"
        );
    }

    #[test]
    fn chunk_markdown_nested_headings_hierarchical_context() {
        let content = concat!(
            "# A\n\nContent for A that is long enough to stand alone as its own chunk here.\n\n",
            "## B\n\nContent for B that is long enough to stand alone as its own chunk here.\n\n",
            "### C\n\nDeep content for section C should carry the full hierarchical context.",
        );
        let chunks = chunk_markdown(content, 1000, 100);
        let deep = chunks.iter().find(|c| c.content.contains("Deep content"));
        assert!(deep.is_some(), "should have chunk with deep content");
        assert_eq!(deep.unwrap().heading_context, "A > B > C");
    }

    #[test]
    fn chunk_markdown_heading_level_reset_pops_stack() {
        let content = concat!(
            "# A\n\nContent for A that is long enough to be a standalone chunk on its own.\n\n",
            "## B\n\nContent for B that is long enough to be a standalone chunk on its own.\n\n",
            "# C\n\nContent after the level reset should have just C as its heading context.",
        );
        let chunks = chunk_markdown(content, 1000, 100);
        let chunk_c = chunks.iter().find(|c| c.content.contains("level reset"));
        assert!(chunk_c.is_some(), "should have chunk for section C");
        assert_eq!(chunk_c.unwrap().heading_context, "C");
    }

    #[test]
    fn chunk_markdown_long_paragraph_split_into_multiple_chunks() {
        // ~640 chars of repeated text, split at max_chars=200
        let sentence = "This is a fairly long sentence that adds up to many characters total. ";
        let content = sentence.repeat(9);
        let max_chars = 200;
        let chunks = chunk_markdown(&content, max_chars, 50);
        assert!(chunks.len() > 1, "long content should be split into multiple chunks");
        for chunk in &chunks {
            // Allow a little slack for the overlap window
            assert!(
                chunk.content.len() <= max_chars + 60,
                "chunk too large: {} chars",
                chunk.content.len()
            );
        }
    }

    #[test]
    fn chunk_markdown_very_long_sentence_falls_back_to_word_split() {
        // 100 words with no sentence-boundary punctuation
        let content = vec!["word"; 100].join(" ");
        let chunks = chunk_markdown(&content, 50, 10);
        assert!(chunks.len() > 1, "very long sentence should be word-split");
    }

    #[test]
    fn chunk_markdown_adjacent_chunks_have_byte_overlap() {
        let content = "Alpha beta gamma delta epsilon. ".repeat(20);
        let chunks = chunk_markdown(&content, 100, 50);
        if chunks.len() >= 2 {
            // The split logic sets chunk[n+1].start_byte = chunk[n].end_byte - overlap_chars
            assert!(
                chunks[0].end_byte >= chunks[1].start_byte,
                "expected byte overlap: first.end={} >= second.start={}",
                chunks[0].end_byte,
                chunks[1].start_byte
            );
        }
    }

    #[test]
    fn chunk_markdown_tiny_section_merged_into_previous() {
        // "Short." (6 chars) is < MIN_CHUNK_CHARS=50 and there is a prior chunk → merges.
        let content = concat!(
            "# Section\n\n",
            "This is a normal section with plenty of content to stand alone as its own chunk.\n\n",
            "## Tiny\n\n",
            "Short.",
        );
        let chunks = chunk_markdown(content, 1000, 100);
        assert!(
            chunks.iter().any(|c| c.content.contains("Short.")),
            "merged content should still be present"
        );
        // Due to merge, total chunks should be small
        assert!(chunks.len() <= 3, "tiny section should merge, got {} chunks", chunks.len());
    }

    #[test]
    fn chunk_markdown_byte_offsets_within_bounds() {
        let content = "# Title\n\nSome content here.\n\n## Section\n\nMore content.";
        let chunks = chunk_markdown(content, 1000, 100);
        let doc_len = content.len();
        for chunk in &chunks {
            assert!(chunk.start_byte <= doc_len, "start_byte out of bounds");
            assert!(chunk.end_byte <= doc_len, "end_byte out of bounds");
            assert!(chunk.start_byte <= chunk.end_byte, "start_byte > end_byte");
        }
    }
}
