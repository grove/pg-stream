/// Source trait and implementations.
/// A Source produces RelayMessages that are forwarded to a Sink.
pub mod outbox;
pub mod stdin;

#[cfg(feature = "nats")]
pub mod nats;

#[cfg(feature = "webhook")]
pub mod webhook;

#[cfg(feature = "kafka")]
pub mod kafka;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "sqs")]
pub mod sqs;

#[cfg(feature = "rabbitmq")]
pub mod rabbitmq;

use async_trait::async_trait;

use crate::envelope::RelayMessage;
use crate::error::RelayError;

/// A Source yields batches of RelayMessages.
///
/// After successfully publishing a batch to the Sink, the coordinator calls
/// `acknowledge()` with the last message in the batch so the Source can
/// advance its offset.
#[async_trait]
pub trait Source: Send {
    /// Return the source backend type name (for logging/metrics).
    fn name(&self) -> &str;

    /// Poll for the next batch of messages.
    /// Returns an empty Vec if there are no new messages.
    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError>;

    /// Acknowledge successful processing of a batch.
    /// The Source should advance its committed offset to `last_message.ack_token`.
    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError>;

    /// Gracefully close the source (release resources, stop background tasks).
    async fn close(&mut self) -> Result<(), RelayError>;
}
