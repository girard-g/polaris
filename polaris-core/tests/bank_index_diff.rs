use polaris_core::{Bank, BankConfig, IndexOpts, SearchOpts, SharedEmbedding};
use std::fs;
use tempfile::TempDir;

#[test]
fn index_diff_handles_added_modified_removed() {
    let tmp = TempDir::new().unwrap();
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("a.md"), "# A\n\nFirst doc.\n").unwrap();
    fs::write(docs.join("b.md"), "# B\n\nSecond doc.\n").unwrap();

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
    let cfg = BankConfig {
        repo_root: tmp.path().to_path_buf(),
        index_path: tmp.path().join(".polaris/index.db"),
        embedding_dim: 64,
        model_id: "nomic-embed-text-v1.5".to_string(),
    };
    let bank = Bank::open(cfg, embed).unwrap();

    // Initial full index.
    let report = bank.index_path(&docs, IndexOpts::default()).unwrap();
    assert_eq!(report.added.len(), 2);

    // Modify a.md, add c.md, remove b.md.
    fs::write(docs.join("a.md"), "# A v2\n\nFirst doc updated.\n").unwrap();
    fs::write(docs.join("c.md"), "# C\n\nThird doc added.\n").unwrap();
    fs::remove_file(docs.join("b.md")).unwrap();

    let changed = vec![docs.join("a.md"), docs.join("c.md")];
    let removed = vec![docs.join("b.md")];

    let diff_report = bank.index_diff(&changed, &removed).unwrap();
    assert_eq!(diff_report.added.len(), 1, "c.md should be added");
    assert_eq!(diff_report.modified.len(), 1, "a.md should be modified");
    assert_eq!(diff_report.removed.len(), 1, "b.md should be removed");

    // Verify search reflects the new state.
    let results = bank.search("third", SearchOpts::default()).unwrap();
    assert!(results.iter().any(|r| r.file_path.contains("c.md")));
    let stale = bank.search("second", SearchOpts::default()).unwrap();
    assert!(stale.iter().all(|r| !r.file_path.contains("b.md")),
        "b.md should no longer appear in results");
}
