use polaris_core::{Bank, BankConfig, IndexOpts, SearchOpts, SharedEmbedding};
use std::fs;
use tempfile::TempDir;

fn make_fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::write(
        dir.join("docs/intro.md"),
        "# Introduction\n\nPolaris is a lightweight retrieval system for markdown corpora.\n",
    )
    .unwrap();
    fs::write(
        dir.join("docs/usage.md"),
        "# Usage\n\nCall `Bank::search` with a query to retrieve top-k chunks.\n",
    )
    .unwrap();
}

#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn bank_open_index_search_roundtrip() {
    let tmp = TempDir::new().unwrap();
    make_fixture(tmp.path());

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).expect("load model");

    let cfg = BankConfig {
        repo_root: tmp.path().to_path_buf(),
        index_path: tmp.path().join(".polaris/index.db"),
        embedding_dim: 64,
        model_id: "nomic-embed-text-v1.5".to_string(),
        ..Default::default()
    };

    let bank = Bank::open(cfg, embed).expect("open bank");

    let report = bank
        .index_path(&tmp.path().join("docs"), IndexOpts::default())
        .expect("index");
    assert_eq!(report.added.len(), 2, "should have indexed both files");

    let results = bank
        .search("retrieval", SearchOpts::default())
        .expect("search");
    assert!(!results.is_empty(), "search should return at least one result");
}

/// Scores used to be normalised by the per-call maximum, so the top hit read
/// exactly 1.0 whatever the query — "best of this set" wearing the costume of
/// "certain". Pin that the reported score is now the absolute query-chunk
/// cosine: an off-topic query must not come back looking confident, and must
/// rank well below a query the corpus actually answers.
#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn search_score_is_absolute_not_normalised() {
    let tmp = TempDir::new().unwrap();
    make_fixture(tmp.path());

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 512).expect("load model");
    let bank = Bank::open(
        BankConfig {
            repo_root: tmp.path().to_path_buf(),
            index_path: tmp.path().join(".polaris/index.db"),
            embedding_dim: 512,
            model_id: "nomic-embed-text-v1.5".to_string(),
            ..Default::default()
        },
        embed,
    )
    .expect("open bank");
    bank.index_path(&tmp.path().join("docs"), IndexOpts::default()).expect("index");

    let top = |q: &str| {
        bank.search(q, SearchOpts::default())
            .expect("search")
            .iter()
            .map(|r| r.score)
            .fold(f32::MIN, f32::max)
    };

    let off_topic = top("bread and butter sandwich recipe");
    let on_topic = top("how do I retrieve top-k chunks from a markdown corpus");

    assert!(
        off_topic < 0.65,
        "off-topic query should not look confident, got {off_topic:.3}"
    );
    assert!(
        on_topic > 0.65,
        "on-topic query should clear the default threshold, got {on_topic:.3}"
    );
    assert!(
        on_topic - off_topic > 0.1,
        "scores must separate on relevance: on={on_topic:.3} off={off_topic:.3}"
    );
}
