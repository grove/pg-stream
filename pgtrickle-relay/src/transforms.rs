/// Subject/topic routing templates and key extraction (RELAY-14).
/// Variables: `{stream_table}`, `{op}`, `{outbox_id}`, `{refresh_id}`.
///
/// Re-exports `render_subject` from envelope for convenience.
pub use crate::envelope::render_subject;

/// Generate a dedup key for reverse mode messages.
/// Prefers source-specific keys (Kafka offset, NATS msg ID, etc.) over UUID.
pub fn reverse_dedup_key(
    source_type: &str,
    source_key: Option<&str>,
    partition: Option<i32>,
    offset: Option<i64>,
) -> String {
    match (source_type, source_key, partition, offset) {
        ("kafka", _, Some(p), Some(o)) => format!("kafka:{p}:{o}"),
        ("nats", Some(k), _, _) => format!("nats:{k}"),
        ("redis", Some(k), _, _) => format!("redis:{k}"),
        ("sqs", Some(k), _, _) => format!("sqs:{k}"),
        ("rabbitmq", Some(k), _, _) => format!("rabbitmq:{k}"),
        (_, Some(k), _, _) => k.to_string(),
        _ => uuid::Uuid::new_v4().to_string(),
    }
}

/// Extract an event type from a message payload.
/// Tries common field names in order.
pub fn extract_event_type<'a>(
    payload: &'a serde_json::Value,
    default: &'a str,
    field_names: &[&str],
) -> &'a str {
    for name in field_names {
        if let Some(s) = payload.get(name).and_then(|v| v.as_str()) {
            return s;
        }
    }
    default
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_subject_stream_table() {
        let result = render_subject(
            "pgtrickle.{stream_table}.events",
            "orders",
            "insert",
            1,
            None,
        );
        assert_eq!(result, "pgtrickle.orders.events");
    }

    #[test]
    fn test_render_subject_op() {
        let result = render_subject("{stream_table}.{op}", "products", "delete", 5, None);
        assert_eq!(result, "products.delete");
    }

    #[test]
    fn test_render_subject_outbox_id() {
        let result = render_subject("events-{outbox_id}", "orders", "insert", 42, None);
        assert_eq!(result, "events-42");
    }

    #[test]
    fn test_reverse_dedup_key_kafka() {
        let key = reverse_dedup_key("kafka", None, Some(0), Some(100));
        assert_eq!(key, "kafka:0:100");
    }

    #[test]
    fn test_reverse_dedup_key_nats() {
        let key = reverse_dedup_key("nats", Some("msg-123"), None, None);
        assert_eq!(key, "nats:msg-123");
    }

    #[test]
    fn test_reverse_dedup_key_fallback() {
        let key = reverse_dedup_key("unknown", None, None, None);
        // Should be a UUID
        assert!(!key.is_empty());
        assert_eq!(key.len(), 36); // UUID format
    }

    #[test]
    fn test_extract_event_type_found() {
        let payload = serde_json::json!({"event_type": "order.created"});
        let et = extract_event_type(&payload, "default", &["event_type", "type"]);
        assert_eq!(et, "order.created");
    }

    #[test]
    fn test_extract_event_type_fallback_field() {
        let payload = serde_json::json!({"type": "order.updated"});
        let et = extract_event_type(&payload, "default", &["event_type", "type"]);
        assert_eq!(et, "order.updated");
    }

    #[test]
    fn test_extract_event_type_default() {
        let payload = serde_json::json!({"id": 1});
        let et = extract_event_type(&payload, "default.event", &["event_type"]);
        assert_eq!(et, "default.event");
    }
}
