use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PolarisError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolarisConfig {
    /// Path to the SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,

    /// Embedding dimension (64–768, Matryoshka truncation)
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,

    /// Maximum chunk size in approximate tokens (chars / 4)
    #[serde(default = "default_max_chunk_tokens")]
    pub max_chunk_tokens: usize,

    /// Overlap in characters between adjacent non-heading chunks
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// fastembed model ID
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

    /// Minimum query-to-chunk cosine similarity for the auto-search hook to
    /// inject a result. Model-dependent: the default suits the default
    /// `model_id`, and a different embedding model needs it re-tuned.
    #[serde(default = "default_search_min_similarity")]
    pub search_min_similarity: f32,

    /// Maximum file size in bytes that the indexer will process (larger files are skipped)
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,

    /// Additional database paths for multi-DB search (read-only)
    #[serde(default)]
    pub extra_db_paths: Vec<PathBuf>,

    /// `polaris eval` settings
    #[serde(default)]
    pub eval: EvalConfig,
}

fn default_db_path() -> PathBuf {
    PathBuf::from("polaris.db")
}

fn default_embedding_dim() -> usize {
    512
}

fn default_max_chunk_tokens() -> usize {
    450
}

fn default_chunk_overlap_chars() -> usize {
    200
}

fn default_model_id() -> String {
    "nomic-embed-text-v1.5".to_string()
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
/// Re-tune per `model_id`, and re-run `polaris eval` after doing so: the
/// off-topic floor comes from the model's `search_query: ` prefix, which gives
/// any two texts a shared baseline, and it moves with the model.
fn default_search_min_similarity() -> f32 {
    0.63
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
                toml::from_str(&raw).map_err(|e| {
                    PolarisError::Config(format!("Invalid TOML in {}: {e}", p.display()))
                })
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
        if !(0.0..=1.0).contains(&self.search_min_similarity) {
            return Err(PolarisError::Config(format!(
                "search_min_similarity must be in [0.0, 1.0], got {}",
                self.search_min_similarity
            )));
        }
        if self.eval.sample_size == 0 {
            return Err(PolarisError::Config(
                "eval.sample_size must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }

    /// Apply CLI overrides (None means "not specified", keep existing value).
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
        }
        if let Some(m) = model_id {
            self.model_id = m;
        }
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
        cfg.search_min_similarity = 1.5;
        assert!(cfg.validate().is_err());
        cfg.search_min_similarity = 0.65;
        assert!(cfg.validate().is_ok());
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
