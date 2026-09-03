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

/// The gate's confidence must be a property of the query and the corpus, not of
/// how many results the caller asked for. It was not: the MCP tool took `max`
/// over the MMR-reranked, truncated output, and MMR's selection is not nested in
/// `top_k`, so on this repo's own docs "configure OAuth SSO for the web
/// dashboard" reported 0.673 at `top_k=5` and 0.622 at `top_k=10` — admitted by
/// a 0.65 gate at one `k`, rejected at another, same query, same index.
///
/// Ceiling: this fixture is small enough that MMR may not reshuffle across `k`,
/// so a regression is caught by the equality invariant rather than by
/// reproducing the original divergence.
#[test]
#[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
fn confidence_is_independent_of_top_k() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    for (name, body) in [
        ("retrieval", "Hybrid retrieval fuses vector KNN and BM25 with reciprocal rank fusion."),
        ("ranking", "Ranking applies a heading boost before maximal marginal relevance reranking."),
        ("chunking", "Chunking splits markdown at section headings and keeps heading context."),
        ("storage", "Storage uses SQLite with sqlite-vec for vectors and FTS5 for keyword search."),
        ("embedding", "Embedding runs a local ONNX model and truncates vectors matryoshka style."),
        ("config", "Configuration lives in polaris.toml and validates dimensions on open."),
    ] {
        fs::write(
            tmp.path().join(format!("docs/{name}.md")),
            format!("# {name}\n\n{body}\n\n## Detail\n\n{body} {body}\n"),
        )
        .unwrap();
    }

    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).expect("load model");
    let cfg = BankConfig {
        repo_root: tmp.path().to_path_buf(),
        index_path: tmp.path().join(".polaris/index.db"),
        embedding_dim: 64,
        model_id: "nomic-embed-text-v1.5".to_string(),
        ..Default::default()
    };
    let bank = Bank::open(cfg, embed).expect("open bank");
    bank.index_path(&tmp.path().join("docs"), IndexOpts::default()).expect("index");

    for query in ["how does ranking work", "how do I bake sourdough bread"] {
        let mut seen: Option<f32> = None;
        for top_k in [1usize, 2, 3, 5, 10] {
            let (results, confidence) = bank
                .search_with_confidence(query, SearchOpts { top_k })
                .expect("search");

            let set_max = results.iter().map(|r| r.score).fold(0.0f32, f32::max);
            assert!(
                confidence >= set_max - 1e-6,
                "{query:?} at top_k={top_k}: confidence {confidence} is below the set max \
                 {set_max}, so it is not the corpus's best match"
            );

            match seen {
                None => seen = Some(confidence),
                Some(first) => assert!(
                    (confidence - first).abs() < 1e-6,
                    "{query:?}: confidence moved with top_k ({first} -> {confidence} at \
                     top_k={top_k}); the gate's verdict must not depend on the caller"
                ),
            }
        }
    }
}
