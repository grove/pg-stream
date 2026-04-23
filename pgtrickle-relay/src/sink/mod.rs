pub mod inbox;
pub mod pg_outbox;
/// Sink trait and implementations.
/// A Sink consumes RelayMessages published by a Source.
pub mod stdout;

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

/// A Sink consumes batches of RelayMessages.
#[async_trait]
pub trait Sink: Send {
    /// Return the sink backend type name (for logging/metrics).
    fn name(&self) -> &str;

    /// Publish a batch of messages.
    /// Returns Ok(()) if all messages were published successfully.
    /// On error, the caller will retry or fail the pipeline.
    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError>;

    /// Check if the sink is healthy (optional health check).
    async fn is_healthy(&mut self) -> bool;

    /// Gracefully close the sink (flush, disconnect, etc.).
    async fn close(&mut self) -> Result<(), RelayError>;
}
