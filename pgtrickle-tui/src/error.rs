use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliError {
    #[error("connection failed: {0}")]
    Connection(#[from] tokio_postgres::Error),

    #[error("connection failed: {0}")]
    ConnectionMsg(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("query failed: {0}")]
    Query(String),

    #[error("{0}")]
    Other(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
