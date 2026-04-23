/// Amazon SQS consumer source (RELAY-27).
/// Feature-gated: only compiled with `--features sqs`.
use crate::envelope::{AckToken, RelayMessage};
use crate::error::RelayError;

#[cfg(feature = "sqs")]
use aws_sdk_sqs::Client as SqsClient;

#[cfg(feature = "sqs")]
pub struct SqsSource {
    client: SqsClient,
    queue_url: String,
    event_type: String,
    max_messages: i32,
}

#[cfg(feature = "sqs")]
impl SqsSource {
    pub async fn new(
        queue_url: impl Into<String>,
        event_type: impl Into<String>,
        max_messages: i32,
    ) -> Result<Self, RelayError> {
        let config = aws_config::load_from_env().await;
        let client = SqsClient::new(&config);
        Ok(Self {
            client,
            queue_url: queue_url.into(),
            event_type: event_type.into(),
            max_messages,
        })
    }
}

#[cfg(feature = "sqs")]
#[async_trait::async_trait]
impl super::Source for SqsSource {
    fn name(&self) -> &str {
        "sqs"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        let n = std::cmp::min(batch_size, self.max_messages as i64) as i32;
        let result = self
            .client
            .receive_message()
            .queue_url(&self.queue_url)
            .max_number_of_messages(n)
            .wait_time_seconds(1)
            .send()
            .await
            .map_err(|e| RelayError::source_poll("sqs", e))?;

        let mut messages = Vec::new();
        for msg in result.messages.unwrap_or_default() {
            let receipt = msg.receipt_handle.clone().unwrap_or_default();
            let body = msg.body.as_deref().unwrap_or("{}");
            let payload: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);

            let dedup_key = msg
                .message_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            let event_type = payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.event_type)
                .to_string();

            let mut relay_msg = RelayMessage::new_reverse(dedup_key, event_type, payload);
            relay_msg.ack_token = AckToken::SqsReceiptHandle(receipt);
            messages.push(relay_msg);
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError> {
        if let AckToken::SqsReceiptHandle(ref handle) = last_message.ack_token {
            self.client
                .delete_message()
                .queue_url(&self.queue_url)
                .receipt_handle(handle)
                .send()
                .await
                .map_err(|e| RelayError::source_poll("sqs", e))?;
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
