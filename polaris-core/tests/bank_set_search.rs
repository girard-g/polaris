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

/// The CLI reaches search through `BankSet` and the MCP tool through `Bank`.
/// Those must be the same search. They were not: `BankSet` re-sorted a single
/// bank by relevance while `Bank` returned MMR's selection order, so identical
/// queries against an identical index came back in different sequences
/// depending on which interface asked — and `polaris eval`, which measures the
/// `Bank` path, was not measuring what `polaris search` printed.
#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn bank_and_single_bank_bankset_agree_on_order() {
    let tmp = TempDir::new().unwrap();
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    for (name, body) in [
        ("chunking", "Chunking splits markdown at section headings and keeps the heading context with every fragment it produces."),
        ("ranking", "Ranking fuses two ranked lists and then applies maximal marginal relevance so the results are not near duplicates."),
        ("storage", "Storage keeps vectors in sqlite-vec and keyword terms in an FTS5 table alongside the documents."),
        ("embedding", "Embedding runs a local ONNX model and truncates the resulting vector matryoshka style."),
        ("headings", "Heading context travels with each chunk so a fragment can still be attributed to the section it came from."),
        ("overlap", "Overlapping characters are copied from the previous fragment so a sentence split across a boundary survives."),
    ] {
        fs::write(
            docs.join(format!("{name}.md")),
            format!("# {name}\n\n{body}\n\n## More\n\n{body}\n"),
        ).unwrap();
    }

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 512).unwrap();
    let bank = Bank::open(
        BankConfig {
            repo_root: tmp.path().to_path_buf(),
            index_path: tmp.path().join(".polaris/index.db"),
            embedding_dim: 512,
            model_id: "nomic-embed-text-v1.5".to_string(),
            ..Default::default()
        },
        embed.clone(),
    ).unwrap();
    bank.index_path(&docs, IndexOpts::default()).unwrap();

    let mut set = BankSet::new(embed);
    set.mount(bank.clone(), "only".to_string());

    for query in [
        "how does chunking keep heading context",
        "what happens to a sentence split across a boundary",
        "where are vectors stored",
    ] {
        for top_k in [2usize, 5] {
            let direct = bank.search(query, SearchOpts { top_k }).unwrap();
            let via_set = set.search(query, SearchOpts { top_k }).unwrap();

            let ids = |v: &[polaris_core::SearchResult]| -> Vec<i64> {
                v.iter().map(|r| r.chunk_id).collect()
            };
            assert_eq!(
                ids(&direct),
                ids(&via_set),
                "{query:?} at top_k={top_k}: Bank and single-bank BankSet returned \
                 different orderings, so the CLI and the MCP tool disagree"
            );

            let scores = |v: &[polaris_core::SearchResult]| -> Vec<String> {
                v.iter().map(|r| format!("{:.4}", r.score)).collect()
            };
            assert_eq!(scores(&direct), scores(&via_set), "{query:?}: scores diverged");
        }
    }
}
