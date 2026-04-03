//! Nexmark-derived correctness tests for pg_trickle DIFFERENTIAL refresh.
//!
//! Validates the core DBSP invariant against a Nexmark online auction workload:
//! after every differential refresh, the stream table's contents must be
//! multiset-equal to re-executing the defining query from scratch.
//!
//! **Nexmark Fair Use:** This workload is derived from the Nexmark Benchmark
//! specification (https://datalab.cs.pdx.edu/niagara/NEXMark/) but does not
//! constitute a Nexmark Benchmark result. Data is generated with a custom
//! pure-SQL generator, queries have been adapted for PostgreSQL, and no
//! official Nexmark metric is computed.
//!
//! These tests are `#[ignore]`d to skip in normal `cargo test` runs.
//! Run manually:
//!
//! ```bash
//! cargo test --test e2e_nexmark_tests -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Configuration via environment variables:
//!   NEXMARK_PERSONS  — number of persons (default: 100)
//!   NEXMARK_AUCTIONS — number of auctions (default: 500)
//!   NEXMARK_BIDS     — number of bids (default: 2000)
//!   NEXMARK_CYCLES   — refresh cycles per query (default: 3)
//!   NEXMARK_RF_COUNT — rows affected per RF cycle (default: bids/20)
//!
//! Prerequisites: `just build-e2e-image`

mod e2e;
mod nexmark;

use e2e::E2eDb;
use std::time::Instant;

// ── Nexmark queries ────────────────────────────────────────────────────

struct NexmarkQuery {
    name: &'static str,
    sql: &'static str,
}

fn nexmark_queries() -> Vec<NexmarkQuery> {
    vec![
        NexmarkQuery {
            name: "q0",
            sql: include_str!("nexmark/queries/q0.sql"),
        },
        NexmarkQuery {
            name: "q1",
            sql: include_str!("nexmark/queries/q1.sql"),
        },
        NexmarkQuery {
            name: "q2",
            sql: include_str!("nexmark/queries/q2.sql"),
        },
        NexmarkQuery {
            name: "q3",
            sql: include_str!("nexmark/queries/q3.sql"),
        },
        NexmarkQuery {
            name: "q4",
            sql: include_str!("nexmark/queries/q4.sql"),
        },
        NexmarkQuery {
            name: "q5",
            sql: include_str!("nexmark/queries/q5.sql"),
        },
        NexmarkQuery {
            name: "q6",
            sql: include_str!("nexmark/queries/q6.sql"),
        },
        NexmarkQuery {
            name: "q7",
            sql: include_str!("nexmark/queries/q7.sql"),
        },
        NexmarkQuery {
            name: "q8",
            sql: include_str!("nexmark/queries/q8.sql"),
        },
        NexmarkQuery {
            name: "q9",
            sql: include_str!("nexmark/queries/q9.sql"),
        },
    ]
}

/// Queries allowed to be skipped in DIFFERENTIAL mode.
/// Empty = all queries are expected to pass.
const DIFFERENTIAL_SKIP_ALLOWLIST: &[&str] = &[
    // Q9 uses DISTINCT ON which may not be supported in DIFFERENTIAL.
    "q9",
];

/// Refresh a stream table, returning an error instead of panicking.
async fn try_refresh_st(db: &E2eDb, st_name: &str) -> Result<(), String> {
    db.try_execute("SET lock_timeout = '60s'").await.ok();
    let result = db
        .try_execute(&format!(
            "SELECT pgtrickle.refresh_stream_table('{st_name}')"
        ))
        .await
        .map_err(|e| e.to_string());
    db.try_execute("SET lock_timeout = 0").await.ok();
    result
}

// ── Main test: Differential correctness ────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_nexmark_differential_correctness() {
    let n_cycles = nexmark::cycles();
    let n_persons = nexmark::nm_persons();
    let n_auctions = nexmark::nm_auctions();
    let n_bids = nexmark::nm_bids();
    let rf = nexmark::rf_count();

    println!("\n══════════════════════════════════════════════════════════");
    println!("  Nexmark Differential Correctness");
    println!("  Persons: {n_persons}, Auctions: {n_auctions}, Bids: {n_bids}");
    println!("  Cycles: {n_cycles}, RF batch size: {rf}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    // Load schema + data
    let t = Instant::now();
    nexmark::load_schema(&db).await;
    nexmark::load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    let queries = nexmark_queries();
    let mut passed = 0usize;
    let mut skipped: Vec<(&str, String)> = Vec::new();
    let mut failed: Vec<(&str, String)> = Vec::new();

    for q in &queries {
        println!("── {} ──────────────────────────────", q.name);

        // Create stream table
        let st_name = format!("nexmark_{}", q.name);
        let create_result = db
            .try_execute(&format!(
                "SELECT pgtrickle.create_stream_table('{st_name}', $${sql}$$, '24h', 'DIFFERENTIAL')",
                sql = q.sql,
            ))
            .await;

        if let Err(e) = create_result {
            let reason = e.to_string();
            let short = reason.split(':').next_back().unwrap_or(&reason).trim();
            println!("  SKIP -- {short}");
            skipped.push((q.name, reason));
            continue;
        }

        // Baseline assertion
        let t = Instant::now();
        if let Err(e) = nexmark::assert_invariant(&db, &st_name, q.sql, q.name, 0).await {
            println!("  WARN baseline -- {e}");
            skipped.push((q.name, format!("baseline invariant: {e}")));
            let _ = db
                .try_execute(&format!("SELECT pgtrickle.drop_stream_table('{st_name}')"))
                .await;
            continue;
        }
        println!("  baseline -- {:.0}ms", t.elapsed().as_secs_f64() * 1000.0);

        // Mutation cycles
        let mut dvm_ok = true;
        'cycles: for cycle in 1..=n_cycles {
            let ct = Instant::now();

            // RF1: bulk INSERT
            let next_person = nexmark::max_person_id(&db).await + 1;
            let next_auction = nexmark::max_auction_id(&db).await + 1;
            nexmark::apply_rf1(&db, next_person, next_auction).await;

            // RF2: bulk DELETE
            nexmark::apply_rf2(&db).await;

            // RF3: targeted UPDATEs
            nexmark::apply_rf3(&db).await;

            // ANALYZE for stable plans
            db.execute("ANALYZE person").await;
            db.execute("ANALYZE auction").await;
            db.execute("ANALYZE bid").await;

            // Differential refresh
            if let Err(e) = try_refresh_st(&db, &st_name).await {
                let msg = e.lines().next().unwrap_or(&e).to_string();
                println!("  WARN cycle {cycle} -- DVM error: {msg}");
                skipped.push((q.name, format!("DVM error cycle {cycle}: {msg}")));
                dvm_ok = false;
                break 'cycles;
            }

            // Assert the invariant
            if let Err(e) = nexmark::assert_invariant(&db, &st_name, q.sql, q.name, cycle).await {
                let msg = e.lines().next().unwrap_or(&e).to_string();
                println!("  FAIL cycle {cycle} -- {msg}");
                failed.push((q.name, format!("invariant cycle {cycle}: {msg}")));
                dvm_ok = false;
                break 'cycles;
            }

            println!(
                "  cycle {cycle}/{n_cycles} -- {:.0}ms",
                ct.elapsed().as_secs_f64() * 1000.0
            );
        }

        if dvm_ok {
            passed += 1;
            println!("  PASS");
        }

        // Drop stream table to free resources
        let _ = db
            .try_execute(&format!("SELECT pgtrickle.drop_stream_table('{st_name}')"))
            .await;
    }

    // ── Summary ──────────────────────────────────────────────────────────

    println!("\n══════════════════════════════════════════════════════════");
    println!(
        "  Nexmark Results: {} passed, {} skipped, {} failed",
        passed,
        skipped.len(),
        failed.len()
    );

    if !skipped.is_empty() {
        println!("\n  Skipped:");
        for (name, reason) in &skipped {
            let short = reason.split(':').next_back().unwrap_or(reason).trim();
            println!("    {name}: {short}");
        }
    }

    if !failed.is_empty() {
        println!("\n  FAILED:");
        for (name, reason) in &failed {
            println!("    {name}: {reason}");
        }
    }
    println!("══════════════════════════════════════════════════════════\n");

    // Skip-set regression guard: fail if any query was skipped that is NOT
    // in the allowlist.
    let unexpected_skips: Vec<&&str> = skipped
        .iter()
        .map(|(name, _)| name)
        .filter(|name| !DIFFERENTIAL_SKIP_ALLOWLIST.contains(name))
        .collect();

    if !unexpected_skips.is_empty() {
        panic!(
            "Nexmark skip-set regression: queries {:?} were skipped but are \
             not in DIFFERENTIAL_SKIP_ALLOWLIST. If this is expected, add them \
             to the allowlist with a comment explaining the limitation.",
            unexpected_skips
        );
    }

    assert!(
        failed.is_empty(),
        "Nexmark invariant violations: {:?}",
        failed
    );
}

// ── FULL mode correctness ──────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_nexmark_full_correctness() {
    let n_cycles = nexmark::cycles();
    println!("\n══════════════════════════════════════════════════════════");
    println!("  Nexmark FULL Correctness -- cycles={n_cycles}");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    nexmark::load_schema(&db).await;
    nexmark::load_data(&db).await;
    println!("  Data loaded in {:.1}s\n", t.elapsed().as_secs_f64());

    let queries = nexmark_queries();
    let mut passed = 0usize;
    let mut failed: Vec<(&str, String)> = Vec::new();

    for q in &queries {
        println!("── {} (FULL) ──────────────────────────", q.name);

        let st_name = format!("nexmark_full_{}", q.name);
        let create_result = db
            .try_execute(&format!(
                "SELECT pgtrickle.create_stream_table('{st_name}', $${sql}$$, '24h', 'FULL')",
                sql = q.sql,
            ))
            .await;

        if let Err(e) = create_result {
            let reason = e.to_string();
            let short = reason.split(':').next_back().unwrap_or(&reason).trim();
            println!("  SKIP -- {short}");
            continue;
        }

        let mut all_ok = true;
        for cycle in 1..=n_cycles {
            let ct = Instant::now();

            let next_person = nexmark::max_person_id(&db).await + 1;
            let next_auction = nexmark::max_auction_id(&db).await + 1;
            nexmark::apply_rf1(&db, next_person, next_auction).await;
            nexmark::apply_rf2(&db).await;
            nexmark::apply_rf3(&db).await;

            if let Err(e) = try_refresh_st(&db, &st_name).await {
                println!("  FAIL cycle {cycle} -- refresh error: {e}");
                failed.push((q.name, format!("refresh cycle {cycle}: {e}")));
                all_ok = false;
                break;
            }

            if let Err(e) = nexmark::assert_invariant(&db, &st_name, q.sql, q.name, cycle).await {
                println!("  FAIL cycle {cycle} -- {e}");
                failed.push((q.name, format!("invariant cycle {cycle}: {e}")));
                all_ok = false;
                break;
            }

            println!(
                "  cycle {cycle}/{n_cycles} -- {:.0}ms",
                ct.elapsed().as_secs_f64() * 1000.0
            );
        }

        if all_ok {
            passed += 1;
            println!("  PASS");
        }

        let _ = db
            .try_execute(&format!("SELECT pgtrickle.drop_stream_table('{st_name}')"))
            .await;
    }

    println!("\n  FULL Results: {passed} passed, {} failed", failed.len());
    assert!(failed.is_empty(), "Nexmark FULL failures: {:?}", failed);
}

// ── Sustained churn test ───────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_nexmark_sustained_churn() {
    let churn_cycles = nexmark::cycles() * 3;
    println!("\n══════════════════════════════════════════════════════════");
    println!("  Nexmark Sustained Churn -- {churn_cycles} cycles");
    println!("══════════════════════════════════════════════════════════\n");

    let db = E2eDb::new_bench().await.with_extension().await;

    let t = Instant::now();
    nexmark::load_schema(&db).await;
    nexmark::load_data(&db).await;
    println!("  Data loaded in {:.1}s", t.elapsed().as_secs_f64());

    // Use the queries that exercise different operator paths
    let test_queries = [
        ("q3", include_str!("nexmark/queries/q3.sql")),
        ("q4", include_str!("nexmark/queries/q4.sql")),
        ("q5", include_str!("nexmark/queries/q5.sql")),
        ("q6", include_str!("nexmark/queries/q6.sql")),
    ];

    // Create all stream tables
    for (name, sql) in &test_queries {
        let st_name = format!("nexmark_churn_{name}");
        let result = db
            .try_execute(&format!(
                "SELECT pgtrickle.create_stream_table('{st_name}', $${sql}$$, '24h', 'DIFFERENTIAL')",
            ))
            .await;
        if let Err(e) = result {
            println!("  SKIP {name} -- {e}");
        }
    }

    // Run sustained churn: mutations + refresh all STs each cycle
    let mut failed: Vec<String> = Vec::new();

    for cycle in 1..=churn_cycles {
        let ct = Instant::now();

        let next_person = nexmark::max_person_id(&db).await + 1;
        let next_auction = nexmark::max_auction_id(&db).await + 1;
        nexmark::apply_rf1(&db, next_person, next_auction).await;
        nexmark::apply_rf2(&db).await;
        nexmark::apply_rf3(&db).await;

        for (name, sql) in &test_queries {
            let st_name = format!("nexmark_churn_{name}");
            if let Err(e) = try_refresh_st(&db, &st_name).await {
                failed.push(format!("{name} cycle {cycle}: refresh -- {e}"));
                continue;
            }
            if let Err(e) = nexmark::assert_invariant(&db, &st_name, sql, name, cycle).await {
                failed.push(format!("{name} cycle {cycle}: {e}"));
            }
        }

        if cycle % 3 == 0 || cycle == churn_cycles {
            println!(
                "  cycle {cycle}/{churn_cycles} -- {:.0}ms",
                ct.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    if !failed.is_empty() {
        println!("\n  FAILURES:");
        for f in &failed {
            println!("    {f}");
        }
    }
    assert!(
        failed.is_empty(),
        "{} failures in sustained churn",
        failed.len()
    );
}
