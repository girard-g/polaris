use thiserror::Error;

#[derive(Error, Debug)]
pub enum PolarisError {
    #[error("Embedding error: {0}")]
    Embedding(#[from] anyhow::Error),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Indexing error: {0}")]
    Indexing(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error("Dimension mismatch: database has dim={db_dim}, config has dim={config_dim}")]
    DimensionMismatch { db_dim: usize, config_dim: usize },

    #[error("Model mismatch: database was indexed with model '{db_model}', config has '{config_model}' — delete the database and re-index to switch models")]
    ModelMismatch { db_model: String, config_model: String },
}

pub type Result<T> = std::result::Result<T, PolarisError>;
