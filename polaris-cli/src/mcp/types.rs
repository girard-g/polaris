use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Search tool
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// The search query.
    pub query: String,
    /// Number of results to return (default: 5).
    pub top_k: Option<u32>,
}

// ---------------------------------------------------------------------------
// Index tool
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IndexParams {
    /// Path to directory or file to index.
    pub path: String,
    /// Recursively index subdirectories (default: true).
    pub recursive: Option<bool>,
    /// Re-index all files even if unchanged (default: false).
    pub force: Option<bool>,
}

// ---------------------------------------------------------------------------
// Status tool (no params)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {}
