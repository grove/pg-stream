/// RabbitMQ AMQP publisher sink (RELAY-13).
/// Feature-gated: only compiled with `--features rabbitmq`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "rabbitmq")]
use lapin::{
    BasicProperties, Channel, Connection, ConnectionProperties, options::BasicPublishOptions,
};

#[cfg(feature = "rabbitmq")]
pub struct RabbitMqSink {
    channel: Channel,
    exchange: String,
    routing_key_template: String,
}

#[cfg(feature = "rabbitmq")]
impl RabbitMqSink {
    pub async fn new(
        url: &str,
        exchange: impl Into<String>,
        routing_key_template: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let conn = Connection::connect(url, ConnectionProperties::default())
            .await
            .map_err(|e| RelayError::sink("rabbitmq", e))?;
        let channel = conn
            .create_channel()
            .await
            .map_err(|e| RelayError::sink("rabbitmq", e))?;

        Ok(Self {
            channel,
            exchange: exchange.into(),
            routing_key_template: routing_key_template.into(),
        })
    }
}

#[cfg(feature = "rabbitmq")]
#[async_trait::async_trait]
impl super::Sink for RabbitMqSink {
    fn name(&self) -> &str {
        "rabbitmq"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        for msg in messages {
            let routing_key = crate::envelope::render_subject(
                &self.routing_key_template,
                &msg.subject,
                &msg.op,
                msg.outbox_id.unwrap_or(0),
                msg.refresh_id,
            );
            let payload = serde_json::to_vec(msg).map_err(RelayError::Json)?;

            let props = BasicProperties::default()
                .with_message_id(msg.dedup_key.as_str().into())
                .with_content_type("application/json".into());

            self.channel
                .basic_publish(
                    &self.exchange,
                    &routing_key,
                    BasicPublishOptions::default(),
                    &payload,
                    props,
                )
                .await
                .map_err(|e| RelayError::sink("rabbitmq", e))?
                .await
                .map_err(|e| RelayError::sink("rabbitmq", e))?;
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        self.channel.status().connected()
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        let _ = self.channel.close(0, "relay shutdown").await;
        Ok(())
    }
}
