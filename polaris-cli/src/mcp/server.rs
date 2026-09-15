use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::OnceCell;

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

/// An opened index and the config resolved against it.
#[derive(Clone)]
pub struct OpenIndex {
    /// Effective config: model, dimension and threshold resolved from this index.
    pub config: Arc<PolarisConfig>,
    /// Cheaply cloneable (`Arc<BankInner>`); serialises access through its
    /// internal `Mutex<Database>`. MCP tool calls are typically serial.
    pub bank: polaris_core::Bank,
}

/// Shared state for the MCP server.
#[derive(Clone)]
pub struct PolarisState {
    /// Config as resolved when the server started. When no index existed then,
    /// model, dimension and threshold are resolved again when the index is
    /// opened — tools read those from [`OpenIndex::config`], not from here.
    pub config: Arc<PolarisConfig>,
    /// Set at startup when an index exists, otherwise on first use (spec
    /// §4.1.1). `get_or_try_init` leaves the cell empty when initialisation
    /// fails, so a `search` that finds no index, or an `index` whose model
    /// failed to load, does not stop a later `index` from creating one. Do not
    /// replace it with a cell that poisons on error.
    pub bank: Arc<OnceCell<OpenIndex>>,
}

/// Load the model, then open (or create) the bank `cfg` describes. The model
/// loads first, so a failed load leaves no database behind.
pub fn open_index(cfg: PolarisConfig) -> Result<OpenIndex, PolarisError> {
    let embed = polaris_core::SharedEmbedding::load(&cfg.model_id, cfg.embedding_dim)?;
    let bank = polaris_core::Bank::open(
        polaris_core::BankConfig {
            repo_root: crate::corpus_root(),
            index_path: cfg.db_path.clone(),
            embedding_dim: cfg.embedding_dim,
            model_id: cfg.model_id.clone(),
            max_chunk_tokens: cfg.max_chunk_tokens,
            chunk_overlap_chars: cfg.chunk_overlap_chars,
            max_file_size: cfg.max_file_size,
            mmr_lambda: cfg.mmr_lambda,
            mmr_candidate_multiplier: cfg.mmr_candidate_multiplier,
            heading_boost: cfg.heading_boost,
            rrf_k: cfg.rrf_k,
        },
        embed,
    )?;
    Ok(OpenIndex { config: Arc::new(cfg), bank })
}

/// State for `polaris serve`. An existing index opens now, so the first search
/// is warm; with none, nothing is loaded and no file is created.
pub fn serve_state(mut cfg: PolarisConfig) -> Result<PolarisState, PolarisError> {
    polaris_core::config::resolve_effective(&mut cfg);
    let bank = if has_index(&cfg.db_path) {
        OnceCell::new_with(Some(open_index(cfg.clone())?))
    } else {
        OnceCell::new()
    };
    Ok(PolarisState { config: Arc::new(cfg), bank: Arc::new(bank) })
}

/// Resolve `base` against the index (or, once Task 13 wires in selection,
/// against the corpus) and open it: the model loads before the bank, so a
/// failed load leaves no database behind. Shared by `existing_index` and
/// `run_index`'s `get_or_try_init` initialisers, so this ordering is written
/// once rather than duplicated in both.
async fn open_resolved(base: Arc<PolarisConfig>) -> Result<OpenIndex, String> {
    let mut cfg = (*base).clone();
    tokio::task::spawn_blocking(move || {
        polaris_core::config::resolve_effective(&mut cfg);
        open_index(cfg)
    })
    .await
    .map_err(|e| format!("Error: task failed: {e}"))?
    .map_err(|e| format!("Error: {e}"))
}

/// Whether an index is ready to open at `db_path`. Not `exists()`: `polaris
/// index` creates the file, then the schema, and writes the stored model last.
/// Opening inside that gap would resolve the default model and keep that bank
/// for the whole session, so a file without a stored model is no index yet.
pub(crate) fn has_index(db_path: &Path) -> bool {
    polaris_core::db::read_index_metadata(db_path).model_id.is_some()
}

fn no_index_message(db_path: &Path) -> String {
    format!(
        "No index yet at {} — call the `index` tool with your docs path (or run `polaris index <path>`), then try again.",
        db_path.display()
    )
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

    /// The open index, opening an existing database on first use. Never
    /// creates one: with no database this answers that no index exists yet.
    async fn existing_index(&self) -> Result<OpenIndex, String> {
        let base = Arc::clone(&self.state.config);
        self.state
            .bank
            .get_or_try_init(move || async move {
                if !has_index(&base.db_path) {
                    return Err(no_index_message(&base.db_path));
                }
                open_resolved(base).await
            })
            .await
            .cloned()
    }

    /// Body of the `index` tool, apart from its MCP progress plumbing so tests
    /// can drive it. Opens the index, creating it when none exists.
    async fn run_index(
        &self,
        params: IndexParams,
        on_progress: Option<Box<dyn Fn(f32, &str) + Send + Sync>>,
    ) -> String {
        let banner = self.session_banner();
        let path = PathBuf::from(&params.path);
        let recursive = params.recursive.unwrap_or(true);
        let force = params.force.unwrap_or(false);

        if !path.exists() {
            return format!("Error: path not found: {}{banner}", params.path);
        }

        let base = Arc::clone(&self.state.config);
        let opened = self.state.bank.get_or_try_init(move || open_resolved(base)).await.cloned();
        let index = match opened {
            Ok(index) => index,
            Err(msg) => return format!("{msg}{banner}"),
        };
        let bank = index.bank.clone();
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
        })
        .await;

        format!("{}{banner}", result.unwrap_or_else(|e| format!("Error: task failed: {e}")))
    }
}

/// What the `search` tool returns for a result set, and whether the results
/// were handed over (and so count as a saving in the log).
fn search_response(
    results: &[polaris_core::SearchResult],
    best: f32,
    config: &PolarisConfig,
) -> (String, bool) {
    if results.is_empty() {
        return ("No results found.".to_string(), false);
    }
    match config.search_min_similarity {
        // Say nothing rather than hand back the corpus's nearest miss: an agent
        // cannot tell a weak match from a strong one once the text is in its
        // context, and acting on the wrong doc costs more than the search saved.
        Some(threshold) if best < threshold => (
            format!(
                "No reliable context found (best match {best:.2}, threshold {threshold:.2}). The indexed docs likely do not cover this query — answer from your own knowledge or read the source directly."
            ),
            false,
        ),
        Some(_) => (SearchEngine::format_results(results), true),
        None => (
            format!(
                "{}\n\nNote: {} has no calibrated search_min_similarity, so these results were not filtered by relevance. Run `polaris eval` and set search_min_similarity in polaris.toml to enable the gate.",
                SearchEngine::format_results(results),
                config.model_id
            ),
            true,
        ),
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
        let index = match self.existing_index().await {
            Ok(index) => index,
            Err(msg) => return format!("{msg}{banner}"),
        };
        let config = Arc::clone(&index.config);
        let top_k = (params.top_k.unwrap_or(5) as usize).min(config.max_top_k);
        let query = params.query.clone();
        let bank = index.bank.clone();
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
        let (formatted, handed_over) = search_response(&results, best, &config);

        // Log either way — a query that matched nothing is the signal that the
        // docs have a gap, which is worth more than the rows we do return. Pass
        // an empty slice unless we're returning the real results: the refusal
        // and empty-index paths never hand the agent any bytes, so the log
        // must not credit a saving that didn't happen.
        let logged_results: &[polaris_core::SearchResult] = if handed_over { &results } else { &[] };
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

        self.run_index(params, on_progress).await
    }

    /// Get current status of the Polaris index.
    #[tool(
        name = "status",
        description = "Returns statistics about the current index: document count, chunk count, database size, and embedding configuration."
    )]
    async fn status(&self, _params: Parameters<StatusParams>) -> String {
        let banner = self.session_banner();
        let index = match self.existing_index().await {
            Ok(index) => index,
            Err(msg) => return format!("{msg}{banner}"),
        };
        let config = Arc::clone(&index.config);
        let bank = index.bank.clone();

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
        let index = match self.existing_index().await {
            Ok(index) => index,
            Err(msg) => return format!("{msg}{banner}"),
        };
        let config = Arc::clone(&index.config);
        let bank = index.bank.clone();
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
            crate::eval::format_report(&report, &config)
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

    fn server_for(state: PolarisState) -> PolarisServer {
        // Update-check does a detached network spawn from `PolarisServer::new`
        // unless disabled; keep tests hermetic. `std::env::set_var` is unsafe
        // because a concurrent reader elsewhere in the process could observe a
        // torn write; wrapping it in `Once` means the write happens at most
        // once, and happens-before every `PolarisServer::new` reached through
        // this helper, rather than once per (parallel) test racing the others.
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe { std::env::set_var("POLARIS_NO_UPDATE_CHECK", "1") });
        PolarisServer::new(state)
    }

    fn cfg_at(db_path: PathBuf) -> PolarisConfig {
        PolarisConfig { db_path, ..PolarisConfig::default() }
    }

    #[test]
    fn serve_state_without_an_index_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        let state = serve_state(cfg_at(db.clone())).unwrap();
        assert!(state.bank.get().is_none());
        assert!(!db.exists(), "polaris serve must not create a database at startup");
    }

    #[test]
    fn serve_state_on_a_half_created_index_opens_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        std::fs::write(&db, b"").unwrap();
        let state = serve_state(cfg_at(db.clone())).unwrap();
        assert!(state.bank.get().is_none());
        assert_eq!(std::fs::metadata(&db).unwrap().len(), 0, "no schema may be written");
    }

    #[tokio::test]
    async fn read_tools_without_an_index_say_so_and_create_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        let server = server_for(serve_state(cfg_at(db.clone())).unwrap());

        let search = server
            .search(Parameters(SearchParams { query: "anything".into(), top_k: Some(2) }))
            .await;
        let status = server.status(Parameters(StatusParams {})).await;
        let eval = server.eval(Parameters(EvalParams { sample: Some(5) })).await;
        for response in [&search, &status, &eval] {
            assert!(response.starts_with("No index yet at"), "{response}");
        }
        assert!(!db.exists());
        assert!(server.state.bank.get().is_none());
    }

    /// A CLI `polaris index` creates the file before it writes the stored
    /// model. A read tool landing in that gap must not open the file with the
    /// default model and keep that bank for the rest of the session.
    #[tokio::test]
    async fn read_tools_on_a_half_created_index_say_no_index_and_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        let server = server_for(serve_state(cfg_at(db.clone())).unwrap());
        // An empty file is a valid SQLite database with no metadata yet.
        std::fs::write(&db, b"").unwrap();

        let search = server
            .search(Parameters(SearchParams { query: "anything".into(), top_k: Some(2) }))
            .await;
        let status = server.status(Parameters(StatusParams {})).await;
        let eval = server.eval(Parameters(EvalParams { sample: Some(5) })).await;
        for response in [&search, &status, &eval] {
            assert!(response.starts_with("No index yet at"), "{response}");
        }
        assert!(server.state.bank.get().is_none(), "a later call must be able to retry");
        assert_eq!(std::fs::metadata(&db).unwrap().len(), 0, "no schema may be written");
    }

    #[tokio::test]
    async fn a_failed_index_creates_no_database_and_leaves_the_cell_empty() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.md"), "# A\n\nSome text.\n").unwrap();
        let db = dir.path().join("polaris.db");
        let mut cfg = cfg_at(db.clone());
        cfg.apply_overrides(None, None, Some("bad-model".into()));
        let server = server_for(serve_state(cfg).unwrap());

        let response = server
            .run_index(
                IndexParams { path: docs.display().to_string(), recursive: None, force: None },
                None,
            )
            .await;
        assert!(response.starts_with("Error:"), "{response}");
        assert!(!db.exists(), "a model that failed to load must leave no database");
        assert!(server.state.bank.get().is_none(), "the cell must stay empty so a retry can succeed");
    }

    #[test]
    #[ignore = "downloads ~1.2 GB EmbeddingGemma ONNX model; run with `cargo test -- --include-ignored`"]
    fn serve_state_opens_an_existing_gemma_index_with_its_threshold() {
        polaris_core::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        drop(polaris_core::db::Database::open(&db, 768, "embeddinggemma-300m").unwrap());

        let state = serve_state(cfg_at(db)).unwrap();
        let open = state.bank.get().expect("an existing index opens at startup");
        assert_eq!(open.config.model_id, "embeddinggemma-300m");
        assert_eq!(open.config.embedding_dim, 768);
        assert_eq!(open.config.search_min_similarity, Some(0.42));
    }

    #[tokio::test]
    #[ignore = "downloads ~1.2 GB EmbeddingGemma ONNX model; run with `cargo test -- --include-ignored`"]
    async fn an_index_created_after_startup_opens_lazily_with_its_model() {
        polaris_core::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        let server = server_for(serve_state(cfg_at(db.clone())).unwrap());
        let first = server
            .search(Parameters(SearchParams { query: "how do I install polaris".into(), top_k: Some(2) }))
            .await;
        assert!(first.starts_with("No index yet"), "{first}");

        // A CLI `polaris index` creates the index while the server is running.
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("guide.md"),
            "# Installation Guide\n\nTo install polaris, run `cargo install polaris`.\n",
        )
        .unwrap();
        {
            let embed = polaris_core::SharedEmbedding::load("embeddinggemma-300m", 768).unwrap();
            let bank = polaris_core::Bank::open(
                polaris_core::BankConfig {
                    repo_root: dir.path().to_path_buf(),
                    index_path: db.clone(),
                    embedding_dim: 768,
                    model_id: "embeddinggemma-300m".into(),
                    ..Default::default()
                },
                embed,
            )
            .unwrap();
            bank.index_path(&docs, polaris_core::IndexOpts::default()).unwrap();
        }

        let status = server.status(Parameters(StatusParams {})).await;
        assert!(status.contains("Model: embeddinggemma-300m"), "{status}");
        let search = server
            .search(Parameters(SearchParams { query: "how do I install polaris".into(), top_k: Some(2) }))
            .await;
        assert!(!search.starts_with("No index yet") && !search.starts_with("Error"), "{search}");
    }

    /// Regression pin for the empty-index rendering bug: `results` empty must
    /// short-circuit to "No results found." rather than falling into the
    /// refusal branch, which folds `f32::MIN` over nothing and prints it.
    #[tokio::test]
    #[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
    async fn search_on_empty_index_reports_no_results_not_f32_min() {
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

        let config = Arc::new(PolarisConfig::default());
        let open = OpenIndex { config: Arc::clone(&config), bank };
        let server = server_for(PolarisState {
            config,
            bank: Arc::new(tokio::sync::OnceCell::new_with(Some(open))),
        });

        let response = server
            .search(Parameters(SearchParams { query: "anything".into(), top_k: Some(3) }))
            .await;

        assert_eq!(response, "No results found.");
        assert!(!response.contains("340282"), "response leaked f32::MIN: {response}");
    }

    fn hit(score: f32) -> polaris_core::SearchResult {
        polaris_core::SearchResult {
            chunk_id: 1,
            content: "Install with cargo.".into(),
            heading_context: "Install".into(),
            file_path: "docs/install.md".into(),
            score,
            source_db: None,
        }
    }

    #[test]
    fn search_refuses_below_a_calibrated_threshold() {
        let mut cfg = PolarisConfig::default();
        cfg.search_min_similarity = Some(0.63);
        let (text, handed_over) = search_response(&[hit(0.5)], 0.5, &cfg);
        assert!(text.starts_with("No reliable context found"), "{text}");
        assert!(!handed_over);

        let (text, handed_over) = search_response(&[hit(0.7)], 0.7, &cfg);
        assert!(text.contains("docs/install.md") && !text.contains("Note:"), "{text}");
        assert!(handed_over);
    }

    #[test]
    fn uncalibrated_search_returns_results_with_one_note() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(None, None, Some("all-minilm-l6-v2".into()));
        cfg.search_min_similarity = None;
        let (text, handed_over) = search_response(&[hit(0.2)], 0.2, &cfg);
        assert!(text.contains("docs/install.md"), "{text}");
        assert!(text.contains("all-minilm-l6-v2 has no calibrated search_min_similarity"), "{text}");
        assert!(text.contains("polaris eval"), "{text}");
        assert!(handed_over);
    }

    #[test]
    fn empty_results_say_so_whatever_the_threshold() {
        let (text, handed_over) = search_response(&[], f32::MIN, &PolarisConfig::default());
        assert_eq!(text, "No results found.");
        assert!(!handed_over);
    }
}
