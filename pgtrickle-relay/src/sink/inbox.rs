/// pg-trickle inbox sink (RELAY-22).
/// Writes RelayMessages to a pg-trickle inbox table with ON CONFLICT dedup.
use std::sync::Arc;
use tokio_postgres::Client;

use crate::envelope::RelayMessage;
use crate::error::RelayError;

pub struct InboxSink {
    db: Arc<Client>,
    inbox_table: String,
    dedup_count: u64,
}

impl InboxSink {
    pub fn new(db: Arc<Client>, inbox_table: impl Into<String>) -> Self {
        Self {
            db,
            inbox_table: inbox_table.into(),
            dedup_count: 0,
        }
    }
}

#[async_trait::async_trait]
impl super::Sink for InboxSink {
    fn name(&self) -> &str {
        "pg-inbox"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        if messages.is_empty() {
            return Ok(());
        }

        // Batch insert with ON CONFLICT (event_id) DO NOTHING for dedup.
        // The inbox table must have columns: event_id, event_type, payload, received_at.
        // The event_id column maps to the dedup_key from the relay message.
        let schema_table = format!("pgtrickle.{}", self.inbox_table);

        for msg in messages {
            let result = self
                .db
                .execute(
                    &format!(
                        "INSERT INTO {table} (event_id, event_type, payload, received_at)
                         VALUES ($1, $2, $3, now())
                         ON CONFLICT (event_id) DO NOTHING",
                        table = schema_table
                    ),
                    &[&msg.dedup_key, &msg.subject, &msg.payload],
                )
                .await
                .map_err(RelayError::from)?;

            if result == 0 {
                // Conflict — duplicate message.
                self.dedup_count += 1;
                tracing::debug!(
                    dedup_key = %msg.dedup_key,
                    inbox = %self.inbox_table,
                    "duplicate message skipped (dedup)"
                );
            }
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        // Simple health check: run a trivial query.
        self.db.query_opt("SELECT 1", &[]).await.is_ok()
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}

impl InboxSink {
    pub fn dedup_count(&self) -> u64 {
        self.dedup_count
    }
}
