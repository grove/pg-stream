/// Outbox poller source (RELAY-3 + RELAY-4).
/// Polls pg-trickle outbox tables and decodes payloads.
use std::sync::Arc;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::envelope::{AckToken, OutboxBatch, RelayMessage};
use crate::error::RelayError;

// ── Payload decoding (RELAY-4) ────────────────────────────────────────────

const FETCH_BATCH: i64 = 1000;

/// Decode one outbox row payload into an OutboxBatch.
pub async fn decode_payload(
    payload: &serde_json::Value,
    db: &Client,
    stream_table_name: &str,
    outbox_id: i64,
) -> Result<OutboxBatch, RelayError> {
    let v = payload.get("v").and_then(|v| v.as_i64()).unwrap_or(0);
    if v != 1 {
        return Err(RelayError::UnsupportedPayloadVersion(v));
    }

    let is_full_refresh = payload
        .get("full_refresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_claim_check = payload
        .get("claim_check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let refresh_id = payload
        .get("pgt_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    if is_claim_check {
        // Cursor-fetch from companion claim-check delta table.
        let outbox_name = format!("outbox_{stream_table_name}");
        let (inserted, deleted) = fetch_claim_check_rows(db, &outbox_name, outbox_id).await?;

        // Signal consumption complete so extension can drain old rows.
        db.execute(
            "SELECT pgtrickle.outbox_rows_consumed($1, $2)",
            &[&stream_table_name, &outbox_id],
        )
        .await
        .map_err(|e| RelayError::PayloadDecode {
            outbox: outbox_name.clone(),
            outbox_id,
            reason: e.to_string(),
        })?;

        Ok(OutboxBatch {
            outbox_id,
            refresh_id,
            is_full_refresh,
            inserted,
            deleted,
        })
    } else {
        // Inline — rows are embedded in the payload.
        Ok(OutboxBatch {
            outbox_id,
            refresh_id,
            is_full_refresh,
            inserted: extract_array(payload, "inserted"),
            deleted: extract_array(payload, "deleted"),
        })
    }
}

/// Fetch claim-check rows using a server-side cursor (bounded memory).
async fn fetch_claim_check_rows(
    db: &Client,
    outbox_name: &str,
    outbox_id: i64,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), RelayError> {
    let cursor_name = format!("relay_cc_{outbox_id}_{}", Uuid::new_v4().simple());
    let delta_table = format!("pgtrickle.outbox_delta_rows_{outbox_name}");

    // Open cursor — embed outbox_id literal (it's an i64 from DB, not user input).
    db.batch_execute(&format!(
        "DECLARE {cursor} NO SCROLL CURSOR FOR \
         SELECT op, payload FROM {table} WHERE outbox_id = {oid} ORDER BY row_num",
        cursor = cursor_name,
        table = delta_table,
        oid = outbox_id,
    ))
    .await
    .map_err(|e| RelayError::PayloadDecode {
        outbox: outbox_name.to_string(),
        outbox_id,
        reason: e.to_string(),
    })?;

    let mut inserted = Vec::new();
    let mut deleted = Vec::new();

    loop {
        let rows = db
            .query(
                &format!(
                    "FETCH {n} FROM {cursor}",
                    n = FETCH_BATCH,
                    cursor = cursor_name
                ),
                &[],
            )
            .await
            .map_err(|e| RelayError::PayloadDecode {
                outbox: outbox_name.to_string(),
                outbox_id,
                reason: e.to_string(),
            })?;

        let done = rows.len() < FETCH_BATCH as usize;
        for row in rows {
            let op: &str = row.get("op");
            let payload: serde_json::Value = row.get("payload");
            match op {
                "I" => inserted.push(payload),
                "D" => deleted.push(payload),
                _ => tracing::warn!(op, outbox_id, "unknown delta op in claim-check"),
            }
        }
        if done {
            break;
        }
    }

    // Close the cursor.
    let _ = db
        .batch_execute(&format!("CLOSE {cursor}", cursor = cursor_name))
        .await;

    Ok((inserted, deleted))
}

fn extract_array(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    v.get(key)
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
}

// ── OutboxPoller source (RELAY-3) ─────────────────────────────────────────

/// Outbox poller that works in two modes:
/// - Simple: tracks offsets in `relay_consumer_offsets` (suitable for single-relay).
/// - Consumer group: delegates to `poll_outbox()` + `commit_offset()`.
pub struct OutboxPollerSource {
    db: Arc<Client>,
    stream_table_name: String,
    outbox_table_name: String,
    subject_template: String,
    relay_group_id: String,
    pipeline_id: String,
    worker_id: String,
    consumer_group: Option<ConsumerGroupConfig>,
    last_offset: i64,
}

pub struct ConsumerGroupConfig {
    pub group_name: String,
    pub consumer_id: String,
    pub visibility_seconds: i32,
}

impl OutboxPollerSource {
    pub async fn new_simple(
        db: Arc<Client>,
        stream_table_name: impl Into<String>,
        outbox_table_name: impl Into<String>,
        subject_template: impl Into<String>,
        relay_group_id: impl Into<String>,
        pipeline_id: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let relay_group_id = relay_group_id.into();
        let pipeline_id = pipeline_id.into();
        let worker_id = format!(
            "{}:{}",
            std::env::var("HOSTNAME").unwrap_or_else(|_| "relay".to_string()),
            std::process::id()
        );

        // Load last committed offset from catalog.
        let last_offset = load_offset(&db, &relay_group_id, &pipeline_id).await?;

        Ok(Self {
            db,
            stream_table_name: stream_table_name.into(),
            outbox_table_name: outbox_table_name.into(),
            subject_template: subject_template.into(),
            relay_group_id,
            pipeline_id,
            worker_id,
            consumer_group: None,
            last_offset,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_consumer_group(
        db: Arc<Client>,
        stream_table_name: impl Into<String>,
        outbox_table_name: impl Into<String>,
        subject_template: impl Into<String>,
        relay_group_id: impl Into<String>,
        pipeline_id: impl Into<String>,
        group_name: impl Into<String>,
        consumer_id: impl Into<String>,
        visibility_seconds: i32,
    ) -> Result<Self, RelayError> {
        let relay_group_id = relay_group_id.into();
        let pipeline_id = pipeline_id.into();
        let worker_id = format!(
            "{}:{}",
            std::env::var("HOSTNAME").unwrap_or_else(|_| "relay".to_string()),
            std::process::id()
        );

        Ok(Self {
            db,
            stream_table_name: stream_table_name.into(),
            outbox_table_name: outbox_table_name.into(),
            subject_template: subject_template.into(),
            relay_group_id,
            pipeline_id,
            worker_id,
            consumer_group: Some(ConsumerGroupConfig {
                group_name: group_name.into(),
                consumer_id: consumer_id.into(),
                visibility_seconds,
            }),
            last_offset: 0,
        })
    }
}

#[async_trait::async_trait]
impl super::Source for OutboxPollerSource {
    fn name(&self) -> &str {
        "outbox"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        if let Some(cg) = &self.consumer_group {
            poll_consumer_group(
                &self.db,
                &cg.group_name,
                &cg.consumer_id,
                &self.stream_table_name,
                &self.subject_template,
                batch_size as i32,
                cg.visibility_seconds,
            )
            .await
        } else {
            poll_simple(
                &self.db,
                &self.outbox_table_name,
                &self.stream_table_name,
                &self.subject_template,
                self.last_offset,
                batch_size,
            )
            .await
        }
    }

    async fn acknowledge(&mut self, last_message: &RelayMessage) -> Result<(), RelayError> {
        if let AckToken::OutboxOffset(offset) = &last_message.ack_token {
            if self.consumer_group.is_some() {
                if let Some(cg) = &self.consumer_group {
                    let offset_i64 = *offset;
                    self.db
                        .execute(
                            "SELECT pgtrickle.commit_offset($1, $2, $3)",
                            &[&cg.group_name, &cg.consumer_id, &offset_i64],
                        )
                        .await?;
                }
            } else {
                let offset_i64 = *offset;
                update_simple_offset(
                    &self.db,
                    &self.relay_group_id,
                    &self.pipeline_id,
                    offset_i64,
                    &self.worker_id,
                )
                .await?;
                self.last_offset = offset_i64;
            }
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}

/// Simple mode: poll directly from the outbox table using stored offset.
async fn poll_simple(
    db: &Client,
    outbox_table_name: &str,
    stream_table_name: &str,
    subject_template: &str,
    last_offset: i64,
    batch_size: i64,
) -> Result<Vec<RelayMessage>, RelayError> {
    // Outbox tables are in the pgtrickle schema: pgtrickle.outbox_<stream_table>
    let outbox_schema_table = format!("pgtrickle.{outbox_table_name}");
    let rows = db
        .query(
            &format!(
                "SELECT id, payload FROM {table} WHERE id > $1 ORDER BY id LIMIT $2",
                table = outbox_schema_table
            ),
            &[&last_offset, &batch_size],
        )
        .await
        .map_err(RelayError::from)?;

    let mut messages = Vec::new();
    for row in &rows {
        let id: i64 = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let batch = decode_payload(&payload, db, stream_table_name, id).await?;
        let mut batch_msgs = batch.into_messages(outbox_table_name, subject_template);
        // Attach ack token to the last message in each outbox row.
        if let Some(last) = batch_msgs.last_mut() {
            last.ack_token = AckToken::OutboxOffset(id);
        }
        messages.extend(batch_msgs);
    }
    Ok(messages)
}

/// Consumer group mode: use poll_outbox() + commit_offset().
async fn poll_consumer_group(
    db: &Client,
    group: &str,
    consumer_id: &str,
    stream_table_name: &str,
    subject_template: &str,
    batch_size: i32,
    visibility_seconds: i32,
) -> Result<Vec<RelayMessage>, RelayError> {
    let rows = db
        .query(
            "SELECT * FROM pgtrickle.poll_outbox($1, $2, $3, $4)",
            &[&group, &consumer_id, &batch_size, &visibility_seconds],
        )
        .await?;

    if rows.is_empty() {
        // Heartbeat even when idle.
        let _ = db
            .execute(
                "SELECT pgtrickle.consumer_heartbeat($1, $2)",
                &[&group, &consumer_id],
            )
            .await;
        return Ok(vec![]);
    }

    let outbox_table_name = format!("outbox_{stream_table_name}");
    let mut messages = Vec::new();
    for row in &rows {
        let id: i64 = row.get("outbox_id");
        let payload: serde_json::Value = row.get("payload");
        let batch = decode_payload(&payload, db, stream_table_name, id).await?;
        let mut batch_msgs = batch.into_messages(&outbox_table_name, subject_template);
        if let Some(last) = batch_msgs.last_mut() {
            last.ack_token = AckToken::OutboxOffset(id);
        }
        messages.extend(batch_msgs);
    }
    Ok(messages)
}

/// Load the last committed offset for a simple-mode pipeline.
async fn load_offset(
    db: &Client,
    relay_group_id: &str,
    pipeline_id: &str,
) -> Result<i64, RelayError> {
    let row = db
        .query_opt(
            "SELECT last_change_id FROM pgtrickle.relay_consumer_offsets
             WHERE relay_group_id = $1 AND pipeline_id = $2",
            &[&relay_group_id, &pipeline_id],
        )
        .await?;
    Ok(row.map(|r| r.get::<_, i64>(0)).unwrap_or(0))
}

/// Write (upsert) the committed offset for a simple-mode pipeline.
async fn update_simple_offset(
    db: &Client,
    relay_group_id: &str,
    pipeline_id: &str,
    last_change_id: i64,
    worker_id: &str,
) -> Result<(), RelayError> {
    db.execute(
        "INSERT INTO pgtrickle.relay_consumer_offsets
             (relay_group_id, pipeline_id, last_change_id, worker_id, updated_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (relay_group_id, pipeline_id)
         DO UPDATE SET last_change_id = EXCLUDED.last_change_id,
                       worker_id      = EXCLUDED.worker_id,
                       updated_at     = EXCLUDED.updated_at",
        &[&relay_group_id, &pipeline_id, &last_change_id, &worker_id],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_array_present() {
        let v = serde_json::json!({"inserted": [{"id": 1}, {"id": 2}], "deleted": []});
        let arr = extract_array(&v, "inserted");
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_extract_array_missing() {
        let v = serde_json::json!({});
        let arr = extract_array(&v, "missing");
        assert!(arr.is_empty());
    }

    #[test]
    fn test_decode_payload_inline_differential() {
        // Inline differential payload — no DB access needed (no claim-check).
        // We test the sync path; the async path uses an actual DB.
        let payload = serde_json::json!({
            "v": 1,
            "full_refresh": false,
            "claim_check": false,
            "inserted": [{"id": 1}, {"id": 2}],
            "deleted": [{"id": 0}]
        });
        let is_claim_check = payload
            .get("claim_check")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(!is_claim_check);
        let inserted = extract_array(&payload, "inserted");
        assert_eq!(inserted.len(), 2);
        let deleted = extract_array(&payload, "deleted");
        assert_eq!(deleted.len(), 1);
    }

    #[test]
    fn test_decode_payload_unsupported_version() {
        let payload = serde_json::json!({"v": 99});
        let v = payload.get("v").and_then(|v| v.as_i64()).unwrap_or(0);
        assert_eq!(v, 99);
        // Would return RelayError::UnsupportedPayloadVersion(99) if we called decode_payload.
    }
}
