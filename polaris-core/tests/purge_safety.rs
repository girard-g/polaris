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

#[test]
fn index_path_keeps_rows_when_root_is_relative() {
    // The CLI passes whatever path the user typed straight through (e.g.
    // `polaris index .` or `polaris index docs`), with no canonicalization.
    // Stored paths are exactly what `WalkDir::new(root)` produced, so for a
    // relative `root` they are relative too. Naively re-joining `root` onto an
    // already root-prefixed stored path (`root.join(db_path)`) double-prefixes
    // it into a nonexistent path and would wrongly purge a live row. Create
    // the tempdir *inside* the current cwd (instead of chdir'ing, which would
    // race other tests running in parallel in this process) so a genuinely
    // relative root can be constructed.
    register_vec_extension();
    let cwd = std::env::current_dir().unwrap();
    let tmp = tempfile::tempdir_in(&cwd).unwrap();
    let root_abs = tmp.path();
    let root_rel = root_abs.strip_prefix(&cwd).unwrap();

    let pdf_path = root_abs.join("manual.pdf");
    fs::write(&pdf_path, b"%PDF-1.4 fake").unwrap();

    let db = Database::open_in_memory(64, "test-model").unwrap();
    let norm = normalise_path(&root_rel.join("manual.pdf")).unwrap();
    db.insert_document(&norm, "deadbeefhash", None, 13).unwrap();

    let indexer = Indexer::new_dry_run(450, 200, 10 * 1024 * 1024);
    indexer
        .index_path(&db, root_rel, true, false, false, None)
        .unwrap();

    let hashes = db.get_all_document_hashes().unwrap();
    assert!(
        hashes.iter().any(|(p, _)| p == &norm),
        "manual.pdf row was purged under a relative root even though the file exists on disk"
    );
}
