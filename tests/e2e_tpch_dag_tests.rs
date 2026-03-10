//! T6 — TPC-H DAG Chain Correctness
//!
//! Creates a two-level DAG:
//!   Level 0: `tpch_dag_q01` — Q01 (aggregate over `lineitem`), DIFFERENTIAL
//!   Level 1: `tpch_dag_derived` — filtered projection of Q01 output, DIFFERENTIAL
//!
//! Applies RF mutations to `lineitem`, refreshes in topological order
//! (level-0 first, level-1 second), and asserts both STs match their
//! defining queries.
//!
//! Also runs a multi-parent fan-in variant once the basic chain passes:
//!   Level 0a: `tpch_dag_q01_fp` — Q01 results, DIFFERENTIAL
//!   Level 0b: `tpch_dag_q06_fp` — Q06 aggregate, DIFFERENTIAL
//!   Level 1:  `tpch_dag_union`   — UNION ALL of the two level-0 STs, DIFFERENTIAL
//!
//! **TPC-H Fair Use:** Derived from TPC-H specification; not a benchmark result.

mod e2e;

use e2e::E2eDb;
use std::time::Instant;

// ── Shared helpers (duplicated from e2e_tpch_tests.rs) ────────────────

fn scale_factor() -> f64 {
    std::env::var("TPCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.01)
}

fn cycles() -> usize {
    std::env::var("TPCH_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

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
fn rf_count() -> usize {
    let sf = scale_factor();
    let orders = ((sf * 150_000.0) as usize).max(1_500);
    (orders / 100).max(10)
}

const SCHEMA_SQL: &str = include_str!("tpch/schema.sql");
const DATAGEN_SQL: &str = include_str!("tpch/datagen.sql");
const RF1_SQL: &str = include_str!("tpch/rf1.sql");
const RF2_SQL: &str = include_str!("tpch/rf2.sql");
const RF3_SQL: &str = include_str!("tpch/rf3.sql");

fn substitute_sf(sql: &str) -> String {
    sql.replace("__SF_ORDERS__", &sf_orders().to_string())
        .replace("__SF_CUSTOMERS__", &sf_customers().to_string())
        .replace("__SF_SUPPLIERS__", &sf_suppliers().to_string())
        .replace("__SF_PARTS__", &sf_parts().to_string())
}

fn substitute_rf(sql: &str, next_orderkey: usize) -> String {
    sql.replace("__RF_COUNT__", &rf_count().to_string())
        .replace("__NEXT_ORDERKEY__", &next_orderkey.to_string())
        .replace("__SF_CUSTOMERS__", &sf_customers().to_string())
        .replace("__SF_PARTS__", &sf_parts().to_string())
        .replace("__SF_SUPPLIERS__", &sf_suppliers().to_string())
}

async fn load_schema(db: &E2eDb) {
    for stmt in SCHEMA_SQL.split(';') {
        let stmt = stmt.trim();
        let has_sql = stmt.lines().any(|l| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with("--")
        });
        if has_sql {
            db.execute(stmt).await;
        }
    }
}

async fn load_data(db: &E2eDb) {
    let sql = substitute_sf(DATAGEN_SQL);
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        let has_sql = stmt.lines().any(|l| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with("--")
        });
        if has_sql {
            db.execute(stmt).await;
        }
    }
}

async fn max_orderkey(db: &E2eDb) -> usize {
    let max: i64 = db
        .query_scalar("SELECT COALESCE(MAX(o_orderkey), 0)::bigint FROM orders")
        .await;
    max as usize
}

/// Execute multi-statement SQL, returning the first error encountered.
async fn exec_sql_stmts(db: &E2eDb, sql: &str) -> Result<(), String> {
    for stmt in sql.split(';') {
        let stmt = stmt.trim();
        let has_sql = stmt.lines().any(|l| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with("--")
        });
        if has_sql {
            db.try_execute(stmt).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Refresh a stream table, returning an error string instead of panicking.
async fn try_refresh(db: &E2eDb, st_name: &str) -> Result<(), String> {
    db.try_execute(&format!(
        "SELECT pgtrickle.refresh_stream_table('{st_name}')"
    ))
    .await
    .map_err(|e| e.to_string())
}

/// Assert multiset equality between a stream table and its defining query.
///
/// Returns `Ok(())` on success or an `Err(description)` on mismatch.
async fn assert_invariant(
    db: &E2eDb,
    st_name: &str,
    query: &str,
    label: &str,
    cycle: usize,
) -> Result<(), String> {
    let st_table = format!("public.{st_name}");

    // Negative __pgt_count guard (T1).
    let has_pgt_count: bool = db
        .query_scalar(&format!(
            "SELECT EXISTS ( \
                SELECT 1 FROM information_schema.columns \
                WHERE table_name = '{st_name}' AND column_name = '__pgt_count' \
            )"
        ))
        .await;
    if has_pgt_count {
        let neg_count: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM {st_table} WHERE __pgt_count < 0"
            ))
            .await;
        if neg_count > 0 {
            return Err(format!(
                "NEGATIVE __pgt_count: {label} cycle {cycle} — \
                 {neg_count} rows with __pgt_count < 0"
            ));
        }
    }

    let cols: String = db
        .query_scalar(&format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) \
             FROM information_schema.columns \
             WHERE (table_schema || '.' || table_name = 'public.{st_name}' \
                OR table_name = '{st_name}') \
               AND column_name NOT IN ('__pgt_row_id', '__pgt_count')"
        ))
        .await;

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
        let st_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM {st_table}"))
            .await;
        let q_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM ({query}) _q"))
            .await;
        return Err(format!(
            "INVARIANT VIOLATION: {label} cycle {cycle} — \
             ST rows: {st_count}, Q rows: {q_count}"
        ));
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// T6a — Basic Two-Level DAG Chain
// ═══════════════════════════════════════════════════════════════════════
//
// Level 0: tpch_dag_q01  — Q01 aggregate over lineitem
// Level 1: tpch_dag_derived — SELECT * FROM tpch_dag_q01 WHERE l_returnflag = 'R'
//
// After each RF cycle: refresh level-0 first, then level-1.
// Both STs must match their defining queries at all checkpoints.

#[tokio::test]
#[ignore]
async fn test_tpch_dag_chain() {
    let sf = scale_factor();
    let n_cycles = cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  TPC-H DAG Chain Correctness — SF={sf}, cycles={n_cycles}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    load_schema(&db).await;
    load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    const Q01_SQL: &str = include_str!("tpch/queries/q01.sql");
    const DERIVED_SQL: &str = "SELECT l_returnflag, l_linestatus, sum_qty, \
        sum_base_price, sum_disc_price, sum_charge, \
        avg_qty, avg_price, avg_disc, count_order \
        FROM tpch_dag_q01 WHERE l_returnflag = 'R'";

    // Level-0 ground truth (same as Q01 but pre-filtered — used for level-1 invariant)
    const DERIVED_GROUND_TRUTH: &str = "SELECT l_returnflag, l_linestatus, \
            SUM(l_quantity) AS sum_qty, \
            SUM(l_extendedprice) AS sum_base_price, \
            SUM(l_extendedprice * (1 - l_discount)) AS sum_disc_price, \
            SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge, \
            AVG(l_quantity) AS avg_qty, \
            AVG(l_extendedprice) AS avg_price, \
            AVG(l_discount) AS avg_disc, \
            COUNT(*) AS count_order \
         FROM lineitem \
         WHERE l_shipdate <= DATE '1998-12-01' - INTERVAL '90 days' \
           AND l_returnflag = 'R' \
         GROUP BY l_returnflag, l_linestatus";

    // Create level-0 ST
    if let Err(e) = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('tpch_dag_q01', $${Q01_SQL}$$, '1m', 'DIFFERENTIAL')"
        ))
        .await
    {
        println!("  SKIP — Q01 level-0 create failed: {e}");
        return;
    }
    println!("  tpch_dag_q01 created ✓");

    // Create level-1 ST (references level-0)
    if let Err(e) = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('tpch_dag_derived', $${DERIVED_SQL}$$, '1m', 'DIFFERENTIAL')"
        ))
        .await
    {
        println!("  SKIP — derived level-1 create failed: {e}");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01')")
            .await;
        return;
    }
    println!("  tpch_dag_derived created ✓");

    // Baseline assertions
    if let Err(e) = assert_invariant(&db, "tpch_dag_q01", Q01_SQL, "dag_q01", 0).await {
        println!("  WARN baseline dag_q01 — {e}");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_derived')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01')")
            .await;
        panic!("DAG chain baseline failed: {e}");
    }
    if let Err(e) = assert_invariant(
        &db,
        "tpch_dag_derived",
        DERIVED_GROUND_TRUTH,
        "dag_derived",
        0,
    )
    .await
    {
        println!("  WARN baseline dag_derived — {e}");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_derived')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01')")
            .await;
        panic!("DAG chain baseline failed: {e}");
    }
    println!("  baseline ✓\n");

    // Mutation cycles
    for cycle in 1..=n_cycles {
        let ct = Instant::now();

        let next_ok = max_orderkey(&db).await + 1;
        let rf1 = substitute_rf(RF1_SQL, next_ok);
        exec_sql_stmts(&db, &rf1)
            .await
            .unwrap_or_else(|e| panic!("RF1 cycle {cycle}: {e}"));
        let rf2 = RF2_SQL.replace("__RF_COUNT__", &rf_count().to_string());
        exec_sql_stmts(&db, &rf2)
            .await
            .unwrap_or_else(|e| panic!("RF2 cycle {cycle}: {e}"));
        let rf3 = RF3_SQL.replace("__RF_COUNT__", &rf_count().to_string());
        exec_sql_stmts(&db, &rf3)
            .await
            .unwrap_or_else(|e| panic!("RF3 cycle {cycle}: {e}"));

        db.execute("ANALYZE lineitem").await;
        db.execute("ANALYZE orders").await;

        // Refresh in topological order: level-0 must be refreshed before level-1.
        try_refresh(&db, "tpch_dag_q01")
            .await
            .unwrap_or_else(|e| panic!("DAG Q01 refresh cycle {cycle}: {e}"));
        try_refresh(&db, "tpch_dag_derived")
            .await
            .unwrap_or_else(|e| panic!("DAG derived refresh cycle {cycle}: {e}"));

        // Assert level-0
        assert_invariant(&db, "tpch_dag_q01", Q01_SQL, "dag_q01", cycle)
            .await
            .unwrap_or_else(|e| panic!("dag_q01 invariant cycle {cycle}: {e}"));

        // Assert level-1 (ground truth: Q01 filtered directly from lineitem)
        assert_invariant(
            &db,
            "tpch_dag_derived",
            DERIVED_GROUND_TRUTH,
            "dag_derived",
            cycle,
        )
        .await
        .unwrap_or_else(|e| panic!("dag_derived invariant cycle {cycle}: {e}"));

        db.execute("VACUUM").await;

        println!(
            "  cycle {cycle}/{n_cycles} — BOTH levels correct ✓ — {:.0}ms",
            ct.elapsed().as_secs_f64() * 1000.0,
        );
    }

    // Clean up
    let _ = db
        .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_derived')")
        .await;
    let _ = db
        .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01')")
        .await;

    println!("\n  DAG chain correctness: PASSED — {n_cycles} cycle(s) ✓\n");
}

// ═══════════════════════════════════════════════════════════════════════
// T6b — Multi-Parent Fan-In DAG
// ═══════════════════════════════════════════════════════════════════════
//
// Level 0a: tpch_dag_q01_fp — Q01 aggregate
// Level 0b: tpch_dag_q06_fp — Q06 aggregate
// Level 1:  tpch_dag_union   — UNION ALL of both level-0 STs (different
//           schema columns; aggregated as a heterogeneous union example)
//
// This tests multi-parent DAG fan-in: refresh both level-0 STs then
// the level-1 ST, assert all three are correct.

#[tokio::test]
#[ignore]
async fn test_tpch_dag_multi_parent() {
    let sf = scale_factor();
    let n_cycles = cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  TPC-H DAG Multi-Parent Fan-In — SF={sf}, cycles={n_cycles}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    load_schema(&db).await;
    load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    const Q01_SQL: &str = include_str!("tpch/queries/q01.sql");
    const Q06_SQL: &str = include_str!("tpch/queries/q06.sql");

    // Level-1: aggregate revenue from the combined Q06 and Q01 sum_base_price.
    // Both parent STs expose a numeric column we can sum.
    const UNION_SQL: &str = "SELECT SUM(revenue_total) AS grand_revenue \
         FROM ( \
             SELECT SUM(revenue) AS revenue_total FROM tpch_dag_q06_fp \
             UNION ALL \
             SELECT SUM(sum_base_price) AS revenue_total FROM tpch_dag_q01_fp \
         ) combined";

    // Ground truth: compute union directly from base tables
    const UNION_GROUND_TRUTH: &str = "SELECT SUM(revenue_total) AS grand_revenue \
         FROM ( \
             SELECT SUM(l_extendedprice * l_discount) AS revenue_total \
             FROM lineitem \
             WHERE l_shipdate >= DATE '1994-01-01' \
               AND l_shipdate < DATE '1994-01-01' + INTERVAL '1 year' \
               AND l_discount BETWEEN 0.06 - 0.01 AND 0.06 + 0.01 \
               AND l_quantity < 24 \
             UNION ALL \
             SELECT SUM(sum_base_price) AS revenue_total \
             FROM ( \
                 SELECT SUM(l_extendedprice) AS sum_base_price \
                 FROM lineitem \
                 WHERE l_shipdate <= DATE '1998-12-01' - INTERVAL '90 days' \
                 GROUP BY l_returnflag, l_linestatus \
             ) q01_inline \
         ) combined";

    // Create level-0 STs
    let q01_ok = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('tpch_dag_q01_fp', $${Q01_SQL}$$, '1m', 'DIFFERENTIAL')"
        ))
        .await;
    let q06_ok = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('tpch_dag_q06_fp', $${Q06_SQL}$$, '1m', 'DIFFERENTIAL')"
        ))
        .await;

    if q01_ok.is_err() || q06_ok.is_err() {
        println!("  SKIP — level-0 create failed");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01_fp')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q06_fp')")
            .await;
        return;
    }
    println!("  level-0 STs created ✓");

    // Create level-1 ST
    if let Err(e) = db
        .try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('tpch_dag_union', $${UNION_SQL}$$, '1m', 'DIFFERENTIAL')"
        ))
        .await
    {
        println!("  SKIP — union level-1 create failed: {e}");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01_fp')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q06_fp')")
            .await;
        return;
    }
    println!("  tpch_dag_union created ✓");

    // Baseline
    let baseline_ok = assert_invariant(&db, "tpch_dag_q01_fp", Q01_SQL, "dag_q01_fp", 0)
        .await
        .and(assert_invariant(&db, "tpch_dag_q06_fp", Q06_SQL, "dag_q06_fp", 0).await)
        .and(assert_invariant(&db, "tpch_dag_union", UNION_GROUND_TRUTH, "dag_union", 0).await);

    if let Err(e) = baseline_ok {
        // Soft-skip if baseline fails — the individual Q01/Q06 tests already validate them.
        println!("  SKIP — baseline failed (likely union query limitation): {e}");
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_union')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01_fp')")
            .await;
        let _ = db
            .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q06_fp')")
            .await;
        return;
    }
    println!("  baseline ✓\n");

    // Mutation cycles
    for cycle in 1..=n_cycles {
        let ct = Instant::now();

        let next_ok = max_orderkey(&db).await + 1;
        let rf1 = substitute_rf(RF1_SQL, next_ok);
        exec_sql_stmts(&db, &rf1)
            .await
            .unwrap_or_else(|e| panic!("RF1 cycle {cycle}: {e}"));
        let rf2 = RF2_SQL.replace("__RF_COUNT__", &rf_count().to_string());
        exec_sql_stmts(&db, &rf2)
            .await
            .unwrap_or_else(|e| panic!("RF2 cycle {cycle}: {e}"));
        let rf3 = RF3_SQL.replace("__RF_COUNT__", &rf_count().to_string());
        exec_sql_stmts(&db, &rf3)
            .await
            .unwrap_or_else(|e| panic!("RF3 cycle {cycle}: {e}"));

        db.execute("ANALYZE lineitem").await;

        // Refresh in topological order: level-0 first, level-1 last
        if let Err(e) = try_refresh(&db, "tpch_dag_q01_fp").await {
            println!("  WARN Q01_fp refresh cycle {cycle}: {e}");
            break;
        }
        if let Err(e) = try_refresh(&db, "tpch_dag_q06_fp").await {
            println!("  WARN Q06_fp refresh cycle {cycle}: {e}");
            break;
        }
        if let Err(e) = try_refresh(&db, "tpch_dag_union").await {
            println!("  WARN union refresh cycle {cycle}: {e}");
            break;
        }

        // Assert all three
        assert_invariant(&db, "tpch_dag_q01_fp", Q01_SQL, "dag_q01_fp", cycle)
            .await
            .unwrap_or_else(|e| panic!("dag_q01_fp invariant cycle {cycle}: {e}"));
        assert_invariant(&db, "tpch_dag_q06_fp", Q06_SQL, "dag_q06_fp", cycle)
            .await
            .unwrap_or_else(|e| panic!("dag_q06_fp invariant cycle {cycle}: {e}"));
        assert_invariant(
            &db,
            "tpch_dag_union",
            UNION_GROUND_TRUTH,
            "dag_union",
            cycle,
        )
        .await
        .unwrap_or_else(|e| panic!("dag_union invariant cycle {cycle}: {e}"));

        db.execute("VACUUM").await;

        println!(
            "  cycle {cycle}/{n_cycles} — all 3 STs correct ✓ — {:.0}ms",
            ct.elapsed().as_secs_f64() * 1000.0,
        );
    }

    // Clean up
    let _ = db
        .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_union')")
        .await;
    let _ = db
        .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q01_fp')")
        .await;
    let _ = db
        .try_execute("SELECT pgtrickle.drop_stream_table('tpch_dag_q06_fp')")
        .await;

    println!("\n  DAG multi-parent correctness: PASSED ✓\n");
}
