//! Single-bank facade over the retrieval pipeline.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    pub fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<SearchResult>> {
        let db = self.inner.db.lock().expect("bank db poisoned");
        let engine = SearchEngine::new(
            self.inner.indexer.embedding_engine(),
            &db,
            self.inner.mmr_lambda,
            self.inner.mmr_candidate_multiplier,
            self.inner.heading_boost,
            self.inner.rrf_k,
        );
        engine.search(query, opts.top_k)
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

    /// Configured repo root.
    pub fn repo_root(&self) -> &Path {
        &self.inner.config.repo_root
    }
}
