//! Single-bank facade over the retrieval pipeline.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{IndexOpts, SearchOpts};
use crate::db::{ChunkRecord, Database, DbStats, SearchResult};
use crate::embedding::SharedEmbedding;
use crate::error::{PolarisError, Result};
use crate::indexer::{IndexReport, Indexer};
use crate::search::SearchEngine;

/// Configuration for opening a single bank.
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

        // Default chunking parameters mirror the historical CLI defaults.
        let max_chunk_tokens: usize = 450;
        let chunk_overlap_chars: usize = 200;
        let max_file_size: u64 = 5 * 1024 * 1024; // 5 MiB

        let indexer = Indexer::new(
            embed.0.clone(),
            max_chunk_tokens,
            chunk_overlap_chars,
            max_file_size,
        );

        Ok(Self {
            inner: std::sync::Arc::new(BankInner {
                db: Mutex::new(db),
                indexer,
                config: cfg,
                mmr_lambda: 0.7,
                mmr_candidate_multiplier: 3,
                heading_boost: 0.05,
                rrf_k: 60,
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

        // 2. Changed files: delegate to index_files (preserves cross-file batching).
        if !changed.is_empty() {
            let sub = self.inner.indexer.index_files(&db, changed, false, false, None)?;
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
