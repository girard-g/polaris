use polaris_core::db::register_vec_extension;
use polaris_core::eval::{run, EvalOpts};
use polaris_core::{Bank, BankConfig, IndexOpts, SharedEmbedding};
use std::fs;
use tempfile::TempDir;

fn fixture_bank(tmp: &TempDir) -> Bank {
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(
        docs.join("retrieval.md"),
        "# Retrieval\n\n\
         The engine keeps every fragment of the corpus together with the \
         heading it was written under. Two ranked lists are merged into a \
         single order before the caller sees any of them. Diversity reranking \
         then removes near duplicates from the head of that order.\n",
    ).unwrap();
    fs::write(
        docs.join("storage.md"),
        "# Storage\n\n\
         Every document is recorded with a digest of its contents so that a \
         later pass can skip anything that has not changed since. Fragments \
         are written in one transaction rather than one per document.\n",
    ).unwrap();

    register_vec_extension();
    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 512).unwrap();
    let bank = Bank::open(
        BankConfig {
            repo_root: tmp.path().to_path_buf(),
            index_path: tmp.path().join(".polaris/index.db"),
            embedding_dim: 512,
            model_id: "nomic-embed-text-v1.5".to_string(),
            ..Default::default()
        },
        embed,
    ).unwrap();
    bank.index_path(&docs, IndexOpts::default()).unwrap();
    bank
}

#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn eval_finds_source_chunks_and_separates_probes() {
    let tmp = TempDir::new().unwrap();
    let bank = fixture_bank(&tmp);

    let report = run(&bank, EvalOpts { sample_size: 20, probes: vec![] }).unwrap();

    assert!(report.sample_size > 0, "no sentences sampled");
    assert!(report.metrics.recall_3 > 0.0, "recall@3 was zero");
    assert!(
        report.probe_p95.unwrap() < report.positive_p10,
        "probes {:?} did not sit below positives {}",
        report.probe_p95, report.positive_p10
    );
    assert!(report.suggested_threshold.is_some());
    assert!(report.previous.is_none(), "first run has no predecessor");
}

#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn second_run_sees_the_first_and_reports_same_corpus() {
    let tmp = TempDir::new().unwrap();
    let bank = fixture_bank(&tmp);

    let first = run(&bank, EvalOpts { sample_size: 20, probes: vec![] }).unwrap();
    let second = run(&bank, EvalOpts { sample_size: 20, probes: vec![] }).unwrap();

    let prev = second.previous.expect("second run must see the first");
    assert_eq!(prev.corpus_fingerprint, first.corpus_fingerprint);
    assert!(!second.corpus_changed);
    // Search determinism: identical corpus and config, identical numbers.
    // (Sampling determinism is pinned by the unit tests in eval.rs —
    // sample_is_deterministic and sample_survives_rechunking — not here: this
    // fixture has fewer candidate sentences than sample_size, so truncate is a
    // no-op.)
    assert_eq!(second.metrics.recall_1, first.metrics.recall_1);
}

#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn custom_probes_override_the_builtin_set() {
    let tmp = TempDir::new().unwrap();
    let bank = fixture_bank(&tmp);

    let report = run(&bank, EvalOpts {
        sample_size: 10,
        probes: vec!["comment tailler un rosier en hiver".to_string()],
    }).unwrap();

    assert!(report.probe_p95.is_some(), "explicit probes must always apply");
}
