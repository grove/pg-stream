//! DAG Topology Benchmarks — PLAN_DAG_BENCHMARK.md §1–§14.
//!
//! Measures end-to-end propagation latency and throughput through multi-level
//! DAG topologies (linear chains, wide DAGs, fan-out trees, diamonds, mixed).
//!
//! These tests are `#[ignore]`d to skip in normal CI. Run explicitly:
//!
//! ```bash
//! cargo test --test e2e_dag_bench_tests --features pg18 -- --ignored --nocapture
//! ```
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;
use std::time::{Duration, Instant};

// ── Configuration ──────────────────────────────────────────────────────

/// Number of measurement cycles per benchmark (after warm-up).
const CYCLES: usize = 5;

/// Number of throw-away warm-up rounds before measured cycles.
const WARMUP_CYCLES: usize = 2;

/// Rows to INSERT per latency measurement cycle.
const LATENCY_DELTA_ROWS: u32 = 100;

/// Base table size for all topologies.
const BASE_TABLE_SIZE: u32 = 10_000;

/// Number of groups used in source tables.
const NUM_GROUPS: u32 = 10;

/// Delta sizes used for throughput benchmarks.
const THROUGHPUT_DELTA_SIZES: &[u32] = &[10, 100, 1000];

/// Scheduler interval used for latency benchmarks (ms).
const SCHEDULER_INTERVAL_MS: f64 = 200.0;

/// Polling interval for leaf refresh detection (ms).
const POLL_INTERVAL_MS: u64 = 100;

/// Maximum time to wait for a leaf refresh (seconds).
const LEAF_TIMEOUT_SECS: u64 = 120;

// ── Data Structures ────────────────────────────────────────────────────

/// Metadata about a constructed DAG topology.
struct DagTopology {
    /// Base source table names.
    source_tables: Vec<String>,
    /// All ST names in topological (creation) order.
    all_sts: Vec<String>,
    /// Terminal STs (no downstream consumers).
    leaf_sts: Vec<String>,
    /// Longest path from source to leaf.
    depth: u32,
    /// Maximum STs at any single level.
    max_width: u32,
    /// STs grouped by level (for parallel dispatch analysis).
    levels: Vec<Vec<String>>,
}

/// Per-ST timing extracted from `pgt_refresh_history`.
#[derive(Clone, Debug)]
struct StTimingEntry {
    pgt_name: String,
    level: u32,
    refresh_ms: f64,
    action: String,
}

/// A single DAG benchmark measurement.
#[derive(Clone)]
struct DagBenchResult {
    topology: String,
    refresh_mode: String,
    measurement: String,
    dag_depth: u32,
    dag_width: u32,
    total_sts: u32,
    delta_rows: u32,
    cycle: usize,
    propagation_ms: f64,
    per_hop_avg_ms: f64,
    throughput_rows_per_sec: f64,
    theoretical_ms: f64,
    overhead_pct: f64,
    per_st_breakdown: Vec<StTimingEntry>,
}

// ── Topology Builders ──────────────────────────────────────────────────

/// Build a linear chain: src → st_lc_1 → st_lc_2 → ... → st_lc_N
///
/// L1: aggregate (GROUP BY grp, SUM + COUNT)
/// L2+: alternating project (total * 2) and filter (WHERE total > 0)
async fn build_linear_chain(db: &E2eDb, depth: u32) -> DagTopology {
    let src = "lc_src".to_string();
    db.execute(
        "CREATE TABLE lc_src (
            id  SERIAL PRIMARY KEY,
            grp TEXT NOT NULL,
            val INT NOT NULL
        )",
    )
    .await;

    db.execute(&format!(
        "INSERT INTO lc_src (grp, val)
         SELECT 'g' || (i % {NUM_GROUPS}), (random() * 1000)::int
         FROM generate_series(1, {BASE_TABLE_SIZE}) AS s(i)"
    ))
    .await;

    db.execute("ANALYZE lc_src").await;

    let mut all_sts = Vec::new();
    let mut levels: Vec<Vec<String>> = Vec::new();

    // L1: aggregate
    let st1 = "st_lc_1".to_string();
    db.create_st(
        &st1,
        "SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM lc_src GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
    all_sts.push(st1.clone());
    levels.push(vec![st1]);

    // L2+: alternating project / filter on previous ST
    for i in 2..=depth {
        let st_name = format!("st_lc_{i}");
        let prev = format!("st_lc_{}", i - 1);
        let query = if i % 2 == 0 {
            format!("SELECT grp, total * 2 AS total, cnt FROM {prev}")
        } else {
            format!("SELECT grp, total, cnt FROM {prev} WHERE total > 0")
        };
        db.execute(&format!(
            "SELECT pgtrickle.create_stream_table('{st_name}', $${query}$$, 'calculated', 'DIFFERENTIAL')"
        ))
        .await;
        all_sts.push(st_name.clone());
        levels.push(vec![st_name]);
    }

    let leaf = all_sts.last().unwrap().clone();

    DagTopology {
        source_tables: vec![src],
        all_sts,
        leaf_sts: vec![leaf],
        depth,
        max_width: 1,
        levels,
    }
}

// ── Measurement Helpers ────────────────────────────────────────────────

/// Configure scheduler for latency benchmarks with the given refresh mode.
async fn configure_latency_scheduler(db: &E2eDb, mode: &str, concurrency: u32) {
    db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 200")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1")
        .await;
    db.execute("ALTER SYSTEM SET pg_trickle.auto_backoff = off")
        .await;

    if mode.starts_with("PARALLEL") {
        db.execute("ALTER SYSTEM SET pg_trickle.parallel_refresh_mode = 'on'")
            .await;
        db.execute(&format!(
            "ALTER SYSTEM SET pg_trickle.max_concurrent_refreshes = {concurrency}"
        ))
        .await;
    }

    db.reload_config_and_wait().await;
    db.wait_for_setting("pg_trickle.scheduler_interval_ms", "200")
        .await;
    db.wait_for_setting("pg_trickle.min_schedule_seconds", "1")
        .await;
    db.wait_for_setting("pg_trickle.auto_backoff", "off").await;

    if mode.starts_with("PARALLEL") {
        db.wait_for_setting("pg_trickle.parallel_refresh_mode", "on")
            .await;
        db.wait_for_setting(
            "pg_trickle.max_concurrent_refreshes",
            &concurrency.to_string(),
        )
        .await;
    }

    assert!(
        db.wait_for_scheduler(Duration::from_secs(90)).await,
        "pg_trickle scheduler did not appear within 90 s"
    );
}

/// INSERT delta rows into a source table.
async fn insert_delta(db: &E2eDb, source_table: &str, delta_rows: u32) {
    db.execute(&format!(
        "INSERT INTO {source_table} (grp, val)
         SELECT 'g' || (random() * {})::int, (random() * 1000)::int
         FROM generate_series(1, {delta_rows})",
        NUM_GROUPS - 1,
    ))
    .await;
}

/// Apply a mixed DML workload (70% UPDATE, 15% DELETE, 15% INSERT).
async fn apply_dml_mix(db: &E2eDb, source_table: &str, delta_size: u32) {
    let n_update = (delta_size as f64 * 0.70) as u32;
    let n_delete = (delta_size as f64 * 0.15) as u32;
    let n_insert = delta_size - n_update - n_delete;

    if n_update > 0 {
        db.execute(&format!(
            "UPDATE {source_table} SET val = val + 1
             WHERE id IN (SELECT id FROM {source_table} ORDER BY random() LIMIT {n_update})"
        ))
        .await;
    }

    if n_delete > 0 {
        db.execute(&format!(
            "DELETE FROM {source_table}
             WHERE id IN (SELECT id FROM {source_table} ORDER BY random() LIMIT {n_delete})"
        ))
        .await;
    }

    if n_insert > 0 {
        db.execute(&format!(
            "INSERT INTO {source_table} (grp, val)
             SELECT 'g' || (random() * {})::int, (random() * 1000)::int
             FROM generate_series(1, {n_insert})",
            NUM_GROUPS - 1,
        ))
        .await;
    }
}

/// Get the count of COMPLETED refresh entries for a given ST name.
async fn completed_count(db: &E2eDb, pgt_name: &str) -> i64 {
    db.query_scalar::<i64>(&format!(
        "SELECT COUNT(*) FROM pgtrickle.pgt_refresh_history h
         JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
         WHERE st.pgt_name = '{pgt_name}' AND h.status = 'COMPLETED'"
    ))
    .await
}

/// Wait for a leaf ST to have a new COMPLETED refresh entry.
/// Returns elapsed time in ms, or None on timeout.
async fn wait_for_leaf_refresh(
    db: &E2eDb,
    leaf_st: &str,
    before_count: i64,
    timeout: Duration,
) -> Option<f64> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

        let current = completed_count(db, leaf_st).await;
        if current > before_count {
            return Some(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

/// Collect per-ST timing from pgt_refresh_history since a given timestamp.
async fn collect_per_st_timing(
    db: &E2eDb,
    since_ts: &str,
    levels: &[Vec<String>],
) -> Vec<StTimingEntry> {
    // Build a name→level map
    let mut entries = Vec::new();
    for (level_idx, level_sts) in levels.iter().enumerate() {
        for st_name in level_sts {
            let row = db
                .query_scalar_opt::<String>(&format!(
                    "SELECT h.action || '|' || EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000.0
                     FROM pgtrickle.pgt_refresh_history h
                     JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
                     WHERE st.pgt_name = '{st_name}'
                       AND h.status = 'COMPLETED'
                       AND h.start_time >= '{since_ts}'::timestamptz
                     ORDER BY h.start_time DESC
                     LIMIT 1"
                ))
                .await;

            if let Some(row) = row {
                let parts: Vec<&str> = row.splitn(2, '|').collect();
                if parts.len() == 2 {
                    entries.push(StTimingEntry {
                        pgt_name: st_name.clone(),
                        level: (level_idx + 1) as u32,
                        refresh_ms: parts[1].parse::<f64>().unwrap_or(0.0),
                        action: parts[0].to_string(),
                    });
                }
            }
        }
    }
    entries
}

/// Compute theoretical latency from PLAN_DAG_PERFORMANCE.md formulas.
fn theoretical_latency_ms(
    mode: &str,
    levels: &[Vec<String>],
    avg_tr_ms: f64,
    scheduler_interval_ms: f64,
    concurrency: u32,
) -> f64 {
    if mode == "CALCULATED" {
        // L = I_s + N × T_r  where N = total number of STs
        let total_sts: usize = levels.iter().map(|l| l.len()).sum();
        scheduler_interval_ms + total_sts as f64 * avg_tr_ms
    } else {
        // PARALLEL: L = Σ ⌈W_l / C⌉ × max(I_p, T_r) for each level l
        let poll_ms = scheduler_interval_ms;
        levels
            .iter()
            .map(|level| {
                let batches = (level.len() as f64 / concurrency as f64).ceil();
                batches * poll_ms.max(avg_tr_ms)
            })
            .sum()
    }
}

/// Run a complete latency benchmark: enable scheduler, INSERT into source,
/// measure propagation time to leaf ST via pgt_refresh_history polling.
async fn measure_latency(
    db: &E2eDb,
    topo: &DagTopology,
    topology_name: &str,
    mode: &str,
    concurrency: u32,
) -> Vec<DagBenchResult> {
    let leaf = &topo.leaf_sts[0];
    let source = &topo.source_tables[0];
    let timeout = Duration::from_secs(LEAF_TIMEOUT_SECS);

    // Warm-up: wait for initial population, then run warm-up cycles
    db.wait_for_auto_refresh(leaf, Duration::from_secs(120))
        .await;

    for _ in 0..WARMUP_CYCLES {
        let before = completed_count(db, leaf).await;
        insert_delta(db, source, LATENCY_DELTA_ROWS).await;
        wait_for_leaf_refresh(db, leaf, before, timeout).await;
    }

    // Get timestamp for post-measurement timing queries
    let mut results = Vec::new();

    for cycle in 1..=CYCLES {
        let before = completed_count(db, leaf).await;
        let since_ts: String = db.query_scalar("SELECT now()::text").await;

        let start = Instant::now();
        insert_delta(db, source, LATENCY_DELTA_ROWS).await;

        let propagation_ms = wait_for_leaf_refresh(db, leaf, before, timeout)
            .await
            .unwrap_or_else(|| {
                eprintln!("[DAG_BENCH] WARN: timeout waiting for leaf '{leaf}' in cycle {cycle}");
                start.elapsed().as_secs_f64() * 1000.0
            });

        // Collect per-ST timing breakdown
        let breakdown = collect_per_st_timing(db, &since_ts, &topo.levels).await;
        let avg_tr_ms = if breakdown.is_empty() {
            50.0 // fallback estimate
        } else {
            breakdown.iter().map(|e| e.refresh_ms).sum::<f64>() / breakdown.len() as f64
        };

        let theoretical = theoretical_latency_ms(
            mode,
            &topo.levels,
            avg_tr_ms,
            SCHEDULER_INTERVAL_MS,
            concurrency,
        );

        let overhead_pct = if theoretical > 0.0 {
            (propagation_ms - theoretical) / theoretical * 100.0
        } else {
            0.0
        };

        let per_hop_avg = propagation_ms / topo.depth as f64;
        let throughput = LATENCY_DELTA_ROWS as f64 / (propagation_ms / 1000.0);

        // Machine-parseable output
        eprintln!(
            "[DAG_BENCH] topology={topology_name} mode={mode} sts={} depth={} width={} \
             cycle={cycle} actual_ms={propagation_ms:.1} theory_ms={theoretical:.1} \
             overhead_pct={overhead_pct:.1} per_hop_ms={per_hop_avg:.1}",
            topo.all_sts.len(),
            topo.depth,
            topo.max_width,
        );

        results.push(DagBenchResult {
            topology: topology_name.to_string(),
            refresh_mode: mode.to_string(),
            measurement: "latency".to_string(),
            dag_depth: topo.depth,
            dag_width: topo.max_width,
            total_sts: topo.all_sts.len() as u32,
            delta_rows: LATENCY_DELTA_ROWS,
            cycle,
            propagation_ms,
            per_hop_avg_ms: per_hop_avg,
            throughput_rows_per_sec: throughput,
            theoretical_ms: theoretical,
            overhead_pct,
            per_st_breakdown: breakdown,
        });
    }

    results
}

/// Run a throughput benchmark: disable scheduler, manual topological refresh,
/// measure wall-clock time for full DAG refresh.
async fn measure_throughput(
    db: &E2eDb,
    topo: &DagTopology,
    topology_name: &str,
    delta_sizes: &[u32],
) -> Vec<DagBenchResult> {
    let source = &topo.source_tables[0];
    let mut results = Vec::new();

    for &delta_size in delta_sizes {
        for cycle in 1..=CYCLES {
            // Apply mixed DML
            apply_dml_mix(db, source, delta_size).await;

            let since_ts: String = db.query_scalar("SELECT now()::text").await;

            // Refresh all STs in topological order
            let start = Instant::now();
            for st in &topo.all_sts {
                db.refresh_st_with_retry(st).await;
            }
            let total_ms = start.elapsed().as_secs_f64() * 1000.0;

            let breakdown = collect_per_st_timing(db, &since_ts, &topo.levels).await;
            let avg_tr_ms = if breakdown.is_empty() {
                50.0
            } else {
                breakdown.iter().map(|e| e.refresh_ms).sum::<f64>() / breakdown.len() as f64
            };

            let throughput = delta_size as f64 / (total_ms / 1000.0);
            let per_hop_avg = total_ms / topo.depth as f64;

            // For throughput mode, theoretical = N × T_r (no scheduler overhead)
            let theoretical = topo.all_sts.len() as f64 * avg_tr_ms;
            let overhead_pct = if theoretical > 0.0 {
                (total_ms - theoretical) / theoretical * 100.0
            } else {
                0.0
            };

            eprintln!(
                "[DAG_BENCH] topology={topology_name} mode=THROUGHPUT sts={} depth={} \
                 delta={delta_size} cycle={cycle} total_ms={total_ms:.1} \
                 throughput={throughput:.0} per_hop_ms={per_hop_avg:.1}",
                topo.all_sts.len(),
                topo.depth,
            );

            results.push(DagBenchResult {
                topology: topology_name.to_string(),
                refresh_mode: "MANUAL".to_string(),
                measurement: "throughput".to_string(),
                dag_depth: topo.depth,
                dag_width: topo.max_width,
                total_sts: topo.all_sts.len() as u32,
                delta_rows: delta_size,
                cycle,
                propagation_ms: total_ms,
                per_hop_avg_ms: per_hop_avg,
                throughput_rows_per_sec: throughput,
                theoretical_ms: theoretical,
                overhead_pct,
                per_st_breakdown: breakdown,
            });
        }
    }

    results
}

// ── Reporting ──────────────────────────────────────────────────────────

/// Print ASCII summary table for DAG benchmark results.
fn print_dag_results_table(results: &[DagBenchResult]) {
    if results.is_empty() {
        return;
    }

    println!();
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!(
        "║                         pg_trickle DAG Topology Benchmark Results                                 ║"
    );
    println!(
        "╠═══════════════╤═══════════════╤══════╤═══════╤═══════╤════════════╤════════════╤═══════════════════╣"
    );
    println!(
        "║ Topology      │ Mode          │ STs  │ Depth │ Width │ Actual ms  │ Theory ms  │ Overhead          ║"
    );
    println!(
        "╠═══════════════╪═══════════════╪══════╪═══════╪═══════╪════════════╪════════════╪═══════════════════╣"
    );

    for r in results {
        println!(
            "║ {:13} │ {:13} │ {:>4} │ {:>5} │ {:>5} │ {:>10.1} │ {:>10.1} │ {:>+7.1}%            ║",
            r.topology,
            r.refresh_mode,
            r.total_sts,
            r.dag_depth,
            r.dag_width,
            r.propagation_ms,
            r.theoretical_ms,
            r.overhead_pct,
        );
    }

    println!(
        "╚═══════════════╧═══════════════╧══════╧═══════╧═══════╧════════════╧════════════╧═══════════════════╝"
    );
    println!();

    // Print per-level breakdown for first result with per_st_breakdown data
    print_per_level_breakdown(results);
}

/// Print per-level timing breakdown.
fn print_per_level_breakdown(results: &[DagBenchResult]) {
    // Group by (topology, mode), show breakdown for first cycle
    let mut seen = std::collections::HashSet::new();

    for r in results {
        let key = format!("{}_{}", r.topology, r.refresh_mode);
        if seen.contains(&key) || r.per_st_breakdown.is_empty() {
            continue;
        }
        seen.insert(key);

        println!(
            "  Per-Level Breakdown ({} D={}, {}):",
            r.topology, r.dag_depth, r.refresh_mode,
        );

        // Group breakdown entries by level
        let mut by_level: std::collections::BTreeMap<u32, Vec<&StTimingEntry>> =
            std::collections::BTreeMap::new();
        for entry in &r.per_st_breakdown {
            by_level.entry(entry.level).or_default().push(entry);
        }

        let mut total_refresh = 0.0;
        for (level, entries) in &by_level {
            let avg_ms = entries.iter().map(|e| e.refresh_ms).sum::<f64>() / entries.len() as f64;
            total_refresh += avg_ms * entries.len() as f64;
            let names: Vec<&str> = entries.iter().map(|e| e.pgt_name.as_str()).collect();
            println!(
                "  Level {:>2}: avg {:>7.1}ms  [{}]",
                level,
                avg_ms,
                names.join(", ")
            );
        }

        let scheduler_overhead = r.propagation_ms - total_refresh;
        println!(
            "  Total:     {:>7.1}ms  (scheduler overhead: {:.1}ms)",
            total_refresh, scheduler_overhead,
        );
        println!();
    }
}

/// Print summary table with averages across cycles.
fn print_dag_summary(results: &[DagBenchResult]) {
    // Group by (topology, mode, measurement, delta_rows)
    let mut groups: std::collections::BTreeMap<
        (String, String, String, u32),
        Vec<&DagBenchResult>,
    > = std::collections::BTreeMap::new();

    for r in results {
        let key = (
            r.topology.clone(),
            r.refresh_mode.clone(),
            r.measurement.clone(),
            r.delta_rows,
        );
        groups.entry(key).or_default().push(r);
    }

    println!(
        "┌─────────────────────────────────────────────────────────────────────────────────────────────────────┐"
    );
    println!(
        "│                                  Summary (avg across cycles)                                       │"
    );
    println!(
        "├───────────────┬───────────────┬───────────┬─────────┬──────────┬──────────┬──────────┬─────────────┤"
    );
    println!(
        "│ Topology      │ Mode          │ Measure   │ Delta   │ Avg ms   │ Med ms   │ P95 ms   │ Avg r/s     │"
    );
    println!(
        "├───────────────┼───────────────┼───────────┼─────────┼──────────┼──────────┼──────────┼─────────────┤"
    );

    for ((topo, mode, meas, delta), entries) in &groups {
        let times: Vec<f64> = entries.iter().map(|r| r.propagation_ms).collect();
        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let median = {
            let mut sorted = times.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if sorted.len().is_multiple_of(2) {
                (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
            } else {
                sorted[sorted.len() / 2]
            }
        };
        let p95 = {
            let mut sorted = times.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = ((sorted.len() as f64 * 0.95) - 1.0).max(0.0) as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        let avg_throughput = entries
            .iter()
            .map(|r| r.throughput_rows_per_sec)
            .sum::<f64>()
            / entries.len() as f64;

        println!(
            "│ {:13} │ {:13} │ {:9} │ {:>7} │ {:>8.1} │ {:>8.1} │ {:>8.1} │ {:>11.0} │",
            topo, mode, meas, delta, avg, median, p95, avg_throughput,
        );
    }

    println!(
        "└───────────────┴───────────────┴───────────┴─────────┴──────────┴──────────┴──────────┴─────────────┘"
    );
    println!();
}

/// Write results as JSON for cross-run comparison.
fn write_dag_results_json(results: &[DagBenchResult]) {
    use std::io::Write;

    let out_dir = std::env::var("PGS_DAG_BENCH_JSON_DIR")
        .unwrap_or_else(|_| "target/dag_bench_results".to_string());
    let _ = std::fs::create_dir_all(&out_dir);

    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H%M%S");
    let path = format!("{out_dir}/{timestamp}.json");

    let mut entries = Vec::new();
    for r in results {
        let breakdown_json: Vec<String> = r
            .per_st_breakdown
            .iter()
            .map(|e| {
                format!(
                    r#"{{"pgt_name":"{}","level":{},"refresh_ms":{:.3},"action":"{}"}}"#,
                    e.pgt_name, e.level, e.refresh_ms, e.action,
                )
            })
            .collect();

        entries.push(format!(
            r#"  {{"topology":"{}","refresh_mode":"{}","measurement":"{}","dag_depth":{},"dag_width":{},"total_sts":{},"delta_rows":{},"cycle":{},"propagation_ms":{:.3},"per_hop_avg_ms":{:.3},"throughput_rows_per_sec":{:.1},"theoretical_ms":{:.3},"overhead_pct":{:.1},"per_st_breakdown":[{}]}}"#,
            r.topology,
            r.refresh_mode,
            r.measurement,
            r.dag_depth,
            r.dag_width,
            r.total_sts,
            r.delta_rows,
            r.cycle,
            r.propagation_ms,
            r.per_hop_avg_ms,
            r.throughput_rows_per_sec,
            r.theoretical_ms,
            r.overhead_pct,
            breakdown_json.join(","),
        ));
    }

    let json = format!("[\n{}\n]\n", entries.join(",\n"));
    match std::fs::File::create(&path) {
        Ok(mut f) => {
            let _ = f.write_all(json.as_bytes());
            eprintln!("[DAG_BENCH_JSON] Results written to {path}");
        }
        Err(e) => {
            eprintln!("[DAG_BENCH_JSON] WARN: Could not write {path}: {e}");
        }
    }
}

/// Report all results: ASCII tables, summary, and JSON.
fn report_results(results: &[DagBenchResult]) {
    print_dag_results_table(results);
    print_dag_summary(results);
    write_dag_results_json(results);
}

// ═══════════════════════════════════════════════════════════════════════
// Latency Benchmarks — Linear Chain, CALCULATED mode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn bench_latency_linear_5_calc() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_latency_scheduler(&db, "CALCULATED", 1).await;
    let topo = build_linear_chain(&db, 5).await;
    let results = measure_latency(&db, &topo, "linear_chain", "CALCULATED", 1).await;
    report_results(&results);
}

#[tokio::test]
#[ignore]
async fn bench_latency_linear_10_calc() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_latency_scheduler(&db, "CALCULATED", 1).await;
    let topo = build_linear_chain(&db, 10).await;
    let results = measure_latency(&db, &topo, "linear_chain", "CALCULATED", 1).await;
    report_results(&results);
}

#[tokio::test]
#[ignore]
async fn bench_latency_linear_20_calc() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_latency_scheduler(&db, "CALCULATED", 1).await;
    let topo = build_linear_chain(&db, 20).await;
    let results = measure_latency(&db, &topo, "linear_chain", "CALCULATED", 1).await;
    report_results(&results);
}

#[tokio::test]
#[ignore]
async fn bench_latency_linear_10_par4() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_latency_scheduler(&db, "PARALLEL_C4", 4).await;
    let topo = build_linear_chain(&db, 10).await;
    let results = measure_latency(&db, &topo, "linear_chain", "PARALLEL_C4", 4).await;
    report_results(&results);
}

// ═══════════════════════════════════════════════════════════════════════
// Throughput Benchmarks — Linear Chain
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn bench_throughput_linear_5() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    // Disable scheduler for throughput (manual refresh)
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = off")
        .await;
    db.reload_config_and_wait().await;
    let topo = build_linear_chain(&db, 5).await;
    // Initial population
    for st in &topo.all_sts {
        db.refresh_st_with_retry(st).await;
    }
    let results = measure_throughput(&db, &topo, "linear_chain", THROUGHPUT_DELTA_SIZES).await;
    report_results(&results);
}

#[tokio::test]
#[ignore]
async fn bench_throughput_linear_10() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = off")
        .await;
    db.reload_config_and_wait().await;
    let topo = build_linear_chain(&db, 10).await;
    for st in &topo.all_sts {
        db.refresh_st_with_retry(st).await;
    }
    let results = measure_throughput(&db, &topo, "linear_chain", THROUGHPUT_DELTA_SIZES).await;
    report_results(&results);
}

#[tokio::test]
#[ignore]
async fn bench_throughput_linear_20() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    db.execute("ALTER SYSTEM SET pg_trickle.enabled = off")
        .await;
    db.reload_config_and_wait().await;
    let topo = build_linear_chain(&db, 20).await;
    for st in &topo.all_sts {
        db.refresh_st_with_retry(st).await;
    }
    let results = measure_throughput(&db, &topo, "linear_chain", THROUGHPUT_DELTA_SIZES).await;
    report_results(&results);
}
