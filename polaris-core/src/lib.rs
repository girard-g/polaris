//! Polaris retrieval pipeline as a library.
//!
//! See `docs/superpowers/specs/2026-04-26-polaris-core-crate-extraction.md`
//! for design rationale.

pub mod db;
pub mod error;

pub use db::{Database, SearchResult, register_vec_extension};
pub use error::{PolarisError, Result};
