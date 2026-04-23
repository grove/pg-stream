// pgtrickle-relay — entry point (RELAY-1).
// The relay backends and traits are public API used by external consumers.
// Dead code warnings are suppressed because many types are feature-gated or
// used only at runtime via trait objects rather than direct construction.
#![allow(dead_code, unused_imports)]
mod cli;
mod config;
mod coordinator;
mod envelope;
mod error;
mod metrics;
mod sink;
mod source;
mod transforms;

use clap::Parser;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use cli::Cli;
use config::{LogFormat, RelayConfig};
use error::RelayError;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Load config from file if provided, then overlay CLI args.
    let mut cfg = if let Some(ref config_path) = cli.config {
        let content = tokio::fs::read_to_string(config_path).await?;
        toml::from_str::<RelayConfig>(&content)?
    } else {
        RelayConfig::default()
    };

    // CLI args take precedence over file config.
    if let Some(url) = cli.postgres_url {
        cfg.postgres_url = url;
    }
    cfg.metrics_addr = cli.metrics_addr;
    cfg.log_level = cli.log_level;
    cfg.relay_group_id = cli.relay_group_id;
    cfg.log_format = match cli.log_format.as_str() {
        "json" => LogFormat::Json,
        _ => LogFormat::Text,
    };

    // Initialise tracing.
    init_tracing(&cfg);

    if cfg.postgres_url.is_empty() {
        eprintln!("error: --postgres-url is required (or set PGTRICKLE_RELAY_POSTGRES_URL)");
        std::process::exit(1);
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        relay_group_id = %cfg.relay_group_id,
        "pgtrickle-relay starting"
    );

    // Connect to PostgreSQL.
    let (db_client, db_conn) =
        tokio_postgres::connect(&cfg.postgres_url, tokio_postgres::NoTls).await?;
    let db = Arc::new(db_client);

    // Spawn the connection driver.
    tokio::spawn(async move {
        if let Err(e) = db_conn.await {
            tracing::error!("database connection error: {e}");
        }
    });

    // Start metrics + health server.
    let relay_metrics = metrics::RelayMetrics::new()?;
    let health_state = Arc::new(RwLock::new(metrics::HealthState::default()));

    metrics::start_metrics_server(
        &cfg.metrics_addr,
        Arc::clone(&relay_metrics),
        Arc::clone(&health_state),
    )
    .await?;

    // Build coordinator.
    let coordinator = coordinator::Coordinator::new(
        Arc::clone(&db),
        &cfg.relay_group_id,
        Arc::clone(&relay_metrics),
        Arc::clone(&health_state),
    );

    // Load initial pipelines.
    let pipelines = coordinator.load_pipelines().await?;
    tracing::info!(count = pipelines.len(), "loaded relay pipelines");
    for p in &pipelines {
        tracing::info!(name = %p.name, direction = ?p.direction, "pipeline");
    }

    // Start LISTEN for config changes.
    db.execute("LISTEN pgtrickle_relay_config", &[]).await?;

    // Wait for shutdown signal.
    wait_for_shutdown().await;

    tracing::info!("pgtrickle-relay shutting down");
    coordinator.release_all_locks().await?;

    Ok(())
}

fn init_tracing(cfg: &RelayConfig) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    match cfg.log_format {
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .init();
        }
        LogFormat::Text => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("received Ctrl+C") }
        _ = terminate => { tracing::info!("received SIGTERM") }
    }
}
