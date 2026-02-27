mod config;
mod db;
mod embedding;
mod error;
mod indexer;
mod mcp;
mod search;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::EnvFilter;

use config::PolarisConfig;
use db::Database;
use embedding::EmbeddingEngine;
use error::{PolarisError, Result};
use indexer::Indexer;
use mcp::{PolarisServer, PolarisState};
use search::SearchEngine;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "polaris",
    about = "Lightweight RAG system with MCP server for coding agents",
    version
)]
struct Cli {
    /// Path to config file (overrides default search)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override embedding dimension
    #[arg(long, global = true)]
    dim: Option<usize>,

    /// Override database path
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index markdown files from a path
    Index {
        /// Path to directory or file
        path: PathBuf,
        /// Do not recurse into subdirectories
        #[arg(long)]
        no_recursive: bool,
        /// Re-index all files even if unchanged
        #[arg(long)]
        force: bool,
    },

    /// Search the indexed documentation
    Search {
        /// Search query
        query: String,
        /// Number of results to return
        #[arg(short = 'k', long, default_value = "5")]
        top_k: usize,
    },

    /// Start the MCP server over stdio
    Serve,

    /// Show index statistics
    Status,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // For `serve`, logging must go to stderr so stdio stays clean for MCP.
    let log_target = matches!(cli.command, Command::Serve);
    init_tracing(log_target);

    if let Err(e) = run(cli).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Load config and apply CLI overrides.
    let mut cfg = PolarisConfig::load(cli.config.as_deref())?;
    cfg.apply_overrides(cli.db, cli.dim);
    cfg.validate()?;

    // Register sqlite-vec before any DB connection is made.
    db::register_vec_extension();

    match cli.command {
        Command::Index { path, no_recursive, force } => {
            cmd_index(cfg, &path, !no_recursive, force).await
        }
        Command::Search { query, top_k } => cmd_search(cfg, &query, top_k).await,
        Command::Serve => cmd_serve(cfg).await,
        Command::Status => cmd_status(cfg).await,
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn cmd_index(
    cfg: PolarisConfig,
    path: &std::path::Path,
    recursive: bool,
    force: bool,
) -> Result<()> {
    if !path.exists() {
        return Err(PolarisError::Indexing(format!(
            "Path not found: {}",
            path.display()
        )));
    }

    tracing::info!("Opening database: {}", cfg.db_path.display());
    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;

    let model_spinner = ProgressBar::new_spinner();
    model_spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    model_spinner.set_message("Loading embedding model (first run may download ~137 MB)…");
    model_spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim)?);
    model_spinner.finish_and_clear();
    eprintln!("  Model ready.");

    let indexer = Indexer::new(engine, cfg.max_chunk_tokens, cfg.chunk_overlap_chars, cfg.max_file_size);

    eprintln!("  Indexing: {}", path.display());
    let report = indexer.index_path(&db, path, recursive, force)?;

    println!("{}", report.summary());
    for (p, err) in &report.errors {
        eprintln!("  Warning — {}: {err}", p.display());
    }

    Ok(())
}

async fn cmd_search(cfg: PolarisConfig, query: &str, top_k: usize) -> Result<()> {
    if !cfg.db_path.exists() {
        eprintln!(
            "No index found at '{}'. Run `polaris index <path>` first.",
            cfg.db_path.display()
        );
        std::process::exit(1);
    }

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let engine = EmbeddingEngine::new(cfg.embedding_dim)?;

    let search = SearchEngine::new(&engine, &db, cfg.mmr_lambda, cfg.mmr_candidate_multiplier, cfg.heading_boost, cfg.rrf_k);
    let results = search.search(query, top_k)?;

    if results.is_empty() {
        let stats = db.get_stats(&cfg.db_path)?;
        if stats.doc_count == 0 {
            eprintln!(
                "Index is empty. Run `polaris index <path>` to add documents."
            );
        } else {
            println!("No results found.");
        }
        return Ok(());
    }

    print!("{}", SearchEngine::format_results(&results));
    Ok(())
}

async fn cmd_serve(cfg: PolarisConfig) -> Result<()> {
    tracing::info!("Starting Polaris MCP server (stdio transport)");
    tracing::info!("Database: {}", cfg.db_path.display());
    tracing::info!("Embedding dim: {}", cfg.embedding_dim);

    // Open two connections to the same file so reads and writes don't serialise
    // each other under WAL mode.
    let read_db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let write_db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    tracing::info!("Loading embedding model…");
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim)?);

    let state = PolarisState {
        config: Arc::new(cfg),
        embedding_engine: engine,
        read_db: Arc::new(Mutex::new(read_db)),
        write_db: Arc::new(Mutex::new(write_db)),
    };

    let server = PolarisServer::new(state);
    server.serve_stdio().await?;

    Ok(())
}

async fn cmd_status(cfg: PolarisConfig) -> Result<()> {
    println!("Database   : {}", cfg.db_path.display());

    if !cfg.db_path.exists() {
        println!("Status     : not initialized (run `polaris index <path>` first)");
        return Ok(());
    }

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let stats = db.get_stats(&cfg.db_path)?;

    let avg_chunks = if stats.doc_count > 0 {
        format!("{:.1}", stats.chunk_count as f64 / stats.doc_count as f64)
    } else {
        "—".to_string()
    };
    let empty_docs = if stats.empty_doc_count > 0 {
        format!("{} (no chunks — too small or empty)", stats.empty_doc_count)
    } else {
        "0".to_string()
    };

    println!("Documents  : {}", stats.doc_count);
    println!("  Source   : {:.1} MB", stats.total_source_bytes as f64 / 1_048_576.0);
    println!("  No chunks: {}", empty_docs);
    println!("Chunks     : {}", stats.chunk_count);
    println!("  Avg/doc  : {}", avg_chunks);
    println!("DB size    : {:.1} KB", stats.db_size_bytes as f64 / 1024.0);
    println!("Model      : {}", cfg.model_id);
    println!("Embed dim  : {}", stats.embedding_dim);
    println!(
        "Last index : {}",
        stats.last_indexed.unwrap_or_else(|| "never".to_string())
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tracing setup
// ---------------------------------------------------------------------------

fn init_tracing(stderr: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("polaris=info"));

    if stderr {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .init();
    }
}
