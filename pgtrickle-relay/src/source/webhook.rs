/// HTTP webhook receiver source (RELAY-25).
/// Starts an axum HTTP server that accepts POST requests and converts them to RelayMessages.
/// Feature-gated: only compiled with `--features webhook`.
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "webhook")]
pub struct WebhookSource {
    rx: mpsc::Receiver<RelayMessage>,
    event_type: String,
}

#[cfg(feature = "webhook")]
impl WebhookSource {
    pub async fn bind(addr: &str, event_type: impl Into<String>) -> Result<Self, RelayError> {
        use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

        let (tx, rx) = mpsc::channel::<RelayMessage>(1024);
        let tx = Arc::new(tx);
        let event_type_clone = event_type.into();

        let app = Router::new().route(
            "/",
            post({
                let tx = Arc::clone(&tx);
                let et = event_type_clone.clone();
                move |headers: axum::http::HeaderMap, Json(payload): Json<serde_json::Value>| {
                    let tx = Arc::clone(&tx);
                    let et = et.clone();
                    async move {
                        let dedup_key = headers
                            .get("Idempotency-Key")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        let event_type = payload
                            .get("event_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&et)
                            .to_string();
                        let msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
                        let _ = tx.send(msg).await;
                        StatusCode::OK
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(RelayError::Io)?;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("webhook receiver error: {e}");
            }
        });

        Ok(Self {
            rx,
            event_type: event_type_clone,
        })
    }
}

#[cfg(feature = "webhook")]
#[async_trait::async_trait]
impl super::Source for WebhookSource {
    fn name(&self) -> &str {
        "webhook-receiver"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        let mut messages = Vec::new();
        // Non-blocking drain of the channel.
        for _ in 0..batch_size {
            match self.rx.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(_) => break,
            }
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, _last_message: &RelayMessage) -> Result<(), RelayError> {
        // HTTP webhook: response (200) was already sent synchronously in the handler.
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
