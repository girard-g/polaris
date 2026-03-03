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
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::EnvFilter;

use config::PolarisConfig;
use db::{Database, SearchResult};
use embedding::EmbeddingEngine;
use error::{PolarisError, Result};
use indexer::{IndexReport, Indexer};
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

    /// Embedding model to use [nomic-embed-text-v1.5 (default), mxbai-embed-large-v1, all-minilm-l6-v2]
    #[arg(long, global = true)]
    model: Option<String>,

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

    /// Watch paths and automatically re-index on changes
    Watch {
        /// One or more paths to watch (files or directories)
        paths: Vec<PathBuf>,
        /// Do not recurse into subdirectories
        #[arg(long)]
        no_recursive: bool,
    },
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
        eprintln!("{} {e}", style("✗").red().bold());
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let mut cfg = PolarisConfig::load(cli.config.as_deref())?;
    cfg.apply_overrides(cli.db, cli.dim, cli.model.clone());

    // If --model was given without --dim and the current dim exceeds the model's
    // native maximum, clamp to the native dim so the user doesn't have to
    // always pair --model with --dim manually.
    if cli.model.is_some() && cli.dim.is_none() {
        if let Ok(native) = crate::embedding::native_dim_for(&cfg.model_id) {
            if cfg.embedding_dim > native {
                cfg.embedding_dim = native;
            }
        }
    }

    cfg.validate()?;

    db::register_vec_extension();

    match cli.command {
        Command::Index { path, no_recursive, force } => {
            cmd_index(cfg, &path, !no_recursive, force).await
        }
        Command::Search { query, top_k } => cmd_search(cfg, &query, top_k).await,
        Command::Serve => cmd_serve(cfg).await,
        Command::Status => cmd_status(cfg).await,
        Command::Watch { paths, no_recursive } => {
            cmd_watch(cfg, &paths, !no_recursive).await
        }
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
            "path not found: {}",
            path.display()
        )));
    }

    // Header — always first, printed immediately.
    eprintln!();
    eprintln!(
        "{}  {}  {}",
        style("polaris").cyan().bold(),
        style("·").dim(),
        style(format!("index  {}", path.display())).bold(),
    );
    eprintln!();

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;

    // Model loading.
    let model_spinner = make_spinner("loading model…");
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);
    model_spinner.finish_and_clear();
    eprintln!(
        "{}  model ready  {}",
        style("✓").green().bold(),
        style(&cfg.model_id).dim(),
    );

    let indexer = Indexer::new(
        engine,
        cfg.max_chunk_tokens,
        cfg.chunk_overlap_chars,
        cfg.max_file_size,
    );

    let report = indexer.index_path(&db, path, recursive, force)?;

    // Summary.
    eprintln!();
    let no_changes = report.added.is_empty()
        && report.modified.is_empty()
        && report.removed.is_empty();

    if no_changes {
        eprintln!(
            "{}  nothing to index  {}",
            style("✓").green().bold(),
            style(format!("{} unchanged", report.unchanged.len())).dim(),
        );
    } else {
        let elapsed = report.elapsed.as_secs_f64();
        let size_kb = report.total_bytes as f64 / 1024.0;
        eprintln!(
            "{}  indexed in {:.1}s  {}  {}",
            style("✓").green().bold(),
            elapsed,
            style("·").dim(),
            style(format!("{} chunks  {:.1} KB", report.total_chunks, size_kb)).dim(),
        );
        eprintln!();

        let mut parts: Vec<String> = Vec::new();
        if !report.added.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("+").green(),
                style(format!("{} added", report.added.len())).green()
            ));
        }
        if !report.modified.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("~").yellow(),
                style(format!("{} modified", report.modified.len())).yellow()
            ));
        }
        if !report.removed.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("-").red(),
                style(format!("{} removed", report.removed.len())).red()
            ));
        }
        if !report.unchanged.is_empty() {
            parts.push(format!(
                "{}",
                style(format!("{} unchanged", report.unchanged.len())).dim()
            ));
        }
        eprintln!("  {}", parts.join("   "));
    }

    for (p, err) in &report.errors {
        eprintln!(
            "  {}  {}  {}",
            style("⚠").yellow(),
            style(p.display().to_string()).dim(),
            err,
        );
    }

    eprintln!();
    Ok(())
}

async fn cmd_search(cfg: PolarisConfig, query: &str, top_k: usize) -> Result<()> {
    if !cfg.db_path.exists() {
        return Err(PolarisError::Indexing(format!(
            "no index at {}  —  run `polaris index <path>` first",
            cfg.db_path.display()
        )));
    }

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let engine = EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?;

    let search = SearchEngine::new(
        &engine,
        &db,
        cfg.mmr_lambda,
        cfg.mmr_candidate_multiplier,
        cfg.heading_boost,
        cfg.rrf_k,
    );
    let results = search.search(query, top_k)?;

    if results.is_empty() {
        let stats = db.get_stats(&cfg.db_path)?;
        if stats.doc_count == 0 {
            eprintln!(
                "{}  index is empty  —  run {} to add documents",
                style("○").dim(),
                style("polaris index <path>").cyan(),
            );
        } else {
            println!("{} results for \"{}\"", style("0").bold(), style(query).dim());
            println!();
            println!("  {}", style("no matches found").dim());
            println!();
            println!(
                "  {}  try broader terms or re-index with {}",
                style("tip:").dim(),
                style("polaris index <path>").cyan(),
            );
            println!();
        }
        return Ok(());
    }

    print!("{}", format_results_terminal(&results, query));
    Ok(())
}

async fn cmd_serve(cfg: PolarisConfig) -> Result<()> {
    tracing::info!("Starting Polaris MCP server (stdio transport)");
    tracing::info!("Database: {}", cfg.db_path.display());
    tracing::info!("Embedding dim: {}", cfg.embedding_dim);

    let read_db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let write_db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    tracing::info!("Loading embedding model…");
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);

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
    println!();
    println!(
        "{}  {}",
        style("polaris").cyan().bold(),
        style("· status").dim(),
    );
    println!();

    // Label column width (pad before styling to avoid ANSI-offset issues).
    let w = 10usize;

    if !cfg.db_path.exists() {
        println!(
            "  {}  {}",
            style(format!("{:<w$}", "database")).dim(),
            cfg.db_path.display(),
        );
        println!();
        println!(
            "  {}  not initialized  —  run {} to get started",
            style("⚠").yellow(),
            style("polaris index <path>").cyan(),
        );
        println!();
        return Ok(());
    }

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;
    let stats = db.get_stats(&cfg.db_path)?;

    let avg_chunks = if stats.doc_count > 0 {
        format!("{:.1}", stats.chunk_count as f64 / stats.doc_count as f64)
    } else {
        "—".to_string()
    };

    // ── Database group ───────────────────────────────────────────────────────
    println!(
        "  {}  {}",
        style(format!("{:<w$}", "database")).dim(),
        style(cfg.db_path.display().to_string()).bold(),
    );
    println!(
        "  {}  {}  {}  {} dim",
        style(format!("{:<w$}", "model")).dim(),
        cfg.model_id,
        style("·").dim(),
        stats.embedding_dim,
    );
    println!();

    // ── Documents group ──────────────────────────────────────────────────────
    println!(
        "  {}  {}",
        style(format!("{:<w$}", "documents")).dim(),
        style(stats.doc_count.to_string()).bold(),
    );
    println!(
        "  {}  {:.1} MB",
        style(format!("{:<w$}", "source")).dim(),
        stats.total_source_bytes as f64 / 1_048_576.0,
    );
    if stats.empty_doc_count > 0 {
        println!(
            "  {}  {} docs have no chunks",
            style("⚠").yellow(),
            stats.empty_doc_count,
        );
    }
    println!();

    // ── Chunks group ─────────────────────────────────────────────────────────
    println!(
        "  {}  {}",
        style(format!("{:<w$}", "chunks")).dim(),
        style(stats.chunk_count.to_string()).bold(),
    );
    println!(
        "  {}  {}",
        style(format!("{:<w$}", "avg/doc")).dim(),
        avg_chunks,
    );
    println!();

    // ── Storage + timestamp ──────────────────────────────────────────────────
    println!(
        "  {}  {:.1} MB",
        style(format!("{:<w$}", "db size")).dim(),
        stats.db_size_bytes as f64 / 1_048_576.0,
    );
    println!(
        "  {}  {}",
        style(format!("{:<w$}", "indexed")).dim(),
        stats.last_indexed.as_deref().unwrap_or("never"),
    );
    println!();

    Ok(())
}

async fn cmd_watch(cfg: PolarisConfig, paths: &[PathBuf], recursive: bool) -> Result<()> {
    use notify_debouncer_mini::notify::RecursiveMode;
    use notify_debouncer_mini::{new_debouncer, DebounceEventResult};

    // Validate paths up front.
    for path in paths {
        if !path.exists() {
            return Err(PolarisError::Indexing(format!(
                "path not found: {}",
                path.display()
            )));
        }
    }

    let paths_display = paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    eprintln!();
    eprintln!(
        "{}  {}  {}",
        style("polaris").cyan().bold(),
        style("·").dim(),
        style(format!("watch  {paths_display}")).bold(),
    );
    eprintln!();

    let db = Database::open(&cfg.db_path, cfg.embedding_dim, &cfg.model_id)?;

    let model_spinner = make_spinner("loading model…");
    let engine = Arc::new(EmbeddingEngine::new(cfg.embedding_dim, &cfg.model_id)?);
    model_spinner.finish_and_clear();
    eprintln!(
        "{}  model ready  {}",
        style("✓").green().bold(),
        style(&cfg.model_id).dim(),
    );
    eprintln!();

    let indexer = Indexer::new(
        engine,
        cfg.max_chunk_tokens,
        cfg.chunk_overlap_chars,
        cfg.max_file_size,
    );

    // Initial index for every path.
    for path in paths {
        eprintln!(
            "{}  initial index  {}",
            style("◆").cyan().bold(),
            style(path.display().to_string()).bold(),
        );
        let report = indexer.index_path(&db, path, recursive, false)?;
        print_watch_report(&report);
    }

    // Set up the debounced watcher with a tokio mpsc channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut debouncer = new_debouncer(
        std::time::Duration::from_millis(500),
        move |res: DebounceEventResult| {
            if let Ok(events) = res {
                let _ = tx.send(events);
            }
        },
    )
    .map_err(|e| PolarisError::Indexing(format!("watcher error: {e}")))?;

    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    for path in paths {
        debouncer
            .watcher()
            .watch(path, mode)
            .map_err(|e| PolarisError::Indexing(format!("watch error: {e}")))?;
    }

    let n = paths.len();
    eprintln!(
        "{}  watching  {}  {}",
        style("◆").cyan().bold(),
        style(format!("{n} path{}", if n == 1 { "" } else { "s" })).bold(),
        style("· Ctrl+C to stop").dim(),
    );

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!();
                eprintln!("{}  stopped", style("◆").cyan().bold());
                eprintln!();
                break;
            }
            Some(events) = rx.recv() => {
                for root in find_affected_roots(&events, &paths) {
                    eprintln!();
                    eprintln!(
                        "{}  re-indexing  {}",
                        style("◆").cyan().bold(),
                        style(root.display().to_string()).bold(),
                    );
                    let report = indexer.index_path(&db, root, recursive, false)?;
                    print_watch_report(&report);
                }
            }
        }
    }

    Ok(())
}

fn find_affected_roots<'a>(
    events: &[notify_debouncer_mini::DebouncedEvent],
    roots: &'a [PathBuf],
) -> Vec<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| {
            let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            events.iter().any(|e| e.path.starts_with(&canonical_root))
        })
        .collect()
}

fn print_watch_report(report: &IndexReport) {
    let no_changes = report.added.is_empty()
        && report.modified.is_empty()
        && report.removed.is_empty();

    if no_changes {
        eprintln!(
            "{}  no changes  {}",
            style("✓").green().bold(),
            style(format!("{} unchanged", report.unchanged.len())).dim(),
        );
    } else {
        let elapsed = report.elapsed.as_secs_f64();
        let size_kb = report.total_bytes as f64 / 1024.0;
        eprintln!(
            "{}  indexed in {:.1}s  {}  {}",
            style("✓").green().bold(),
            elapsed,
            style("·").dim(),
            style(format!("{} chunks  {:.1} KB", report.total_chunks, size_kb)).dim(),
        );
        eprintln!();

        let mut parts: Vec<String> = Vec::new();
        if !report.added.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("+").green(),
                style(format!("{} added", report.added.len())).green()
            ));
        }
        if !report.modified.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("~").yellow(),
                style(format!("{} modified", report.modified.len())).yellow()
            ));
        }
        if !report.removed.is_empty() {
            parts.push(format!(
                "{}  {}",
                style("-").red(),
                style(format!("{} removed", report.removed.len())).red()
            ));
        }
        if !report.unchanged.is_empty() {
            parts.push(format!(
                "{}",
                style(format!("{} unchanged", report.unchanged.len())).dim()
            ));
        }
        eprintln!("  {}", parts.join("   "));
    }

    for (p, err) in &report.errors {
        eprintln!(
            "  {}  {}  {}",
            style("⚠").yellow(),
            style(p.display().to_string()).dim(),
            err,
        );
    }

    eprintln!();
}

// ---------------------------------------------------------------------------
// TUI helpers
// ---------------------------------------------------------------------------

/// Spinner used for the model-loading phase in cmd_index.
/// Indexer phases use their own internally configured spinners.
fn make_spinner(msg: impl Into<String>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_message(msg.into());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

/// Build a 40-char visual score bar: cyan filled █, dim empty ░.
fn score_bar(score: f32) -> String {
    let width = 40usize;
    let filled = ((score * width as f32).round() as usize).min(width);
    let empty = width - filled;
    format!(
        "{}{}",
        style("█".repeat(filled)).cyan(),
        style("░".repeat(empty)).dim(),
    )
}

/// CLI-specific search result formatter (terminal colours + score bar).
/// The MCP server uses `SearchEngine::format_results` (markdown) instead.
fn format_results_terminal(results: &[SearchResult], query: &str) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let sep = style("─".repeat(80)).dim().to_string();
    let n = results.len();

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{} {}",
        style(format!("{n} result{}", if n == 1 { "" } else { "s" })).bold(),
        style(format!("for \"{query}\"")).dim(),
    );

    for (i, r) in results.iter().enumerate() {
        let _ = writeln!(out, "{sep}");

        // Index number + file path.
        let _ = writeln!(
            out,
            " {}  {}",
            style(i + 1).bold(),
            style(&r.file_path).dim(),
        );

        // Heading breadcrumb (optional).
        if !r.heading_context.is_empty() {
            let _ = writeln!(out, "     {}", style(&r.heading_context).dim());
        }

        // Score bar.
        let _ = writeln!(
            out,
            "     {}  {}",
            score_bar(r.score),
            style(format!("{:.3}", r.score)).bold(),
        );
        let _ = writeln!(out);

        // Content body — max 8 lines, remainder summarised.
        let lines: Vec<&str> = r.content.lines().collect();
        let shown = lines.len().min(8);
        for line in &lines[..shown] {
            if line.is_empty() {
                let _ = writeln!(out);
            } else {
                let _ = writeln!(out, "     {line}");
            }
        }
        if lines.len() > 8 {
            let _ = writeln!(
                out,
                "     {}",
                style(format!("… {} more lines", lines.len() - 8)).dim(),
            );
        }

        let _ = writeln!(out);
    }

    let _ = writeln!(out);
    out
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
