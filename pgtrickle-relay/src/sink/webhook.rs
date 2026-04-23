/// HTTP webhook sink (RELAY-7).
/// POSTs RelayMessages to a webhook URL with idempotency key header.
/// Feature-gated: only compiled with `--features webhook`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "webhook")]
use reqwest::{Client, Url};

#[cfg(feature = "webhook")]
pub struct WebhookSink {
    client: Client,
    url: Url,
    timeout_secs: u64,
}

#[cfg(feature = "webhook")]
impl WebhookSink {
    pub fn new(url: &str, timeout_secs: u64) -> Result<Self, RelayError> {
        let url = Url::parse(url).map_err(|e| RelayError::config(e.to_string()))?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| RelayError::sink("webhook", e))?;
        Ok(Self {
            client,
            url,
            timeout_secs,
        })
    }
}

#[cfg(feature = "webhook")]
#[async_trait::async_trait]
impl super::Sink for WebhookSink {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        if messages.is_empty() {
            return Ok(());
        }

        // POST the full batch as a JSON array with idempotency key from the last message.
        let idempotency_key = messages
            .last()
            .map(|m| m.dedup_key.clone())
            .unwrap_or_default();

        let payload = serde_json::to_value(messages).map_err(RelayError::Json)?;

        let resp = self
            .client
            .post(self.url.clone())
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", &idempotency_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| RelayError::sink("webhook", e))?;

        if !resp.status().is_success() {
            return Err(RelayError::SinkPublish {
                sink: "webhook".to_string(),
                source: format!("HTTP {}", resp.status()).into(),
            });
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        true // checked via metrics endpoint
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
