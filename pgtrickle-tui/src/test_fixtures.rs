use crate::state::*;

/// Build a minimal stream table for testing.
pub fn stream_table(name: &str, status: &str) -> StreamTableInfo {
    StreamTableInfo {
        name: name.to_string(),
        schema: "public".to_string(),
        status: status.to_string(),
        refresh_mode: "DIFF".to_string(),
        is_populated: true,
        consecutive_errors: 0,
        schedule: Some("5s".to_string()),
        staleness: None,
        tier: Some("hot".to_string()),
        last_refresh_at: Some("2026-04-01 12:00:00".to_string()),
        total_refreshes: 10,
        failed_refreshes: 0,
        avg_duration_ms: Some(5.0),
        stale: false,
        last_error_message: None,
        defining_query: None,
        cascade_stale: false,
    }
}

/// Build a stream table in error state.
pub fn error_stream_table(name: &str, error_msg: &str) -> StreamTableInfo {
    StreamTableInfo {
        status: "ERROR".to_string(),
        consecutive_errors: 3,
        failed_refreshes: 3,
        last_error_message: Some(error_msg.to_string()),
        stale: true,
        ..stream_table(name, "ERROR")
    }
}

/// Build a stale but active stream table.
pub fn stale_stream_table(name: &str) -> StreamTableInfo {
    StreamTableInfo {
        stale: true,
        staleness: Some("120s".to_string()),
        ..stream_table(name, "ACTIVE")
    }
}

/// Build an AppState with several stream tables and a dependency graph.
pub fn sample_state() -> AppState {
    let mut state = AppState {
        stream_tables: vec![
            stream_table("orders_live", "ACTIVE"),
            error_stream_table("revenue_daily", "division by zero"),
            stream_table("inventory_snap", "ACTIVE"),
            stale_stream_table("customer_agg"),
            StreamTableInfo {
                status: "SUSPENDED".to_string(),
                ..stream_table("broken_view", "SUSPENDED")
            },
        ],
        health_checks: vec![
            HealthCheck {
                check_name: "extension_version".to_string(),
                severity: "ok".to_string(),
                detail: "pg_trickle 0.13.0".to_string(),
            },
            HealthCheck {
                check_name: "cdc_lag".to_string(),
                severity: "warning".to_string(),
                detail: "Buffer lag on orders > 1MB".to_string(),
            },
            HealthCheck {
                check_name: "fuse_blown".to_string(),
                severity: "critical".to_string(),
                detail: "Circuit breaker triggered on revenue_daily".to_string(),
            },
        ],
        cdc_buffers: vec![
            CdcBuffer {
                stream_table: "orders_live".to_string(),
                source_table: "orders".to_string(),
                cdc_mode: "TRIGGER".to_string(),
                pending_rows: 250,
                buffer_bytes: 51200,
            },
            CdcBuffer {
                stream_table: "revenue_daily".to_string(),
                source_table: "sales".to_string(),
                cdc_mode: "TRIGGER".to_string(),
                pending_rows: 50000,
                buffer_bytes: 2_500_000,
            },
        ],
        dag_edges: vec![
            DagEdge {
                tree_line: "orders (source)".to_string(),
                node: "orders".to_string(),
                node_type: "source".to_string(),
                depth: 0,
                status: None,
                refresh_mode: None,
            },
            DagEdge {
                tree_line: "├── orders_live".to_string(),
                node: "orders_live".to_string(),
                node_type: "stream_table".to_string(),
                depth: 1,
                status: Some("ACTIVE".to_string()),
                refresh_mode: Some("DIFF".to_string()),
            },
            DagEdge {
                tree_line: "│   └── revenue_daily".to_string(),
                node: "revenue_daily".to_string(),
                node_type: "stream_table".to_string(),
                depth: 2,
                status: Some("ERROR".to_string()),
                refresh_mode: Some("DIFF".to_string()),
            },
            DagEdge {
                tree_line: "│       └── customer_agg".to_string(),
                node: "customer_agg".to_string(),
                node_type: "stream_table".to_string(),
                depth: 3,
                status: Some("ACTIVE".to_string()),
                refresh_mode: Some("DIFF".to_string()),
            },
            DagEdge {
                tree_line: "inventory (source)".to_string(),
                node: "inventory".to_string(),
                node_type: "source".to_string(),
                depth: 0,
                status: None,
                refresh_mode: None,
            },
            DagEdge {
                tree_line: "└── inventory_snap".to_string(),
                node: "inventory_snap".to_string(),
                node_type: "stream_table".to_string(),
                depth: 1,
                status: Some("ACTIVE".to_string()),
                refresh_mode: Some("DIFF".to_string()),
            },
        ],
        alerts: vec![],
        guc_params: vec![
            GucParam {
                name: "pg_trickle.enabled".to_string(),
                setting: "on".to_string(),
                unit: None,
                short_desc: "Enable pg_trickle background workers".to_string(),
                category: "General".to_string(),
            },
            GucParam {
                name: "pg_trickle.poll_interval".to_string(),
                setting: "2".to_string(),
                unit: Some("s".to_string()),
                short_desc: "Background poll interval".to_string(),
                category: "Scheduling".to_string(),
            },
        ],
        diagnostics: vec![
            DiagRecommendation {
                schema: "public".to_string(),
                name: "orders_live".to_string(),
                current_mode: "DIFF".to_string(),
                recommended_mode: "KEEP".to_string(),
                confidence: "high".to_string(),
                reason: "Already optimal".to_string(),
            },
            DiagRecommendation {
                schema: "public".to_string(),
                name: "inventory_snap".to_string(),
                current_mode: "FULL".to_string(),
                recommended_mode: "DIFF".to_string(),
                confidence: "medium".to_string(),
                reason: "Table supports differential".to_string(),
            },
        ],
        efficiency: vec![RefreshEfficiency {
            schema: "public".to_string(),
            name: "orders_live".to_string(),
            refresh_mode: "DIFF".to_string(),
            total_refreshes: 100,
            diff_count: 95,
            full_count: 5,
            avg_diff_ms: Some(2.5),
            avg_full_ms: Some(120.0),
            diff_speedup: Some("48.0".to_string()),
        }],
        refresh_log: vec![
            RefreshLogEntry {
                timestamp: "2026-04-01 12:00:00".to_string(),
                st_name: "orders_live".to_string(),
                action: "DIFF".to_string(),
                status: "success".to_string(),
                duration_ms: Some(3.2),
                rows_affected: Some(15),
            },
            RefreshLogEntry {
                timestamp: "2026-04-01 11:59:55".to_string(),
                st_name: "revenue_daily".to_string(),
                action: "DIFF".to_string(),
                status: "error".to_string(),
                duration_ms: Some(1.1),
                rows_affected: None,
            },
        ],
        workers: vec![
            WorkerInfo {
                worker_id: 1,
                state: "busy".to_string(),
                table_name: Some("orders_live".to_string()),
                started_at: Some("2026-04-01 12:00:00".to_string()),
                duration_ms: Some(3.2),
            },
            WorkerInfo {
                worker_id: 2,
                state: "idle".to_string(),
                table_name: None,
                started_at: None,
                duration_ms: None,
            },
        ],
        job_queue: vec![JobQueueEntry {
            position: 1,
            table_name: "inventory_snap".to_string(),
            priority: 10,
            queued_at: "2026-04-01 12:00:01".to_string(),
            wait_ms: Some(500.0),
        }],
        fuses: vec![
            FuseInfo {
                stream_table: "revenue_daily".to_string(),
                fuse_state: "BLOWN".to_string(),
                consecutive_errors: 5,
                last_error: Some("division by zero".to_string()),
                blown_at: Some("2026-04-01 11:58:00".to_string()),
            },
            FuseInfo {
                stream_table: "orders_live".to_string(),
                fuse_state: "OK".to_string(),
                consecutive_errors: 0,
                last_error: None,
                blown_at: None,
            },
        ],
        watermark_groups: vec![WatermarkGroup {
            group_name: "default".to_string(),
            member_count: 3,
            min_watermark: Some("2026-04-01 11:59:50".to_string()),
            max_watermark: Some("2026-04-01 12:00:00".to_string()),
            gated: false,
        }],
        trigger_inventory: vec![TriggerInfo {
            source_table: "orders".to_string(),
            trigger_name: "pgt_cdc_orders".to_string(),
            firing_events: "INSERT OR UPDATE OR DELETE".to_string(),
        }],
        connected: true,
        reconnecting: false,
        last_poll: None,
        poll_interval_ms: 2000,
        sparkline_data: std::collections::HashMap::new(),
        issues: vec![],
        error_message: None,
    };

    state.compute_cascade_staleness();
    state.detect_issues();
    state
}

/// Empty state — no data loaded yet.
pub fn empty_state() -> AppState {
    AppState {
        connected: true,
        ..AppState::default()
    }
}
