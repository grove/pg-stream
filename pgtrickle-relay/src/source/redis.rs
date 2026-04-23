/// Redis Streams consumer source (RELAY-26).
/// Feature-gated: only compiled with `--features redis`.
use crate::envelope::{AckToken, RelayMessage};
use crate::error::RelayError;

#[cfg(feature = "redis")]
use redis::aio::MultiplexedConnection;

#[cfg(feature = "redis")]
pub struct RedisSource {
    conn: MultiplexedConnection,
    stream_key: String,
    group: String,
    consumer: String,
    event_type: String,
}

#[cfg(feature = "redis")]
impl RedisSource {
    pub async fn new(
        url: &str,
        stream_key: impl Into<String>,
        group: impl Into<String>,
        consumer: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let stream_key = stream_key.into();
        let group = group.into();
        let consumer = consumer.into();

        let client = redis::Client::open(url).map_err(|e| RelayError::source_poll("redis", e))?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| RelayError::source_poll("redis", e))?;

        // Create the consumer group if it doesn't exist.
        let _: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&stream_key)
            .arg(&group)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut conn)
            .await;

        Ok(Self {
            conn,
            stream_key,
            group,
            consumer,
            event_type: event_type.into(),
        })
    }
}

#[cfg(feature = "redis")]
#[async_trait::async_trait]
impl super::Source for RedisSource {
    fn name(&self) -> &str {
        "redis"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        use redis::streams::StreamReadReply;

        let reply: StreamReadReply = redis::cmd("XREADGROUP")
            .arg("GROUP")
            .arg(&self.group)
            .arg(&self.consumer)
            .arg("COUNT")
            .arg(batch_size)
            .arg("BLOCK")
            .arg(100) // 100ms block
            .arg("STREAMS")
            .arg(&self.stream_key)
            .arg(">")
            .query_async(&mut self.conn)
            .await
            .map_err(|e| RelayError::source_poll("redis", e))?;

        let mut messages = Vec::new();
        for stream in reply.keys {
            for entry in stream.ids {
                let payload_str = entry
                    .map
                    .get("payload")
                    .and_then(|v| v.as_bytes().map(|b| String::from_utf8_lossy(b).to_string()))
                    .unwrap_or_default();

                let payload: serde_json::Value =
                    serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);

                let dedup_key = entry
                    .map
                    .get("pgt_dedup_key")
                    .and_then(|v| v.as_bytes().map(|b| String::from_utf8_lossy(b).to_string()))
                    .unwrap_or_else(|| entry.id.clone());

                let event_type = payload
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.event_type)
                    .to_string();

                let mut msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
                msg.ack_token = AckToken::RedisStreamId(entry.id);
                messages.push(msg);
            }
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError> {
        if let AckToken::RedisStreamId(ref id) = last_message.ack_token {
            let _: redis::RedisResult<()> = redis::cmd("XACK")
                .arg(&self.stream_key)
                .arg(&self.group)
                .arg(id)
                .query_async(&mut self.conn)
                .await;
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
