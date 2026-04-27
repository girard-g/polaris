//! Terminal UI helpers used by the polaris CLI.
//! These are presentation-only and have no place in `polaris-core`.

use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use polaris_core::db::SearchResult;

/// Spinner used for the model-loading phase in cmd_index.
/// Indexer phases use their own internally configured spinners.
pub fn make_spinner(msg: impl Into<String>) -> ProgressBar {
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
pub fn score_bar(score: f32) -> String {
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
pub fn format_results_terminal(results: &[SearchResult], query: &str) -> String {
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

        // Source database (multi-DB mode only).
        if let Some(ref db_name) = r.source_db {
            let _ = writeln!(out, "     {}", style(format!("[{}]", db_name)).dim());
        }

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
