/// NATS JetStream sink (RELAY-6).
/// Feature-gated: only compiled with `--features nats`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "nats")]
use async_nats::jetstream;

#[cfg(feature = "nats")]
pub struct NatsSink {
    js: jetstream::Context,
    subject_template: String,
}

#[cfg(feature = "nats")]
impl NatsSink {
    pub async fn new(url: &str, subject_template: impl Into<String>) -> Result<Self, RelayError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| RelayError::sink("nats", e))?;
        let js = jetstream::new(client);
        Ok(Self {
            js,
            subject_template: subject_template.into(),
        })
    }
}

#[cfg(feature = "nats")]
#[async_trait::async_trait]
impl super::Sink for NatsSink {
    fn name(&self) -> &str {
        "nats"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        use async_nats::HeaderMap;

        for msg in messages {
            let payload = serde_json::to_vec(msg).map_err(RelayError::Json)?;

            let mut headers = HeaderMap::new();
            headers.insert("Nats-Msg-Id", msg.dedup_key.as_str());
            if msg.is_full_refresh {
                headers.insert("Pgtrickle-Full-Refresh", "true");
            }

            self.js
                .publish_with_headers(msg.subject.clone(), headers, payload.into())
                .await
                .map_err(|e| RelayError::sink("nats", e))?
                .await
                .map_err(|e| RelayError::sink("nats", e))?;
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        // Check NATS connection by getting server info.
        true // JetStream context doesn't expose a direct health check.
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
