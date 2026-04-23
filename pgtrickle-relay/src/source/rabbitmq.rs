/// RabbitMQ AMQP consumer source (RELAY-28).
/// Feature-gated: only compiled with `--features rabbitmq`.
use crate::envelope::{AckToken, RelayMessage};
use crate::error::RelayError;

#[cfg(feature = "rabbitmq")]
use lapin::{
    Channel, Connection, ConnectionProperties, consumer::Consumer as LapinConsumer, options::*,
    types::FieldTable,
};

#[cfg(feature = "rabbitmq")]
pub struct RabbitMqSource {
    consumer: LapinConsumer,
    channel: Channel,
    event_type: String,
}

#[cfg(feature = "rabbitmq")]
impl RabbitMqSource {
    pub async fn new(
        url: &str,
        queue: impl Into<String>,
        consumer_tag: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let queue = queue.into();
        let consumer_tag = consumer_tag.into();

        let conn = Connection::connect(url, ConnectionProperties::default())
            .await
            .map_err(|e| RelayError::source_poll("rabbitmq", e))?;

        let channel = conn
            .create_channel()
            .await
            .map_err(|e| RelayError::source_poll("rabbitmq", e))?;

        // Declare the queue (idempotent).
        channel
            .queue_declare(
                &queue,
                QueueDeclareOptions::default(),
                FieldTable::default(),
            )
            .await
            .map_err(|e| RelayError::source_poll("rabbitmq", e))?;

        // Basic consume with manual ack.
        let consumer = channel
            .basic_consume(
                &queue,
                &consumer_tag,
                BasicConsumeOptions {
                    no_ack: false,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await
            .map_err(|e| RelayError::source_poll("rabbitmq", e))?;

        Ok(Self {
            consumer,
            channel,
            event_type: event_type.into(),
        })
    }
}

#[cfg(feature = "rabbitmq")]
#[async_trait::async_trait]
impl super::Source for RabbitMqSource {
    fn name(&self) -> &str {
        "rabbitmq"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        use futures_util::StreamExt;

        let mut messages = Vec::new();
        for _ in 0..batch_size {
            match tokio::time::timeout(std::time::Duration::from_millis(100), self.consumer.next())
                .await
            {
                Ok(Some(Ok(delivery))) => {
                    let payload: serde_json::Value =
                        serde_json::from_slice(&delivery.data).unwrap_or(serde_json::Value::Null);

                    let dedup_key = delivery
                        .properties
                        .message_id()
                        .as_ref()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

                    let event_type = payload
                        .get("event_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&self.event_type)
                        .to_string();

                    let mut msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
                    msg.ack_token = AckToken::RabbitMqDeliveryTag(delivery.delivery_tag);
                    messages.push(msg);

                    // Ack inline after collecting (before writing to inbox).
                    // NOTE: in production, ack should happen AFTER inbox write.
                    // This is the single-message path; batch ack is handled in acknowledge().
                }
                _ => break,
            }
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError> {
        if let AckToken::RabbitMqDeliveryTag(tag) = last_message.ack_token {
            self.channel
                .basic_ack(tag, BasicAckOptions::default())
                .await
                .map_err(|e| RelayError::source_poll("rabbitmq", e))?;
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        let _ = self.channel.close(0, "relay shutdown").await;
        Ok(())
    }
}
