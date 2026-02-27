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

    /// Maximum file size in bytes that the indexer will process (larger files are skipped)
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
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

fn default_rrf_k() -> usize {
    60
}

fn default_max_top_k() -> usize {
    50
}

fn default_max_file_size() -> u64 {
    10 * 1024 * 1024 // 10 MB
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
            max_file_size: default_max_file_size(),
        }
    }
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
        if self.embedding_dim < 64 || self.embedding_dim > 768 {
            return Err(PolarisError::Config(format!(
                "embedding_dim must be in [64, 768], got {}",
                self.embedding_dim
            )));
        }
        if self.max_chunk_tokens == 0 {
            return Err(PolarisError::Config(
                "max_chunk_tokens must be greater than 0".to_string(),
            ));
        }
        if self.chunk_overlap_chars >= self.max_chunk_tokens * 4 {
            return Err(PolarisError::Config(format!(
                "chunk_overlap_chars ({}) must be less than max_chunk_tokens * 4 ({})",
                self.chunk_overlap_chars,
                self.max_chunk_tokens * 4,
            )));
        }
        Ok(())
    }

    /// Apply CLI overrides (None means "not specified", keep existing value).
    pub fn apply_overrides(
        &mut self,
        db_path: Option<PathBuf>,
        embedding_dim: Option<usize>,
    ) {
        if let Some(p) = db_path {
            self.db_path = p;
        }
        if let Some(d) = embedding_dim {
            self.embedding_dim = d;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

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
        cfg.apply_overrides(Some(PathBuf::from("override.db")), Some(384));
        assert_eq!(cfg.db_path, PathBuf::from("override.db"));
        assert_eq!(cfg.embedding_dim, 384);
    }

    #[test]
    fn apply_overrides_with_none_unchanged() {
        let mut cfg = PolarisConfig::default();
        cfg.apply_overrides(None, None);
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
        let mut cfg = PolarisConfig::default();
        cfg.embedding_dim = 769;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("embedding_dim"), "{err}");
    }

    #[test]
    fn validate_embedding_dim_boundary_values() {
        let mut cfg = PolarisConfig::default();
        cfg.embedding_dim = 64;
        cfg.validate().unwrap();
        cfg.embedding_dim = 768;
        cfg.validate().unwrap();
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
