//! A20 (v0.36.0): Structured logging for pg_trickle.
//!
//! When `pg_trickle.log_format = json`, log events are emitted as JSON
//! objects with structured fields for OpenTelemetry / Loki integration.
//! When `log_format = text` (default), standard `pgrx::log!()` is used.
//!
//! # JSON Log Format
//!
//! ```json
//! {
//!   "event": "refresh_completed",
//!   "pgt_id": 42,
//!   "cycle_id": "abc123",
//!   "duration_ms": 15.3,
//!   "refresh_reason": "scheduled",
//!   "error_code": null,
//!   "msg": "Differential refresh completed"
//! }
//! ```
//!
//! # Usage
//!
//! Use the `pgt_log!` and `pgt_info!` macros which dispatch based on the
//! current `pg_trickle.log_format` GUC value:
//!
//! ```rust
//! pgt_log!(event: "refresh_started", pgt_id: 42, msg: "Starting refresh");
//! ```

/// A structured log event for pg_trickle.
///
/// All fields are optional to allow partial construction; the serializer
/// omits `None` fields from the JSON output.
#[derive(Debug, Default)]
pub struct PgtLogEvent<'a> {
    /// Event type identifier (e.g., `"refresh_completed"`, `"fuse_blown"`).
    pub event: &'a str,
    /// pg_trickle stream table ID (`pgt_id`).
    pub pgt_id: Option<i64>,
    /// Refresh cycle identifier (opaque string, useful for tracing).
    pub cycle_id: Option<&'a str>,
    /// Duration of the operation in milliseconds.
    pub duration_ms: Option<f64>,
    /// Human-readable reason for the refresh (e.g., `"scheduled"`, `"forced"`).
    pub refresh_reason: Option<&'a str>,
    /// PostgreSQL SQLSTATE error code, if this event represents an error.
    pub error_code: Option<&'a str>,
    /// Free-form human-readable message.
    pub msg: Option<&'a str>,
}

impl<'a> PgtLogEvent<'a> {
    /// Serialize this event to a compact JSON string.
    pub fn to_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "event".to_string(),
            serde_json::Value::String(self.event.to_string()),
        );
        if let Some(pgt_id) = self.pgt_id {
            obj.insert(
                "pgt_id".to_string(),
                serde_json::Value::Number(pgt_id.into()),
            );
        }
        if let Some(cycle_id) = self.cycle_id {
            obj.insert(
                "cycle_id".to_string(),
                serde_json::Value::String(cycle_id.to_string()),
            );
        }
        if let Some(duration_ms) = self.duration_ms
            && let Some(n) = serde_json::Number::from_f64(duration_ms)
        {
            obj.insert("duration_ms".to_string(), serde_json::Value::Number(n));
        }
        if let Some(refresh_reason) = self.refresh_reason {
            obj.insert(
                "refresh_reason".to_string(),
                serde_json::Value::String(refresh_reason.to_string()),
            );
        }
        if let Some(error_code) = self.error_code {
            obj.insert(
                "error_code".to_string(),
                serde_json::Value::String(error_code.to_string()),
            );
        }
        if let Some(msg) = self.msg {
            obj.insert(
                "msg".to_string(),
                serde_json::Value::String(msg.to_string()),
            );
        }
        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| {
            format!(
                r#"{{"event":"{}","error":"serialization_failed"}}"#,
                self.event
            )
        })
    }
}

/// Emit a pg_trickle structured log event.
///
/// When `pg_trickle.log_format = json` the event is serialized to JSON and
/// emitted at `LOG` level. Otherwise the `msg` field is emitted as plain text.
///
/// This function is not directly callable from SQL — use the `pgt_log!` macro
/// for more ergonomic construction.
#[inline]
pub fn emit_log(event: &PgtLogEvent<'_>) {
    use crate::config::{LogFormat, pg_trickle_log_format};

    match pg_trickle_log_format() {
        LogFormat::Json => {
            let json = event.to_json();
            pgrx::log!("{}", json);
        }
        LogFormat::Text => {
            // Structured text: "event=<name> pgt_id=<n> msg=<m>"
            let mut parts = vec![format!("event={}", event.event)];
            if let Some(pgt_id) = event.pgt_id {
                parts.push(format!("pgt_id={pgt_id}"));
            }
            if let Some(duration_ms) = event.duration_ms {
                parts.push(format!("duration_ms={duration_ms:.1}"));
            }
            if let Some(msg) = event.msg {
                parts.push(format!("msg={msg}"));
            }
            if let Some(err) = event.error_code {
                parts.push(format!("error_code={err}"));
            }
            pgrx::log!("{}", parts.join(" "));
        }
    }
}

/// A20 (v0.36.0): Emit a structured INFO log event.
#[macro_export]
macro_rules! pgt_info {
    (event: $event:expr, $($field:ident: $value:expr),* $(,)?) => {{
        let ev = $crate::logging::PgtLogEvent {
            event: $event,
            $($field: Some($value),)*
            ..Default::default()
        };
        use $crate::config::{LogFormat, pg_trickle_log_format};
        match pg_trickle_log_format() {
            LogFormat::Json => {
                pgrx::info!("{}", ev.to_json());
            }
            LogFormat::Text => {
                if let Some(msg) = ev.msg {
                    pgrx::info!("{}", msg);
                }
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pgt_log_event_to_json_minimal() {
        let ev = PgtLogEvent {
            event: "test_event",
            ..Default::default()
        };
        let json = ev.to_json();
        assert!(json.contains(r#""event":"test_event""#));
    }

    #[test]
    fn test_pgt_log_event_to_json_all_fields() {
        let ev = PgtLogEvent {
            event: "refresh_completed",
            pgt_id: Some(42),
            cycle_id: Some("abc123"),
            duration_ms: Some(15.3),
            refresh_reason: Some("scheduled"),
            error_code: None,
            msg: Some("Differential refresh completed"),
        };
        let json = ev.to_json();
        assert!(json.contains(r#""pgt_id":42"#));
        assert!(json.contains(r#""cycle_id":"abc123""#));
        assert!(json.contains(r#""refresh_reason":"scheduled""#));
        assert!(json.contains(r#""msg":"Differential refresh completed""#));
        // None fields are omitted
        assert!(!json.contains("error_code"));
    }

    #[test]
    fn test_pgt_log_event_omits_none_fields() {
        let ev = PgtLogEvent {
            event: "drain_completed",
            pgt_id: None,
            ..Default::default()
        };
        let json = ev.to_json();
        assert!(!json.contains("pgt_id"));
        assert!(!json.contains("duration_ms"));
    }
}
