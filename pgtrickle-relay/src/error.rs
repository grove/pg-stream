/// RelayError — all errors that can occur in the relay binary.
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    // Database errors
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),

    #[error("postgres connection failed: {url}: {err}")]
    ConnectionFailed {
        url: String,
        err: tokio_postgres::Error,
    },

    // Configuration errors
    #[error("config error: {0}")]
    Config(String),

    #[error("invalid config for pipeline '{name}': {reason}")]
    InvalidConfig { name: String, reason: String },

    #[error("pipeline '{0}' not found")]
    PipelineNotFound(String),

    #[error("missing required config key '{key}' in pipeline '{pipeline}'")]
    MissingConfigKey { pipeline: String, key: String },

    // Payload errors
    #[error("unsupported outbox payload version: {0}")]
    UnsupportedPayloadVersion(i64),

    #[error("payload decode error in outbox '{outbox}' id={outbox_id}: {reason}")]
    PayloadDecode {
        outbox: String,
        outbox_id: i64,
        reason: String,
    },

    // Sink errors
    #[error("sink '{sink}' publish error: {source}")]
    SinkPublish {
        sink: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("sink '{sink}' unhealthy: {reason}")]
    SinkUnhealthy { sink: String, reason: String },

    // Source errors
    #[error("source '{src}' poll error: {inner}")]
    SourcePoll {
        src: String,
        inner: Box<dyn std::error::Error + Send + Sync>,
    },

    // IO errors
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    // JSON errors
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    // TOML errors
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),

    // Channel errors
    #[error("channel closed")]
    ChannelClosed,

    // Generic
    #[error("{0}")]
    Other(String),
}

impl RelayError {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub fn sink<E: std::error::Error + Send + Sync + 'static>(
        sink: impl Into<String>,
        source: E,
    ) -> Self {
        Self::SinkPublish {
            sink: sink.into(),
            source: Box::new(source),
        }
    }

    pub fn source_poll<E: std::error::Error + Send + Sync + 'static>(
        source: impl Into<String>,
        inner: E,
    ) -> Self {
        Self::SourcePoll {
            src: source.into(),
            inner: Box::new(inner),
        }
    }
}
