use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Central state store for the TUI — updated by async pollers.
#[derive(Default)]
pub struct AppState {
    pub stream_tables: Vec<StreamTableInfo>,
    pub health_checks: Vec<HealthCheck>,
    pub cdc_buffers: Vec<CdcBuffer>,
    pub dag_edges: Vec<DagEdge>,
    pub alerts: Vec<AlertEvent>,
    pub guc_params: Vec<GucParam>,
    pub diagnostics: Vec<DiagRecommendation>,
    pub efficiency: Vec<RefreshEfficiency>,
    pub refresh_log: Vec<RefreshLogEntry>,
    pub last_poll: Option<DateTime<Utc>>,
    pub connected: bool,
    pub reconnecting: bool,
    #[allow(dead_code)]
    pub poll_interval_ms: u64,
    pub error_message: Option<String>,
    /// Sparkline data: st_name -> last N refresh durations
    pub sparkline_data: HashMap<String, Vec<f64>>,
    pub workers: Vec<WorkerInfo>,
    pub job_queue: Vec<JobQueueEntry>,
    pub fuses: Vec<FuseInfo>,
    pub watermark_groups: Vec<WatermarkGroup>,
    pub trigger_inventory: Vec<TriggerInfo>,
    /// Detected issues for DAG health view (F20)
    pub issues: Vec<Issue>,
}

#[derive(Clone, Serialize)]
pub struct StreamTableInfo {
    pub name: String,
    pub schema: String,
    pub status: String,
    pub refresh_mode: String,
    pub is_populated: bool,
    pub consecutive_errors: i64,
    pub schedule: Option<String>,
    pub staleness: Option<String>,
    pub tier: Option<String>,
    pub last_refresh_at: Option<String>,
    pub total_refreshes: i64,
    pub failed_refreshes: i64,
    pub avg_duration_ms: Option<f64>,
    pub stale: bool,
    pub last_error_message: Option<String>,
    /// Defining SQL query (from export_definition or pgt_stream_tables)
    pub defining_query: Option<String>,
    /// Cascade-stale: upstream has errors (F21)
    pub cascade_stale: bool,
}

#[derive(Clone, Serialize)]
pub struct HealthCheck {
    pub check_name: String,
    pub severity: String,
    pub detail: String,
}

#[derive(Clone, Serialize)]
pub struct CdcBuffer {
    pub stream_table: String,
    pub source_table: String,
    pub cdc_mode: String,
    pub pending_rows: i64,
    pub buffer_bytes: i64,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct DagEdge {
    pub tree_line: String,
    pub node: String,
    pub node_type: String,
    pub depth: i32,
    pub status: Option<String>,
    pub refresh_mode: Option<String>,
}

#[derive(Clone)]
pub struct AlertEvent {
    pub timestamp: DateTime<Utc>,
    pub severity: String,
    pub message: String,
}

#[derive(Clone, Serialize)]
pub struct GucParam {
    pub name: String,
    pub setting: String,
    pub unit: Option<String>,
    pub short_desc: String,
    pub category: String,
}

#[derive(Clone, Serialize)]
pub struct DiagRecommendation {
    pub schema: String,
    pub name: String,
    pub current_mode: String,
    pub recommended_mode: String,
    pub confidence: String,
    pub reason: String,
}

#[derive(Clone, Serialize)]
pub struct RefreshEfficiency {
    pub schema: String,
    pub name: String,
    pub refresh_mode: String,
    pub total_refreshes: i64,
    pub diff_count: i64,
    pub full_count: i64,
    pub avg_diff_ms: Option<f64>,
    pub avg_full_ms: Option<f64>,
    pub diff_speedup: Option<String>,
}

#[derive(Clone)]
pub struct RefreshLogEntry {
    pub timestamp: String,
    pub st_name: String,
    pub action: String,
    pub status: String,
    pub duration_ms: Option<f64>,
    #[allow(dead_code)]
    pub rows_affected: Option<i64>,
}

#[derive(Clone, Serialize)]
pub struct WorkerInfo {
    pub worker_id: i32,
    pub state: String,
    pub table_name: Option<String>,
    pub started_at: Option<String>,
    pub duration_ms: Option<f64>,
}

#[derive(Clone, Serialize)]
pub struct JobQueueEntry {
    pub position: i32,
    pub table_name: String,
    pub priority: i32,
    pub queued_at: String,
    pub wait_ms: Option<f64>,
}

#[derive(Clone, Serialize)]
pub struct FuseInfo {
    pub stream_table: String,
    pub fuse_state: String,
    pub consecutive_errors: i64,
    pub last_error: Option<String>,
    pub blown_at: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct WatermarkGroup {
    pub group_name: String,
    pub member_count: i64,
    pub min_watermark: Option<String>,
    pub max_watermark: Option<String>,
    pub gated: bool,
}

#[derive(Clone, Serialize)]
pub struct TriggerInfo {
    pub source_table: String,
    pub trigger_name: String,
    pub firing_events: String,
}

impl AppState {
    pub fn active_count(&self) -> usize {
        self.stream_tables
            .iter()
            .filter(|st| st.status == "ACTIVE")
            .count()
    }

    pub fn error_count(&self) -> usize {
        self.stream_tables
            .iter()
            .filter(|st| st.status == "ERROR" || st.status == "SUSPENDED")
            .count()
    }

    pub fn stale_count(&self) -> usize {
        self.stream_tables.iter().filter(|st| st.stale).count()
    }

    pub fn cascade_stale_count(&self) -> usize {
        self.stream_tables
            .iter()
            .filter(|st| st.cascade_stale)
            .count()
    }

    pub fn critical_health_count(&self) -> usize {
        self.health_checks
            .iter()
            .filter(|h| h.severity == "critical")
            .count()
    }

    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }

    /// Compute cascade staleness from DAG topology (F21).
    /// A stream table is cascade-stale if any upstream node is ERROR/SUSPENDED.
    pub fn compute_cascade_staleness(&mut self) {
        // Build set of error nodes
        let error_nodes: HashSet<&str> = self
            .stream_tables
            .iter()
            .filter(|st| st.status == "ERROR" || st.status == "SUSPENDED")
            .map(|st| st.name.as_str())
            .collect();

        if error_nodes.is_empty() {
            for st in &mut self.stream_tables {
                st.cascade_stale = false;
            }
            return;
        }

        // Build adjacency: parent -> children from dag_edges
        let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in &self.dag_edges {
            // In the dependency tree, a node at depth N+1 depends on depth N above it
            // We mark nodes that have any upstream error as cascade-stale
            if edge.node_type == "stream_table" || edge.node_type == "source" {
                children_of.entry(edge.node.as_str()).or_default();
            }
        }

        // Simple: mark all STs that have an error ST in their upstream chain
        // Walk dag_edges: if a parent is error, all its subtree is cascade-stale
        let mut cascade_set: HashSet<String> = HashSet::new();
        let mut in_error_subtree = false;
        let mut error_depth = i32::MAX;

        for edge in &self.dag_edges {
            if edge.depth <= error_depth {
                in_error_subtree = false;
                error_depth = i32::MAX;
            }
            if error_nodes.contains(edge.node.as_str()) {
                in_error_subtree = true;
                error_depth = edge.depth;
            } else if in_error_subtree && edge.depth > error_depth {
                cascade_set.insert(edge.node.clone());
            }
        }

        for st in &mut self.stream_tables {
            st.cascade_stale = cascade_set.contains(&st.name);
        }
    }

    /// Detect issues from current state (F20).
    pub fn detect_issues(&mut self) {
        let mut issues = Vec::new();

        // Broken chains: ERROR/SUSPENDED tables
        for st in &self.stream_tables {
            if st.status == "ERROR" || st.status == "SUSPENDED" {
                let downstream: Vec<String> = self
                    .stream_tables
                    .iter()
                    .filter(|other| other.cascade_stale)
                    .map(|other| other.name.clone())
                    .collect();
                issues.push(Issue {
                    severity: "error".to_string(),
                    category: "Broken Chain".to_string(),
                    summary: format!(
                        "{} in {} — {} downstream affected",
                        st.name,
                        st.status,
                        downstream.len()
                    ),
                    detail: st
                        .last_error_message
                        .clone()
                        .unwrap_or_else(|| "No error message".to_string()),
                    affected_table: Some(st.name.clone()),
                    blast_radius: downstream.len() + 1,
                });
            }
        }

        // Buffer growth warnings
        for buf in &self.cdc_buffers {
            if buf.buffer_bytes > 1_000_000 {
                issues.push(Issue {
                    severity: "warning".to_string(),
                    category: "Buffer Growth".to_string(),
                    summary: format!(
                        "{}: {:.1} MB ({} rows)",
                        buf.source_table,
                        buf.buffer_bytes as f64 / (1024.0 * 1024.0),
                        buf.pending_rows
                    ),
                    detail: "Change buffer is growing faster than consumption".to_string(),
                    affected_table: Some(buf.stream_table.clone()),
                    blast_radius: 1,
                });
            }
        }

        // Blown fuses
        for fuse in &self.fuses {
            if fuse.fuse_state == "BLOWN" {
                issues.push(Issue {
                    severity: "error".to_string(),
                    category: "Blown Fuse".to_string(),
                    summary: format!(
                        "{} — {} consecutive errors",
                        fuse.stream_table, fuse.consecutive_errors
                    ),
                    detail: fuse
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "No error detail".to_string()),
                    affected_table: Some(fuse.stream_table.clone()),
                    blast_radius: 1,
                });
            }
        }

        // Stale data
        for st in &self.stream_tables {
            if st.stale && st.status == "ACTIVE" {
                issues.push(Issue {
                    severity: "warning".to_string(),
                    category: "Stale Data".to_string(),
                    summary: format!(
                        "{} — staleness: {}",
                        st.name,
                        st.staleness.as_deref().unwrap_or("unknown")
                    ),
                    detail: "Data age exceeds schedule threshold".to_string(),
                    affected_table: Some(st.name.clone()),
                    blast_radius: 1,
                });
            }
        }

        // Sort: errors first, then by blast radius desc
        issues.sort_by(|a, b| {
            let sev_ord = |s: &str| match s {
                "error" => 0,
                "warning" => 1,
                _ => 2,
            };
            sev_ord(&a.severity)
                .cmp(&sev_ord(&b.severity))
                .then(b.blast_radius.cmp(&a.blast_radius))
        });

        self.issues = issues;
    }
}

/// A detected issue in the DAG (F20).
#[derive(Clone)]
pub struct Issue {
    pub severity: String,
    pub category: String,
    pub summary: String,
    #[allow(dead_code)]
    pub detail: String,
    pub affected_table: Option<String>,
    pub blast_radius: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;

    // ── Counter helpers ──────────────────────────────────────────────

    #[test]
    fn test_active_count_with_mixed_statuses() {
        let state = test_fixtures::sample_state();
        // orders_live, inventory_snap, customer_agg are ACTIVE
        assert_eq!(state.active_count(), 3);
    }

    #[test]
    fn test_error_count_includes_error_and_suspended() {
        let state = test_fixtures::sample_state();
        // revenue_daily=ERROR, broken_view=SUSPENDED
        assert_eq!(state.error_count(), 2);
    }

    #[test]
    fn test_stale_count() {
        let state = test_fixtures::sample_state();
        // customer_agg is stale, revenue_daily is stale (error_stream_table sets stale=true)
        assert_eq!(state.stale_count(), 2);
    }

    #[test]
    fn test_cascade_stale_count() {
        let state = test_fixtures::sample_state();
        // customer_agg is downstream of revenue_daily (ERROR), so cascade-stale
        assert!(state.cascade_stale_count() >= 1);
    }

    #[test]
    fn test_critical_health_count() {
        let state = test_fixtures::sample_state();
        // One critical health check: fuse_blown
        assert_eq!(state.critical_health_count(), 1);
    }

    #[test]
    fn test_issue_count() {
        let state = test_fixtures::sample_state();
        // Should have issues from ERROR/SUSPENDED tables, buffer growth, blown fuse, stale data
        assert!(state.issue_count() > 0);
    }

    #[test]
    fn test_empty_state_counters() {
        let state = test_fixtures::empty_state();
        assert_eq!(state.active_count(), 0);
        assert_eq!(state.error_count(), 0);
        assert_eq!(state.stale_count(), 0);
        assert_eq!(state.cascade_stale_count(), 0);
        assert_eq!(state.critical_health_count(), 0);
        assert_eq!(state.issue_count(), 0);
    }

    // ── Cascade staleness ────────────────────────────────────────────

    #[test]
    fn test_cascade_staleness_marks_downstream() {
        let mut state = AppState {
            stream_tables: vec![
                StreamTableInfo {
                    name: "parent".to_string(),
                    status: "ERROR".to_string(),
                    ..test_fixtures::stream_table("parent", "ERROR")
                },
                test_fixtures::stream_table("child", "ACTIVE"),
            ],
            dag_edges: vec![
                DagEdge {
                    tree_line: "parent".to_string(),
                    node: "parent".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 0,
                    status: Some("ERROR".to_string()),
                    refresh_mode: Some("DIFF".to_string()),
                },
                DagEdge {
                    tree_line: "└── child".to_string(),
                    node: "child".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 1,
                    status: Some("ACTIVE".to_string()),
                    refresh_mode: Some("DIFF".to_string()),
                },
            ],
            ..AppState::default()
        };

        state.compute_cascade_staleness();

        assert!(
            !state.stream_tables[0].cascade_stale,
            "error node itself is not cascade-stale"
        );
        assert!(
            state.stream_tables[1].cascade_stale,
            "child of error node should be cascade-stale"
        );
    }

    #[test]
    fn test_cascade_staleness_clears_when_no_errors() {
        let mut state = AppState {
            stream_tables: vec![
                test_fixtures::stream_table("a", "ACTIVE"),
                test_fixtures::stream_table("b", "ACTIVE"),
            ],
            dag_edges: vec![
                DagEdge {
                    tree_line: "a".to_string(),
                    node: "a".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 0,
                    status: Some("ACTIVE".to_string()),
                    refresh_mode: None,
                },
                DagEdge {
                    tree_line: "└── b".to_string(),
                    node: "b".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 1,
                    status: Some("ACTIVE".to_string()),
                    refresh_mode: None,
                },
            ],
            ..AppState::default()
        };

        // Pre-set cascade_stale to true, should be cleared
        state.stream_tables[1].cascade_stale = true;
        state.compute_cascade_staleness();

        assert!(!state.stream_tables[0].cascade_stale);
        assert!(!state.stream_tables[1].cascade_stale);
    }

    #[test]
    fn test_cascade_staleness_deep_chain() {
        let mut state = AppState {
            stream_tables: vec![
                test_fixtures::stream_table("root", "ERROR"),
                test_fixtures::stream_table("mid", "ACTIVE"),
                test_fixtures::stream_table("leaf", "ACTIVE"),
            ],
            dag_edges: vec![
                DagEdge {
                    tree_line: "root".to_string(),
                    node: "root".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 0,
                    status: Some("ERROR".to_string()),
                    refresh_mode: None,
                },
                DagEdge {
                    tree_line: "├── mid".to_string(),
                    node: "mid".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 1,
                    status: Some("ACTIVE".to_string()),
                    refresh_mode: None,
                },
                DagEdge {
                    tree_line: "│   └── leaf".to_string(),
                    node: "leaf".to_string(),
                    node_type: "stream_table".to_string(),
                    depth: 2,
                    status: Some("ACTIVE".to_string()),
                    refresh_mode: None,
                },
            ],
            ..AppState::default()
        };

        state.compute_cascade_staleness();

        assert!(!state.stream_tables[0].cascade_stale);
        assert!(state.stream_tables[1].cascade_stale);
        assert!(state.stream_tables[2].cascade_stale);
    }

    // ── Issue detection ──────────────────────────────────────────────

    #[test]
    fn test_detect_issues_broken_chain() {
        let mut state = AppState {
            stream_tables: vec![test_fixtures::error_stream_table("broken", "syntax error")],
            ..AppState::default()
        };

        state.detect_issues();

        let broken = state.issues.iter().find(|i| i.category == "Broken Chain");
        assert!(broken.is_some());
        assert_eq!(broken.unwrap().severity, "error");
        assert!(broken.unwrap().summary.contains("broken"));
    }

    #[test]
    fn test_detect_issues_buffer_growth() {
        let mut state = AppState {
            cdc_buffers: vec![CdcBuffer {
                stream_table: "big_table".to_string(),
                source_table: "orders".to_string(),
                cdc_mode: "TRIGGER".to_string(),
                pending_rows: 100_000,
                buffer_bytes: 5_000_000,
            }],
            ..AppState::default()
        };

        state.detect_issues();

        let buf = state.issues.iter().find(|i| i.category == "Buffer Growth");
        assert!(buf.is_some());
        assert_eq!(buf.unwrap().severity, "warning");
    }

    #[test]
    fn test_detect_issues_blown_fuse() {
        let mut state = AppState {
            fuses: vec![FuseInfo {
                stream_table: "fused".to_string(),
                fuse_state: "BLOWN".to_string(),
                consecutive_errors: 10,
                last_error: Some("timeout".to_string()),
                blown_at: Some("2026-04-01".to_string()),
            }],
            ..AppState::default()
        };

        state.detect_issues();

        let fuse = state.issues.iter().find(|i| i.category == "Blown Fuse");
        assert!(fuse.is_some());
        assert_eq!(fuse.unwrap().severity, "error");
    }

    #[test]
    fn test_detect_issues_stale_data() {
        let mut state = AppState {
            stream_tables: vec![test_fixtures::stale_stream_table("slow_table")],
            ..AppState::default()
        };

        state.detect_issues();

        let stale = state.issues.iter().find(|i| i.category == "Stale Data");
        assert!(stale.is_some());
        assert_eq!(stale.unwrap().severity, "warning");
    }

    #[test]
    fn test_detect_issues_no_issues_when_healthy() {
        let mut state = AppState {
            stream_tables: vec![test_fixtures::stream_table("healthy", "ACTIVE")],
            ..AppState::default()
        };

        state.detect_issues();
        assert!(state.issues.is_empty());
    }

    #[test]
    fn test_detect_issues_sorting_errors_before_warnings() {
        let mut state = AppState {
            stream_tables: vec![
                test_fixtures::stale_stream_table("stale_one"),
                test_fixtures::error_stream_table("broken_one", "fail"),
            ],
            fuses: vec![FuseInfo {
                stream_table: "broken_one".to_string(),
                fuse_state: "BLOWN".to_string(),
                consecutive_errors: 5,
                last_error: Some("fail".to_string()),
                blown_at: None,
            }],
            ..AppState::default()
        };

        state.detect_issues();

        // Errors should come before warnings
        let first_error_idx = state
            .issues
            .iter()
            .position(|i| i.severity == "error")
            .unwrap_or(usize::MAX);
        let first_warning_idx = state
            .issues
            .iter()
            .position(|i| i.severity == "warning")
            .unwrap_or(usize::MAX);
        assert!(first_error_idx < first_warning_idx);
    }

    #[test]
    fn test_detect_issues_buffer_below_threshold_no_issue() {
        let mut state = AppState {
            cdc_buffers: vec![CdcBuffer {
                stream_table: "small".to_string(),
                source_table: "src".to_string(),
                cdc_mode: "TRIGGER".to_string(),
                pending_rows: 10,
                buffer_bytes: 500_000,
            }],
            ..AppState::default()
        };

        state.detect_issues();
        assert!(
            state.issues.is_empty(),
            "buffer under 1MB should not create an issue"
        );
    }

    #[test]
    fn test_detect_issues_fuse_ok_no_issue() {
        let mut state = AppState {
            fuses: vec![FuseInfo {
                stream_table: "ok_fuse".to_string(),
                fuse_state: "OK".to_string(),
                consecutive_errors: 0,
                last_error: None,
                blown_at: None,
            }],
            ..AppState::default()
        };

        state.detect_issues();
        assert!(state.issues.is_empty());
    }
}
