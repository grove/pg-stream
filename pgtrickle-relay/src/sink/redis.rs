/// Redis Streams sink (RELAY-10).
/// Feature-gated: only compiled with `--features redis`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "redis")]
use redis::aio::MultiplexedConnection;

#[cfg(feature = "redis")]
pub struct RedisSink {
    conn: MultiplexedConnection,
    stream_key_template: String,
    max_len: Option<usize>,
}

#[cfg(feature = "redis")]
impl RedisSink {
    pub async fn new(
        url: &str,
        stream_key_template: impl Into<String>,
        max_len: Option<usize>,
    ) -> Result<Self, RelayError> {
        let client = redis::Client::open(url).map_err(|e| RelayError::sink("redis", e))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| RelayError::sink("redis", e))?;
        Ok(Self {
            conn,
            stream_key_template: stream_key_template.into(),
            max_len,
        })
    }
}

#[cfg(feature = "redis")]
#[async_trait::async_trait]
impl super::Sink for RedisSink {
    fn name(&self) -> &str {
        "redis"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        for msg in messages {
            let stream_key = crate::envelope::render_subject(
                &self.stream_key_template,
                &msg.subject,
                &msg.op,
                msg.outbox_id.unwrap_or(0),
                msg.refresh_id,
            );
            let payload = serde_json::to_string(&msg.payload).map_err(RelayError::Json)?;

            let mut cmd = redis::cmd("XADD");
            cmd.arg(&stream_key);

            if let Some(maxlen) = self.max_len {
                cmd.arg("MAXLEN").arg("~").arg(maxlen);
            }

            cmd.arg("*")
                .arg("dedup_key")
                .arg(&msg.dedup_key)
                .arg("subject")
                .arg(&msg.subject)
                .arg("op")
                .arg(&msg.op)
                .arg("payload")
                .arg(&payload);

            cmd.query_async::<_, ()>(&mut self.conn)
                .await
                .map_err(|e| RelayError::sink("redis", e))?;
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        redis::cmd("PING")
            .query_async::<_, String>(&mut self.conn)
            .await
            .map(|r| r == "PONG")
            .unwrap_or(false)
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
