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
use std::time::Duration;
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

    // A30: Expand ${ENV:VAR_NAME} placeholders in connection strings.
    cfg = cfg.resolve_env_vars();

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

    // A38: Connect to PostgreSQL with exponential backoff.
    let (db_client, db_conn) = connect_with_backoff(&cfg.postgres_url).await?;
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

/// A38: Connect to PostgreSQL with exponential backoff.
///
/// Retries with initial delay 100 ms, doubling each attempt up to 30 s,
/// with ±20 % jitter to avoid thundering-herd reconnects.
async fn connect_with_backoff(
    url: &str,
) -> Result<
    (
        tokio_postgres::Client,
        tokio_postgres::Connection<tokio_postgres::Socket, tokio_postgres::tls::NoTlsStream>,
    ),
    Box<dyn std::error::Error>,
> {
    const INITIAL_DELAY_MS: u64 = 100;
    const MAX_DELAY_MS: u64 = 30_000;
    const JITTER_PCT: f64 = 0.20;

    let mut delay_ms = INITIAL_DELAY_MS;
    let mut attempt = 0u32;

    loop {
        match tokio_postgres::connect(url, tokio_postgres::NoTls).await {
            Ok(pair) => return Ok(pair),
            Err(e) => {
                attempt += 1;
                // Apply ±20% jitter: seed from attempt number for determinism in tests.
                let jitter_range = (delay_ms as f64 * JITTER_PCT) as u64;
                let jitter = if jitter_range > 0 {
                    // Simple deterministic jitter: (attempt * 6364136223846793005) % range
                    let pseudo = attempt as u64 * 6_364_136_223_846_793_005_u64;
                    (pseudo % (jitter_range * 2)).saturating_sub(jitter_range)
                } else {
                    0
                };
                let sleep_ms = delay_ms.saturating_add(jitter);
                tracing::warn!(
                    attempt,
                    sleep_ms,
                    error = %e,
                    "PostgreSQL connection failed, retrying"
                );
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
            }
        }
    }
}
