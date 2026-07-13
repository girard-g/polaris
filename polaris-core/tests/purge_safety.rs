//! Regression tests for the purge-safety fix: `index_path` must delete a
//! `documents` row only when its source file no longer exists on disk, not
//! merely because it fell outside the current run's *discovery* set (e.g. a
//! markdown-only run must never erase a Pro-indexed PDF/code row).
//!
//! These tests use `Database` + `Indexer::new_dry_run` directly (no embedding
//! model) since the scenarios below never need to (re)embed anything: the
//! foreign row is inserted directly via the public `Database` API, and no new
//! `.md` files are discovered in either run.

use polaris_core::{Database, Indexer, normalise_path, register_vec_extension};
use std::fs;

#[test]
fn index_path_keeps_rows_whose_source_file_still_exists() {
    register_vec_extension();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // A real file on disk simulating a Pro-indexed PDF (not part of the
    // free-tier `.md` discovery set).
    let pdf_path = root.join("manual.pdf");
    fs::write(&pdf_path, b"%PDF-1.4 fake").unwrap();

    let db = Database::open_in_memory(64, "test-model").unwrap();
    let norm = normalise_path(&pdf_path).unwrap();
    db.insert_document(&norm, "deadbeefhash", None, 13).unwrap();

    // Markdown-only run: no .md files exist, so discovery is empty. The
    // foreign row must survive because its source file still exists on disk.
    let indexer = Indexer::new_dry_run(450, 200, 10 * 1024 * 1024);
    indexer
        .index_path(&db, root, true, false, false, None)
        .unwrap();

    let hashes = db.get_all_document_hashes().unwrap();
    assert!(
        hashes.iter().any(|(p, _)| p == &norm),
        "manual.pdf row was purged even though the file exists on disk"
    );
}

#[test]
fn index_path_still_purges_truly_deleted_files() {
    register_vec_extension();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let gone_path = root.join("gone.md");
    fs::write(&gone_path, "# Gone").unwrap();

    let db = Database::open_in_memory(64, "test-model").unwrap();
    let norm = normalise_path(&gone_path).unwrap();
    db.insert_document(&norm, "somehash", None, 6).unwrap();

    // Source file genuinely deleted before the next run.
    fs::remove_file(&gone_path).unwrap();

    let indexer = Indexer::new_dry_run(450, 200, 10 * 1024 * 1024);
    indexer
        .index_path(&db, root, true, false, false, None)
        .unwrap();

    let hashes = db.get_all_document_hashes().unwrap();
    assert!(
        !hashes.iter().any(|(p, _)| p == &norm),
        "deleted file was not purged"
    );
}
