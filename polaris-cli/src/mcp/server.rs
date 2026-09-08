use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

use super::types::{EvalParams, IndexParams, SearchParams, StatusParams};

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
    /// One-line "newer version available" note, computed once at boot. `None`
    /// when up to date or the check is unavailable.
    update_note: Option<String>,
    /// Flips to `true` the first time the note is emitted this session.
    note_shown: Arc<AtomicBool>,
}

impl PolarisServer {
    pub fn new(state: PolarisState) -> Self {
        // Warm the cache for next session and read the current note in one pass
        // (returns None when checks are disabled, so no extra gate needed here).
        let update_note = crate::update_check::refresh_and_pending()
            .map(|v| format!("Polaris {v} available — run 'polaris update'."));
        Self {
            state,
            tool_router: Self::tool_router(),
            update_note,
            note_shown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// One-time session banner suffix, `""` after the first emission.
    fn session_banner(&self) -> String {
        crate::update_check::banner_once(&self.update_note, &self.note_shown)
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
        description = "Search indexed documentation by semantic similarity. \
                       Returns ranked chunks with section + file context. \
                       Token-efficiency tips: (1) query with 2-4 specific \
                       domain nouns rather than full natural-language \
                       questions (\"embedding pipeline fastembed prefix\" \
                       beats \"how does polaris embed documents\"); \
                       (2) default top_k=2-3 — only raise to 5 if recall \
                       looks poor."
    )]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        let banner = self.session_banner();
        let config = Arc::clone(&self.state.config);
        let top_k = (params.top_k.unwrap_or(5) as usize).min(config.max_top_k);
        let query = params.query.clone();
        let bank = self.state.bank.clone();
        let repo_root = bank.repo_root().to_path_buf();

        // Run the synchronous search on a blocking thread; capture the raw result
        // set so we can feed it both into the formatter (returned to the client)
        // and into the savings log writer.
        let bank_for_search = bank.clone();
        let query_for_search = query.clone();
        let search_outcome = tokio::task::spawn_blocking(move || {
            bank_for_search
                .search_with_confidence(&query_for_search, polaris_core::SearchOpts { top_k })
        }).await;

        let (results, best) = match search_outcome {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => return format!("Error: {e}{banner}"),
            Err(e) => return format!("Error: task failed: {e}{banner}"),
        };

        // `best` is the corpus's nearest chunk, taken from the KNN pool before
        // MMR reordered and truncated. Taking `max` over `results` instead made
        // the verdict depend on the caller's `top_k`: `candidate_count` is
        // `top_k * mmr_candidate_multiplier`, so a different `top_k` gives a
        // different candidate pool, different RRF ranks and a different MMR
        // selection. Measured on this repo's docs, "configure OAuth SSO for the
        // web dashboard" scored 0.674 at top_k=2 and 0.646 at top_k=5 — admitted
        // and refused by the then-shipped 0.65 gate on an identical index. KNN is
        // nested, so its maximum is a property of the query and the corpus alone.
        let confident = !results.is_empty() && best >= config.search_min_similarity;

        let formatted = if results.is_empty() {
            "No results found.".to_string()
        } else if confident {
            SearchEngine::format_results(&results)
        } else {
            // Say nothing rather than hand back the corpus's nearest miss: an
            // agent cannot tell a weak match from a strong one once the text is
            // in its context, and acting on the wrong doc costs more than the
            // search saved.
            format!(
                "No reliable context found (best match {best:.2}, threshold {:.2}). The indexed docs likely do not cover this query — answer from your own knowledge or read the source directly.",
                config.search_min_similarity
            )
        };

        // Log either way — a query that matched nothing is the signal that the
        // docs have a gap, which is worth more than the rows we do return. Pass
        // an empty slice unless we're returning the real results: the refusal
        // and empty-index paths never hand the agent any bytes, so the log
        // must not credit a saving that didn't happen.
        let logged_results: &[polaris_core::SearchResult] = if confident { &results } else { &[] };
        let _handle = crate::savings::spawn_search_log(
            bank,
            repo_root,
            polaris_core::db::LogSource::Mcp,
            query,
            top_k,
            logged_results,
        );

        format!("{formatted}{banner}")
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
        let banner = self.session_banner();
        let path = PathBuf::from(&params.path);
        let recursive = params.recursive.unwrap_or(true);
        let force = params.force.unwrap_or(false);
        let bank = self.state.bank.clone();

        if !path.exists() {
            return format!("Error: path not found: {}{banner}", params.path);
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
            // Agents reach the MCP server without a shared cwd, so they
            // naturally pass an absolute path — but `polaris setup` indexes
            // relative ones, and document identity is the raw path string.
            // Left alone, the two spellings produce disjoint `documents` rows
            // and the older set becomes unreachable by removal detection.
            // `under_indexed_root` already reconciles this on the hook path;
            // reuse it, falling back to the raw path on a first-ever index.
            let indexed: Vec<String> = bank
                .document_hashes()
                .map(|v| v.into_iter().map(|(p, _)| p).collect())
                .unwrap_or_default();
            let cwd = std::env::current_dir().ok();
            let path = crate::hook::under_indexed_root(&path, cwd.as_deref(), &indexed)
                .unwrap_or(path);

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

        format!("{}{banner}", result.unwrap_or_else(|e| format!("Error: task failed: {e}")))
    }

    /// Get current status of the Polaris index.
    #[tool(
        name = "status",
        description = "Returns statistics about the current index: document count, chunk count, database size, and embedding configuration."
    )]
    async fn status(&self, _params: Parameters<StatusParams>) -> String {
        let banner = self.session_banner();
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

        format!("{}{banner}", result.unwrap_or_else(|e| format!("Error: task failed: {e}")))
    }

    /// Measure retrieval quality against ground truth derived from the indexed corpus.
    #[tool(
        name = "eval",
        description = "Measure retrieval quality against ground truth derived \
                       from the indexed corpus itself, and report whether \
                       search_min_similarity is well calibrated for it. \
                       Queries are drawn from the corpus, so scores are \
                       optimistic — this calibrates the threshold, it is not \
                       an accuracy measure. Takes several seconds; call it \
                       when diagnosing poor results, not per query."
    )]
    async fn eval(&self, Parameters(params): Parameters<EvalParams>) -> String {
        let banner = self.session_banner();
        let config = Arc::clone(&self.state.config);
        let bank = self.state.bank.clone();
        let sample = params.sample.map(|s| s as usize).unwrap_or(config.eval.sample_size);
        if sample == 0 {
            return format!("Error: sample must be greater than 0{banner}");
        }
        let probes = config.eval.probes.clone();

        let outcome = tokio::task::spawn_blocking(move || {
            if bank.stats()?.doc_count == 0 {
                return Err(PolarisError::Indexing(
                    "index is empty  —  run `polaris index <path>` to add documents".to_string(),
                ));
            }
            polaris_core::eval::run(&bank, polaris_core::eval::EvalOpts { sample_size: sample, probes })
        }).await;

        let report = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return format!("Error: {e}{banner}"),
            Err(e) => return format!("Error: task failed: {e}{banner}"),
        };

        format!(
            "{}{banner}",
            crate::eval::format_report(&report, config.search_min_similarity)
        )
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
                "Polaris is a semantic search MCP for project documentation. \
                 Prefer `search` over grep/read for documentation questions — \
                 it returns ranked, section-aware chunks and is typically \
                 10-40× cheaper in tokens than grepping the docs and reading \
                 files. Query with specific domain terms; start with top_k=2 \
                 and raise only if recall is poor. Use `index` to add files, \
                 `status` to check index health, and `eval` to check \
                 retrieval quality if results look consistently poor."
                    .to_string(),
            ),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression pin for the empty-index rendering bug: `results` empty must
    /// short-circuit to "No results found." rather than falling into the
    /// refusal branch, which folds `f32::MIN` over nothing and prints it.
    #[tokio::test]
    #[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
    async fn search_on_empty_index_reports_no_results_not_f32_min() {
        // Update-check does a detached network spawn from `PolarisServer::new`
        // unless disabled; keep this test hermetic.
        // SAFETY: single-threaded test process, set before any other thread
        // reads the env var.
        unsafe { std::env::set_var("POLARIS_NO_UPDATE_CHECK", "1") };

        polaris_core::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("polaris.db");
        let embed = polaris_core::SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
        let bank = polaris_core::Bank::open(
            polaris_core::BankConfig {
                repo_root: dir.path().to_path_buf(),
                index_path,
                embedding_dim: 64,
                model_id: "nomic-embed-text-v1.5".into(),
                ..Default::default()
            },
            embed,
        )
        .unwrap();

        let server = PolarisServer::new(PolarisState {
            config: Arc::new(PolarisConfig::default()),
            bank,
        });

        let response = server
            .search(Parameters(SearchParams { query: "anything".into(), top_k: Some(3) }))
            .await;

        assert_eq!(response, "No results found.");
        assert!(!response.contains("340282"), "response leaked f32::MIN: {response}");
    }
}
