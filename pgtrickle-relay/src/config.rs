/// Configuration types for the relay binary.
/// All pipeline definitions live in the PostgreSQL catalog tables.
/// This module only handles CLI/env/TOML configuration for the relay process itself.
use serde::{Deserialize, Serialize};

/// Top-level relay process configuration (not pipeline config — that lives in PG).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    /// PostgreSQL connection URL (required).
    /// Supports `${ENV:VAR_NAME}` substitution at load time (A30).
    pub postgres_url: String,

    /// Prometheus metrics + health endpoint address.
    pub metrics_addr: String,

    /// Log format: "text" or "json".
    pub log_format: LogFormat,

    /// Log level (e.g. "info", "debug", "warn", "error").
    pub log_level: String,

    /// Poll interval for pipeline discovery (seconds).
    pub discovery_interval_secs: u64,

    /// Default batch size when not specified per-pipeline.
    pub default_batch_size: i64,

    /// Relay group ID for advisory locks and offset namespacing.
    pub relay_group_id: String,

    /// A39: Maximum number of in-flight messages to the downstream sink
    /// before upstream polling is paused.  0 = unlimited (legacy behaviour).
    pub sink_max_inflight: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            postgres_url: String::new(),
            metrics_addr: "0.0.0.0:9090".to_string(),
            log_format: LogFormat::Text,
            log_level: "info".to_string(),
            discovery_interval_secs: 30,
            default_batch_size: 100,
            relay_group_id: "default".to_string(),
            sink_max_inflight: 1_000,
        }
    }
}

impl RelayConfig {
    /// A30: Expand `${ENV:VAR_NAME}` placeholders in a connection string using
    /// the current process environment.  Unknown variables are left as-is so
    /// callers can detect mis-configuration.
    ///
    /// # Security
    /// Only reads from the process environment — no eval or shell expansion.
    pub fn expand_env_vars(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(start) = rest.find("${ENV:") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 6..];
            if let Some(end) = after.find('}') {
                let var_name = &after[..end];
                match std::env::var(var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        // Leave the placeholder intact so the caller can detect the error.
                        result.push_str("${ENV:");
                        result.push_str(var_name);
                        result.push('}');
                    }
                }
                rest = &after[end + 1..];
            } else {
                // Malformed placeholder — pass through verbatim.
                result.push_str("${ENV:");
                rest = after;
            }
        }
        result.push_str(rest);
        result
    }

    /// Expand all connection string fields.
    pub fn resolve_env_vars(mut self) -> Self {
        self.postgres_url = Self::expand_env_vars(&self.postgres_url);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

/// Pipeline configuration loaded from `relay_outbox_config` or `relay_inbox_config`.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Pipeline name (primary key in catalog table).
    pub name: String,
    /// "forward" or "reverse".
    pub direction: PipelineDirection,
    /// Whether the pipeline is enabled.
    pub enabled: bool,
    /// The full config JSONB from the catalog.
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineDirection {
    Forward,
    Reverse,
}

impl PipelineConfig {
    /// Extract a required string value from the pipeline config.
    pub fn require_str<'a>(&'a self, path: &[&str]) -> Result<&'a str, crate::error::RelayError> {
        let mut v = &self.config;
        for key in path {
            v = v
                .get(key)
                .ok_or_else(|| crate::error::RelayError::MissingConfigKey {
                    pipeline: self.name.clone(),
                    key: key.to_string(),
                })?;
        }
        v.as_str()
            .ok_or_else(|| crate::error::RelayError::InvalidConfig {
                name: self.name.clone(),
                reason: format!("{}: expected string", path.join(".")),
            })
    }

    /// Extract an optional string value from the pipeline config.
    pub fn opt_str<'a>(&'a self, path: &[&str]) -> Option<&'a str> {
        let mut v = &self.config;
        for key in path {
            v = v.get(key)?;
        }
        v.as_str()
    }

    /// Extract an optional i64 value from the pipeline config.
    pub fn opt_i64(&self, path: &[&str]) -> Option<i64> {
        let mut v = &self.config;
        for key in path {
            v = v.get(key)?;
        }
        v.as_i64()
    }

    /// Extract an optional bool value from the pipeline config.
    pub fn opt_bool(&self, path: &[&str]) -> Option<bool> {
        let mut v = &self.config;
        for key in path {
            v = v.get(key)?;
        }
        v.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_pipeline(config: serde_json::Value) -> PipelineConfig {
        PipelineConfig {
            name: "test".to_string(),
            direction: PipelineDirection::Forward,
            enabled: true,
            config,
        }
    }

    #[test]
    fn test_require_str_nested() {
        let cfg = make_pipeline(json!({
            "source_type": "outbox",
            "source": { "outbox": "orders", "group": "relay-1" },
            "sink_type": "nats",
            "sink": { "type": "nats", "url": "nats://localhost:4222" }
        }));
        assert_eq!(cfg.require_str(&["source", "outbox"]).unwrap(), "orders");
        assert_eq!(
            cfg.require_str(&["sink", "url"]).unwrap(),
            "nats://localhost:4222"
        );
    }

    #[test]
    fn test_require_str_missing() {
        let cfg = make_pipeline(json!({"source_type": "outbox"}));
        assert!(cfg.require_str(&["source", "outbox"]).is_err());
    }

    #[test]
    fn test_opt_i64() {
        let cfg = make_pipeline(json!({"sink": {"batch_size": 500}}));
        assert_eq!(cfg.opt_i64(&["sink", "batch_size"]), Some(500));
        assert_eq!(cfg.opt_i64(&["sink", "missing"]), None);
    }

    #[test]
    fn test_relay_config_defaults() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.metrics_addr, "0.0.0.0:9090");
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.default_batch_size, 100);
    }

    #[test]
    fn test_relay_config_toml_roundtrip() {
        let cfg = RelayConfig {
            postgres_url: "postgres://localhost/test".to_string(),
            metrics_addr: "127.0.0.1:9091".to_string(),
            log_format: LogFormat::Json,
            log_level: "debug".to_string(),
            discovery_interval_secs: 60,
            default_batch_size: 200,
            relay_group_id: "prod-cluster-1".to_string(),
            sink_max_inflight: 500,
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        let decoded: RelayConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(decoded.postgres_url, cfg.postgres_url);
        assert_eq!(decoded.relay_group_id, cfg.relay_group_id);
        assert_eq!(decoded.sink_max_inflight, 500);
    }

    // ── A30: ENV variable expansion ───────────────────────────────────────

    #[test]
    fn test_expand_env_vars_no_placeholders() {
        let s = "postgres://localhost/mydb";
        assert_eq!(RelayConfig::expand_env_vars(s), s);
    }

    #[test]
    fn test_expand_env_vars_known_var() {
        // SAFETY: test-only; single-threaded cargo test with no parallel access.
        unsafe { std::env::set_var("PGTRICKLE_TEST_CONN_VAR", "secret_password") };
        let s = "postgres://user:${ENV:PGTRICKLE_TEST_CONN_VAR}@localhost/db";
        let result = RelayConfig::expand_env_vars(s);
        assert_eq!(result, "postgres://user:secret_password@localhost/db");
        // SAFETY: same as above.
        unsafe { std::env::remove_var("PGTRICKLE_TEST_CONN_VAR") };
    }

    #[test]
    fn test_expand_env_vars_unknown_var_preserved() {
        // Unknown variable placeholder must be left intact so callers can detect
        // mis-configuration rather than silently passing an empty password.
        // SAFETY: test-only; single-threaded cargo test.
        unsafe { std::env::remove_var("PGTRICKLE_DEFINITELY_NOT_SET_9XQ") };
        let s = "postgres://${ENV:PGTRICKLE_DEFINITELY_NOT_SET_9XQ}@host/db";
        let result = RelayConfig::expand_env_vars(s);
        assert_eq!(result, s, "unknown var placeholder should be preserved");
    }

    #[test]
    fn test_expand_env_vars_multiple_vars() {
        // SAFETY: test-only; single-threaded cargo test.
        unsafe {
            std::env::set_var("PGTRICKLE_TEST_USER", "alice");
            std::env::set_var("PGTRICKLE_TEST_DB", "analytics");
        }
        let s = "postgres://${ENV:PGTRICKLE_TEST_USER}@host/${ENV:PGTRICKLE_TEST_DB}";
        let result = RelayConfig::expand_env_vars(s);
        assert_eq!(result, "postgres://alice@host/analytics");
        // SAFETY: test-only; single-threaded cargo test.
        unsafe {
            std::env::remove_var("PGTRICKLE_TEST_USER");
            std::env::remove_var("PGTRICKLE_TEST_DB");
        }
    }

    #[test]
    fn test_expand_env_vars_malformed_unclosed() {
        // Malformed placeholder (no closing brace) is passed through verbatim.
        let s = "postgres://${ENV:UNCLOSED";
        let result = RelayConfig::expand_env_vars(s);
        assert_eq!(result, s);
    }
}
