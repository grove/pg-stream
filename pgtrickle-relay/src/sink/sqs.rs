/// Amazon SQS sink (RELAY-11).
/// Feature-gated: only compiled with `--features sqs`.
use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[cfg(feature = "sqs")]
use aws_sdk_sqs::Client as SqsClient;

#[cfg(feature = "sqs")]
pub struct SqsSink {
    client: SqsClient,
    queue_url: String,
    is_fifo: bool,
}

#[cfg(feature = "sqs")]
impl SqsSink {
    pub async fn new(queue_url: impl Into<String>, is_fifo: bool) -> Result<Self, RelayError> {
        let config = aws_config::load_from_env().await;
        let client = SqsClient::new(&config);
        Ok(Self {
            client,
            queue_url: queue_url.into(),
            is_fifo,
        })
    }
}

#[cfg(feature = "sqs")]
#[async_trait::async_trait]
impl super::Sink for SqsSink {
    fn name(&self) -> &str {
        "sqs"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        use aws_sdk_sqs::types::SendMessageBatchRequestEntry;

        // SQS batch limit is 10 messages per request.
        for chunk in messages.chunks(10) {
            let mut entries = Vec::new();
            for (i, msg) in chunk.iter().enumerate() {
                let body = serde_json::to_string(msg).map_err(RelayError::Json)?;
                let mut entry = SendMessageBatchRequestEntry::builder()
                    .id(i.to_string())
                    .message_body(body);

                if self.is_fifo {
                    entry = entry
                        .message_deduplication_id(&msg.dedup_key)
                        .message_group_id(
                            msg.outbox_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "default".to_string()),
                        );
                }

                entries.push(entry.build().map_err(|e| RelayError::sink("sqs", e))?);
            }

            let result = self
                .client
                .send_message_batch()
                .queue_url(&self.queue_url)
                .set_entries(Some(entries))
                .send()
                .await
                .map_err(|e| RelayError::sink("sqs", e))?;

            if !result.failed.is_empty() {
                let first = &result.failed[0];
                return Err(RelayError::SinkPublish {
                    sink: "sqs".to_string(),
                    source: format!(
                        "SQS batch failure: {} — {}",
                        first.code,
                        first.message.as_deref().unwrap_or("")
                    )
                    .into(),
                });
            }
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        true
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
