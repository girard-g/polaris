use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PolarisError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolarisConfig {
    /// Path to the SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,

    /// Embedding dimension. Effective value; see [`resolve_effective`].
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,

    /// Maximum chunk size in approximate tokens (chars / 4)
    #[serde(default = "default_max_chunk_tokens")]
    pub max_chunk_tokens: usize,

    /// Overlap in characters between adjacent non-heading chunks
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// fastembed model ID. Effective value; see [`resolve_effective`].
    #[serde(default = "default_model_id")]
    pub model_id: String,

    /// MMR lambda: 0.0 = pure diversity, 1.0 = pure relevance
    #[serde(default = "default_mmr_lambda")]
    pub mmr_lambda: f32,

    /// Fetch `top_k * mmr_candidate_multiplier` candidates before MMR reranking
    #[serde(default = "default_mmr_candidate_multiplier")]
    pub mmr_candidate_multiplier: usize,

    /// Max additive score boost for heading matches (0.0 disables)
    #[serde(default = "default_heading_boost")]
    pub heading_boost: f32,

    /// RRF k constant for Reciprocal Rank Fusion (higher → smoother score distribution)
    #[serde(default = "default_rrf_k")]
    pub rrf_k: usize,

    /// Maximum allowed `top_k` value for search requests (caps runaway queries)
    #[serde(default = "default_max_top_k")]
    pub max_top_k: usize,

    /// Minimum query-to-chunk cosine similarity for the MCP `search` tool to
    /// return results and the auto-search hook to inject one. Effective value;
    /// see [`resolve_effective`]. `None` means uncalibrated: search is not
    /// gated and the hook stays silent.
    #[serde(default = "default_search_min_similarity")]
    pub search_min_similarity: Option<f32>,

    /// Maximum file size in bytes that the indexer will process (larger files are skipped)
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,

    /// Additional database paths for multi-DB search (read-only)
    #[serde(default)]
    pub extra_db_paths: Vec<PathBuf>,

    /// `polaris eval` settings
    #[serde(default)]
    pub eval: EvalConfig,

    /// The values the user actually set — in the config file, or through
    /// `--model` / `--dim`. `None` means defaulted: [`resolve_effective`] fills
    /// the matching public field from the index, then from the model table.
    #[serde(skip)]
    pub explicit: Explicit,
}

/// Which model-dependent settings were set by the user rather than defaulted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Explicit {
    pub model_id: Option<String>,
    pub embedding_dim: Option<usize>,
    pub search_min_similarity: Option<f32>,
}

fn default_db_path() -> PathBuf {
    PathBuf::from("polaris.db")
}

fn default_embedding_dim() -> usize {
    crate::embedding::default_dim_for(crate::embedding::DEFAULT_MODEL_ID)
        .expect("the default model is in the model table")
}

fn default_max_chunk_tokens() -> usize {
    450
}

fn default_chunk_overlap_chars() -> usize {
    200
}

fn default_model_id() -> String {
    crate::embedding::DEFAULT_MODEL_ID.to_string()
}

fn default_mmr_lambda() -> f32 {
    0.7
}

fn default_mmr_candidate_multiplier() -> usize {
    3
}

fn default_heading_boost() -> f32 {
    0.05
}

/// Measured against this repo's own index with the default
/// `nomic-embed-text-v1.5`. `polaris eval` over 200 corpus sentences puts the
/// 10th percentile of on-topic scores at 0.632 and the 95th percentile of
/// off-topic probes at 0.628, and recommends their midpoint: 0.63.
///
/// Those two numbers are 0.004 apart, so the classes overlap and no threshold
/// separates them — this value trades false negatives against false positives
/// rather than removing either. It was 0.65, which cost roughly twice the false
/// negatives for the same false-positive rate on this corpus.
///
/// Before resolution: the default model's threshold, so a config that is never
/// resolved gates exactly as it always has. The evidence behind each model's
/// value lives with the model table in `embedding.rs`.
fn default_search_min_similarity() -> Option<f32> {
    crate::embedding::default_threshold_for(crate::embedding::DEFAULT_MODEL_ID)
        .expect("the default model is in the model table")
}

fn default_rrf_k() -> usize {
    60
}

fn default_max_top_k() -> usize {
    50
}

fn default_max_file_size() -> u64 {
    10 * 1024 * 1024 // 10 MB
}

/// Settings for `polaris eval`. Its own table because these are the only
/// knobs a user tunes per-run rather than per-query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalConfig {
    /// Number of corpus sentences sampled as queries.
    #[serde(default = "default_eval_sample_size")]
    pub sample_size: usize,

    /// Off-topic probe queries used to measure what this corpus scores when it
    /// has no answer. Empty means "use the built-in set for the detected
    /// corpus language"; a non-empty list replaces the built-in set and skips
    /// detection, because writing in a language states it.
    #[serde(default)]
    pub probes: Vec<String>,
}

fn default_eval_sample_size() -> usize {
    200
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self { sample_size: default_eval_sample_size(), probes: Vec::new() }
    }
}

impl Default for PolarisConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            embedding_dim: default_embedding_dim(),
            max_chunk_tokens: default_max_chunk_tokens(),
            chunk_overlap_chars: default_chunk_overlap_chars(),
            model_id: default_model_id(),
            mmr_lambda: default_mmr_lambda(),
            mmr_candidate_multiplier: default_mmr_candidate_multiplier(),
            heading_boost: default_heading_boost(),
            rrf_k: default_rrf_k(),
            max_top_k: default_max_top_k(),
            search_min_similarity: default_search_min_similarity(),
            max_file_size: default_max_file_size(),
            extra_db_paths: Vec::new(),
            eval: EvalConfig::default(),
            explicit: Explicit::default(),
        }
    }
}

/// Shared range checks for the tuning parameters, so every entry point that
/// builds a config (`PolarisConfig` from the CLI, `BankConfig` from a library
/// caller) enforces one rule rather than its own copy of the bounds.
pub fn validate_params(
    model_id: &str,
    embedding_dim: usize,
    max_chunk_tokens: usize,
    chunk_overlap_chars: usize,
) -> Result<()> {
    let native_dim = crate::embedding::native_dim_for(model_id)?;

    if embedding_dim < 64 || embedding_dim > native_dim {
        return Err(PolarisError::Config(format!(
            "embedding_dim must be in [64, {native_dim}] for model '{model_id}', got {embedding_dim}"
        )));
    }
    if max_chunk_tokens == 0 {
        return Err(PolarisError::Config(
            "max_chunk_tokens must be greater than 0".to_string(),
        ));
    }
    // Saturating: `max_chunk_tokens` is unbounded above, and a nonsense config
    // value near `usize::MAX` would otherwise panic here in debug builds.
    let max_overlap = max_chunk_tokens.saturating_mul(4);
    if chunk_overlap_chars >= max_overlap {
        return Err(PolarisError::Config(format!(
            "chunk_overlap_chars ({chunk_overlap_chars}) must be less than max_chunk_tokens * 4 ({max_overlap})"
        )));
    }
    Ok(())
}

impl PolarisConfig {
    /// Load config following the priority chain:
    /// explicit path > ./polaris.toml > ~/.config/polaris/polaris.toml > defaults
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let path = if let Some(p) = explicit_path {
            if p.exists() {
                Some(p.to_path_buf())
            } else {
                return Err(PolarisError::Config(format!(
                    "Config file not found: {}",
                    p.display()
                )));
            }
        } else if Path::new("polaris.toml").exists() {
            Some(PathBuf::from("polaris.toml"))
        } else if let Some(cfg_dir) = dirs::config_dir() {
            let global = cfg_dir.join("polaris").join("polaris.toml");
            if global.exists() { Some(global) } else { None }
        } else {
            None
        };

        match path {
            None => Ok(Self::default()),
            Some(p) => {
                let raw = std::fs::read_to_string(&p).map_err(|e| {
                    PolarisError::Config(format!("Cannot read {}: {e}", p.display()))
                })?;
                let mut cfg: Self = toml::from_str(&raw).map_err(|e| {
                    PolarisError::Config(format!("Invalid TOML in {}: {e}", p.display()))
                })?;
                // Serde fills absent keys with defaults, which erases whether the
                // user wrote them. The key set is what records it.
                let keys: toml::Table = toml::from_str(&raw).map_err(|e| {
                    PolarisError::Config(format!("Invalid TOML in {}: {e}", p.display()))
                })?;
                cfg.explicit = Explicit {
                    model_id: keys.contains_key("model_id").then(|| cfg.model_id.clone()),
                    embedding_dim: keys.contains_key("embedding_dim").then_some(cfg.embedding_dim),
                    search_min_similarity: if keys.contains_key("search_min_similarity") {
                        cfg.search_min_similarity
                    } else {
                        None
                    },
                };
                Ok(cfg)
            }
        }
    }

    /// Validate config values, returning a descriptive error if any are out of range.
    pub fn validate(&self) -> Result<()> {
        validate_params(
            &self.model_id,
            self.embedding_dim,
            self.max_chunk_tokens,
            self.chunk_overlap_chars,
        )?;
        if let Some(t) = self.search_min_similarity {
            if !(0.0..=1.0).contains(&t) {
                return Err(PolarisError::Config(format!(
                    "search_min_similarity must be in [0.0, 1.0], got {t}"
                )));
            }
        }
        if self.eval.sample_size == 0 {
            return Err(PolarisError::Config(
                "eval.sample_size must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }

    /// Apply CLI overrides (None means "not specified", keep existing value).
    /// A given `--dim` / `--model` counts as explicit.
    pub fn apply_overrides(
        &mut self,
        db_path: Option<PathBuf>,
        embedding_dim: Option<usize>,
        model_id: Option<String>,
    ) {
        if let Some(p) = db_path {
            self.db_path = p;
        }
        if let Some(d) = embedding_dim {
            self.embedding_dim = d;
            self.explicit.embedding_dim = Some(d);
        }
        if let Some(m) = model_id {
            self.explicit.model_id = Some(m.clone());
            self.model_id = m;
        }
    }
}

/// Fill the effective model-dependent settings (spec §3.2): what the user set,
/// then the index at `db_path`, then the model table.
///
/// Never fails and never creates a file: the index is peeked read-only and a
/// missing or unreadable one falls through; `Database::open` reports genuine
/// problems afterwards. A value assigned straight to a public field without
/// recording it in [`PolarisConfig::explicit`] counts as defaulted and is
/// overwritten — set model and dim through `apply_overrides`.
pub fn resolve_effective(cfg: &mut PolarisConfig) {
    let source = crate::db::read_index_metadata(&cfg.db_path);
    apply_resolution(cfg, source);
}

/// Resolve against `source` — an existing index's metadata, or a model chosen
/// for a new index — instead of peeking at `db_path`.
pub(crate) fn apply_resolution(cfg: &mut PolarisConfig, source: crate::db::IndexMetadata) {
    cfg.model_id = cfg
        .explicit
        .model_id
        .clone()
        .or_else(|| source.model_id.clone())
        .unwrap_or_else(default_model_id);

    // A stored dimension belongs to the stored model. When an explicit model
    // contradicts the index, use that model's default instead: the stored one
    // can exceed its native size, and `validate` would then report a dimension
    // error in place of the `ModelMismatch` that `Database::open` gives.
    let source_dim = source
        .embedding_dim
        .filter(|_| source.model_id.as_deref().is_none_or(|m| m == cfg.model_id));
    // Unknown model: leave dim and threshold alone; `validate` reports the model.
    if let Some(dim) = cfg
        .explicit
        .embedding_dim
        .or(source_dim)
        .or_else(|| crate::embedding::default_dim_for(&cfg.model_id).ok())
    {
        cfg.embedding_dim = dim;
    }

    if let Some(t) = cfg.explicit.search_min_similarity {
        cfg.search_min_similarity = Some(t);
    } else if let Ok(default) = crate::embedding::default_threshold_for(&cfg.model_id) {
        cfg.search_min_similarity = default;
    }
}

/// Options for `Bank::index_path` and `Bank::index_diff`.
#[derive(Debug, Clone)]
pub struct IndexOpts {
    pub recursive: bool,
    pub force: bool,
    pub dry_run: bool,
}

impl Default for IndexOpts {
    fn default() -> Self {
        Self { recursive: true, force: false, dry_run: false }
    }
}

/// Options for `Bank::search` and `BankSet::search`.
#[derive(Debug, Clone)]
pub struct SearchOpts {
    pub top_k: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self { top_k: 5 }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn validate_params_survives_absurd_chunk_size() {
        // `max_chunk_tokens * 4` used to overflow here: a debug-build panic
        // instead of a config error.
        assert!(validate_params("nomic-embed-text-v1.5", 512, usize::MAX, 200).is_ok());
        // …and saturating must not quietly disable the bound it guards.
        assert!(validate_params("nomic-embed-text-v1.5", 512, 10, 40).is_err());
    }

    #[test]
    fn default_values_correct() {
        let cfg = PolarisConfig::default();
        assert_eq!(cfg.db_path, PathBuf::from("polaris.db"));
        assert_eq!(cfg.embedding_dim, 512);
        assert_eq!(cfg.max_chunk_tokens, 450);
        assert_eq!(cfg.chunk_overlap_chars, 200);
        assert_eq!(cfg.model_id, "nomic-embed-text-v1.5");
        assert!((cfg.mmr_lambda - 0.7).abs() < f32::EPSILON);
        assert_eq!(cfg.mmr_candidate_multiplier, 3);
        assert!((cfg.heading_boost - 0.05).abs() < f32::EPSILON);
        assert_eq!(cfg.rrf_k, 60);
    }

    #[test]
    fn toml_parse_all_fields() {
        let raw = r#"
            db_path = "custom.db"
            embedding_dim = 128
            max_chunk_tokens = 300
            chunk_overlap_chars = 100
            model_id = "custom-model"
        "#;
        let cfg: PolarisConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.db_path, PathBuf::from("custom.db"));
        assert_eq!(cfg.embedding_dim, 128);
        assert_eq!(cfg.max_chunk_tokens, 300);
        assert_eq!(cfg.chunk_overlap_chars, 100);
        assert_eq!(cfg.model_id, "custom-model");
    }

    #[test]
    fn toml_parse_partial_fields_uses_defaults() {
        let raw = "embedding_dim = 512";
        let cfg: PolarisConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.embedding_dim, 512);
        assert_eq!(cfg.db_path, PathBuf::from("polaris.db"));
        assert_eq!(cfg.max_chunk_tokens, 450);
        assert_eq!(cfg.chunk_overlap_chars, 200);
    }

    #[test]
    fn toml_parse_empty_uses_all_defaults() {
        let cfg: PolarisConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.embedding_dim, 512);
        assert_eq!(cfg.db_path, PathBuf::from("polaris.db"));
        assert_eq!(cfg.model_id, "nomic-embed-text-v1.5");
        assert!((cfg.mmr_lambda - 0.7).abs() < f32::EPSILON);
        assert_eq!(cfg.mmr_candidate_multiplier, 3);
        assert!((cfg.heading_boost - 0.05).abs() < f32::EPSILON);
        assert_eq!(cfg.rrf_k, 60);
    }

    #[test]
    fn apply_overrides_with_some_values() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(Some(PathBuf::from("override.db")), Some(384), None);
        assert_eq!(cfg.db_path, PathBuf::from("override.db"));
        assert_eq!(cfg.embedding_dim, 384);
    }

    #[test]
    fn apply_overrides_with_none_unchanged() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(None, None, None);
        assert_eq!(cfg.db_path, PathBuf::from("polaris.db"));
        assert_eq!(cfg.embedding_dim, 512);
    }

    #[test]
    fn validate_defaults_ok() {
        PolarisConfig::default().validate().unwrap();
    }

    #[test]
    fn validate_embedding_dim_too_small() {
        let mut cfg = PolarisConfig::default();
        cfg.embedding_dim = 32;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("embedding_dim"), "{err}");
    }

    #[test]
    fn validate_embedding_dim_too_large() {
        let mut cfg = PolarisConfig::default(); // nomic, native_dim=768
        cfg.embedding_dim = 769;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("embedding_dim"), "{err}");
    }

    #[test]
    fn validate_embedding_dim_boundary_values() {
        let mut cfg = PolarisConfig::default(); // nomic, native_dim=768
        cfg.embedding_dim = 64;
        cfg.validate().unwrap();
        cfg.embedding_dim = 768;
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_mxbai_accepts_up_to_1024() {
        let mut cfg = PolarisConfig::default();
        cfg.model_id = "mxbai-embed-large-v1".to_string();
        cfg.embedding_dim = 1024;
        cfg.validate().unwrap();
        cfg.embedding_dim = 1025;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("embedding_dim"), "{err}");
    }

    #[test]
    fn validate_unknown_model_id_errors() {
        let mut cfg = PolarisConfig::default();
        cfg.model_id = "bad-model".to_string();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("bad-model"), "{err}");
    }

    #[test]
    fn validate_max_chunk_tokens_zero() {
        let mut cfg = PolarisConfig::default();
        cfg.max_chunk_tokens = 0;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("max_chunk_tokens"), "{err}");
    }

    #[test]
    fn validate_chunk_overlap_too_large() {
        let mut cfg = PolarisConfig::default();
        cfg.max_chunk_tokens = 100;
        cfg.chunk_overlap_chars = 400; // == max_chunk_tokens * 4, not strictly less
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("chunk_overlap_chars"), "{err}");
    }

    #[test]
    fn validate_chunk_overlap_boundary_ok() {
        let mut cfg = PolarisConfig::default();
        cfg.max_chunk_tokens = 100;
        cfg.chunk_overlap_chars = 399; // one below the limit
        cfg.validate().unwrap();
    }

    #[test]
    fn load_from_tempfile() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, r#"embedding_dim = 64"#).unwrap();
        writeln!(file, r#"model_id = "test-model""#).unwrap();
        let cfg = PolarisConfig::load(Some(file.path())).unwrap();
        assert_eq!(cfg.embedding_dim, 64);
        assert_eq!(cfg.model_id, "test-model");
        assert_eq!(cfg.db_path, PathBuf::from("polaris.db")); // default
    }

    // -----------------------------------------------------------------------
    // Explicit vs defaulted, and resolution (spec §3)
    // -----------------------------------------------------------------------

    use crate::db::{Database, IndexMetadata, register_vec_extension};

    fn cfg_at(db_path: PathBuf) -> PolarisConfig {
        PolarisConfig { db_path, ..PolarisConfig::default() }
    }

    #[test]
    fn load_records_only_the_keys_the_file_sets() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, r#"model_id = "embeddinggemma-300m""#).unwrap();
        let cfg = PolarisConfig::load(Some(file.path())).unwrap();
        assert_eq!(cfg.explicit.model_id.as_deref(), Some("embeddinggemma-300m"));
        assert_eq!(cfg.explicit.embedding_dim, None);

        let mut both = tempfile::NamedTempFile::new().unwrap();
        writeln!(both, "embedding_dim = 256").unwrap();
        writeln!(both, r#"model_id = "nomic-embed-text-v1.5""#).unwrap();
        let cfg = PolarisConfig::load(Some(both.path())).unwrap();
        assert_eq!(cfg.explicit.embedding_dim, Some(256));
        assert_eq!(cfg.explicit.model_id.as_deref(), Some("nomic-embed-text-v1.5"));
    }

    #[test]
    fn a_default_config_sets_nothing_explicitly() {
        assert_eq!(PolarisConfig::default().explicit, Explicit::default());
    }

    #[test]
    fn apply_overrides_marks_model_and_dim_explicit() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(None, Some(256), Some("embeddinggemma-300m".into()));
        assert_eq!(cfg.explicit.model_id.as_deref(), Some("embeddinggemma-300m"));
        assert_eq!(cfg.explicit.embedding_dim, Some(256));
        assert_eq!(cfg.model_id, "embeddinggemma-300m");
        assert_eq!(cfg.embedding_dim, 256);
    }

    #[test]
    fn resolve_without_an_index_uses_the_models_default_dim() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");

        let mut cfg = cfg_at(db.clone());
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 512));

        let mut cfg = cfg_at(db.clone());
        cfg.apply_overrides(None, None, Some("embeddinggemma-300m".into()));
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 768));

        assert!(!db.exists(), "resolution must never create the database");
    }

    #[test]
    fn resolve_takes_model_and_dim_from_an_existing_index() {
        register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        { let _d = Database::open(&db, 768, "embeddinggemma-300m").unwrap(); }

        let mut cfg = cfg_at(db);
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 768));
        cfg.validate().unwrap();
    }

    #[test]
    fn resolve_falls_through_a_database_without_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        }
        let mut cfg = cfg_at(db);
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 512));
    }

    #[test]
    fn resolve_falls_through_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        std::fs::write(&db, b"not a database").unwrap();
        let mut cfg = cfg_at(db);
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 512));
    }

    #[test]
    fn explicit_dim_wins_over_the_index_and_still_fails_at_open() {
        register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        { let _d = Database::open(&db, 768, "embeddinggemma-300m").unwrap(); }

        let mut cfg = cfg_at(db.clone());
        cfg.apply_overrides(None, Some(512), None);
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("embeddinggemma-300m", 512));
        assert!(matches!(
            Database::open(&db, cfg.embedding_dim, &cfg.model_id),
            Err(PolarisError::DimensionMismatch { db_dim: 768, config_dim: 512 })
        ));
    }

    #[test]
    fn explicit_model_contradicting_the_index_fails_with_model_mismatch() {
        // An mxbai index stores dim 1024, above nomic's native 768. Taking the
        // stored dim for an explicit nomic would fail `validate` with a
        // dimension error and hide the real problem.
        register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        { let _d = Database::open(&db, 1024, "mxbai-embed-large-v1").unwrap(); }

        let mut cfg = cfg_at(db.clone());
        cfg.apply_overrides(None, None, Some("nomic-embed-text-v1.5".into()));
        resolve_effective(&mut cfg);
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 512));
        cfg.validate().expect("the model's own default dim is valid");
        assert!(matches!(
            Database::open(&db, cfg.embedding_dim, &cfg.model_id),
            Err(PolarisError::ModelMismatch { .. })
        ));
    }

    #[test]
    fn an_index_that_records_no_model_still_supplies_its_dim() {
        let mut cfg = PolarisConfig::default();
        apply_resolution(&mut cfg, IndexMetadata { model_id: None, embedding_dim: Some(256) });
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("nomic-embed-text-v1.5", 256));
    }

    #[test]
    fn resolution_leaves_an_unknown_model_for_validate_to_report() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(None, None, Some("bad-model".into()));
        apply_resolution(&mut cfg, IndexMetadata::default());
        assert_eq!((cfg.model_id.as_str(), cfg.embedding_dim), ("bad-model", 512));
        assert!(cfg.validate().unwrap_err().to_string().contains("bad-model"));
    }

    #[test]
    fn threshold_follows_the_resolved_model() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        for (model, expected) in [
            ("nomic-embed-text-v1.5", Some(0.63)),
            ("nomic-embed-text-v1.5-quantized", Some(0.63)),
            ("embeddinggemma-300m", Some(0.42)),
            ("mxbai-embed-large-v1", None),
            ("all-minilm-l6-v2", None),
        ] {
            let mut cfg = cfg_at(db.clone());
            cfg.apply_overrides(None, None, Some(model.into()));
            resolve_effective(&mut cfg);
            assert_eq!(cfg.search_min_similarity, expected, "{model}");
        }
    }

    #[test]
    fn threshold_follows_the_model_of_an_existing_index() {
        register_vec_extension();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("polaris.db");
        { let _d = Database::open(&db, 768, "embeddinggemma-300m").unwrap(); }
        let mut cfg = cfg_at(db);
        resolve_effective(&mut cfg);
        assert_eq!(cfg.search_min_similarity, Some(0.42));
    }

    #[test]
    fn an_explicit_threshold_is_kept_for_any_model() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, r#"model_id = "all-minilm-l6-v2""#).unwrap();
        writeln!(file, "search_min_similarity = 0.3").unwrap();
        let mut cfg = PolarisConfig::load(Some(file.path())).unwrap();
        assert_eq!(cfg.explicit.search_min_similarity, Some(0.3));
        let dir = tempfile::tempdir().unwrap();
        cfg.db_path = dir.path().join("polaris.db");
        resolve_effective(&mut cfg);
        assert_eq!(cfg.search_min_similarity, Some(0.3));
    }

    #[test]
    fn a_threshold_the_file_omits_is_not_explicit() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, r#"db_path = "x.db""#).unwrap();
        let cfg = PolarisConfig::load(Some(file.path())).unwrap();
        assert_eq!(cfg.explicit.search_min_similarity, None);
    }
}

#[cfg(test)]
mod opts_tests {
    use super::*;

    #[test]
    fn index_opts_defaults_are_sensible() {
        let opts = IndexOpts::default();
        assert!(opts.recursive);
        assert!(!opts.force);
        assert!(!opts.dry_run);
    }

    #[test]
    fn validate_rejects_out_of_range_min_similarity() {
        let mut cfg = PolarisConfig::default();
        cfg.search_min_similarity = Some(1.5);
        assert!(cfg.validate().is_err());
        cfg.search_min_similarity = Some(0.65);
        assert!(cfg.validate().is_ok());
        cfg.search_min_similarity = None;
        assert!(cfg.validate().is_ok(), "uncalibrated is a valid state");
    }

    #[test]
    fn an_unresolved_config_keeps_todays_threshold() {
        // Spec §6: anything that never resolves must behave exactly as before.
        assert_eq!(PolarisConfig::default().search_min_similarity, Some(0.63));
        let parsed: PolarisConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.search_min_similarity, Some(0.63));
    }

    #[test]
    fn search_opts_defaults_to_top_5() {
        assert_eq!(SearchOpts::default().top_k, 5);
    }

    #[test]
    fn eval_config_defaults() {
        let cfg = PolarisConfig::default();
        assert_eq!(cfg.eval.sample_size, 200);
        assert!(cfg.eval.probes.is_empty());
    }

    #[test]
    fn validate_rejects_zero_eval_sample_size() {
        let mut cfg = PolarisConfig::default();
        cfg.eval.sample_size = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn eval_section_parses_from_toml() {
        let toml = r#"
db_path = "polaris.db"

[eval]
sample_size = 50
probes = ["comment cuire un oeuf"]
"#;
        let cfg: PolarisConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.eval.sample_size, 50);
        assert_eq!(cfg.eval.probes.len(), 1);
    }

    #[test]
    fn eval_section_absent_uses_defaults() {
        let cfg: PolarisConfig = toml::from_str("db_path = \"polaris.db\"").unwrap();
        assert_eq!(cfg.eval.sample_size, 200);
    }
}
