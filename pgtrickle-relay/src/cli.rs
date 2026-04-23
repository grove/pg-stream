/// CLI argument definitions for pgtrickle-relay.
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "pgtrickle-relay",
    version,
    about = "Bidirectional relay for pg_trickle stream tables",
    long_about = "pgtrickle-relay bridges pg_trickle outboxes and inboxes with external \
                  messaging systems (NATS, Kafka, HTTP webhooks, Redis, SQS, RabbitMQ, \
                  stdout/file). All pipeline configuration lives in PostgreSQL — the only \
                  required startup parameter is --postgres-url.\n\n\
                  Forward mode: polls outbox tables → publishes to external sinks.\n\
                  Reverse mode: consumes from external sources → writes to inbox tables."
)]
pub struct Cli {
    /// PostgreSQL connection string (required).
    /// Example: postgres://user:pass@localhost:5432/mydb
    #[arg(
        long,
        env = "PGTRICKLE_RELAY_POSTGRES_URL",
        help = "PostgreSQL connection URL"
    )]
    pub postgres_url: Option<String>,

    /// Prometheus metrics + health endpoint address.
    #[arg(
        long,
        default_value = "0.0.0.0:9090",
        env = "PGTRICKLE_RELAY_METRICS_ADDR",
        help = "Prometheus metrics + health endpoint (default: 0.0.0.0:9090)"
    )]
    pub metrics_addr: String,

    /// Log format.
    #[arg(
        long,
        default_value = "text",
        env = "PGTRICKLE_RELAY_LOG_FORMAT",
        help = "Log format: text or json (default: text)"
    )]
    pub log_format: String,

    /// Log level.
    #[arg(
        long,
        default_value = "info",
        env = "PGTRICKLE_RELAY_LOG_LEVEL",
        help = "Log level: error, warn, info, debug, trace (default: info)"
    )]
    pub log_level: String,

    /// Relay group ID for advisory locks and offset namespacing.
    /// Use a unique value per relay deployment group.
    #[arg(
        long,
        default_value = "default",
        env = "PGTRICKLE_RELAY_GROUP_ID",
        help = "Relay group ID for advisory locks (default: default)"
    )]
    pub relay_group_id: String,

    /// Optional TOML config file path.
    #[arg(
        long,
        env = "PGTRICKLE_RELAY_CONFIG",
        help = "Path to TOML config file (optional)"
    )]
    pub config: Option<String>,
}
