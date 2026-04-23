/// Prometheus metrics + health endpoint (RELAY-9).
use prometheus::{
    IntCounterVec, IntGaugeVec, Registry, TextEncoder, register_int_counter_vec,
    register_int_gauge_vec,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared relay metrics.
pub struct RelayMetrics {
    pub messages_published: IntCounterVec,
    pub messages_consumed: IntCounterVec,
    pub publish_errors: IntCounterVec,
    pub dedup_skipped: IntCounterVec,
    pub pipeline_healthy: IntGaugeVec,
    registry: Registry,
}

impl RelayMetrics {
    pub fn new() -> Result<Arc<Self>, prometheus::Error> {
        let registry = Registry::new();

        let messages_published = register_int_counter_vec!(
            prometheus::opts!(
                "pgtrickle_relay_messages_published_total",
                "Total messages published to sink"
            ),
            &["pipeline", "direction"]
        )?;

        let messages_consumed = register_int_counter_vec!(
            prometheus::opts!(
                "pgtrickle_relay_messages_consumed_total",
                "Total messages consumed from source"
            ),
            &["pipeline", "direction"]
        )?;

        let publish_errors = register_int_counter_vec!(
            prometheus::opts!(
                "pgtrickle_relay_publish_errors_total",
                "Total publish errors"
            ),
            &["pipeline", "direction"]
        )?;

        let dedup_skipped = register_int_counter_vec!(
            prometheus::opts!(
                "pgtrickle_relay_dedup_skipped_total",
                "Total messages skipped due to deduplication"
            ),
            &["pipeline"]
        )?;

        let pipeline_healthy = register_int_gauge_vec!(
            prometheus::opts!(
                "pgtrickle_relay_pipeline_healthy",
                "1 if pipeline is healthy, 0 otherwise"
            ),
            &["pipeline"]
        )?;

        registry.register(Box::new(messages_published.clone()))?;
        registry.register(Box::new(messages_consumed.clone()))?;
        registry.register(Box::new(publish_errors.clone()))?;
        registry.register(Box::new(dedup_skipped.clone()))?;
        registry.register(Box::new(pipeline_healthy.clone()))?;

        Ok(Arc::new(Self {
            messages_published,
            messages_consumed,
            publish_errors,
            dedup_skipped,
            pipeline_healthy,
            registry,
        }))
    }

    pub fn render(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = String::new();
        encoder.encode_utf8(&metric_families, &mut buffer)?;
        Ok(buffer)
    }
}

/// Health state for the relay process.
#[derive(Debug, Clone, Default)]
pub struct HealthState {
    pub healthy_pipelines: Vec<String>,
    pub unhealthy_pipelines: Vec<String>,
}

impl HealthState {
    pub fn is_healthy(&self) -> bool {
        self.unhealthy_pipelines.is_empty()
    }
}

/// Start the metrics + health HTTP server.
pub async fn start_metrics_server(
    addr: &str,
    metrics: Arc<RelayMetrics>,
    health: Arc<RwLock<HealthState>>,
) -> Result<(), crate::error::RelayError> {
    use axum::{Router, extract::State, http::StatusCode, routing::get};

    #[derive(Clone)]
    struct AppState {
        metrics: Arc<RelayMetrics>,
        health: Arc<RwLock<HealthState>>,
    }

    let state = AppState {
        metrics: Arc::clone(&metrics),
        health: Arc::clone(&health),
    };

    let app = Router::new()
        .route(
            "/metrics",
            get(|State(s): State<AppState>| async move {
                match s.metrics.render() {
                    Ok(body) => (StatusCode::OK, body),
                    Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                }
            }),
        )
        .route(
            "/health",
            get(|State(s): State<AppState>| async move {
                let h = s.health.read().await;
                if h.is_healthy() {
                    (StatusCode::OK, "healthy".to_string())
                } else {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("unhealthy: {:?}", h.unhealthy_pipelines),
                    )
                }
            }),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(crate::error::RelayError::Io)?;

    tracing::info!("metrics server listening on {addr}");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("metrics server error: {e}");
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_metrics_creation() {
        let metrics = RelayMetrics::new().unwrap();
        metrics
            .messages_published
            .with_label_values(&["test-pipeline", "forward"])
            .inc();
        let rendered = metrics.render().unwrap();
        assert!(rendered.contains("pgtrickle_relay_messages_published_total"));
    }

    #[test]
    fn test_health_state_default_healthy() {
        let h = HealthState::default();
        assert!(h.is_healthy());
    }

    #[test]
    fn test_health_state_unhealthy() {
        let h = HealthState {
            healthy_pipelines: vec![],
            unhealthy_pipelines: vec!["broken-pipeline".to_string()],
        };
        assert!(!h.is_healthy());
    }
}
