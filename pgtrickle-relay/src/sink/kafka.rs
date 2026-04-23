/// Apache Kafka producer sink (RELAY-8).
/// Feature-gated: only compiled with `--features kafka`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "kafka")]
use rdkafka::{
    ClientConfig,
    producer::{FutureProducer, FutureRecord},
};

#[cfg(feature = "kafka")]
pub struct KafkaSink {
    producer: FutureProducer,
    topic_template: String,
}

#[cfg(feature = "kafka")]
impl KafkaSink {
    pub fn new(brokers: &str, topic_template: impl Into<String>) -> Result<Self, RelayError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("message.timeout.ms", "5000")
            .set("enable.idempotence", "true")
            .create()
            .map_err(|e| RelayError::sink("kafka", e))?;

        Ok(Self {
            producer,
            topic_template: topic_template.into(),
        })
    }
}

#[cfg(feature = "kafka")]
#[async_trait::async_trait]
impl super::Sink for KafkaSink {
    fn name(&self) -> &str {
        "kafka"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        use std::time::Duration;

        for msg in messages {
            let payload = serde_json::to_string(msg).map_err(RelayError::Json)?;
            let key = msg.dedup_key.as_str();

            self.producer
                .send(
                    FutureRecord::to(&self.topic_template)
                        .key(key)
                        .payload(&payload),
                    Duration::from_secs(5),
                )
                .await
                .map_err(|(e, _)| RelayError::sink("kafka", e))?;
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        true
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        self.producer
            .flush(std::time::Duration::from_secs(5))
            .map_err(|e| RelayError::sink("kafka", e))?;
        Ok(())
    }
}
