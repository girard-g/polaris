use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        Implementation, Meta, ProgressNotificationParam, ServerCapabilities, ServerInfo, ToolsCapability,
    },
    tool, tool_handler, tool_router,
    transport::stdio,
};

use polaris_core::config::PolarisConfig;
use polaris_core::error::PolarisError;
use polaris_core::search::SearchEngine;

use super::types::{IndexParams, SearchParams, StatusParams};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Shared state for the MCP server.
///
/// `Bank` is cheaply cloneable (`Arc<BankInner>` internally) and serialises
/// concurrent access through its internal `Mutex<Database>`. MCP tool calls
/// are typically serial so this single-connection model is acceptable.
#[derive(Clone)]
pub struct PolarisState {
    pub config: Arc<PolarisConfig>,
    pub bank: polaris_core::Bank,
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
        let bank = self.state.bank.clone();

        let result = tokio::task::spawn_blocking(move || {
            match bank.search(&query, polaris_core::SearchOpts { top_k }) {
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
    async fn index(
        &self,
        Parameters(params): Parameters<IndexParams>,
        peer: Peer<RoleServer>,
        meta: Meta,
    ) -> String {
        let path = PathBuf::from(&params.path);
        let recursive = params.recursive.unwrap_or(true);
        let force = params.force.unwrap_or(false);
        let bank = self.state.bank.clone();

        if !path.exists() {
            return format!("Error: path not found: {}", params.path);
        }

        let progress_token = meta.get_progress_token();
        let handle = tokio::runtime::Handle::current();

        let on_progress: Option<Box<dyn Fn(f32, &str) + Send + Sync>> =
            if let Some(token) = progress_token {
                Some(Box::new(move |fraction: f32, message: &str| {
                    let token = token.clone();
                    let peer = peer.clone();
                    let msg = message.to_string();
                    handle.block_on(async move {
                        let _ = peer.notify_progress(ProgressNotificationParam {
                            progress_token: token,
                            progress: (fraction * 100.0) as f64,
                            total: Some(100.0),
                            message: Some(msg),
                        }).await;
                    });
                }))
            } else {
                None
            };

        let opts = polaris_core::IndexOpts { recursive, force, dry_run: false };

        let result = tokio::task::spawn_blocking(move || {
            let index_result = match on_progress {
                Some(cb) => bank.index_path_with_progress(&path, opts, cb),
                None => bank.index_path(&path, opts),
            };
            match index_result {
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
        let config = Arc::clone(&self.state.config);
        let bank = self.state.bank.clone();

        let result = tokio::task::spawn_blocking(move || {
            match bank.stats() {
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
