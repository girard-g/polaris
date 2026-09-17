//! Embedding-model selection for an index that does not exist yet (spec §4.3).

use std::path::PathBuf;

use crate::config::{PolarisConfig, apply_resolution, resolve_effective};
use crate::db::{IndexMetadata, has_index};
use crate::indexer::discover_indexable_markdown;
use crate::language::{choose_model, english_share};

/// Resolve `cfg` for an indexing run over `targets`, choosing the embedding
/// model from the corpus when this run will create the index.
///
/// Selection runs only when no index exists at `db_path` ([`has_index`], not
/// mere file existence) and `model_id` is not explicit; otherwise this is
/// [`resolve_effective`] and returns `None`, so an existing index always keeps
/// its model. When it runs, it reads the
/// Markdown files the run would index (the indexer's own discovery and
/// `max_file_size` guard), picks the model from their English-prose share,
/// takes dimension and threshold from that model unless set, and returns the
/// line to show before any download or embedding. Unreadable files are
/// skipped; no measurable prose keeps the default model.
///
/// Call it before loading the model, and create the database only after the
/// load succeeds: a failed download then leaves no database behind, and the
/// retry selects again.
pub fn resolve_for_new_index(
    cfg: &mut PolarisConfig,
    targets: &[PathBuf],
    recursive: bool,
) -> Option<String> {
    if cfg.explicit.model_id.is_some() || has_index(&cfg.db_path) {
        resolve_effective(cfg);
        return None;
    }

    // ponytail: holds every file's text at once; stream per file if a corpus
    // ever outgrows memory (max_file_size already caps each file).
    let texts: Vec<String> = targets
        .iter()
        .flat_map(|target| discover_indexable_markdown(target, recursive, cfg.max_file_size))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();
    let docs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let share = english_share(&docs);
    let model = choose_model(share);

    apply_resolution(
        cfg,
        IndexMetadata { model_id: Some(model.to_string()), embedding_dim: None },
    );

    let basis = match share {
        Some(s) => format!("{:.0}% English prose", s * 100.0),
        None => "no prose to measure".to_string(),
    };
    Some(format!("model: {model} ({basis}) — set model_id to override"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, register_vec_extension};
    use crate::language::test_fixtures::{EN_PROSE, FR_PROSE};
    use std::path::Path;

    fn corpus(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        for (name, body) in files {
            std::fs::write(docs.join(name), body).unwrap();
        }
        docs
    }

    fn cfg_at(dir: &Path) -> PolarisConfig {
        PolarisConfig { db_path: dir.join("polaris.db"), ..PolarisConfig::default() }
    }

    #[test]
    fn a_new_french_index_selects_embeddinggemma() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", FR_PROSE), ("b.md", FR_PROSE)]);
        let mut cfg = cfg_at(dir.path());

        let line = resolve_for_new_index(&mut cfg, &[docs], true).expect("selection ran");
        assert_eq!(line, "model: embeddinggemma-300m (0% English prose) — set model_id to override");
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 768));
        assert_eq!(cfg.search_min_similarity, Some(0.42));
        assert!(!cfg.db_path.exists(), "selection must not create the database");
    }

    #[test]
    fn a_new_english_index_keeps_nomic() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", EN_PROSE)]);
        let mut cfg = cfg_at(dir.path());

        let line = resolve_for_new_index(&mut cfg, &[docs], true).expect("selection ran");
        assert_eq!(line, "model: nomic-embed-text-v1.5 (100% English prose) — set model_id to override");
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 512));
        assert_eq!(cfg.search_min_similarity, Some(0.63));
    }

    #[test]
    fn a_corpus_without_prose_keeps_nomic() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("short.md", "# Title\n\nToo short to classify.\n")]);
        let mut cfg = cfg_at(dir.path());

        let line = resolve_for_new_index(&mut cfg, &[docs], true).expect("selection ran");
        assert_eq!(line, "model: nomic-embed-text-v1.5 (no prose to measure) — set model_id to override");
        assert_eq!(cfg.model_id, "nomic-embed-text-v1.5");
    }

    #[test]
    fn an_existing_index_is_never_reselected() {
        register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", EN_PROSE)]);
        let mut cfg = cfg_at(dir.path());
        drop(Database::open(&cfg.db_path, 768, "embeddinggemma-300m").unwrap());

        assert_eq!(resolve_for_new_index(&mut cfg, &[docs], true), None);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 768));
        assert_eq!(cfg.search_min_similarity, Some(0.42));
    }

    /// `touch polaris.db`, or a file left behind by a run that never got to
    /// write its model, is not an index: selection must still run, or a French
    /// corpus silently gets the English default.
    #[test]
    fn a_file_that_is_not_an_index_does_not_disable_selection() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", FR_PROSE)]);
        let mut cfg = cfg_at(dir.path());
        std::fs::write(&cfg.db_path, b"").unwrap();

        let line = resolve_for_new_index(&mut cfg, &[docs], true).expect("selection ran");
        assert!(line.starts_with("model: embeddinggemma-300m"), "{line}");
        assert_eq!(cfg.model_id, "embeddinggemma-300m");
    }

    #[test]
    fn an_explicit_model_disables_selection() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", FR_PROSE)]);
        let mut cfg = cfg_at(dir.path());
        cfg.apply_overrides(None, None, Some("mxbai-embed-large-v1".into()));

        assert_eq!(resolve_for_new_index(&mut cfg, &[docs], true), None);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("mxbai-embed-large-v1", 1024));
        assert_eq!(cfg.search_min_similarity, None);
    }

    #[test]
    fn an_explicit_dim_survives_selection() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("a.md", FR_PROSE)]);
        let mut cfg = cfg_at(dir.path());
        cfg.apply_overrides(None, Some(512), None);

        assert!(resolve_for_new_index(&mut cfg, &[docs], true).is_some());
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 512));
    }

    #[test]
    fn files_over_max_file_size_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let big_french = [FR_PROSE; 4].join("\n\n");
        let docs = corpus(dir.path(), &[("en.md", EN_PROSE), ("fr.md", big_french.as_str())]);
        let mut cfg = cfg_at(dir.path());
        cfg.max_file_size = (EN_PROSE.len() + 1) as u64;
        assert!(big_french.len() as u64 > cfg.max_file_size);

        let line = resolve_for_new_index(&mut cfg, &[docs], true).unwrap();
        assert!(line.starts_with("model: nomic-embed-text-v1.5 (100% English prose)"), "{line}");
    }

    #[test]
    fn every_target_is_read_including_single_files() {
        let dir = tempfile::tempdir().unwrap();
        let docs = corpus(dir.path(), &[("en.md", EN_PROSE)]);
        let french_file = dir.path().join("notes.md");
        std::fs::write(&french_file, FR_PROSE).unwrap();
        let mut cfg = cfg_at(dir.path());

        let line = resolve_for_new_index(&mut cfg, &[docs, french_file], true).unwrap();
        assert!(line.starts_with("model: embeddinggemma-300m"), "{line}");
    }
}
