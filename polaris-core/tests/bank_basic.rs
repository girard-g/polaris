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
fn bank_open_index_search_roundtrip() {
    let tmp = TempDir::new().unwrap();
    make_fixture(tmp.path());

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).expect("load model");

    let cfg = BankConfig {
        repo_root: tmp.path().to_path_buf(),
        index_path: tmp.path().join(".polaris/index.db"),
        embedding_dim: 64,
        model_id: "nomic-embed-text-v1.5".to_string(),
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
