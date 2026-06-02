//! `polaris savings` — render cumulative tokens-saved analytics from `search_log`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use console::style;
use polaris_core::Bank;
use polaris_core::db::{Database, LogSource, SavingsAggregate, SearchLogRow, SearchResult};
use polaris_core::error::{PolarisError, Result};
use tokio::task::JoinHandle;

/// Heuristic: ~4 chars per token (matches README's existing claim).
pub const BYTES_PER_TOKEN: f64 = 4.0;

/// Hardcoded model name shown in the savings cost-comparison block.
const COST_MODEL_NAME: &str = "Opus4.7";

/// Hardcoded input price (US dollars per 1,000,000 tokens) for the cost block.
const COST_PRICE_USD_PER_MTOK: f64 = 5.0;

/// Render `SavingsAggregate` as the plain-text summary block.
pub fn format_summary(agg: &SavingsAggregate) -> String {
    if agg.total_searches == 0 {
        return "No searches recorded yet. Run a search to start tracking.\n".to_string();
    }

    let delivered_tok = bytes_to_tokens(agg.total_result_bytes);
    let baseline_tok = bytes_to_tokens(agg.total_baseline_bytes);
    let saved_tok = baseline_tok.saturating_sub(delivered_tok);
    let multiplier = if delivered_tok > 0 {
        baseline_tok as f64 / delivered_tok as f64
    } else {
        0.0
    };

    let mut out = String::new();
    out.push_str(&format!("\n  {}  ·  savings\n\n", style("polaris").bold()));
    out.push_str(&format!(
        "  Total searches      {}  (mcp {} / cli {})\n",
        agg.total_searches, agg.by_source.mcp.searches, agg.by_source.cli.searches,
    ));
    out.push_str(&format!(
        "  Tokens delivered   {}\n",
        fmt_count(delivered_tok)
    ));
    out.push_str(&format!(
        "  Baseline           {}\n",
        fmt_count(baseline_tok)
    ));
    out.push_str(&format!(
        "  Tokens saved       {}  ~{:.1}× cheaper\n",
        fmt_count(saved_tok),
        multiplier,
    ));
    if let Some(ts) = agg.tracking_since_ts {
        out.push_str(&format!("  Tracking since     {}\n", fmt_iso_date(ts)));
    }
    out.push_str("\n  Tokens estimated at ~4 chars/token.\n");

    let cost = cost_breakdown(agg);
    out.push_str(&format!(
        "\n  {} ({}$ for 1_000_000 tokens)\n\n",
        cost.model, cost.price_usd_per_mtok,
    ));
    out.push_str(&format!(
        "  {:<15} : {:<8}-> ${:.2}\n",
        "without polaris",
        fmt_count(baseline_tok),
        round_cents(cost.cost_without_usd),
    ));
    out.push_str(&format!(
        "  {:<15} : {:<8}-> ${:.2}\n",
        "with polaris",
        fmt_count(delivered_tok),
        round_cents(cost.cost_with_usd),
    ));
    out.push_str(&format!(
        "  {:<15} : {:<8}-> ${:.2}\n",
        "saved",
        fmt_count(saved_tok),
        round_cents(cost.saved_usd),
    ));

    out
}

/// Render the most recent rows as the plain-text history table.
pub fn format_history(rows: &[SearchLogRow]) -> String {
    if rows.is_empty() {
        return "No searches recorded yet. Run a search to start tracking.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "\n  {}  ·  savings  ·  history (last {})\n\n",
        style("polaris").bold(),
        rows.len(),
    ));
    out.push_str("  ts                    src   top_k  delivered  saved  query\n");
    for r in rows {
        let delivered = bytes_to_tokens(r.result_bytes);
        let saved = bytes_to_tokens(r.baseline_bytes).saturating_sub(delivered);
        out.push_str(&format!(
            "  {}  {:<4}  {:<5}  {:<9}  {:<5}  {}\n",
            fmt_iso_seconds(r.ts),
            r.source.as_str(),
            r.top_k,
            fmt_count(delivered),
            fmt_count(saved),
            truncate(&r.query, 50),
        ));
    }
    out
}

fn bytes_to_tokens(bytes: usize) -> usize {
    ((bytes as f64) / BYTES_PER_TOKEN).round() as usize
}

struct CostBreakdown {
    model: &'static str,
    price_usd_per_mtok: f64,
    cost_without_usd: f64,
    cost_with_usd: f64,
    saved_usd: f64,
}

fn cost_breakdown(agg: &SavingsAggregate) -> CostBreakdown {
    let delivered_tok = bytes_to_tokens(agg.total_result_bytes);
    let baseline_tok = bytes_to_tokens(agg.total_baseline_bytes);
    let cost_without_usd = baseline_tok as f64 * COST_PRICE_USD_PER_MTOK / 1_000_000.0;
    let cost_with_usd = delivered_tok as f64 * COST_PRICE_USD_PER_MTOK / 1_000_000.0;
    let saved_usd = (cost_without_usd - cost_with_usd).max(0.0);
    CostBreakdown {
        model: COST_MODEL_NAME,
        price_usd_per_mtok: COST_PRICE_USD_PER_MTOK,
        cost_without_usd,
        cost_with_usd,
        saved_usd,
    }
}

/// Round a dollar amount to the nearest cent using half-away-from-zero.
///
/// Rust's `{:.2}` formatter uses round-half-to-even (banker's rounding), which
/// produces $1.12 for 1.125. Currency convention rounds 0.5 up, giving $1.13,
/// so we pre-round here before formatting.
fn round_cents(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn fmt_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_iso_date(ts: i64) -> String {
    fmt_iso_seconds(ts)
        .split('T')
        .next()
        .unwrap_or("")
        .to_string()
}

fn fmt_iso_seconds(ts: i64) -> String {
    // RFC 3339 / ISO-8601 in UTC, second precision. Handles ts ≥ 0.
    let secs = ts.max(0) as u64;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let (y, m, d) = days_to_ymd(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, minute, second
    )
}

/// Convert "days since 1970-01-01" to a (year, month, day) tuple. Civil-from-days
/// algorithm by Howard Hinnant, public domain.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Entry point for the `savings` command.
///
/// Reads the savings log from `db_path` and prints the rendered output.
pub fn run(
    db_path: &Path,
    embedding_dim: usize,
    model_id: &str,
    history: bool,
    limit: usize,
    json: bool,
) -> Result<()> {
    if !db_path.exists() {
        return Err(PolarisError::Indexing(format!(
            "no index at {}  —  run `polaris index <path>` first",
            db_path.display()
        )));
    }

    let db = Database::open(db_path, embedding_dim, model_id)?;

    if history {
        let rows = db.recent_search_log(limit)?;
        if json {
            let val = serde_json::to_string_pretty(&history_json(&rows))
                .map_err(|e| PolarisError::Indexing(format!("json encode failed: {e}")))?;
            println!("{val}");
        } else {
            print!("{}", format_history(&rows));
        }
    } else {
        let agg = db.aggregate_savings()?;
        if json {
            let val = serde_json::to_string_pretty(&summary_json(&agg))
                .map_err(|e| PolarisError::Indexing(format!("json encode failed: {e}")))?;
            println!("{val}");
        } else {
            print!("{}", format_summary(&agg));
        }
    }
    Ok(())
}

/// Compute `result_bytes` (sum of `content` bytes) and the unique result file paths.
fn measure_result(results: &[SearchResult]) -> (usize, Vec<PathBuf>) {
    let result_bytes: usize = results.iter().map(|r| r.content.len()).sum();
    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    for r in results {
        paths.insert(PathBuf::from(&r.file_path));
    }
    (result_bytes, paths.into_iter().collect())
}

fn baseline_from_paths(repo_root: &Path, paths: &[PathBuf]) -> usize {
    paths
        .iter()
        .filter_map(|p| {
            let absolute = if p.is_absolute() {
                p.clone()
            } else {
                repo_root.join(p)
            };
            std::fs::metadata(&absolute).ok().map(|m| m.len() as usize)
        })
        .sum()
}

/// Fire-and-forget: compute baseline + insert one row into `search_log`.
///
/// Returns the `JoinHandle` so tests can `.await` for determinism. Production
/// callers drop it.
pub fn spawn_search_log(
    bank: Bank,
    repo_root: PathBuf,
    source: LogSource,
    query: String,
    top_k: usize,
    results: &[SearchResult],
) -> JoinHandle<()> {
    let (result_bytes, paths) = measure_result(results);
    tokio::spawn(async move {
        let baseline_bytes = baseline_from_paths(&repo_root, &paths);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = bank.log_search(source, &query, top_k, result_bytes, baseline_bytes, ts) {
            tracing::warn!("search log write failed: {e}");
        }
    })
}

fn summary_json(agg: &SavingsAggregate) -> serde_json::Value {
    let cost = cost_breakdown(agg);
    serde_json::json!({
        "total_searches": agg.total_searches,
        "total_result_bytes": agg.total_result_bytes,
        "total_baseline_bytes": agg.total_baseline_bytes,
        "tracking_since_ts": agg.tracking_since_ts,
        "cost_model": cost.model,
        "cost_price_usd_per_mtok": cost.price_usd_per_mtok,
        "cost_without_polaris_usd": cost.cost_without_usd,
        "cost_with_polaris_usd": cost.cost_with_usd,
        "source_breakdown": {
            "mcp": {
                "searches": agg.by_source.mcp.searches,
                "result_bytes": agg.by_source.mcp.result_bytes,
                "baseline_bytes": agg.by_source.mcp.baseline_bytes,
            },
            "cli": {
                "searches": agg.by_source.cli.searches,
                "result_bytes": agg.by_source.cli.result_bytes,
                "baseline_bytes": agg.by_source.cli.baseline_bytes,
            },
        },
    })
}

fn history_json(rows: &[SearchLogRow]) -> serde_json::Value {
    let arr: Vec<_> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "ts": fmt_iso_seconds(r.ts),
                "source": r.source.as_str(),
                "query": r.query,
                "top_k": r.top_k,
                "result_bytes": r.result_bytes,
                "baseline_bytes": r.baseline_bytes,
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use polaris_core::db::{
        LogSource, SavingsAggregate, SavingsBySource, SavingsCounters, SearchLogRow, SearchResult,
    };

    fn agg_with(
        cli: (usize, usize, usize),
        mcp: (usize, usize, usize),
        since: Option<i64>,
    ) -> SavingsAggregate {
        SavingsAggregate {
            total_searches: cli.0 + mcp.0,
            total_result_bytes: cli.1 + mcp.1,
            total_baseline_bytes: cli.2 + mcp.2,
            tracking_since_ts: since,
            by_source: SavingsBySource {
                cli: SavingsCounters {
                    searches: cli.0,
                    result_bytes: cli.1,
                    baseline_bytes: cli.2,
                },
                mcp: SavingsCounters {
                    searches: mcp.0,
                    result_bytes: mcp.1,
                    baseline_bytes: mcp.2,
                },
            },
        }
    }

    #[test]
    fn format_summary_empty() {
        let agg = SavingsAggregate::default();
        let out = format_summary(&agg);
        assert!(out.contains("No searches recorded yet"));
    }

    #[test]
    fn format_summary_populated() {
        let agg = agg_with(
            (1, 4_000, 100_000),
            (2, 8_000, 200_000),
            Some(1_700_000_000),
        );
        let out = format_summary(&agg);
        assert!(out.contains("Total searches      3"));
        assert!(out.contains("(mcp 2 / cli 1)"));
        assert!(out.contains("Tokens delivered   3.0K"));
        assert!(out.contains("Baseline           75.0K"));
        // 75K - 3K = 72K saved; multiplier = 75/3 = 25.0
        assert!(out.contains("Tokens saved       72.0K"));
        assert!(out.contains("~25.0× cheaper"));
        assert!(out.contains("Tracking since"));
        assert!(out.contains("Tokens estimated at ~4 chars/token"));
    }

    #[test]
    fn format_history_empty() {
        let out = format_history(&[]);
        assert!(out.contains("No searches recorded yet"));
    }

    #[test]
    fn format_history_truncates_long_queries() {
        let row = SearchLogRow {
            id: 1,
            ts: 1_700_000_000,
            source: LogSource::Cli,
            query: "a".repeat(80),
            top_k: 5,
            result_bytes: 400,
            baseline_bytes: 8_000,
        };
        let out = format_history(&[row]);
        assert!(out.contains("…"));
        // Truncated string itself should be present (49 a's + ellipsis).
        let line_with_query = out.lines().find(|l| l.contains("aaaa")).unwrap();
        assert!(line_with_query.contains(&format!("{}…", "a".repeat(49))));
    }

    #[test]
    fn fmt_count_thresholds() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1.0K");
        assert_eq!(fmt_count(31_200), "31.2K");
        assert_eq!(fmt_count(1_500_000), "1.5M");
    }

    #[test]
    fn cost_breakdown_math() {
        // delivered = 4_000 + 8_000 = 12_000 bytes → 3_000 tokens
        // baseline  = 100_000 + 200_000 = 300_000 bytes → 75_000 tokens
        let agg = agg_with(
            (1, 4_000, 100_000),
            (2, 8_000, 200_000),
            Some(1_700_000_000),
        );
        let cb = cost_breakdown(&agg);
        assert_eq!(cb.model, "Opus4.7");
        assert!((cb.price_usd_per_mtok - 15.0).abs() < 1e-9);
        // 75_000 * 15 / 1_000_000 = 1.125
        assert!((cb.cost_without_usd - 1.125).abs() < 1e-9);
        // 3_000 * 15 / 1_000_000 = 0.045
        assert!((cb.cost_with_usd - 0.045).abs() < 1e-9);
        // 1.125 - 0.045 = 1.08
        assert!((cb.saved_usd - 1.08).abs() < 1e-9);
    }

    #[test]
    fn format_summary_includes_cost_block() {
        // Same aggregate as format_summary_populated:
        // delivered = 3_000 tokens, baseline = 75_000 tokens.
        let agg = agg_with(
            (1, 4_000, 100_000),
            (2, 8_000, 200_000),
            Some(1_700_000_000),
        );
        let out = format_summary(&agg);

        // Header line, rendered verbatim from the constants.
        assert!(
            out.contains("Opus4.7 (15$ for 1_000_000 tokens)"),
            "missing cost header. Output was:\n{out}",
        );

        // Three data rows. The token-count column is padded to width 8
        // with no separator space before `->`, so:
        //   75.0K  → 5 chars + 3 spaces padding
        //   3.0K   → 4 chars + 4 spaces padding
        //   72.0K  → 5 chars + 3 spaces padding
        // Costs: 1.125 → $1.13, 0.045 → $0.05, 1.08 → $1.08.
        assert!(
            out.contains("without polaris : 75.0K   -> $1.13"),
            "missing 'without polaris' row. Output was:\n{out}",
        );
        assert!(
            out.contains("with polaris    : 3.0K    -> $0.05"),
            "missing 'with polaris' row. Output was:\n{out}",
        );
        assert!(
            out.contains("saved           : 72.0K   -> $1.08"),
            "missing 'saved' row. Output was:\n{out}",
        );
    }

    #[test]
    fn summary_json_includes_cost_fields() {
        // Same aggregate as cost_breakdown_math: 75_000 tok baseline, 3_000 tok delivered.
        let agg = agg_with(
            (1, 4_000, 100_000),
            (2, 8_000, 200_000),
            Some(1_700_000_000),
        );
        let v = summary_json(&agg);

        assert_eq!(v["cost_model"], "Opus4.7");
        assert!(
            (v["cost_price_usd_per_mtok"].as_f64().unwrap() - 15.0).abs() < 1e-9,
            "unexpected price: {v}",
        );
        assert!(
            (v["cost_without_polaris_usd"].as_f64().unwrap() - 1.125).abs() < 1e-6,
            "unexpected cost_without_polaris_usd: {v}",
        );
        assert!(
            (v["cost_with_polaris_usd"].as_f64().unwrap() - 0.045).abs() < 1e-6,
            "unexpected cost_with_polaris_usd: {v}",
        );
        // Spec: saved_usd is intentionally NOT in JSON (trivial subtraction).
        assert!(
            v.get("cost_saved_usd").is_none(),
            "saved_usd should not be in JSON"
        );
    }

    #[test]
    fn fmt_iso_seconds_known_value() {
        // 2023-11-14T22:13:20Z (well-known 1700000000 epoch).
        assert_eq!(fmt_iso_seconds(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn summary_json_shape() {
        let agg = agg_with((1, 100, 1_000), (2, 200, 2_000), Some(42));
        let v = summary_json(&agg);
        assert_eq!(v["total_searches"], 3);
        assert_eq!(v["source_breakdown"]["cli"]["searches"], 1);
        assert_eq!(v["source_breakdown"]["mcp"]["searches"], 2);
        assert_eq!(v["tracking_since_ts"], 42);
    }

    #[test]
    fn savings_run_summary_against_seeded_db() {
        polaris_core::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("savings.db");

        {
            let db = Database::open(&db_path, 4, "test-model").unwrap();
            db.insert_search_log(1_700_000_000, LogSource::Cli, "q1", 5, 400, 8_000)
                .unwrap();
            db.insert_search_log(1_700_000_100, LogSource::Mcp, "q2", 2, 200, 4_000)
                .unwrap();
        }

        // Capture stdout via a pipe-style helper. For simplicity we just call run()
        // and verify the underlying aggregate; the rendered formatting is covered by
        // format_summary tests above.
        run(&db_path, 4, "test-model", false, 20, false).unwrap();

        // Re-open and assert the data is what we expect.
        let db = Database::open(&db_path, 4, "test-model").unwrap();
        let agg = db.aggregate_savings().unwrap();
        assert_eq!(agg.total_searches, 2);
        assert_eq!(agg.by_source.cli.searches, 1);
        assert_eq!(agg.by_source.mcp.searches, 1);
    }

    #[test]
    fn savings_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let err = run(&missing, 512, "nomic-embed-text-v1.5", false, 20, false).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no index at"));
    }

    #[test]
    fn measure_result_dedups_paths_and_sums_content_bytes() {
        let results = vec![
            SearchResult {
                chunk_id: 1,
                content: "hello".into(),
                heading_context: "".into(),
                file_path: "docs/a.md".into(),
                score: 1.0,
                source_db: None,
            },
            SearchResult {
                chunk_id: 2,
                content: "world".into(),
                heading_context: "".into(),
                file_path: "docs/a.md".into(),
                score: 0.9,
                source_db: None,
            },
            SearchResult {
                chunk_id: 3,
                content: "!".into(),
                heading_context: "".into(),
                file_path: "docs/b.md".into(),
                score: 0.8,
                source_db: None,
            },
        ];
        let (bytes, paths) = measure_result(&results);
        assert_eq!(bytes, 11);
        assert_eq!(
            paths,
            vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/b.md")]
        );
    }

    #[tokio::test]
    #[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
    async fn spawn_search_log_inserts_row_for_cli_source() {
        polaris_core::db::register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        let doc_a = docs.join("a.md");
        std::fs::write(
            &doc_a,
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
        )
        .unwrap();

        let index_path = dir.path().join("polaris.db");
        let embed = polaris_core::SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
        let bank = polaris_core::Bank::open(
            polaris_core::BankConfig {
                repo_root: dir.path().to_path_buf(),
                index_path: index_path.clone(),
                embedding_dim: 64,
                model_id: "nomic-embed-text-v1.5".into(),
                ..Default::default()
            },
            embed,
        )
        .unwrap();

        let fake_results = vec![SearchResult {
            chunk_id: 1,
            content: "Lorem ipsum".into(),
            heading_context: "".into(),
            file_path: "docs/a.md".into(),
            score: 1.0,
            source_db: None,
        }];

        let handle = spawn_search_log(
            bank.clone(),
            dir.path().to_path_buf(),
            LogSource::Cli,
            "test query".into(),
            5,
            &fake_results,
        );
        handle.await.unwrap();

        let db = Database::open(&index_path, 64, "nomic-embed-text-v1.5").unwrap();
        let agg = db.aggregate_savings().unwrap();
        assert_eq!(agg.total_searches, 1);
        assert_eq!(agg.by_source.cli.searches, 1);
        assert_eq!(agg.by_source.cli.result_bytes, 11);
        assert!(
            agg.by_source.cli.baseline_bytes >= 50,
            "baseline should reflect the file size"
        );
    }
}
