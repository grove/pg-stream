//! Phase 9 — External Correctness Gate
//!
//! Validates DIFFERENTIAL refresh correctness against a curated set of five
//! TPC-H-derived queries, covering the key DVM operator classes:
//!
//! | Query | Pattern |
//! |-------|---------|
//! | q01   | Single-table GROUP BY with SUM/AVG/COUNT |
//! | q06   | Single-table filter-aggregate (no GROUP BY) |
//! | q14   | CASE WHEN within SUM (semi-algebraic aggregate) |
//! | q03   | Three-way equi-join (customer × orders × lineitem) |
//! | q13   | LEFT OUTER JOIN + GROUP BY COUNT |
//!
//! All five queries pass in DIFFERENTIAL mode at SF-0.01 with zero known
//! skips.  The test hard-fails on any skip or correctness mismatch,
//! making it a strict binary gate: either DVM is correct for these patterns,
//! or CI fails.
//!
//! **Gate position in CI:** This test is NOT `#[ignore]`d.  It runs
//! automatically in every CI job that exercises the full E2E Docker image
//! (push to main, daily schedule, manual dispatch).  The full image is
//! required because background-worker scheduling and `shared_preload_libraries`
//! must be active for `refresh_stream_table()` to work.
//!
//! **TPC-H Fair Use:** This workload is *derived from* the TPC-H Benchmark
//! specification but does not constitute a TPC-H Benchmark result.  Data is
//! generated with a custom pure-SQL generator (not `dbgen`), queries have
//! been modified, and no TPC-defined metric (QphH) is computed.  "TPC-H" is
//! a trademark of the Transaction Processing Performance Council (tpc.org).

mod e2e;
mod tpch;

use e2e::E2eDb;
use std::time::Instant;

// ── Gate query set ─────────────────────────────────────────────────────────
//
// Five queries are enough to exercise every major DVM operator class without
// hitting the infrastructure limits (temp_file_limit) that cause q05/q07/q08/
// q09 to be skipped in the full TPC-H suite.  Expand this list as the skip
// allowlist in e2e_tpch_tests.rs shrinks.

struct GateQuery {
    name: &'static str,
    sql: &'static str,
}

fn gate_queries() -> Vec<GateQuery> {
    vec![
        // Single-table aggregate — baseline correctness for algebraic aggs.
        GateQuery {
            name: "q01",
            sql: include_str!("tpch/queries/q01.sql"),
        },
        // Single-table filter-aggregate — tests WHERE-clause CDC filter path.
        GateQuery {
            name: "q06",
            sql: include_str!("tpch/queries/q06.sql"),
        },
        // CASE WHEN inside SUM — semi-algebraic aggregate differential.
        GateQuery {
            name: "q14",
            sql: include_str!("tpch/queries/q14.sql"),
        },
        // Three-way equi-join — inner join differential with multi-table CDC.
        GateQuery {
            name: "q03",
            sql: include_str!("tpch/queries/q03.sql"),
        },
        // LEFT OUTER JOIN + GROUP BY COUNT — outer join retraction correctness.
        GateQuery {
            name: "q13",
            sql: include_str!("tpch/queries/q13.sql"),
        },
    ]
}

// ══════════════════════════════════════════════════════════════════════════════
// test_differential_correctness_gate
// ══════════════════════════════════════════════════════════════════════════════
//
// For each gate query:
//   1. Create stream table in DIFFERENTIAL mode (24h schedule — disables auto-
//      refresh so the test drives refreshes explicitly, avoiding scheduler races)
//   2. Assert baseline: ST content == defining query (multiset equality)
//   3. One mutation cycle: RF1 (bulk INSERT) + RF2 (bulk DELETE) → refresh → assert
//   4. Drop stream table
//
// Hard-fail on ANY skip or invariant violation — zero tolerance.

#[tokio::test]
async fn test_differential_correctness_gate() {
    let sf = tpch::scale_factor();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  Phase 9 — Differential Correctness Gate");
    println!(
        "  SF={sf} | orders={} | lineitems≈{} | RF batch={}",
        tpch::sf_orders(),
        tpch::sf_orders() * 4,
        tpch::rf_count(),
    );
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new().await.with_extension().await;

    // ── Schema + data load ─────────────────────────────────────────────────
    let t_load = Instant::now();
    tpch::load_schema(&db).await;
    tpch::load_data(&db).await;
    println!(
        "  Schema + data loaded in {:.1}s\n",
        t_load.elapsed().as_secs_f64()
    );

    let queries = gate_queries();
    let mut results: Vec<(&str, Result<(), String>)> = Vec::new();

    for q in &queries {
        println!(
            "── {} ─────────────────────────────────────────────",
            q.name
        );
        let st_name = format!("gate_{}", q.name);

        // ── Create stream table ────────────────────────────────────────────
        let create_result = db
            .try_execute(&format!(
                "SELECT pgtrickle.create_stream_table(\
                    '{st_name}', $${sql}$$, '24h', 'DIFFERENTIAL')",
                sql = q.sql,
            ))
            .await;

        if let Err(e) = create_result {
            let msg = e.to_string();
            let short = msg.split(':').next_back().unwrap_or(&msg).trim();
            println!("  FAIL (create) — {short}");
            results.push((q.name, Err(format!("create failed: {msg}"))));
            continue;
        }

        // ── Baseline assertion ─────────────────────────────────────────────
        let t = Instant::now();
        match tpch::assert_invariant(&db, &st_name, q.sql, q.name, 0).await {
            Err(e) => {
                println!("  FAIL baseline — {e}");
                let _ = db
                    .try_execute(&format!("SELECT pgtrickle.drop_stream_table('{st_name}')"))
                    .await;
                results.push((q.name, Err(format!("baseline: {e}"))));
                continue;
            }
            Ok(()) => println!(
                "  baseline ✓  ({:.0}ms)",
                t.elapsed().as_secs_f64() * 1000.0
            ),
        }

        // ── One mutation cycle: RF1 + RF2 → refresh → assert ──────────────
        let t_cycle = Instant::now();

        let next_ok = tpch::max_orderkey(&db).await + 1;
        tpch::apply_rf1(&db, next_ok).await;
        tpch::apply_rf2(&db).await;

        db.execute("ANALYZE orders").await;
        db.execute("ANALYZE lineitem").await;
        db.execute("ANALYZE customer").await;

        // SET lock_timeout prevents hanging if the background scheduler races.
        db.execute("SET lock_timeout = '60s'").await;
        let refresh_result = db
            .try_execute(&format!(
                "SELECT pgtrickle.refresh_stream_table('{st_name}')"
            ))
            .await
            .map_err(|e| e.to_string());
        db.execute("SET lock_timeout = 0").await;

        let cycle_result = match refresh_result {
            Err(e) => {
                let msg = e.lines().next().unwrap_or(&e).to_string();
                Err(format!("refresh error: {msg}"))
            }
            Ok(()) => tpch::assert_invariant(&db, &st_name, q.sql, q.name, 1).await,
        };

        match &cycle_result {
            Ok(()) => println!(
                "  cycle 1 ✓  ({:.0}ms)",
                t_cycle.elapsed().as_secs_f64() * 1000.0
            ),
            Err(e) => println!("  FAIL cycle 1 — {e}"),
        }

        // ── Cleanup ────────────────────────────────────────────────────────
        let _ = db
            .try_execute(&format!("SELECT pgtrickle.drop_stream_table('{st_name}')"))
            .await;

        results.push((q.name, cycle_result));
    }

    // ── Summary ────────────────────────────────────────────────────────────
    let passed: Vec<&str> = results
        .iter()
        .filter(|(_, r)| r.is_ok())
        .map(|(n, _)| *n)
        .collect();
    let failed: Vec<(&str, String)> = results
        .iter()
        .filter(|(_, r)| r.is_err())
        .map(|(n, r)| (*n, r.as_ref().err().cloned().unwrap()))
        .collect();

    println!("\n══════════════════════════════════════════════════════════");
    println!("  Gate result: {}/{} passed", passed.len(), queries.len());
    if !failed.is_empty() {
        println!("  FAILED queries:");
        for (name, reason) in &failed {
            println!("    {name}: {reason}");
        }
    }
    println!("══════════════════════════════════════════════════════════\n");

    // Hard-fail: zero tolerance. Every gate query must pass.
    assert!(
        failed.is_empty(),
        "Phase 9 correctness gate FAILED — {}/{} queries had errors: {:?}",
        failed.len(),
        queries.len(),
        failed.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
    );
}
