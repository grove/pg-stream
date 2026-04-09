//! G17-SOAK: Long-running stability soak test for pg_trickle.
//!
//! Validates that the extension operates correctly under sustained mixed DML
//! workload over an extended period. Checks:
//!   - Zero background worker crashes
//!   - Zero stream tables in zombie/ERROR state
//!   - Stable memory usage (RSS growth < 20% over the test duration)
//!   - Correctness: stream table contents match defining queries
//!
//! These tests are `#[ignore]`d to skip in normal `cargo test` runs.
//! Run manually or via CI:
//!
//! ```bash
//! just test-soak          # Default: 10 minutes (configurable via SOAK_DURATION_SECS)
//! just test-soak-short    # Quick: 2 minutes
//! ```
//!
//! Environment variables:
//!   SOAK_DURATION_SECS  — Total soak duration in seconds (default: 600 = 10 min)
//!   SOAK_CYCLE_MS       — Milliseconds between DML batches (default: 500)
//!   SOAK_BATCH_SIZE     — Rows per INSERT/UPDATE/DELETE batch (default: 50)
//!   SOAK_NUM_SOURCES    — Number of source tables (default: 5)
//!
//! Prerequisites: E2E Docker image (`just build-e2e-image`)

mod e2e;

use e2e::E2eDb;
use std::time::{Duration, Instant};

// ── Configuration ──────────────────────────────────────────────────────

fn soak_duration_secs() -> u64 {
    std::env::var("SOAK_DURATION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600)
}

fn cycle_delay_ms() -> u64 {
    std::env::var("SOAK_CYCLE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

fn batch_size() -> usize {
    std::env::var("SOAK_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

fn num_sources() -> usize {
    std::env::var("SOAK_NUM_SOURCES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

// ── Schema Setup ───────────────────────────────────────────────────────

/// Create N source tables with varied schemas to exercise different DVM paths.
async fn create_source_tables(db: &E2eDb, n: usize) {
    for i in 1..=n {
        db.execute(&format!(
            "CREATE TABLE source_{i} (
                id SERIAL PRIMARY KEY,
                category INT NOT NULL DEFAULT (random() * 10)::int,
                value NUMERIC(12,2) NOT NULL DEFAULT (random() * 1000)::numeric(12,2),
                label TEXT NOT NULL DEFAULT 'row_' || gen_random_uuid()::text,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )"
        ))
        .await;

        // Seed with initial data
        db.execute(&format!(
            "INSERT INTO source_{i} (category, value, label)
             SELECT (random() * 10)::int,
                    (random() * 1000)::numeric(12,2),
                    'seed_' || g
             FROM generate_series(1, 200) g"
        ))
        .await;
    }
}

/// Create stream tables covering different DVM operator paths.
async fn create_stream_tables(db: &E2eDb, n: usize) {
    // ST1: Simple aggregation (SUM/COUNT)
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'soak_agg',
            'SELECT category, SUM(value) AS total, COUNT(*) AS cnt
             FROM source_1 GROUP BY category',
            schedule => '1s',
            refresh_mode => 'DIFFERENTIAL'
        )",
    )
    .await;

    // ST2: Two-table join
    if n >= 2 {
        db.execute(
            "SELECT pgtrickle.create_stream_table(
                'soak_join',
                'SELECT s1.id, s1.category, s1.value, s2.label AS label_2
                 FROM source_1 s1
                 JOIN source_2 s2 ON s1.category = s2.category',
                schedule => '1s',
                refresh_mode => 'DIFFERENTIAL'
            )",
        )
        .await;
    }

    // ST3: UNION ALL of multiple sources
    if n >= 3 {
        db.execute(
            "SELECT pgtrickle.create_stream_table(
                'soak_union',
                'SELECT id, category, value FROM source_1
                 UNION ALL
                 SELECT id, category, value FROM source_2
                 UNION ALL
                 SELECT id, category, value FROM source_3',
                schedule => '1s',
                refresh_mode => 'DIFFERENTIAL'
            )",
        )
        .await;
    }

    // ST4: LEFT JOIN with aggregate
    if n >= 4 {
        db.execute(
            "SELECT pgtrickle.create_stream_table(
                'soak_left_agg',
                'SELECT s1.category,
                        COUNT(s2.id) AS matched_count,
                        COALESCE(SUM(s2.value), 0) AS matched_total
                 FROM source_1 s1
                 LEFT JOIN source_4 s2 ON s1.category = s2.category
                 GROUP BY s1.category',
                schedule => '1s',
                refresh_mode => 'DIFFERENTIAL'
            )",
        )
        .await;
    }

    // ST5: Filter + projection (simple scan path)
    if n >= 5 {
        db.execute(
            "SELECT pgtrickle.create_stream_table(
                'soak_filter',
                'SELECT id, category, value, label
                 FROM source_5
                 WHERE category >= 5',
                schedule => '1s',
                refresh_mode => 'DIFFERENTIAL'
            )",
        )
        .await;
    }
}

// ── DML Operations ─────────────────────────────────────────────────────

/// Apply a mixed DML batch to a random source table.
async fn apply_dml_batch(db: &E2eDb, source_idx: usize, batch: usize) {
    let table = format!("source_{source_idx}");

    // INSERT batch
    db.execute(&format!(
        "INSERT INTO {table} (category, value, label)
         SELECT (random() * 10)::int,
                (random() * 1000)::numeric(12,2),
                'batch_{batch}_' || g
         FROM generate_series(1, {bs}) g",
        bs = batch
    ))
    .await;

    // UPDATE ~30% of the batch size
    let update_count = (batch as f64 * 0.3).max(1.0) as usize;
    db.execute(&format!(
        "UPDATE {table}
         SET value = value + (random() * 100)::numeric(12,2),
             label = label || '_upd'
         WHERE id IN (
             SELECT id FROM {table} ORDER BY random() LIMIT {update_count}
         )"
    ))
    .await;

    // DELETE ~10% of the batch size
    let delete_count = (batch as f64 * 0.1).max(1.0) as usize;
    db.execute(&format!(
        "DELETE FROM {table}
         WHERE id IN (
             SELECT id FROM {table} ORDER BY random() LIMIT {delete_count}
         )"
    ))
    .await;
}

// ── Health Checks ──────────────────────────────────────────────────────

/// Check that no stream tables are in ERROR or SUSPENDED state.
async fn check_no_error_states(db: &E2eDb) -> Result<(), String> {
    let error_count: i64 = db
        .query_scalar(
            "SELECT COUNT(*) FROM pgtrickle.pgt_stream_tables
             WHERE status IN ('ERROR', 'SUSPENDED')",
        )
        .await;

    if error_count > 0 {
        let errors: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT pgt_name::text, status::text, last_error_message
             FROM pgtrickle.pgt_stream_tables
             WHERE status IN ('ERROR', 'SUSPENDED')",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap_or_default();

        let details: Vec<String> = errors
            .iter()
            .map(|(name, status, msg)| {
                format!(
                    "  {} ({}): {}",
                    name,
                    status,
                    msg.as_deref().unwrap_or("(no message)")
                )
            })
            .collect();

        return Err(format!(
            "{} stream table(s) in error state:\n{}",
            error_count,
            details.join("\n")
        ));
    }
    Ok(())
}

/// Check that the background worker is running (not crashed).
async fn check_worker_alive(db: &E2eDb) -> Result<(), String> {
    let alive: bool = db
        .query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                WHERE backend_type = 'pg_trickle scheduler'
            )",
        )
        .await;

    if !alive {
        return Err("pg_trickle scheduler background worker not found in pg_stat_activity".into());
    }
    Ok(())
}

/// Get the current RSS of the PostgreSQL backend (in KB).
/// Returns None if the platform doesn't support this.
async fn get_rss_kb(db: &E2eDb) -> Option<i64> {
    // Use /proc/self/status on Linux (Docker container)
    db.try_execute(
        "CREATE OR REPLACE FUNCTION pg_temp.get_rss_kb()
         RETURNS BIGINT LANGUAGE plpgsql AS $$
         DECLARE
             line TEXT;
             rss_kb BIGINT;
         BEGIN
             FOR line IN SELECT pg_read_file('/proc/self/status') LOOP
                 IF line LIKE 'VmRSS:%' THEN
                     rss_kb := regexp_replace(line, '[^0-9]', '', 'g')::bigint;
                     RETURN rss_kb;
                 END IF;
             END LOOP;
             RETURN NULL;
         EXCEPTION WHEN OTHERS THEN
             RETURN NULL;
         END; $$",
    )
    .await
    .ok()?;

    let rss: Option<i64> = sqlx::query_scalar("SELECT pg_temp.get_rss_kb()")
        .fetch_one(&db.pool)
        .await
        .ok()?;

    rss
}

/// Verify stream table correctness by comparing contents to defining query.
async fn verify_correctness(db: &E2eDb, st_name: &str) -> Result<(), String> {
    // Refresh first to ensure we're comparing against latest data
    db.try_execute(&format!(
        "SELECT pgtrickle.refresh_stream_table('{st_name}')"
    ))
    .await
    .map_err(|e| format!("refresh failed for {st_name}: {e}"))?;

    // Get defining query
    let defining_query: String = db
        .query_scalar(&format!(
            "SELECT defining_query FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name = '{st_name}'"
        ))
        .await;

    // Get user-visible columns
    let cols: String = db
        .query_scalar(&format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position)
             FROM information_schema.columns
             WHERE table_name = '{st_name}'
               AND left(column_name, 6) <> '__pgt_'"
        ))
        .await;

    // Multiset equality check
    let matches: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS (
                (SELECT {cols} FROM public.{st_name} EXCEPT ALL ({defining_query}))
                UNION ALL
                (({defining_query}) EXCEPT ALL SELECT {cols} FROM public.{st_name})
            )"
        ))
        .await;

    if !matches {
        let st_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM public.{st_name}"))
            .await;
        let q_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM ({defining_query}) _q"))
            .await;
        return Err(format!(
            "CORRECTNESS VIOLATION: {st_name} has {st_count} rows, query returns {q_count}"
        ));
    }
    Ok(())
}

// ── Main Soak Test ─────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_soak_stability() {
    let duration = Duration::from_secs(soak_duration_secs());
    let cycle_delay = Duration::from_millis(cycle_delay_ms());
    let batch = batch_size();
    let n_sources = num_sources();

    println!("\n══════════════════════════════════════════════════════════");
    println!("  G17-SOAK: Long-Running Stability Soak Test");
    println!(
        "  Duration: {}s, Cycle: {}ms, Batch: {}, Sources: {}",
        duration.as_secs(),
        cycle_delay.as_millis(),
        batch,
        n_sources,
    );
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new().await.with_extension().await;

    // Enable verbose error logging for diagnostics
    db.execute("ALTER SYSTEM SET log_min_messages = 'warning'")
        .await;
    db.execute("ALTER SYSTEM SET log_error_verbosity = 'verbose'")
        .await;
    // Force ALL refreshes to use MERGE (disable PH-D1) to isolate the error source
    db.execute("ALTER SYSTEM SET pg_trickle.merge_strategy = 'merge'")
        .await;
    db.execute("SELECT pg_reload_conf()").await;

    // ── Setup ──────────────────────────────────────────────────────────
    println!("  Setting up source tables and stream tables...");
    create_source_tables(&db, n_sources).await;
    create_stream_tables(&db, n_sources).await;

    // Initial refresh to populate all STs
    let st_names = vec![
        "soak_agg",
        "soak_join",
        "soak_union",
        "soak_left_agg",
        "soak_filter",
    ];
    let active_sts: Vec<&str> = st_names.into_iter().take(n_sources).collect();

    for st in &active_sts {
        db.execute(&format!("SELECT pgtrickle.refresh_stream_table('{st}')"))
            .await;
    }
    println!(
        "  Initial refresh complete for {} stream tables",
        active_sts.len()
    );

    // Record initial RSS
    let initial_rss = get_rss_kb(&db).await;
    if let Some(rss) = initial_rss {
        println!("  Initial RSS: {} KB", rss);
    }

    // ── Soak Loop ──────────────────────────────────────────────────────
    let start = Instant::now();
    let mut cycle = 0u64;
    let mut total_dml_ops = 0u64;
    let mut total_refreshes = 0u64;
    let mut health_check_failures: Vec<String> = Vec::new();
    let check_interval = Duration::from_secs(30);
    let mut last_check = Instant::now();
    let correctness_interval = Duration::from_secs(120);
    let mut last_correctness = Instant::now();

    println!("  Starting soak loop...\n");

    while start.elapsed() < duration {
        cycle += 1;

        // Pick a source table (round-robin)
        let source_idx = ((cycle as usize - 1) % n_sources) + 1;

        // Apply mixed DML
        apply_dml_batch(&db, source_idx, batch).await;
        total_dml_ops += 3; // INSERT + UPDATE + DELETE

        // Manual refresh on a rotating stream table
        let st_idx = (cycle as usize - 1) % active_sts.len();
        let st = active_sts[st_idx];
        if let Err(e) = db
            .try_execute(&format!("SELECT pgtrickle.refresh_stream_table('{st}')"))
            .await
        {
            health_check_failures.push(format!("Cycle {cycle}: refresh failed for {st}: {e}"));
        } else {
            total_refreshes += 1;
        }

        // Periodic health checks (every 30s)
        if last_check.elapsed() >= check_interval {
            let elapsed = start.elapsed().as_secs();
            print!("  [{elapsed}s] cycle {cycle}: ");

            // Check error states
            if let Err(e) = check_no_error_states(&db).await {
                println!("FAIL — {e}");
                // Query catalog for detailed error info + pgt_id
                let err_detail: Vec<(i64, String, String)> = sqlx::query_as(
                    "SELECT pgt_id, pgt_name::text, last_error_message \
                     FROM pgtrickle.pgt_stream_tables \
                     WHERE status = 'ERROR' AND last_error_message IS NOT NULL",
                )
                .fetch_all(&db.pool)
                .await
                .unwrap_or_default();
                for (pgt_id, name, _msg) in &err_detail {
                    // Read strategy marker file written by the extension
                    let strat: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT pg_read_file('/tmp/pgt_strat_{}.txt', true)",
                        pgt_id
                    ))
                    .fetch_one(&db.pool)
                    .await
                    .ok()
                    .flatten();
                    println!(
                        "  STRATEGY for {} (pgt_id={}): {}",
                        name,
                        pgt_id,
                        strat.as_deref().unwrap_or("(none)")
                    );
                    // Read delta diagnostic
                    let diag: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT pg_read_file('/tmp/pgt_delta_diag_{}.txt', true)",
                        pgt_id
                    ))
                    .fetch_one(&db.pool)
                    .await
                    .ok()
                    .flatten();
                    if let Some(d) = &diag {
                        println!("  DELTA DIAG: {}", d);
                    }
                    // Read pre-delete count
                    let predel: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT pg_read_file('/tmp/pgt_predel_{}.txt', true)",
                        pgt_id
                    ))
                    .fetch_one(&db.pool)
                    .await
                    .ok()
                    .flatten();
                    if let Some(p) = &predel {
                        println!("  PRE-DELETE: {}", p);
                    }
                    // Read step marker
                    let step: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT pg_read_file('/tmp/pgt_step_{}.txt', true)",
                        pgt_id
                    ))
                    .fetch_one(&db.pool)
                    .await
                    .ok()
                    .flatten();
                    if let Some(s) = &step {
                        println!("  LAST STEP: {}", s);
                    }
                    // Read the full INSERT SQL
                    let insert_sql: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT pg_read_file('/tmp/pgt_insert_sql_{}.txt', true)",
                        pgt_id
                    ))
                    .fetch_one(&db.pool)
                    .await
                    .ok()
                    .flatten();
                    if let Some(sql) = &insert_sql {
                        println!("  INSERT SQL: {}", sql);
                    }
                }
                health_check_failures.push(format!("[{elapsed}s] {e}"));
            } else {
                print!("states OK, ");
            }

            // Check worker alive
            if let Err(e) = check_worker_alive(&db).await {
                println!("FAIL — {e}");
                health_check_failures.push(format!("[{elapsed}s] {e}"));
            } else {
                print!("worker OK");
            }

            // RSS check
            if let Some(rss) = get_rss_kb(&db).await {
                print!(", RSS={rss}KB");
                if let Some(initial) = initial_rss {
                    let growth_pct =
                        ((rss as f64 - initial as f64) / initial as f64 * 100.0).max(0.0);
                    print!(" (+{growth_pct:.1}%)");
                }
            }

            println!(", refreshes={total_refreshes}, dml_ops={total_dml_ops}");
            last_check = Instant::now();
        }

        // Periodic correctness checks (every 2 min)
        if last_correctness.elapsed() >= correctness_interval {
            let elapsed = start.elapsed().as_secs();
            println!("  [{elapsed}s] Running correctness check...");
            for st in &active_sts {
                if let Err(e) = verify_correctness(&db, st).await {
                    println!("  [{elapsed}s] CORRECTNESS FAIL: {e}");
                    health_check_failures.push(format!("[{elapsed}s] {e}"));
                }
            }
            last_correctness = Instant::now();
        }

        tokio::time::sleep(cycle_delay).await;
    }

    // ── Final Checks ───────────────────────────────────────────────────
    let total_elapsed = start.elapsed();
    println!(
        "\n  Soak loop complete: {cycle} cycles in {:.1}s",
        total_elapsed.as_secs_f64()
    );
    println!("  Total DML operations: {total_dml_ops}");
    println!("  Total refreshes: {total_refreshes}");

    // Final correctness check
    println!("\n  Final correctness verification...");
    for st in &active_sts {
        match verify_correctness(&db, st).await {
            Ok(()) => println!("    {st}: ✓"),
            Err(e) => {
                println!("    {st}: FAIL — {e}");
                health_check_failures.push(format!("[final] {e}"));
            }
        }
    }

    // Final RSS check
    if let (Some(initial), Some(final_rss)) = (initial_rss, get_rss_kb(&db).await) {
        let growth_pct = ((final_rss as f64 - initial as f64) / initial as f64 * 100.0).max(0.0);
        println!("\n  RSS: initial={initial}KB, final={final_rss}KB, growth={growth_pct:.1}%");
        assert!(
            growth_pct < 20.0,
            "RSS grew by {growth_pct:.1}% (threshold: 20%): initial={initial}KB, final={final_rss}KB"
        );
    }

    // Final error state check
    check_no_error_states(&db)
        .await
        .expect("Stream tables in error state at soak end");
    check_worker_alive(&db)
        .await
        .expect("Background worker not alive at soak end");

    // Report
    if !health_check_failures.is_empty() {
        println!("\n  ⚠ Health check failures during soak:");
        for f in &health_check_failures {
            println!("    - {f}");
        }
        panic!(
            "Soak test had {} health check failure(s)",
            health_check_failures.len()
        );
    }

    println!("\n  ✓ Soak test PASSED: {cycle} cycles, {total_refreshes} refreshes, zero failures");
}
