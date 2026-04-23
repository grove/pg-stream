/// Apache Kafka consumer source (RELAY-24).
/// Feature-gated: only compiled with `--features kafka`.
use crate::envelope::{AckToken, RelayMessage};
use crate::error::RelayError;

#[cfg(feature = "kafka")]
use rdkafka::{
    ClientConfig, Message,
    consumer::{Consumer, StreamConsumer},
};

#[cfg(feature = "kafka")]
pub struct KafkaSource {
    consumer: StreamConsumer,
    topic: String,
    event_type: String,
}

#[cfg(feature = "kafka")]
impl KafkaSource {
    pub fn new(
        brokers: &str,
        group_id: &str,
        topic: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("group.id", group_id)
            .set("auto.offset.reset", "earliest")
            .set("enable.auto.commit", "false")
            .create()
            .map_err(|e| RelayError::source_poll("kafka", e))?;

        let topic = topic.into();
        consumer
            .subscribe(&[&topic])
            .map_err(|e| RelayError::source_poll("kafka", e))?;

        Ok(Self {
            consumer,
            topic,
            event_type: event_type.into(),
        })
    }
}

#[cfg(feature = "kafka")]
#[async_trait::async_trait]
impl super::Source for KafkaSource {
    fn name(&self) -> &str {
        "kafka"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        use futures_util::StreamExt;
        let mut messages = Vec::new();
        let stream = self.consumer.stream();
        // Poll with a short timeout to collect up to batch_size messages.
        let timeout = std::time::Duration::from_millis(100);
        let mut stream = tokio::time::timeout(timeout, async {
            let mut msgs = Vec::new();
            let mut pinned = std::pin::pin!(stream);
            while msgs.len() < batch_size as usize {
                match tokio::time::timeout(std::time::Duration::from_millis(50), pinned.next())
                    .await
                {
                    Ok(Some(Ok(msg))) => msgs.push(msg),
                    _ => break,
                }
            }
            msgs
        })
        .await
        .unwrap_or_default();

        for msg in stream {
            let payload_bytes = msg.payload().unwrap_or_default();
            let payload: serde_json::Value =
                serde_json::from_slice(payload_bytes).unwrap_or(serde_json::Value::Null);

            let dedup_key = msg
                .key()
                .map(|k| String::from_utf8_lossy(k).to_string())
                .unwrap_or_else(|| format!("{}:{}", msg.partition(), msg.offset()));

            let event_type = payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.event_type)
                .to_string();

            let mut relay_msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
            relay_msg.ack_token = AckToken::KafkaOffset {
                partition: msg.partition(),
                offset: msg.offset(),
            };
            messages.push(relay_msg);
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError> {
        if let AckToken::KafkaOffset { partition, offset } = last_message.ack_token {
            use rdkafka::TopicPartitionList;
            let mut tpl = TopicPartitionList::new();
            tpl.add_partition_offset(&self.topic, partition, rdkafka::Offset::Offset(offset + 1))
                .map_err(|e| RelayError::source_poll("kafka", e))?;
            self.consumer
                .commit(&tpl, rdkafka::consumer::CommitMode::Async)
                .map_err(|e| RelayError::source_poll("kafka", e))?;
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
