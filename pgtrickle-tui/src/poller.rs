use tokio_postgres::Client;

use crate::state::*;

/// Poll all data from the database and update the state store.
pub async fn poll_all(client: &Client, state: &mut AppState) {
    state.connected = true;
    state.reconnecting = false;
    state.last_poll = Some(chrono::Utc::now());

    // Phase 1: All core polls run in parallel
    let (r_st, r_hc, r_cdc, r_dag, r_diag, r_eff, r_guc, r_log, r_wk, r_fuse, r_wm, r_trig) = tokio::join!(
        poll_stream_tables_query(client),
        poll_health_query(client),
        poll_cdc_query(client),
        poll_dag_query(client),
        poll_diagnostics_query(client),
        poll_efficiency_query(client),
        poll_gucs_query(client),
        poll_refresh_log_query(client),
        poll_workers_query(client),
        poll_fuses_query(client),
        poll_watermarks_query(client),
        poll_triggers_query(client),
    );

    // Phase 2: New SQL API polls run in parallel (graceful degradation)
    let (r_dedup, r_cdc_h, r_qh, r_gates, r_wm_st, r_sbs, r_diamond, r_scc) = tokio::join!(
        poll_dedup_stats_query(client),
        poll_cdc_health_query(client),
        poll_quick_health_query(client),
        poll_source_gates_query(client),
        poll_watermark_status_query(client),
        poll_shared_buffer_stats_query(client),
        poll_diamond_groups_query(client),
        poll_scc_status_query(client),
    );

    // Apply core results
    if let Some((tables, sparkline_updates)) = r_st {
        for (name, ms) in sparkline_updates {
            let entry = state.sparkline_data.entry(name).or_default();
            entry.push(ms);
            if entry.len() > 20 {
                entry.remove(0);
            }
        }
        state.stream_tables = tables;
        if state
            .error_message
            .as_deref()
            .is_some_and(|m| m.starts_with("poll_stream_tables:"))
        {
            state.error_message = None;
        }
    }
    if let Some(v) = r_hc {
        state.health_checks = v;
    }
    if let Some(v) = r_cdc {
        state.cdc_buffers = v;
    }
    if let Some(v) = r_dag {
        state.dag_edges = v;
    }
    if let Some((diags, signals)) = r_diag {
        state.diagnostics = diags;
        for (k, v) in signals {
            state.diag_signals.insert(k, v);
        }
    }
    if let Some(v) = r_eff {
        state.efficiency = v;
    }
    if let Some(v) = r_guc {
        state.guc_params = v;
    }
    if let Some(v) = r_log {
        state.refresh_log = v;
    }
    if let Some((workers, queue)) = r_wk {
        state.workers = workers;
        state.job_queue = queue;
    }
    if let Some(v) = r_fuse {
        state.fuses = v;
    }
    if let Some(v) = r_wm {
        state.watermark_groups = v;
    }
    if let Some(v) = r_trig {
        state.trigger_inventory = v;
    }

    // Apply new SQL API results
    match r_dedup {
        Ok(v) => {
            state.dedup_stats = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.dedup_stats = None;
            state.record_poll_failure();
        }
    }
    match r_cdc_h {
        Ok(v) => {
            state.cdc_health = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.cdc_health = vec![];
            state.record_poll_failure();
        }
    }
    match r_qh {
        Ok(v) => {
            state.quick_health = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.quick_health = None;
            state.record_poll_failure();
        }
    }
    match r_gates {
        Ok(v) => {
            state.source_gates = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.source_gates = vec![];
            state.record_poll_failure();
        }
    }
    match r_wm_st {
        Ok(v) => {
            state.watermark_alignment = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.watermark_alignment = vec![];
            state.record_poll_failure();
        }
    }
    match r_sbs {
        Ok(v) => {
            state.shared_buffer_stats = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.shared_buffer_stats = vec![];
            state.record_poll_failure();
        }
    }
    match r_diamond {
        Ok(v) => {
            state.diamond_groups = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.diamond_groups = vec![];
            state.record_poll_failure();
        }
    }
    match r_scc {
        Ok(v) => {
            state.scc_groups = v;
            state.record_poll_success();
        }
        Err(_) => {
            state.scc_groups = vec![];
            state.record_poll_failure();
        }
    }

    // Post-poll computations (client-side, no DB queries)
    state.compute_cascade_staleness();
    state.detect_issues();
}

/// Execute a write action on the database. Returns a result message.
pub async fn execute_action(client: &Client, action: &ActionRequest) -> ActionResult {
    let result = match action {
        ActionRequest::RefreshTable(name) => {
            client
                .execute(
                    "SELECT pgtrickle.refresh_stream_table($1)",
                    &[name],
                )
                .await
                .map(|_| format!("Refreshed {name}"))
        }
        ActionRequest::RefreshAll => {
            client
                .execute("SELECT pgtrickle.refresh_all_stream_tables()", &[])
                .await
                .map(|_| "Refreshed all stream tables".to_string())
        }
        ActionRequest::PauseTable(name) => {
            client
                .execute(
                    "SELECT pgtrickle.alter_stream_table($1, status => 'paused')",
                    &[name],
                )
                .await
                .map(|_| format!("Paused {name}"))
        }
        ActionRequest::ResumeTable(name) => {
            client
                .execute(
                    "SELECT pgtrickle.alter_stream_table($1, status => 'active')",
                    &[name],
                )
                .await
                .map(|_| format!("Resumed {name}"))
        }
        ActionRequest::ResetFuse(name, strategy) => {
            client
                .execute(
                    "SELECT pgtrickle.reset_fuse($1, $2)",
                    &[name, strategy],
                )
                .await
                .map(|_| format!("Reset fuse for {name} (strategy: {strategy})"))
        }
        ActionRequest::RepairTable(name) => {
            client
                .execute(
                    "SELECT pgtrickle.repair_stream_table($1)",
                    &[name],
                )
                .await
                .map(|_| format!("Repaired {name}"))
        }
        ActionRequest::GateSource(name) => {
            client
                .execute("SELECT pgtrickle.gate_source($1)", &[name])
                .await
                .map(|_| format!("Gated source {name}"))
        }
        ActionRequest::UngateSource(name) => {
            client
                .execute("SELECT pgtrickle.ungate_source($1)", &[name])
                .await
                .map(|_| format!("Ungated source {name}"))
        }
        ActionRequest::FetchDeltaSql(name) => {
            match client
                .query("SELECT pgtrickle.explain_delta($1)", &[name])
                .await
            {
                Ok(rows) => {
                    let sql = rows
                        .iter()
                        .map(|row| row.get::<_, String>(0))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(sql)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchDdl(name) => {
            // Parse schema.name — name is always schema-qualified by the time it gets here.
            let (schema, table) = match name.split_once('.') {
                Some(pair) => pair,
                None => ("public", name.as_str()),
            };
            match client
                .query_one(
                    "SELECT defining_query::text, schedule::text, refresh_mode::text
                       FROM pgtrickle.pgt_stream_tables
                      WHERE pgt_schema = $1 AND pgt_name = $2",
                    &[&schema, &table],
                )
                .await
            {
                Ok(row) => {
                    let query: String = row.get(0);
                    let schedule: Option<String> = row.get(1);
                    let refresh_mode: String = row.get(2);
                    let mut ddl = format!(
                        "-- Stream table: {name}\n\
                         DROP STREAM TABLE IF EXISTS {name};\n\n\
                         SELECT pgtrickle.create_stream_table(\n  name   => '{name}',\n  query  => $pgt$\n{query}\n$pgt$"
                    );
                    if let Some(sched) = schedule {
                        ddl.push_str(&format!(",\n  schedule => '{sched}'"));
                    }
                    if refresh_mode != "DIFFERENTIAL" {
                        ddl.push_str(&format!(",\n  refresh_mode => '{refresh_mode}'"));
                    }
                    ddl.push_str("\n);");
                    Ok(ddl)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::ValidateQuery(query) => {
            match client
                .query(
                    "SELECT check_name::text, result::text, severity::text FROM pgtrickle.validate_query($1)",
                    &[query],
                )
                .await
            {
                Ok(rows) => {
                    let mut result = String::new();
                    for row in &rows {
                        let check: String = row.get(0);
                        let res: String = row.get(1);
                        let sev: String = row.get(2);
                        result.push_str(&format!("[{sev}] {check}: {res}\n"));
                    }
                    Ok(result)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchDiagnoseErrors(name) => {
            match client
                .query(
                    "SELECT event_time::text, error_type::text, error_message::text, remediation::text
                     FROM pgtrickle.diagnose_errors($1)
                     ORDER BY event_time DESC
                     LIMIT 5",
                    &[name],
                )
                .await
            {
                Ok(rows) => {
                    let json = serde_json::to_string(
                        &rows
                            .iter()
                            .map(|row| {
                                serde_json::json!({
                                    "event_time": row.get::<_, String>(0),
                                    "error_type": row.get::<_, String>(1),
                                    "error_message": row.get::<_, String>(2),
                                    "remediation": row.get::<_, String>(3),
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default();
                    Ok(json)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchExplainMode(name) => {
            match client
                .query_one(
                    "SELECT configured_mode::text, effective_mode::text, downgrade_reason::text
                     FROM pgtrickle.explain_refresh_mode($1)",
                    &[name],
                )
                .await
            {
                Ok(row) => {
                    let json = serde_json::json!({
                        "configured_mode": row.get::<_, String>(0),
                        "effective_mode": row.get::<_, String>(1),
                        "downgrade_reason": row.get::<_, Option<String>>(2),
                    });
                    Ok(json.to_string())
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchSources(name) => {
            match client
                .query(
                    "SELECT source_table::text, source_type::text,
                            cdc_mode::text, columns_used::text
                     FROM pgtrickle.list_sources($1)
                     ORDER BY source_table",
                    &[name],
                )
                .await
            {
                Ok(rows) => {
                    let json = serde_json::to_string(
                        &rows
                            .iter()
                            .map(|row| {
                                serde_json::json!({
                                    "source_table": row.get::<_, String>(0),
                                    "source_type": row.get::<_, String>(1),
                                    "cdc_mode": row.get::<_, String>(2),
                                    "columns_used": row.get::<_, Option<String>>(3),
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default();
                    Ok(json)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchRefreshHistory(name) => {
            match client
                .query(
                    "SELECT action::text, status::text, rows_inserted, rows_deleted,
                            delta_row_count, duration_ms, was_full_fallback,
                            start_time::text, error_message::text
                     FROM pgtrickle.get_refresh_history($1, 10)
                     ORDER BY start_time DESC",
                    &[name],
                )
                .await
            {
                Ok(rows) => {
                    let json = serde_json::to_string(
                        &rows
                            .iter()
                            .map(|row| {
                                serde_json::json!({
                                    "action": row.get::<_, String>(0),
                                    "status": row.get::<_, String>(1),
                                    "rows_inserted": row.get::<_, Option<i64>>(2),
                                    "rows_deleted": row.get::<_, Option<i64>>(3),
                                    "delta_row_count": row.get::<_, Option<i64>>(4),
                                    "duration_ms": row.get::<_, Option<f64>>(5),
                                    "was_full_fallback": row.get::<_, bool>(6),
                                    "start_time": row.get::<_, String>(7),
                                    "error_message": row.get::<_, Option<String>>(8),
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default();
                    Ok(json)
                }
                Err(e) => Err(e),
            }
        }
        ActionRequest::FetchAuxiliaryColumns(name) => {
            match client
                .query(
                    "SELECT column_name::text, data_type::text, purpose::text
                     FROM pgtrickle.list_auxiliary_columns($1)",
                    &[name],
                )
                .await
            {
                Ok(rows) => {
                    let json = serde_json::to_string(
                        &rows
                            .iter()
                            .map(|row| {
                                serde_json::json!({
                                    "column_name": row.get::<_, String>(0),
                                    "data_type": row.get::<_, String>(1),
                                    "purpose": row.get::<_, String>(2),
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default();
                    Ok(json)
                }
                Err(e) => Err(e),
            }
        }
    };

    match result {
        Ok(msg) => ActionResult {
            success: true,
            message: msg,
        },
        Err(e) => ActionResult {
            success: false,
            message: e
                .as_db_error()
                .map(|db| format!("Error: {}", db.message()))
                .unwrap_or_else(|| format!("Error: {e}")),
        },
    }
}

// ── Parallel-friendly query functions ─────────────────────────────
// Each returns parsed domain types without needing &mut AppState,
// enabling concurrent execution via tokio::join!.

type PgErr = tokio_postgres::Error;

/// Returns (tables, sparkline_updates) or None on error.
async fn poll_stream_tables_query(
    client: &Client,
) -> Option<(Vec<StreamTableInfo>, Vec<(String, f64)>)> {
    let rows = client
        .query(
            "SELECT
                s.pgt_name::text,
                s.pgt_schema::text,
                s.status::text,
                s.refresh_mode::text,
                s.is_populated,
                COALESCE(s.total_refreshes, 0)::bigint,
                COALESCE(s.failed_refreshes, 0)::bigint,
                s.avg_duration_ms,
                s.last_refresh_at::text,
                s.staleness_secs,
                s.stale,
                COALESCE(s.consecutive_errors, 0)::bigint,
                s.schedule::text,
                s.refresh_tier::text,
                s.last_error_message::text
             FROM pgtrickle.st_refresh_stats() s
             ORDER BY s.pgt_schema, s.pgt_name",
            &[],
        )
        .await
        .ok()?;

    let mut tables = Vec::with_capacity(rows.len());
    let mut sparkline_updates = Vec::new();
    for row in &rows {
        let staleness_secs: Option<f64> = row.get(9);
        let avg_ms: Option<f64> = row.get(7);
        let name: String = row.get(0);
        if let Some(ms) = avg_ms {
            sparkline_updates.push((name.clone(), ms));
        }
        tables.push(StreamTableInfo {
            name,
            schema: row.get(1),
            status: row.get(2),
            refresh_mode: row.get(3),
            is_populated: row.get(4),
            consecutive_errors: row.get(11),
            schedule: row.get(12),
            staleness: staleness_secs.map(|s| format!("{s:.0}s")),
            tier: row.get(13),
            last_refresh_at: row.get(8),
            total_refreshes: row.get(5),
            failed_refreshes: row.get(6),
            avg_duration_ms: avg_ms,
            stale: row.get(10),
            last_error_message: row.get(14),
            defining_query: None,
            cascade_stale: false,
        });
    }
    Some((tables, sparkline_updates))
}

async fn poll_health_query(client: &Client) -> Option<Vec<HealthCheck>> {
    let rows = client
        .query(
            "SELECT check_name::text, severity::text, detail::text
             FROM pgtrickle.health_check()
             ORDER BY CASE severity WHEN 'critical' THEN 1 WHEN 'warning' THEN 2 ELSE 3 END",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| HealthCheck {
                check_name: row.get(0),
                severity: row.get(1),
                detail: row.get(2),
            })
            .collect(),
    )
}

async fn poll_cdc_query(client: &Client) -> Option<Vec<CdcBuffer>> {
    let rows = client
        .query(
            "SELECT stream_table::text, source_table::text, cdc_mode::text,
                    pending_rows, buffer_bytes
             FROM pgtrickle.change_buffer_sizes()
             ORDER BY buffer_bytes DESC",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| CdcBuffer {
                stream_table: row.get(0),
                source_table: row.get(1),
                cdc_mode: row.get(2),
                pending_rows: row.get(3),
                buffer_bytes: row.get(4),
            })
            .collect(),
    )
}

async fn poll_dag_query(client: &Client) -> Option<Vec<DagEdge>> {
    let rows = client
        .query(
            "SELECT tree_line::text, node::text, node_type::text, depth,
                    status::text, refresh_mode::text
             FROM pgtrickle.dependency_tree()",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| DagEdge {
                tree_line: row.get(0),
                node: row.get(1),
                node_type: row.get(2),
                depth: row.get(3),
                status: row.get(4),
                refresh_mode: row.get(5),
            })
            .collect(),
    )
}

/// Returns (diagnostics, signal_map).
async fn poll_diagnostics_query(
    client: &Client,
) -> Option<(Vec<DiagRecommendation>, Vec<(String, serde_json::Value)>)> {
    let rows = client
        .query(
            "SELECT pgt_schema::text, pgt_name::text, current_mode::text,
                    recommended_mode::text, confidence::text, reason::text,
                    signals::text
             FROM pgtrickle.recommend_refresh_mode(NULL)
             ORDER BY pgt_schema, pgt_name",
            &[],
        )
        .await
        .ok()?;

    let mut diags = Vec::with_capacity(rows.len());
    let mut signals_map = Vec::new();
    for row in &rows {
        let signals_text: Option<String> = row.get(6);
        let signals = signals_text.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let name: String = row.get(1);
        if let Some(ref sig) = signals {
            signals_map.push((name.clone(), sig.clone()));
        }
        diags.push(DiagRecommendation {
            schema: row.get(0),
            name,
            current_mode: row.get(2),
            recommended_mode: row.get(3),
            confidence: row.get(4),
            reason: row.get(5),
            signals,
        });
    }
    Some((diags, signals_map))
}

async fn poll_efficiency_query(client: &Client) -> Option<Vec<RefreshEfficiency>> {
    let rows = client
        .query(
            "SELECT pgt_schema::text, pgt_name::text, refresh_mode::text,
                    total_refreshes, diff_count, full_count,
                    avg_diff_ms, avg_full_ms, diff_speedup::text
             FROM pgtrickle.refresh_efficiency()
             ORDER BY total_refreshes DESC",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| RefreshEfficiency {
                schema: row.get(0),
                name: row.get(1),
                refresh_mode: row.get(2),
                total_refreshes: row.get(3),
                diff_count: row.get(4),
                full_count: row.get(5),
                avg_diff_ms: row.get(6),
                avg_full_ms: row.get(7),
                diff_speedup: row.get(8),
            })
            .collect(),
    )
}

async fn poll_gucs_query(client: &Client) -> Option<Vec<GucParam>> {
    let rows = client
        .query(
            "SELECT name, setting, unit, short_desc, category
             FROM pg_settings WHERE name LIKE 'pg_trickle.%' ORDER BY name",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| GucParam {
                name: row.get(0),
                setting: row.get(1),
                unit: row.get(2),
                short_desc: row.get(3),
                category: row.get(4),
            })
            .collect(),
    )
}

async fn poll_refresh_log_query(client: &Client) -> Option<Vec<RefreshLogEntry>> {
    let rows = client
        .query(
            "SELECT refreshed_at::text, pgt_name::text, action::text,
                    status::text, duration_ms, rows_affected
             FROM pgtrickle.refresh_timeline()
             ORDER BY refreshed_at DESC
             LIMIT 200",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| RefreshLogEntry {
                timestamp: row.get(0),
                st_name: row.get(1),
                action: row.get(2),
                status: row.get(3),
                duration_ms: row.get(4),
                rows_affected: row.get(5),
            })
            .collect(),
    )
}

/// Returns (workers, job_queue).
async fn poll_workers_query(client: &Client) -> Option<(Vec<WorkerInfo>, Vec<JobQueueEntry>)> {
    let workers = client
        .query(
            "SELECT worker_id, state::text, table_name::text,
                    started_at::text, duration_ms
             FROM pgtrickle.worker_pool_status()
             ORDER BY worker_id",
            &[],
        )
        .await
        .ok()
        .map(|rows| {
            rows.iter()
                .map(|row| WorkerInfo {
                    worker_id: row.get(0),
                    state: row.get(1),
                    table_name: row.get(2),
                    started_at: row.get(3),
                    duration_ms: row.get(4),
                })
                .collect()
        })
        .unwrap_or_default();

    let queue = client
        .query(
            "SELECT position, table_name::text, priority, queued_at::text, wait_ms
             FROM pgtrickle.parallel_job_status()
             ORDER BY position",
            &[],
        )
        .await
        .ok()
        .map(|rows| {
            rows.iter()
                .map(|row| JobQueueEntry {
                    position: row.get(0),
                    table_name: row.get(1),
                    priority: row.get(2),
                    queued_at: row.get(3),
                    wait_ms: row.get(4),
                })
                .collect()
        })
        .unwrap_or_default();

    Some((workers, queue))
}

async fn poll_fuses_query(client: &Client) -> Option<Vec<FuseInfo>> {
    let rows = client
        .query(
            "SELECT pgt_name::text, fuse_state::text,
                    consecutive_errors, last_error_message::text,
                    blown_at::text
             FROM pgtrickle.fuse_status()
             ORDER BY CASE fuse_state WHEN 'BLOWN' THEN 1 WHEN 'TRIPPED' THEN 2 ELSE 3 END",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| FuseInfo {
                stream_table: row.get(0),
                fuse_state: row.get(1),
                consecutive_errors: row.get(2),
                last_error: row.get(3),
                blown_at: row.get(4),
            })
            .collect(),
    )
}

async fn poll_watermarks_query(client: &Client) -> Option<Vec<WatermarkGroup>> {
    let rows = client
        .query(
            "SELECT group_name::text, member_count, min_watermark::text,
                    max_watermark::text, gated
             FROM pgtrickle.watermark_groups()
             ORDER BY group_name",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| WatermarkGroup {
                group_name: row.get(0),
                member_count: row.get(1),
                min_watermark: row.get(2),
                max_watermark: row.get(3),
                gated: row.get(4),
            })
            .collect(),
    )
}

async fn poll_triggers_query(client: &Client) -> Option<Vec<TriggerInfo>> {
    let rows = client
        .query(
            "SELECT source_table::text, trigger_name::text, firing_events::text
             FROM pgtrickle.trigger_inventory()
             ORDER BY source_table, trigger_name",
            &[],
        )
        .await
        .ok()?;
    Some(
        rows.iter()
            .map(|row| TriggerInfo {
                source_table: row.get(0),
                trigger_name: row.get(1),
                firing_events: row.get(2),
            })
            .collect(),
    )
}

// ── New SQL API queries (graceful degradation on function-not-found) ──

async fn poll_dedup_stats_query(client: &Client) -> Result<Option<DedupStats>, PgErr> {
    let rows = client
        .query(
            "SELECT total_diff_refreshes, dedup_needed, dedup_ratio_pct
             FROM pgtrickle.dedup_stats()",
            &[],
        )
        .await?;
    Ok(rows.first().map(|row| DedupStats {
        total_diff_refreshes: row.get(0),
        dedup_needed: row.get(1),
        dedup_ratio_pct: row.get(2),
    }))
}

async fn poll_cdc_health_query(client: &Client) -> Result<Vec<CdcHealthEntry>, PgErr> {
    let rows = client
        .query(
            "SELECT source_table::text, cdc_mode::text, slot_name::text,
                    lag_bytes, confirmed_lsn::text, alert::text
             FROM pgtrickle.check_cdc_health()
             ORDER BY COALESCE(lag_bytes, 0) DESC",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| CdcHealthEntry {
            source_table: row.get(0),
            cdc_mode: row.get(1),
            slot_name: row.get(2),
            lag_bytes: row.get(3),
            confirmed_lsn: row.get(4),
            alert: row.get(5),
        })
        .collect())
}

async fn poll_quick_health_query(client: &Client) -> Result<Option<QuickHealth>, PgErr> {
    let rows = client
        .query(
            "SELECT total_stream_tables, error_tables, stale_tables,
                    scheduler_running, status::text
             FROM pgtrickle.quick_health",
            &[],
        )
        .await?;
    Ok(rows.first().map(|row| QuickHealth {
        total_stream_tables: row.get(0),
        error_tables: row.get(1),
        stale_tables: row.get(2),
        scheduler_running: row.get(3),
        status: row.get(4),
    }))
}

async fn poll_source_gates_query(client: &Client) -> Result<Vec<SourceGate>, PgErr> {
    let rows = client
        .query(
            "SELECT source_table::text, schema_name::text, gated,
                    gated_at::text, gate_duration::text,
                    affected_stream_tables::text
             FROM pgtrickle.bootstrap_gate_status()
             ORDER BY gated DESC, source_table",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| SourceGate {
            source_table: row.get(0),
            schema_name: row.get(1),
            gated: row.get(2),
            gated_at: row.get(3),
            gate_duration: row.get(4),
            affected_stream_tables: row.get(5),
        })
        .collect())
}

async fn poll_watermark_status_query(client: &Client) -> Result<Vec<WatermarkAlignment>, PgErr> {
    let rows = client
        .query(
            "SELECT group_name::text, lag_secs, aligned,
                    sources_with_watermark, sources_total
             FROM pgtrickle.watermark_status()",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| WatermarkAlignment {
            group_name: row.get(0),
            lag_secs: row.get(1),
            aligned: row.get(2),
            sources_with_watermark: row.get(3),
            sources_total: row.get(4),
        })
        .collect())
}

async fn poll_shared_buffer_stats_query(client: &Client) -> Result<Vec<SharedBufferInfo>, PgErr> {
    let rows = client
        .query(
            "SELECT source_table::text, consumer_count,
                    consumers::text, columns_tracked,
                    safe_frontier_lsn::text, buffer_rows, is_partitioned
             FROM pgtrickle.shared_buffer_stats()
             ORDER BY buffer_rows DESC",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| SharedBufferInfo {
            source_table: row.get(0),
            consumer_count: row.get(1),
            consumers: row.get(2),
            columns_tracked: row.get(3),
            safe_frontier_lsn: row.get(4),
            buffer_rows: row.get(5),
            is_partitioned: row.get(6),
        })
        .collect())
}

async fn poll_diamond_groups_query(client: &Client) -> Result<Vec<DiamondGroup>, PgErr> {
    let rows = client
        .query(
            "SELECT group_id, member_name::text, is_convergence, epoch
             FROM pgtrickle.diamond_groups()
             ORDER BY group_id, member_name",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| DiamondGroup {
            group_id: row.get(0),
            member_name: row.get(1),
            is_convergence: row.get(2),
            epoch: row.get(3),
        })
        .collect())
}

async fn poll_scc_status_query(client: &Client) -> Result<Vec<SccGroup>, PgErr> {
    let rows = client
        .query(
            "SELECT scc_id, member_count, members::text,
                    last_iterations, last_converged_at::text
             FROM pgtrickle.pgt_scc_status()
             ORDER BY scc_id",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| SccGroup {
            scc_id: row.get(0),
            member_count: row.get(1),
            members: row.get(2),
            last_iterations: row.get(3),
            last_converged_at: row.get(4),
        })
        .collect())
}
