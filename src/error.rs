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
}

pub type Result<T> = std::result::Result<T, PolarisError>;
