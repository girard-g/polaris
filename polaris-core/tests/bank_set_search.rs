use polaris_core::{Bank, BankConfig, BankSet, IndexOpts, SearchOpts, SharedEmbedding};
use std::fs;
use tempfile::TempDir;

#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn bank_set_search_returns_attributed_results() {
    let tmp = TempDir::new().unwrap();

    // Bank A: about cats.
    let dir_a = tmp.path().join("bank_a");
    fs::create_dir_all(&dir_a).unwrap();
    fs::write(dir_a.join("cats.md"), "# Cats\n\nCats purr when content.\n").unwrap();

    // Bank B: about dogs.
    let dir_b = tmp.path().join("bank_b");
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_b.join("dogs.md"), "# Dogs\n\nDogs bark to alert their owners.\n").unwrap();

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();

    let bank_a = Bank::open(
        BankConfig {
            repo_root: dir_a.clone(),
            index_path: dir_a.join(".polaris/index.db"),
            embedding_dim: 64,
            model_id: "nomic-embed-text-v1.5".to_string(),
            ..Default::default()
        },
        embed.clone(),
    ).unwrap();
    bank_a.index_path(&dir_a, IndexOpts::default()).unwrap();

    let bank_b = Bank::open(
        BankConfig {
            repo_root: dir_b.clone(),
            index_path: dir_b.join(".polaris/index.db"),
            embedding_dim: 64,
            model_id: "nomic-embed-text-v1.5".to_string(),
            ..Default::default()
        },
        embed.clone(),
    ).unwrap();
    bank_b.index_path(&dir_b, IndexOpts::default()).unwrap();

    let mut set = BankSet::new(embed);
    set.mount(bank_a, "cats".to_string());
    set.mount(bank_b, "dogs".to_string());

    let results = set.search("bark", SearchOpts { top_k: 3 }).unwrap();
    assert!(!results.is_empty());
    // Top result should come from the dogs bank.
    assert_eq!(results[0].source_db.as_deref(), Some("dogs"));
}
