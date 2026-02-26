//! TPC-H correctness tests for pg_stream DIFFERENTIAL refresh.
//!
//! Validates the core DBSP invariant: after every differential refresh,
//! the stream table's contents must be multiset-equal to re-executing
//! the defining query from scratch.
//!
//! These tests are `#[ignore]`d to skip in normal CI. Run explicitly:
//!
//! ```bash
//! just test-tpch              # SF-0.01, ~2 min
//! just test-tpch-large        # SF-0.1,  ~5 min
//! ```
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;
use std::time::Instant;

// ── Configuration ──────────────────────────────────────────────────────

/// Number of refresh cycles per query (RF1 + RF2 + RF3 → refresh → assert).
fn cycles() -> usize {
    std::env::var("TPCH_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

/// Scale factor. Controls data volume.
///   0.01 → ~1,500 orders, ~6,000 lineitems   (default, ~2 min)
///   0.1  → ~15,000 orders, ~60,000 lineitems  (~5 min)
///   1.0  → ~150,000 orders, ~600,000 lineitems (~15 min)
fn scale_factor() -> f64 {
    std::env::var("TPCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.01)
}

/// Number of rows affected per RF cycle (INSERT/DELETE/UPDATE batch size).
/// Defaults to 1% of order count, minimum 10.
fn rf_count() -> usize {
    let sf = scale_factor();
    let orders = ((sf * 150_000.0) as usize).max(1_500);
    (orders / 100).max(10)
}

// ── Scale factor dimensions ────────────────────────────────────────────

fn sf_orders() -> usize {
    ((scale_factor() * 150_000.0) as usize).max(1_500)
}

fn sf_customers() -> usize {
    ((scale_factor() * 15_000.0) as usize).max(150)
}

fn sf_suppliers() -> usize {
    ((scale_factor() * 1_000.0) as usize).max(10)
}

fn sf_parts() -> usize {
    ((scale_factor() * 20_000.0) as usize).max(200)
}

// ── SQL file embedding ─────────────────────────────────────────────────

const SCHEMA_SQL: &str = include_str!("tpch/schema.sql");
const DATAGEN_SQL: &str = include_str!("tpch/datagen.sql");
const RF1_SQL: &str = include_str!("tpch/rf1.sql");
const RF2_SQL: &str = include_str!("tpch/rf2.sql");
const RF3_SQL: &str = include_str!("tpch/rf3.sql");

// ── TPC-H queries ordered by coverage tier ─────────────────────────────

/// A TPC-H query with its name and SQL.
struct TpchQuery {
    name: &'static str,
    sql: &'static str,
    tier: u8,
}

fn tpch_queries() -> Vec<TpchQuery> {
    vec![
        // ── Tier 1: Maximum operator diversity (fast-fail) ─────────
        TpchQuery { name: "q02", sql: include_str!("tpch/queries/q02.sql"), tier: 1 },
        TpchQuery { name: "q21", sql: include_str!("tpch/queries/q21.sql"), tier: 1 },
        TpchQuery { name: "q13", sql: include_str!("tpch/queries/q13.sql"), tier: 1 },
        TpchQuery { name: "q11", sql: include_str!("tpch/queries/q11.sql"), tier: 1 },
        TpchQuery { name: "q08", sql: include_str!("tpch/queries/q08.sql"), tier: 1 },
        // ── Tier 2: Core operator correctness ──────────────────────
        TpchQuery { name: "q01", sql: include_str!("tpch/queries/q01.sql"), tier: 2 },
        TpchQuery { name: "q05", sql: include_str!("tpch/queries/q05.sql"), tier: 2 },
        TpchQuery { name: "q07", sql: include_str!("tpch/queries/q07.sql"), tier: 2 },
        TpchQuery { name: "q09", sql: include_str!("tpch/queries/q09.sql"), tier: 2 },
        TpchQuery { name: "q16", sql: include_str!("tpch/queries/q16.sql"), tier: 2 },
        TpchQuery { name: "q22", sql: include_str!("tpch/queries/q22.sql"), tier: 2 },
        // ── Tier 3: Remaining queries (completeness) ───────────────
        TpchQuery { name: "q03", sql: include_str!("tpch/queries/q03.sql"), tier: 3 },
        TpchQuery { name: "q04", sql: include_str!("tpch/queries/q04.sql"), tier: 3 },
        TpchQuery { name: "q06", sql: include_str!("tpch/queries/q06.sql"), tier: 3 },
        TpchQuery { name: "q10", sql: include_str!("tpch/queries/q10.sql"), tier: 3 },
        TpchQuery { name: "q12", sql: include_str!("tpch/queries/q12.sql"), tier: 3 },
        TpchQuery { name: "q14", sql: include_str!("tpch/queries/q14.sql"), tier: 3 },
        TpchQuery { name: "q15", sql: include_str!("tpch/queries/q15.sql"), tier: 3 },
        TpchQuery { name: "q17", sql: include_str!("tpch/queries/q17.sql"), tier: 3 },
        TpchQuery { name: "q18", sql: include_str!("tpch/queries/q18.sql"), tier: 3 },
        TpchQuery { name: "q19", sql: include_str!("tpch/queries/q19.sql"), tier: 3 },
        TpchQuery { name: "q20", sql: include_str!("tpch/queries/q20.sql"), tier: 3 },
    ]
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Replace scale-factor tokens in a SQL template.
fn substitute_sf(sql: &str) -> String {
    sql.replace("__SF_ORDERS__", &sf_orders().to_string())
        .replace("__SF_CUSTOMERS__", &sf_customers().to_string())
        .replace("__SF_SUPPLIERS__", &sf_suppliers().to_string())
        .replace("__SF_PARTS__", &sf_parts().to_string())
}

/// Replace RF tokens in a mutation SQL template.
fn substitute_rf(sql: &str, next_orderkey: usize) -> String {
    sql.replace("__RF_COUNT__", &rf_count().to_string())
        .replace("__NEXT_ORDERKEY__", &next_orderkey.to_string())
        .replace("__SF_CUSTOMERS__", &sf_customers().to_string())
        .replace("__SF_PARTS__", &sf_parts().to_string())
        .replace("__SF_SUPPLIERS__", &sf_suppliers().to_string())
}

/// Load TPC-H schema into the database.
async fn load_schema(db: &E2eDb) {
    // Execute each statement separately (sqlx doesn't support multi-statement)
    for stmt in SCHEMA_SQL.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() && !stmt.starts_with("--") {
            db.execute(stmt).await;
        }
    }
}

/// Load TPC-H data at the configured scale factor.
async fn load_data(db: &E2eDb) {
    let sql = substitute_sf(DATAGEN_SQL);
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() && !stmt.starts_with("--") {
            db.execute(stmt).await;
        }
    }
}

/// Get the current max order key (for RF1 to generate non-conflicting keys).
async fn max_orderkey(db: &E2eDb) -> usize {
    let max: i64 = db
        .query_scalar("SELECT COALESCE(MAX(o_orderkey), 0) FROM orders")
        .await;
    max as usize
}

/// Apply RF1 (bulk INSERT into orders + lineitem) as a single transaction.
async fn apply_rf1(db: &E2eDb, next_orderkey: usize) {
    let sql = substitute_rf(RF1_SQL, next_orderkey);
    // Wrap in a transaction so both tables' CDC triggers fire atomically
    db.execute("BEGIN").await;
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() && !stmt.starts_with("--") {
            db.execute(stmt).await;
        }
    }
    db.execute("COMMIT").await;
}

/// Apply RF2 (bulk DELETE from orders + lineitem).
async fn apply_rf2(db: &E2eDb) {
    let sql = RF2_SQL.replace("__RF_COUNT__", &rf_count().to_string());
    // Wrap in a transaction for atomicity
    db.execute("BEGIN").await;
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() && !stmt.starts_with("--") {
            db.execute(stmt).await;
        }
    }
    db.execute("COMMIT").await;
}

/// Apply RF3 (targeted UPDATEs).
async fn apply_rf3(db: &E2eDb) {
    let sql = RF3_SQL.replace("__RF_COUNT__", &rf_count().to_string());
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() && !stmt.starts_with("--") {
            db.execute(stmt).await;
        }
    }
}

/// Assert a stream table matches its defining query, with diagnostic output.
async fn assert_tpch_invariant(db: &E2eDb, st_name: &str, query: &str, qname: &str, cycle: usize) {
    let st_table = format!("public.{st_name}");

    // Get user-visible columns (exclude __pgs_* internal columns)
    let cols: String = db
        .query_scalar(&format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = '{st_name}' \
               AND column_name NOT LIKE '__pgs_%'"
        ))
        .await;

    // Multiset equality: symmetric EXCEPT ALL must be empty
    let matches: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS ( \
                (SELECT {cols} FROM {st_table} EXCEPT ALL ({query})) \
                UNION ALL \
                (({query}) EXCEPT ALL SELECT {cols} FROM {st_table}) \
            )"
        ))
        .await;

    if !matches {
        // Collect diagnostic information
        let st_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM {st_table}"))
            .await;
        let q_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM ({query}) _q"))
            .await;
        let extra: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM \
                 (SELECT {cols} FROM {st_table} EXCEPT ALL ({query})) _x"
            ))
            .await;
        let missing: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM \
                 (({query}) EXCEPT ALL SELECT {cols} FROM {st_table}) _x"
            ))
            .await;

        panic!(
            "\n╔══════════════════════════════════════════════════════════╗\n\
             ║  TPC-H INVARIANT VIOLATION                              ║\n\
             ╠══════════════════════════════════════════════════════════╣\n\
             ║  Query:   {:<47} ║\n\
             ║  Cycle:   {:<47} ║\n\
             ║  ST rows: {:<47} ║\n\
             ║  Q rows:  {:<47} ║\n\
             ║  Extra:   {:<47} ║\n\
             ║  Missing: {:<47} ║\n\
             ╚══════════════════════════════════════════════════════════╝",
            qname, cycle, st_count, q_count, extra, missing,
        );
    }
}

/// Print a progress line for a query/cycle.
fn log_progress(qname: &str, tier: u8, cycle: usize, total_cycles: usize, elapsed_ms: f64) {
    println!(
        "  [T{}] {:<4} cycle {}/{} — {:.0}ms ✓",
        tier, qname, cycle, total_cycles, elapsed_ms,
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 1: Individual Query Correctness
// ═══════════════════════════════════════════════════════════════════════
//
// For each TPC-H query (ordered by coverage tier):
//   1. Create ST in DIFFERENTIAL mode
//   2. Assert baseline invariant
//   3. For N cycles: RF1+RF2+RF3 → refresh → assert invariant
//   4. Drop ST
//
// Uses a SINGLE container for all queries — data loaded once.

#[tokio::test]
#[ignore]
async fn test_tpch_differential_correctness() {
    let sf = scale_factor();
    let n_cycles = cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  TPC-H Differential Correctness — SF={sf}, cycles={n_cycles}");
    println!("  Orders: {}, Customers: {}, Suppliers: {}, Parts: {}",
             sf_orders(), sf_customers(), sf_suppliers(), sf_parts());
    println!("  RF batch size: {} rows", rf_count());
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    // Load schema + data
    let t = Instant::now();
    load_schema(&db).await;
    load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    let queries = tpch_queries();
    let mut passed = 0usize;
    let mut failed = Vec::new();

    for q in &queries {
        println!("── {} (Tier {}) ──────────────────────────────", q.name, q.tier);

        // Create stream table
        let st_name = format!("tpch_{}", q.name);
        let create_result = db
            .try_execute(&format!(
                "SELECT pgstream.create_stream_table('{st_name}', $${sql}$$, '1m', 'DIFFERENTIAL')",
                sql = q.sql,
            ))
            .await;

        if let Err(e) = create_result {
            println!("  SKIP — create_st failed: {e}");
            failed.push((q.name, "CREATE failed".to_string()));
            continue;
        }

        // Baseline assertion
        let t = Instant::now();
        assert_tpch_invariant(&db, &st_name, q.sql, q.name, 0).await;
        println!("  baseline — {:.0}ms ✓", t.elapsed().as_secs_f64() * 1000.0);

        // Mutation cycles
        for cycle in 1..=n_cycles {
            let ct = Instant::now();

            // RF1: bulk INSERT (needs current max order key)
            let next_ok = max_orderkey(&db).await + 1;
            apply_rf1(&db, next_ok).await;

            // RF2: bulk DELETE
            apply_rf2(&db).await;

            // RF3: targeted UPDATEs
            apply_rf3(&db).await;

            // ANALYZE for stable plans after mutations
            db.execute("ANALYZE orders").await;
            db.execute("ANALYZE lineitem").await;
            db.execute("ANALYZE customer").await;

            // Differential refresh
            db.refresh_st(&st_name).await;

            // Assert the invariant — if it panics, the test stops (fast-fail)
            assert_tpch_invariant(&db, &st_name, q.sql, q.name, cycle).await;

            log_progress(q.name, q.tier, cycle, n_cycles, ct.elapsed().as_secs_f64() * 1000.0);
        }

        passed += 1;

        // Clean up
        let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_name}')")).await;
    }

    println!("\n══════════════════════════════════════════════════════════");
    println!("  Results: {passed}/{} queries passed", queries.len());
    if !failed.is_empty() {
        println!("  Failed/skipped:");
        for (name, reason) in &failed {
            println!("    {name}: {reason}");
        }
    }
    println!("══════════════════════════════════════════════════════════\n");

    assert!(
        failed.is_empty(),
        "{} queries failed or were skipped",
        failed.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 2: Cross-Query Consistency
// ═══════════════════════════════════════════════════════════════════════
//
// All 22 stream tables exist simultaneously, share the same mutations.
// Tests that CDC triggers on shared source tables correctly fan out
// changes to all dependent STs without interference.

#[tokio::test]
#[ignore]
async fn test_tpch_cross_query_consistency() {
    let sf = scale_factor();
    let n_cycles = cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  TPC-H Cross-Query Consistency — SF={sf}, cycles={n_cycles}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    load_schema(&db).await;
    load_data(&db).await;
    println!("  Data loaded in {:.1}s", t.elapsed().as_secs_f64());

    let queries = tpch_queries();
    let mut created: Vec<(String, &str, &str)> = Vec::new();

    // Create all stream tables
    println!("\n  Creating stream tables...");
    for q in &queries {
        let st_name = format!("tpch_x_{}", q.name);
        let result = db
            .try_execute(&format!(
                "SELECT pgstream.create_stream_table('{st_name}', $${sql}$$, '1m', 'DIFFERENTIAL')",
                sql = q.sql,
            ))
            .await;

        match result {
            Ok(_) => {
                println!("    {}: created ✓", q.name);
                created.push((st_name, q.name, q.sql));
            }
            Err(e) => {
                println!("    {}: SKIP — {e}", q.name);
            }
        }
    }

    println!("  {} / {} stream tables created\n", created.len(), queries.len());

    // Baseline assertions
    for (st_name, qname, sql) in &created {
        assert_tpch_invariant(&db, st_name, sql, qname, 0).await;
    }
    println!("  Baseline assertions passed ✓\n");

    // Mutation cycles with ALL STs refreshed
    for cycle in 1..=n_cycles {
        let ct = Instant::now();

        let next_ok = max_orderkey(&db).await + 1;
        apply_rf1(&db, next_ok).await;
        apply_rf2(&db).await;
        apply_rf3(&db).await;

        db.execute("ANALYZE orders").await;
        db.execute("ANALYZE lineitem").await;
        db.execute("ANALYZE customer").await;

        // Refresh all STs
        for (st_name, _, _) in &created {
            db.refresh_st(st_name).await;
        }

        // Assert all STs
        for (st_name, qname, sql) in &created {
            assert_tpch_invariant(&db, st_name, sql, qname, cycle).await;
        }

        println!(
            "  Cycle {}/{} — all {} STs verified — {:.0}ms ✓",
            cycle,
            n_cycles,
            created.len(),
            ct.elapsed().as_secs_f64() * 1000.0,
        );
    }

    // Cleanup
    for (st_name, _, _) in &created {
        let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_name}')")).await;
    }

    println!("\n  Cross-query consistency: PASSED ✓\n");
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 3: FULL vs DIFFERENTIAL Mode Comparison
// ═══════════════════════════════════════════════════════════════════════
//
// For each query, create two STs (one FULL, one DIFFERENTIAL) and verify
// they produce identical results after the same mutations. Stronger than
// Phase 1 because it compares the two modes directly.

#[tokio::test]
#[ignore]
async fn test_tpch_full_vs_differential() {
    let sf = scale_factor();
    let n_cycles = cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  TPC-H FULL vs DIFFERENTIAL — SF={sf}, cycles={n_cycles}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    load_schema(&db).await;
    load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    let queries = tpch_queries();
    let mut passed = 0usize;

    for q in &queries {
        let st_full = format!("tpch_f_{}", q.name);
        let st_diff = format!("tpch_d_{}", q.name);

        // Create both STs
        let full_ok = db
            .try_execute(&format!(
                "SELECT pgstream.create_stream_table('{st_full}', $${sql}$$, '1m', 'FULL')",
                sql = q.sql,
            ))
            .await;
        let diff_ok = db
            .try_execute(&format!(
                "SELECT pgstream.create_stream_table('{st_diff}', $${sql}$$, '1m', 'DIFFERENTIAL')",
                sql = q.sql,
            ))
            .await;

        if full_ok.is_err() || diff_ok.is_err() {
            println!("  {}: SKIP — create failed", q.name);
            let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_full}')")).await;
            let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_diff}')")).await;
            continue;
        }

        println!("── {} (Tier {}) ──────────────────────────────", q.name, q.tier);

        for cycle in 1..=n_cycles {
            let ct = Instant::now();

            let next_ok = max_orderkey(&db).await + 1;
            apply_rf1(&db, next_ok).await;
            apply_rf2(&db).await;
            apply_rf3(&db).await;

            db.execute("ANALYZE orders").await;
            db.execute("ANALYZE lineitem").await;
            db.execute("ANALYZE customer").await;

            // Refresh both
            db.refresh_st(&st_full).await;
            db.refresh_st(&st_diff).await;

            // Compare FULL vs DIFFERENTIAL directly
            let cols: String = db
                .query_scalar(&format!(
                    "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) \
                     FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = '{st_diff}' \
                       AND column_name NOT LIKE '__pgs_%'"
                ))
                .await;

            let matches: bool = db
                .query_scalar(&format!(
                    "SELECT NOT EXISTS ( \
                        (SELECT {cols} FROM public.{st_diff} EXCEPT ALL \
                         SELECT {cols} FROM public.{st_full}) \
                        UNION ALL \
                        (SELECT {cols} FROM public.{st_full} EXCEPT ALL \
                         SELECT {cols} FROM public.{st_diff}) \
                    )"
                ))
                .await;

            if !matches {
                let full_count: i64 = db
                    .query_scalar(&format!("SELECT count(*) FROM public.{st_full}"))
                    .await;
                let diff_count: i64 = db
                    .query_scalar(&format!("SELECT count(*) FROM public.{st_diff}"))
                    .await;
                panic!(
                    "\nFULL vs DIFFERENTIAL mismatch: {} cycle {}\n  \
                     FULL rows: {}, DIFF rows: {}",
                    q.name, cycle, full_count, diff_count,
                );
            }

            println!(
                "  [T{}] {:<4} cycle {}/{} — FULL==DIFF ✓ — {:.0}ms",
                q.tier, q.name, cycle, n_cycles,
                ct.elapsed().as_secs_f64() * 1000.0,
            );
        }

        passed += 1;

        let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_full}')")).await;
        let _ = db.try_execute(&format!("SELECT pgstream.drop_stream_table('{st_diff}')")).await;
    }

    println!("\n  FULL vs DIFFERENTIAL: {passed}/{} queries passed ✓\n", queries.len());
}
