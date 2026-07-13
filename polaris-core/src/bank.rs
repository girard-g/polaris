//! Single-bank facade over the retrieval pipeline.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{IndexOpts, SearchOpts};
use crate::db::{ChunkRecord, Database, DbStats, SearchResult};
use crate::embedding::SharedEmbedding;
use crate::error::{PolarisError, Result};
use crate::indexer::{IndexReport, Indexer, WorkItem};
use crate::search::SearchEngine;

/// Configuration for opening a single bank.
///
/// All tuning parameters have sensible defaults matching the historical Polaris
/// CLI defaults. Use `..Default::default()` to fill in only what differs.
#[derive(Debug, Clone)]
pub struct BankConfig {
    /// Root of the indexed corpus on disk (e.g. a git working tree).
    pub repo_root: PathBuf,
    /// Where the SQLite index file lives. Should be inside `repo_root` and gitignored.
    pub index_path: PathBuf,
    /// Embedding vector dimension. Must match the bank's pinned dimension.
    pub embedding_dim: usize,
    /// Embedding model id (e.g. "nomic-embed-text-v1.5").
    pub model_id: String,
    // ── Tuning parameters ─────────────────────────────────────────────────
    /// Maximum chunk size in approximate tokens (chars / 4).
    pub max_chunk_tokens: usize,
    /// Overlap in characters between adjacent non-heading chunks.
    pub chunk_overlap_chars: usize,
    /// Maximum file size in bytes; larger files are skipped by the indexer.
    pub max_file_size: u64,
    /// MMR lambda: 0.0 = pure diversity, 1.0 = pure relevance.
    pub mmr_lambda: f32,
    /// Fetch `top_k * mmr_candidate_multiplier` candidates before MMR reranking.
    pub mmr_candidate_multiplier: usize,
    /// Additive score boost for heading matches (0.0 disables).
    pub heading_boost: f32,
    /// RRF k constant for Reciprocal Rank Fusion.
    pub rrf_k: usize,
}

impl Default for BankConfig {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::new(),
            index_path: PathBuf::new(),
            embedding_dim: 512,
            model_id: "nomic-embed-text-v1.5".to_string(),
            max_chunk_tokens: 450,
            chunk_overlap_chars: 200,
            max_file_size: 10 * 1024 * 1024, // 10 MiB — matches PolarisConfig default
            mmr_lambda: 0.7,
            mmr_candidate_multiplier: 3,
            heading_boost: 0.05,
            rrf_k: 60,
        }
    }
}

/// One document whose Markdown is supplied in memory rather than read from disk.
///
/// Fed to [`Bank::index_documents`] by an external ingestion crate (e.g. after
/// converting a PDF/DOCX to Markdown). The caller owns discovery, conversion and
/// hashing; the document is stored under its original `source_path`.
pub struct InMemoryDoc {
    /// Original source path, e.g. `docs/manual.pdf` (stored verbatim, not a temp path).
    pub source_path: PathBuf,
    /// Already-converted Markdown content.
    pub markdown: String,
    /// Caller-computed content hash, used for skip-unchanged detection.
    pub hash: String,
    /// Optional document title.
    pub title: Option<String>,
}

/// A single bank: one indexed corpus with its own SQLite index.
///
/// `Bank` is cheap to clone — the clone shares the underlying `Arc<BankInner>`.
#[derive(Clone)]
pub struct Bank {
    inner: std::sync::Arc<BankInner>,
}

struct BankInner {
    db: Mutex<Database>,
    indexer: Indexer,
    config: BankConfig,
    // Hyper-parameters used by SearchEngine. Pulled from PolarisConfig defaults.
    mmr_lambda: f32,
    mmr_candidate_multiplier: usize,
    heading_boost: f32,
    rrf_k: usize,
}

impl Bank {
    /// Open or create a bank.
    ///
    /// `embed` provides the shared embedding model. The model is not reloaded;
    /// cloning a [`SharedEmbedding`] is cheap (just an `Arc` clone).
    pub fn open(cfg: BankConfig, embed: SharedEmbedding) -> Result<Self> {
        crate::db::register_vec_extension();

        // Create parent directory for the index file if it does not exist.
        if let Some(parent) = cfg.index_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                PolarisError::Indexing(format!(
                    "cannot create index directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let db = Database::open(&cfg.index_path, cfg.embedding_dim, &cfg.model_id)?;

        let indexer = Indexer::new(
            embed.0.clone(),
            cfg.max_chunk_tokens,
            cfg.chunk_overlap_chars,
            cfg.max_file_size,
        );

        Ok(Self {
            inner: std::sync::Arc::new(BankInner {
                db: Mutex::new(db),
                indexer,
                mmr_lambda: cfg.mmr_lambda,
                mmr_candidate_multiplier: cfg.mmr_candidate_multiplier,
                heading_boost: cfg.heading_boost,
                rrf_k: cfg.rrf_k,
                config: cfg,
            }),
        })
    }

    /// Index a path (file or directory) into this bank.
    pub fn index_path(&self, path: &Path, opts: IndexOpts) -> Result<IndexReport> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        self.inner.indexer.index_path(
            &db,
            path,
            opts.recursive,
            opts.force,
            opts.dry_run,
            None,
        )
    }

    /// Index a path with a progress callback.
    ///
    /// The callback receives `(fraction, message)` where `fraction` is in `[0, 1]`.
    /// Used by the MCP `index` tool to stream progress notifications to the client.
    pub fn index_path_with_progress(
        &self,
        path: &Path,
        opts: IndexOpts,
        on_progress: Box<dyn Fn(f32, &str) + Send + Sync>,
    ) -> Result<IndexReport> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        self.inner.indexer.index_path(
            &db,
            path,
            opts.recursive,
            opts.force,
            opts.dry_run,
            Some(on_progress),
        )
    }

    /// Search this bank.
    ///
    /// Scores are normalized to [0, 1] within this result set (top result = 1.0).
    pub fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<SearchResult>> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        let engine = self.engine(&db);
        engine.search(query, opts.top_k)
    }

    /// Like [`search`], but returns raw (unnormalized) RRF scores.
    ///
    /// Used by [`BankSet`] to fuse results from multiple banks with a single
    /// cross-bank normalization pass instead of per-bank normalization.
    pub(crate) fn search_raw(&self, query: &str, opts: SearchOpts) -> Result<Vec<SearchResult>> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        let engine = self.engine(&db);
        engine.search_raw(query, opts.top_k)
    }

    /// Expand a search hit into a reading-order context window.
    ///
    /// Keyed by `chunk_id` (the [`SearchResult::chunk_id`] of a prior hit) — the
    /// caller never needs the chunk's positional index. Returns the target chunk
    /// plus up to `radius` neighbor chunks on each side **from the same
    /// document**, in document order, joined by blank lines and capped at
    /// `max_chars` characters (char boundary). Neighbors are kept whole or
    /// dropped outermost-first; only the target itself is ever truncated.
    ///
    /// Returns `Err` if `chunk_id` is not present in this bank. Synchronous;
    /// call via `spawn_blocking` from async contexts, like the other methods.
    pub fn chunk_window(&self, chunk_id: i64, radius: usize, max_chars: usize) -> Result<String> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        db.chunk_window(chunk_id, radius, max_chars)
    }

    /// Append one row to the search log.
    ///
    /// Used by the CLI and MCP server to record per-search counters.
    /// Library users who do not need savings tracking can ignore this method.
    pub fn log_search(
        &self,
        source: crate::db::LogSource,
        query: &str,
        top_k: usize,
        result_bytes: usize,
        baseline_bytes: usize,
        ts: i64,
    ) -> Result<()> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        db.insert_search_log(ts, source, query, top_k, result_bytes, baseline_bytes)
    }

    fn engine<'a>(&'a self, db: &'a Database) -> SearchEngine<'a> {
        SearchEngine::new(
            self.inner.indexer.embedding_engine(),
            db,
            self.inner.mmr_lambda,
            self.inner.mmr_candidate_multiplier,
            self.inner.heading_boost,
            self.inner.rrf_k,
        )
    }

    /// Index statistics.
    pub fn stats(&self) -> Result<DbStats> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        db.get_stats(&self.inner.config.index_path)
    }

    /// Return chunks for a given indexed file path (debug helper).
    ///
    /// `rel_path` should be a path that was used during indexing (relative or
    /// normalised form). The path is normalised before the DB lookup.
    pub fn chunks_for(&self, rel_path: &Path) -> Result<Vec<ChunkRecord>> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        let norm = crate::indexer::normalise_path(rel_path).ok_or_else(|| {
            PolarisError::Indexing(format!("invalid path: {}", rel_path.display()))
        })?;
        db.get_chunks_for_document(&norm)
    }

    /// Index a precomputed delta (typically from a git diff).
    ///
    /// `changed` paths are added or modified; `removed` paths are deleted from the index.
    /// The caller is responsible for filtering paths (e.g. to `.md` files only).
    /// Skips the filesystem walk performed by [`Bank::index_path`]; used by git-driven
    /// sync to avoid re-walking the whole corpus on each pull.
    pub fn index_diff(
        &self,
        changed: &[PathBuf],
        removed: &[PathBuf],
    ) -> Result<IndexReport> {
        let started = Instant::now();
        let db = self.inner.db.lock().expect("bank db poisoned");
        let mut report = IndexReport::default();

        // 1. Removals: delete each document (cascades chunks + FTS + vec_chunks).
        for path in removed {
            let norm = match crate::indexer::normalise_path(path) {
                Some(n) => n,
                None => {
                    report.errors.push((
                        path.clone(),
                        format!("non-UTF-8 path: {}", path.display()),
                    ));
                    continue;
                }
            };
            match db.delete_document(&norm) {
                Ok(_) => report.removed.push(path.clone()),
                Err(e) => report.errors.push((path.clone(), e.to_string())),
            }
        }

        // 2. Changed files: delegate to the item-based pipeline (preserves cross-file batching).
        if !changed.is_empty() {
            let items: Vec<WorkItem> = changed.iter().cloned().map(WorkItem::from_path).collect();
            let sub = self
                .inner
                .indexer
                .index_files_items(&db, &items, false, false, None)?;
            // Merge sub-report, preserving our own `removed` list.
            report.added.extend(sub.added);
            report.modified.extend(sub.modified);
            report.unchanged.extend(sub.unchanged);
            report.errors.extend(sub.errors);
            report.total_chunks += sub.total_chunks;
            report.total_bytes += sub.total_bytes;
        }

        report.elapsed = started.elapsed();
        Ok(report)
    }

    /// Index already-generated Markdown supplied in memory, and delete `removed`
    /// paths, through the same chunk→embed→store→skip-unchanged pipeline as
    /// [`Bank::index_diff`] — nothing is read from disk.
    ///
    /// The caller owns discovery, conversion and hashing (see [`InMemoryDoc`]).
    /// Each document is stored under its original `source_path`. Used by the
    /// `polaris-pro` ingestion crate to feed converted PDFs/DOCX/etc. into the
    /// pipeline without a temp file.
    pub fn index_documents(
        &self,
        docs: Vec<InMemoryDoc>,
        removed: &[PathBuf],
    ) -> Result<IndexReport> {
        let started = Instant::now();
        let db = self.inner.db.lock().expect("bank db poisoned");
        let mut report = IndexReport::default();

        // 1. Removals: delete each document (cascades chunks + FTS + vec_chunks).
        for path in removed {
            let norm = match crate::indexer::normalise_path(path) {
                Some(n) => n,
                None => {
                    report.errors.push((
                        path.clone(),
                        format!("non-UTF-8 path: {}", path.display()),
                    ));
                    continue;
                }
            };
            match db.delete_document(&norm) {
                Ok(_) => report.removed.push(path.clone()),
                Err(e) => report.errors.push((path.clone(), e.to_string())),
            }
        }

        // 2. Supplied docs → in-memory work items → shared Phase A/B/C pipeline.
        if !docs.is_empty() {
            let items: Vec<WorkItem> = docs
                .into_iter()
                .map(|d| WorkItem {
                    path: d.source_path,
                    content: Some(d.markdown),
                    hash: Some(d.hash),
                    title: d.title,
                })
                .collect();
            let sub = self
                .inner
                .indexer
                .index_files_items(&db, &items, false, false, None)?;
            report.added.extend(sub.added);
            report.modified.extend(sub.modified);
            report.unchanged.extend(sub.unchanged);
            report.errors.extend(sub.errors);
            report.total_chunks += sub.total_chunks;
            report.total_bytes += sub.total_bytes;
        }

        report.elapsed = started.elapsed();
        Ok(report)
    }

    /// Configured repo root.
    pub fn repo_root(&self) -> &Path {
        &self.inner.config.repo_root
    }
}

/// A set of banks searched as one fused result.
pub struct BankSet {
    embed: SharedEmbedding,
    banks: Vec<(String, Bank)>,
}

impl BankSet {
    pub fn new(embed: SharedEmbedding) -> Self {
        Self { embed, banks: Vec::new() }
    }

    pub fn mount(&mut self, bank: Bank, label: String) {
        self.banks.push((label, bank));
    }

    pub fn unmount(&mut self, label: &str) -> Option<Bank> {
        if let Some(pos) = self.banks.iter().position(|(l, _)| l == label) {
            Some(self.banks.remove(pos).1)
        } else {
            None
        }
    }

    pub fn labels(&self) -> Vec<&str> {
        self.banks.iter().map(|(l, _)| l.as_str()).collect()
    }

    /// Search across all mounted banks. Per-bank results are tagged with
    /// `source_db = label`, fused by score, truncated to `top_k`, and renormalized.
    pub fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<SearchResult>> {
        let mut all_results: Vec<SearchResult> = Vec::new();

        for (label, bank) in &self.banks {
            // Use raw (unnormalized) scores so that cross-bank comparison is
            // meaningful. Per-bank normalization would collapse every bank's top
            // result to 1.0, making the merged sort arbitrary.
            let mut results = bank.search_raw(query, opts.clone())?;
            for r in &mut results {
                r.source_db = Some(label.clone());
            }
            all_results.extend(results);
        }

        // Sort by score descending, take top_k.
        all_results.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(opts.top_k);

        // Renormalize so max score = 1.0 across the merged result set.
        if let Some(max_score) = all_results.first().map(|r| r.score) {
            if max_score > 0.0 {
                for r in &mut all_results {
                    r.score /= max_score;
                }
            }
        }

        // Avoid `embed` field warning; the SharedEmbedding is held so the
        // model stays loaded for as long as the set exists.
        let _ = &self.embed;

        Ok(all_results)
    }
}

#[cfg(test)]
mod tests_log_search {
    use super::*;
    use crate::db::LogSource;

    #[test]
    #[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
    fn bank_log_search_inserts_row_visible_via_aggregate() {
        crate::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("bank.db");

        let embed = crate::embedding::SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
        let bank = Bank::open(
            BankConfig {
                repo_root: dir.path().to_path_buf(),
                index_path: index_path.clone(),
                embedding_dim: 64,
                model_id: "nomic-embed-text-v1.5".into(),
                ..Default::default()
            },
            embed,
        ).unwrap();

        bank.log_search(LogSource::Cli, "q", 5, 200, 5_000, 1_700_000_000).unwrap();

        // Re-open the underlying DB to check the row landed.
        let db = crate::db::Database::open(&index_path, 64, "nomic-embed-text-v1.5").unwrap();
        let agg = db.aggregate_savings().unwrap();
        assert_eq!(agg.total_searches, 1);
        assert_eq!(agg.total_result_bytes, 200);
        assert_eq!(agg.total_baseline_bytes, 5_000);
    }
}
