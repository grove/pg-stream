/// PostgreSQL inbox sink for remote PG (RELAY-12).
/// Writes messages to a pg-trickle inbox table on a different PostgreSQL instance.
use tokio_postgres::Client;

use crate::envelope::RelayMessage;
use crate::error::RelayError;

/// Remote PostgreSQL inbox sink.
/// Uses tokio-postgres directly (same as pgtrickle-tui).
pub struct PgInboxSink {
    client: Client,
    inbox_table: String,
    dedup_count: u64,
}

impl PgInboxSink {
    pub async fn new(
        postgres_url: &str,
        inbox_table: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let (client, conn) = tokio_postgres::connect(postgres_url, tokio_postgres::NoTls)
            .await
            .map_err(|e| RelayError::ConnectionFailed {
                url: postgres_url.to_string(),
                err: e,
            })?;

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::error!("pg-inbox remote connection error: {e}");
            }
        });

        Ok(Self {
            client,
            inbox_table: inbox_table.into(),
            dedup_count: 0,
        })
    }
}

#[async_trait::async_trait]
impl super::Sink for PgInboxSink {
    fn name(&self) -> &str {
        "pg-inbox-remote"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        for msg in messages {
            let result = self
                .client
                .execute(
                    &format!(
                        "INSERT INTO pgtrickle.{table} (event_id, event_type, payload, received_at)
                         VALUES ($1, $2, $3, now())
                         ON CONFLICT (event_id) DO NOTHING",
                        table = self.inbox_table
                    ),
                    &[&msg.dedup_key, &msg.subject, &msg.payload],
                )
                .await
                .map_err(RelayError::from)?;

            if result == 0 {
                self.dedup_count += 1;
            }
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        self.client.query_opt("SELECT 1", &[]).await.is_ok()
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}
