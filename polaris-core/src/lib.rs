//! Polaris retrieval pipeline as a library.

pub mod bank;
pub mod config;
pub mod db;
pub mod embedding;
pub mod error;
pub mod indexer;
pub mod search;

pub use bank::{Bank, BankConfig, BankSet};
pub use config::{IndexOpts, PolarisConfig, SearchOpts};
pub use db::{
    ChunkRecord, Database, DbStats, LogSource, SavingsAggregate, SavingsBySource,
    SavingsCounters, SearchLogRow, SearchResult, register_vec_extension,
};
pub use embedding::{EmbeddingEngine, SharedEmbedding, native_dim_for};
pub use error::{PolarisError, Result};
pub use indexer::{Chunk, IndexReport, Indexer, normalise_path};
pub use search::SearchEngine;
