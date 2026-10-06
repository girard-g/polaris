use polaris_core::{Bank, BankConfig, InMemoryDoc, SearchOpts, SharedEmbedding};
use std::path::PathBuf;

/// End-to-end proof that supplied Markdown is chunked, embedded, stored under
/// the ORIGINAL source path, and skipped on an unchanged re-run.
///
/// Gated exactly like the other embedding tests: `Bank::open` requires a
/// `SharedEmbedding`, which downloads a ~137 MB ONNX model. The hermetic
/// routing / skip-unchanged behavior is covered offline by the unit tests in
/// `indexer.rs` (`index_files_items_*`); this test additionally proves the
/// chunk-writing path, which is only reachable with a real embedding engine.
#[test]
#[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
fn index_documents_stores_supplied_markdown_and_skips_unchanged() {
    polaris_core::register_vec_extension();
    let tmp = tempfile::tempdir().unwrap();
    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
    let bank = Bank::open(
        BankConfig {
            repo_root: tmp.path().to_path_buf(),
            index_path: tmp.path().join("polaris.db"),
            embedding_dim: 64,
            model_id: "nomic-embed-text-v1.5".into(),
            ..Default::default()
        },
        embed,
    )
    .unwrap();

    let doc = || InMemoryDoc {
        source_path: PathBuf::from("docs/manual.pdf"),
        markdown: "# Manual\n\nInstall by running the setup wizard.".to_string(),
        hash: "hash-v1".to_string(),
        title: Some("Manual".to_string()),
    };

    // (1) Supplied markdown is stored & chunked under the ORIGINAL path.
    let report = bank.index_documents(vec![doc()], &[], false).unwrap();
    assert_eq!(report.added.len(), 1);

    let chunks = bank.chunks_for(&PathBuf::from("docs/manual.pdf")).unwrap();
    assert!(
        !chunks.is_empty(),
        "expected chunks under the original path"
    );

    // The supplied title flows through to the stored document row.
    let stored =
        polaris_core::Database::open(&tmp.path().join("polaris.db"), 64, "nomic-embed-text-v1.5")
            .unwrap()
            .get_document_by_path("docs/manual.pdf")
            .unwrap()
            .expect("document row stored under original path");
    assert_eq!(stored.title.as_deref(), Some("Manual"));

    let hits = bank
        .search("setup wizard install", SearchOpts { top_k: 5 })
        .unwrap();
    assert!(hits.iter().any(|h| h.file_path.ends_with("manual.pdf")));

    // (2) Unchanged supplied hash is skipped on re-run (not re-embedded).
    let r2 = bank.index_documents(vec![doc()], &[], false).unwrap();
    assert_eq!(
        r2.added.len() + r2.modified.len(),
        0,
        "unchanged hash should be skipped"
    );
    assert_eq!(r2.unchanged.len(), 1);

    // (3) --force re-writes the SAME unchanged hash (the fix: force must honor
    //     through to the embedding-write path, not be swallowed as unchanged).
    let r3 = bank.index_documents(vec![doc()], &[], true).unwrap();
    assert_eq!(
        r3.added.len() + r3.modified.len(),
        1,
        "force must re-index an unchanged hash"
    );
}

/// `index_documents_with_progress` must report embedding progress for
/// in-memory documents the same way `index_path_with_progress` does for
/// on-disk files: fractions non-decreasing, within `[0, 1]`, ending at `1.0`.
///
/// Gated like the other embedding tests above: `Bank::open` requires a real
/// `SharedEmbedding`.
#[test]
#[ignore = "Bank::open requires SharedEmbedding which downloads a ~137 MB ONNX model"]
fn index_documents_with_progress_reports_non_decreasing_fractions_ending_at_one() {
    use std::sync::{Arc, Mutex};

    polaris_core::register_vec_extension();
    let tmp = tempfile::tempdir().unwrap();
    let embed = SharedEmbedding::load("nomic-embed-text-v1.5", 64).unwrap();
    let bank = Bank::open(
        BankConfig {
            repo_root: tmp.path().to_path_buf(),
            index_path: tmp.path().join("polaris.db"),
            embedding_dim: 64,
            model_id: "nomic-embed-text-v1.5".into(),
            ..Default::default()
        },
        embed,
    )
    .unwrap();

    // Enough content/documents to produce several chunks, so Phase B (the
    // embedding loop that drives the progress callback) actually runs.
    let docs: Vec<InMemoryDoc> = (0..5)
        .map(|i| InMemoryDoc {
            source_path: PathBuf::from(format!("docs/manual-{i}.pdf")),
            markdown: format!(
                "# Manual {i}\n\n{}",
                "Install by running the setup wizard. ".repeat(200)
            ),
            hash: format!("hash-v1-{i}"),
            title: Some(format!("Manual {i}")),
        })
        .collect();

    let calls: Arc<Mutex<Vec<(f32, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let report = bank
        .index_documents_with_progress(
            docs,
            &[],
            false,
            Box::new(move |fraction, message| {
                calls_clone
                    .lock()
                    .unwrap()
                    .push((fraction, message.to_string()));
            }),
        )
        .unwrap();
    assert_eq!(report.added.len(), 5);

    let calls = calls.lock().unwrap();
    assert!(!calls.is_empty(), "expected at least one progress callback");

    let mut prev = 0.0f32;
    for (fraction, _) in calls.iter() {
        assert!(
            (0.0..=1.0).contains(fraction),
            "fraction {fraction} out of [0, 1]"
        );
        assert!(
            *fraction >= prev,
            "fractions must be non-decreasing: {prev} then {fraction}"
        );
        prev = *fraction;
    }

    assert_eq!(
        calls.last().unwrap().0,
        1.0,
        "final callback must report completion at fraction 1.0"
    );
}
