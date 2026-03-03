use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        Implementation, ServerCapabilities, ServerInfo, ToolsCapability,
    },
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::config::PolarisConfig;
use crate::db::Database;
use crate::embedding::EmbeddingEngine;
use crate::error::PolarisError;
use crate::indexer::Indexer;
use crate::search::SearchEngine;

use super::types::{IndexParams, SearchParams, StatusParams};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Shared state for the MCP server.
///
/// Two separate SQLite connections to the same file are used so that
/// read operations (search, status) and write operations (index) can proceed
/// concurrently under WAL mode without mutual serialisation.
#[derive(Clone)]
pub struct PolarisState {
    pub config: Arc<PolarisConfig>,
    pub embedding_engine: Arc<EmbeddingEngine>,
    /// Connection used exclusively by read operations (search, status).
    pub read_db: Arc<Mutex<Database>>,
    /// Connection used exclusively by write operations (index).
    pub write_db: Arc<Mutex<Database>>,
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PolarisServer {
    state: PolarisState,
    tool_router: ToolRouter<Self>,
}

impl PolarisServer {
    pub fn new(state: PolarisState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Start the MCP server over stdio.
    pub async fn serve_stdio(self) -> std::result::Result<(), PolarisError> {
        let service = self
            .serve(stdio())
            .await
            .map_err(|e| PolarisError::Mcp(format!("Serve error: {e}")))?;

        service
            .waiting()
            .await
            .map_err(|e| PolarisError::Mcp(format!("Wait error: {e}")))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Acquire a `Mutex` lock and return the guard, or format an error string on
/// poison. Used to deduplicate the lock-or-error pattern across tool handlers.
fn lock_db(db: &std::sync::Mutex<Database>) -> std::result::Result<std::sync::MutexGuard<'_, Database>, String> {
    db.lock().map_err(|e| format!("Error: failed to lock database: {e}"))
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl PolarisServer {
    /// Search the indexed documentation using semantic similarity.
    #[tool(
        name = "search",
        description = "Search indexed documentation using semantic similarity. Returns the most relevant chunks for the given query."
    )]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        let config = Arc::clone(&self.state.config);
        let top_k = (params.top_k.unwrap_or(5) as usize).min(config.max_top_k);
        let query = params.query;
        let db = Arc::clone(&self.state.read_db);
        let engine = Arc::clone(&self.state.embedding_engine);

        let result = tokio::task::spawn_blocking(move || {
            let db = match lock_db(&db) {
                Ok(d) => d,
                Err(e) => return e,
            };
            let search = SearchEngine::new(
                &engine, &db,
                config.mmr_lambda, config.mmr_candidate_multiplier,
                config.heading_boost, config.rrf_k,
            );
            match search.search(&query, top_k) {
                Ok(results) => SearchEngine::format_results(&results),
                Err(e) => format!("Error: {e}"),
            }
        }).await;

        result.unwrap_or_else(|e| format!("Error: task failed: {e}"))
    }

    /// Index markdown files from a directory or file path.
    #[tool(
        name = "index",
        description = "Index markdown files from a path. Supports recursive directory indexing and incremental updates."
    )]
    async fn index(&self, Parameters(params): Parameters<IndexParams>) -> String {
        let path = PathBuf::from(&params.path);
        let recursive = params.recursive.unwrap_or(true);
        let force = params.force.unwrap_or(false);
        let db = Arc::clone(&self.state.write_db);
        let engine = Arc::clone(&self.state.embedding_engine);
        let config = Arc::clone(&self.state.config);

        if !path.exists() {
            return format!("Error: path not found: {}", params.path);
        }

        let result = tokio::task::spawn_blocking(move || {
            let indexer = Indexer::new(
                engine,
                config.max_chunk_tokens,
                config.chunk_overlap_chars,
                config.max_file_size,
            );
            let db = match lock_db(&db) {
                Ok(d) => d,
                Err(e) => return e,
            };
            match indexer.index_path(&db, &path, recursive, force) {
                Ok(report) => {
                    let mut out = report.summary();
                    if !report.errors.is_empty() {
                        out.push_str("\n\nErrors:\n");
                        for (path, err) in &report.errors {
                            out.push_str(&format!("  - {}: {}\n", path.display(), err));
                        }
                    }
                    out
                }
                Err(e) => format!("Error: {e}"),
            }
        }).await;

        result.unwrap_or_else(|e| format!("Error: task failed: {e}"))
    }

    /// Get current status of the Polaris index.
    #[tool(
        name = "status",
        description = "Returns statistics about the current index: document count, chunk count, database size, and embedding configuration."
    )]
    async fn status(&self, _params: Parameters<StatusParams>) -> String {
        let db = Arc::clone(&self.state.read_db);
        let config = Arc::clone(&self.state.config);

        let result = tokio::task::spawn_blocking(move || {
            let db = match lock_db(&db) {
                Ok(d) => d,
                Err(e) => return e,
            };
            match db.get_stats(&config.db_path) {
                Ok(stats) => format!(
                    "Documents: {}\nChunks: {}\nDatabase size: {} bytes\nModel: {}\nEmbedding dim: {}\nLast indexed: {}",
                    stats.doc_count, stats.chunk_count, stats.db_size_bytes,
                    config.model_id,
                    stats.embedding_dim, stats.last_indexed.unwrap_or_else(|| "never".to_string()),
                ),
                Err(e) => format!("Error: {e}"),
            }
        }).await;

        result.unwrap_or_else(|e| format!("Error: task failed: {e}"))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PolarisServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "polaris".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: None,
                description: None,
                icons: None,
                website_url: None,
            },
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: None }),
                ..Default::default()
            },
            instructions: Some(
                "Polaris is a semantic search MCP server for project documentation. \
                 Use `search` to find relevant documentation chunks, `index` to add \
                 new files, and `status` to check the index health."
                    .to_string(),
            ),
            ..Default::default()
        }
    }
}
