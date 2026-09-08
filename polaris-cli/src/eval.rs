//! `polaris eval` — retrieval quality measured against corpus-derived ground
//! truth. Formatting only; the measurement lives in `polaris_core::eval`.

use polaris_core::config::PolarisConfig;
use polaris_core::error::{PolarisError, Result};
use polaris_core::eval::{EvalReport, MIN_RELIABLE_SAMPLE};

/// Render a report for the terminal.
pub fn format_report(r: &EvalReport, current_threshold: f32) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "  sampled {} sentences ({})\n\n",
        r.sample_size,
        r.language.as_str()
    ));
    if r.sample_size > 0 && r.sample_size < MIN_RELIABLE_SAMPLE {
        out.push_str(&format!(
            "  ! only {} sentences — these numbers are noisy\n\n",
            r.sample_size
        ));
    }

    out.push_str(&format!(
        "  recall@1  {:.2}   recall@3  {:.2}   recall@3 (file)  {:.2}\n",
        r.metrics.recall_1, r.metrics.recall_3, r.metrics.recall_3_file
    ));
    out.push_str(&format!("  MRR       {:.2}\n\n", r.metrics.mrr));

    if let Some(prev) = &r.previous {
        if r.corpus_changed {
            out.push_str("  corpus changed since the last run — deltas omitted\n\n");
        } else if prev.sample_size != r.sample_size as i64 {
            // Different sample sizes measure different question sets, so the
            // difference is sampling noise. eval_run stores sample_size, but
            // config_json does not, so the config-change guard cannot catch it.
            out.push_str(&format!(
                "  last run sampled {} sentences, this one {} — deltas omitted\n\n",
                prev.sample_size, r.sample_size
            ));
        } else {
            out.push_str(&format!(
                "  since last run: recall@3 {:+.2}   MRR {:+.2}\n",
                r.metrics.recall_3 - prev.recall_3,
                r.metrics.mrr - prev.mrr
            ));
            if prev.config_json != r.config_json {
                out.push_str("  config changed since that run\n");
            }
            out.push('\n');
        }
    }

    // Nothing was measured, so every number below would be an artefact. Say that
    // instead of routing zeros through the threshold arms, where positives p10 of
    // 0.00 previously rendered as an English-dominant-model diagnosis.
    if r.sample_size == 0 {
        out.push_str(
            "  ! no sentences could be sampled from this corpus, so nothing was
                 measured. Prose under headings is what eval samples — a corpus of
                 only headings, tables and code blocks yields none. This run was
                 not recorded.
",
        );
        return out;
    }

    match (r.suggested_threshold, r.probe_p95) {
        (Some(t), Some(floor)) => {
            out.push_str(&format!("  positives p10      {:.2}\n", r.positive_p10));
            out.push_str(&format!("  probes     p95      {:.2}\n", floor));
            out.push_str(&format!(
                "  -> suggested search_min_similarity = {t:.2}  (currently {current_threshold:.2})\n"
            ));
        }
        (None, Some(floor)) => {
            out.push_str(&format!(
                "  ! probes p95 {:.2} is not below positives p10 {:.2} — no threshold\n  \
                   separates them on this corpus. Often an English-dominant model\n  \
                   on a corpus in another language.\n",
                floor, r.positive_p10
            ));
        }
        _ => {
            out.push_str(
                "  ! no probe set for this corpus language, so no threshold is\n  \
                   suggested. Set `eval.probes` in polaris.toml to calibrate.\n",
            );
        }
    }

    if r.skipped > 0 {
        out.push_str(&format!("\n  {} sentences skipped (no chunk resolved)\n", r.skipped));
    }
    out
}

/// Round-trip an f32 through its shortest decimal representation before
/// widening to f64. A plain `as f64` cast preserves the f32's exact binary
/// value, which is rarely the decimal the number was chosen to be (e.g.
/// `0.89f32 as f64` == `0.8899999856948853`) — that reads as false precision
/// and breaks equality against a JSON literal on the reading side. Do not
/// "simplify" this back to `as f64`.
fn json_f32(v: f32) -> f64 {
    format!("{v}").parse().unwrap_or(v as f64)
}

/// Render a report as JSON.
pub fn report_json(r: &EvalReport, current_threshold: f32) -> String {
    serde_json::json!({
        // A zero-sample run measured nothing, and its zeroed metrics would read
        // to a CI gate diffing recall_3 as a total retrieval collapse. The
        // plain renderer says so in prose; the machine-read path needs it more,
        // not less, because nothing downstream is reading the prose.
        "measured": r.sample_size > 0,
        "sample_size": r.sample_size,
        "skipped": r.skipped,
        "language": r.language.as_str(),
        "recall_1": json_f32(r.metrics.recall_1),
        "recall_3": json_f32(r.metrics.recall_3),
        "recall_3_file": json_f32(r.metrics.recall_3_file),
        "mrr": json_f32(r.metrics.mrr),
        "positive_p10": json_f32(r.positive_p10),
        "positive_median": json_f32(r.positive_median),
        "probe_p95": r.probe_p95.map(json_f32),
        "suggested_threshold": r.suggested_threshold.map(json_f32),
        "current_threshold": json_f32(current_threshold),
        "corpus_fingerprint": r.corpus_fingerprint,
        "corpus_changed": r.corpus_changed,
    })
    .to_string()
}

/// Entry point for `polaris eval`.
pub fn run(cfg: &PolarisConfig, sample: Option<usize>, json: bool) -> Result<()> {
    if !cfg.db_path.exists() {
        return Err(PolarisError::Indexing(format!(
            "no index at {}  —  run `polaris index <path>` first",
            cfg.db_path.display()
        )));
    }

    // The CLI override bypassed PolarisConfig::validate, which rejects
    // eval.sample_size == 0 — so `--sample 0` reached the engine, produced
    // all-zero metrics, and had them rendered as a corpus diagnosis.
    let effective_sample = sample.unwrap_or(cfg.eval.sample_size);
    if effective_sample == 0 {
        return Err(PolarisError::Config(
            "--sample must be greater than 0".to_string(),
        ));
    }

    let bank = open_primary_bank(cfg)?;

    // An empty index yields no sentences, which would otherwise surface as a
    // silent zero-sample run. Match what `polaris search` already says.
    if bank.stats()?.doc_count == 0 {
        return Err(PolarisError::Indexing(
            "index is empty  —  run `polaris index <path>` to add documents".to_string(),
        ));
    }

    let report = polaris_core::eval::run(
        &bank,
        polaris_core::eval::EvalOpts {
            sample_size: effective_sample,
            probes: cfg.eval.probes.clone(),
        },
    )?;

    if json {
        println!("{}", report_json(&report, cfg.search_min_similarity));
    } else {
        print!("{}", format_report(&report, cfg.search_min_similarity));
    }
    Ok(())
}

/// Open the primary bank over `cfg.db_path`.
fn open_primary_bank(cfg: &PolarisConfig) -> Result<polaris_core::Bank> {
    let embed = polaris_core::SharedEmbedding::load(&cfg.model_id, cfg.embedding_dim)?;
    polaris_core::Bank::open(
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use polaris_core::eval::{EvalReport, Language, Metrics};

    fn report() -> EvalReport {
        EvalReport {
            metrics: Metrics { recall_1: 0.71, recall_3: 0.89, recall_3_file: 0.94, mrr: 0.79 },
            sample_size: 200,
            skipped: 0,
            positive_p10: 0.71,
            positive_median: 0.78,
            probe_p95: Some(0.55),
            suggested_threshold: Some(0.63),
            language: Language::English,
            corpus_fingerprint: "abc".to_string(),
            config_json: "{}".to_string(),
            previous: None,
            corpus_changed: false,
        }
    }

    #[test]
    fn plain_output_shows_metrics_and_threshold() {
        let out = format_report(&report(), 0.65);
        assert!(out.contains("recall@1"));
        assert!(out.contains("0.89"));
        assert!(out.contains("0.63"), "suggested threshold missing: {out}");
        assert!(out.contains("0.65"), "current threshold missing: {out}");
    }

    #[test]
    fn plain_output_explains_a_missing_threshold() {
        let mut r = report();
        r.suggested_threshold = None;
        r.probe_p95 = None;
        r.language = Language::Unknown;
        let out = format_report(&r, 0.65);
        assert!(out.contains("eval.probes"), "must point at the escape hatch: {out}");
    }

    #[test]
    fn zero_sample_reports_that_nothing_was_measured() {
        // Regression: an empty sample used to route positives p10 = 0.00 through
        // the overlap arm and print an English-dominant-model diagnosis.
        let mut r = report();
        r.sample_size = 0;
        r.suggested_threshold = None;
        let out = format_report(&r, 0.65);
        assert!(out.contains("no sentences could be sampled"), "got: {out}");
        assert!(
            !out.contains("another language"),
            "must not diagnose a language mismatch it never observed: {out}"
        );
    }

    #[test]
    fn json_flags_a_zero_sample_run_as_unmeasured() {
        // The plain renderer explains a zero sample in prose; JSON is what a CI
        // gate diffs, so an unflagged run of zeroes reads as a retrieval collapse.
        let mut r = report();
        r.sample_size = 0;
        let v: serde_json::Value = serde_json::from_str(&report_json(&r, 0.65)).unwrap();
        assert_eq!(v["measured"], serde_json::json!(false));

        let mut ok = report();
        ok.sample_size = 200;
        let v: serde_json::Value = serde_json::from_str(&report_json(&ok, 0.65)).unwrap();
        assert_eq!(v["measured"], serde_json::json!(true));
    }

    #[test]
    fn zero_sample_does_not_also_call_the_numbers_noisy() {
        let mut r = report();
        r.sample_size = 0;
        let out = format_report(&r, 0.65);
        assert!(
            !out.contains("noisy"),
            "a run that produced no numbers must not warn that its numbers are noisy: {out}"
        );
    }

    #[test]
    fn deltas_are_omitted_across_different_sample_sizes() {
        use polaris_core::db::EvalRunRow;
        let mut r = report();
        r.sample_size = 200;
        r.previous = Some(EvalRunRow {
            id: 1,
            ts: 1_000,
            sample_size: 10,
            recall_1: 0.10,
            recall_3: 0.10,
            recall_3_file: 0.10,
            mrr: 0.10,
            positive_p10: 0.5,
            positive_median: 0.6,
            probe_p95: Some(0.5),
            suggested_threshold: Some(0.55),
            corpus_fingerprint: "abc".to_string(),
            config_json: "{}".to_string(),
        });
        let out = format_report(&r, 0.65);
        assert!(out.contains("deltas omitted"), "got: {out}");
        assert!(
            !out.contains("since last run"),
            "a 10-vs-200 sample delta is sampling noise, not a signal: {out}"
        );
    }

    #[test]
    fn plain_output_warns_on_a_small_sample() {
        let mut r = report();
        r.sample_size = 12;
        let out = format_report(&r, 0.65);
        assert!(out.to_lowercase().contains("noisy"), "no small-sample warning: {out}");
    }

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn run_errors_on_an_empty_index() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("polaris.db");
        polaris_core::db::register_vec_extension();
        let cfg = PolarisConfig::default();
        // Create the DB file with schema but no documents.
        drop(polaris_core::db::Database::open(&db_path, cfg.embedding_dim, &cfg.model_id).unwrap());

        let mut cfg = PolarisConfig::default();
        cfg.db_path = db_path;
        let err = run(&cfg, Some(10), false).unwrap_err().to_string();
        assert!(err.contains("empty"), "expected an empty-index hint, got: {err}");
    }

    #[test]
    fn json_output_is_valid_and_carries_the_metrics() {
        let val: serde_json::Value =
            serde_json::from_str(&report_json(&report(), 0.65)).unwrap();
        assert_eq!(val["recall_3"], 0.89);
        assert_eq!(val["suggested_threshold"], 0.63);
    }
}
