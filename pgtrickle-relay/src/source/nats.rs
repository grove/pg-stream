/// NATS JetStream source (RELAY-23) — durable pull consumer.
/// Feature-gated: only compiled with `--features nats`.
#[cfg(feature = "nats")]
use async_nats::jetstream::{self, consumer::pull::BatchConfig};

use crate::envelope::{AckToken, RelayMessage};
use crate::error::RelayError;

#[cfg(feature = "nats")]
pub struct NatsSource {
    messages:
        async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
    stream_name: String,
    consumer_name: String,
    subject: String,
    event_type: String,
    pending: Vec<async_nats::jetstream::Message>,
}

#[cfg(feature = "nats")]
impl NatsSource {
    pub async fn new(
        url: &str,
        stream_name: impl Into<String>,
        consumer_name: impl Into<String>,
        subject: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let stream_name = stream_name.into();
        let consumer_name = consumer_name.into();
        let subject = subject.into();
        let event_type = event_type.into();
        let client = async_nats::connect(url)
            .await
            .map_err(|e| RelayError::source_poll("nats", e))?;
        let js = jetstream::new(client);

        // Get or create the stream.
        let stream = js
            .get_or_create_stream(jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec![subject.clone()],
                ..Default::default()
            })
            .await
            .map_err(|e| RelayError::source_poll("nats", e))?;

        // Get or create a durable pull consumer.
        let consumer = stream
            .get_or_create_consumer::<jetstream::consumer::pull::Config>(
                &consumer_name,
                jetstream::consumer::pull::Config {
                    durable_name: Some(consumer_name.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| RelayError::source_poll("nats", e))?;

        Ok(Self {
            messages: consumer,
            stream_name,
            consumer_name,
            subject,
            event_type,
            pending: Vec::new(),
        })
    }
}

#[cfg(feature = "nats")]
#[async_trait::async_trait]
impl super::Source for NatsSource {
    fn name(&self) -> &str {
        "nats"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        let batch = self
            .messages
            .fetch()
            .max_messages(batch_size as usize)
            .messages()
            .await
            .map_err(|e| RelayError::source_poll("nats", e))?;

        let mut messages = Vec::new();
        let mut raw_msgs: Vec<async_nats::jetstream::Message> = Vec::new();

        use futures_util::StreamExt;
        let mut stream = batch;
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(msg) => {
                    let dedup_key = msg
                        .headers
                        .as_ref()
                        .and_then(|h| h.get("Nats-Msg-Id"))
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

                    let payload: serde_json::Value =
                        serde_json::from_slice(&msg.payload).unwrap_or(serde_json::Value::Null);

                    let event_type = payload
                        .get("event_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&self.event_type)
                        .to_string();

                    let mut relay_msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
                    relay_msg.ack_token = AckToken::None; // ack handled per-message
                    raw_msgs.push(msg);
                    messages.push(relay_msg);
                }
                Err(e) => {
                    tracing::warn!("nats message error: {e}");
                }
            }
        }

        // Ack all messages inline (NATS ack is cheap).
        for raw_msg in raw_msgs {
            let _ = raw_msg.ack().await;
        }

        Ok(messages)
    }

    async fn acknowledge(&mut self, _last_message: &RelayMessage) -> Result<(), RelayError> {
        // Messages are acked inline in poll().
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
