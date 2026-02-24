//! Database-level benchmarks — PLAN.md §11.4.
//!
//! Measures differential vs full refresh performance across table sizes,
//! change rates, and query complexities.
//!
//! These tests are `#[ignore]`d to skip in normal CI. Run explicitly:
//!
//! ```bash
//! cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture
//! ```
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;
use std::time::Instant;

// ── Configuration ──────────────────────────────────────────────────────

/// Table sizes to benchmark.
const TABLE_SIZES: &[usize] = &[10_000, 100_000];

/// Change rates (fraction of rows mutated per cycle).
const CHANGE_RATES: &[f64] = &[0.01, 0.10, 0.50];

/// Number of refresh cycles per (size, rate, query, mode) combination.
const CYCLES: usize = 10;

/// Number of throw-away warm-up cycles before measured cycles.
/// Eliminates cache-warming effects from the first measured iteration.
const WARMUP_CYCLES: usize = 2;

// ── Data generation helpers ────────────────────────────────────────────

/// Generate SQL to bulk-insert `n` rows into a single source table.
///
/// Schema: `src(id SERIAL PK, region TEXT, category TEXT, amount INT, score INT)`
fn bulk_insert_single(n: usize) -> String {
    // Use generate_series for fast bulk loading
    format!(
        "INSERT INTO src (region, category, amount, score)
         SELECT
             CASE (i % 5)
                 WHEN 0 THEN 'north'
                 WHEN 1 THEN 'south'
                 WHEN 2 THEN 'east'
                 WHEN 3 THEN 'west'
                 ELSE 'central'
             END,
             CASE (i % 4)
                 WHEN 0 THEN 'A'
                 WHEN 1 THEN 'B'
                 WHEN 2 THEN 'C'
                 ELSE 'D'
             END,
             (i * 17 + 13) % 10000,
             (i * 31 + 7) % 100
         FROM generate_series(1, {n}) AS s(i)"
    )
}

/// Generate SQL to create the second source table for join benchmarks.
fn create_join_table() -> &'static str {
    "CREATE TABLE dim (
         id SERIAL PRIMARY KEY,
         region TEXT NOT NULL,
         region_name TEXT NOT NULL,
         multiplier NUMERIC NOT NULL DEFAULT 1.0
     )"
}

/// Populate the dimension table with 5 regions.
fn populate_dim() -> &'static str {
    "INSERT INTO dim (region, region_name, multiplier) VALUES
     ('north', 'Northern Region', 1.1),
     ('south', 'Southern Region', 0.9),
     ('east', 'Eastern Region', 1.0),
     ('west', 'Western Region', 1.2),
     ('central', 'Central Region', 1.05)"
}

/// Apply random changes to `change_pct` fraction of rows.
/// Returns separate SQL statements (sqlx cannot execute multi-statement strings).
fn apply_changes_stmts(table_size: usize, change_pct: f64) -> Vec<String> {
    let n_changes = ((table_size as f64) * change_pct).max(1.0) as usize;
    // Mix of updates (70%), deletes (15%), inserts (15%)
    let n_updates = (n_changes as f64 * 0.70).max(1.0) as usize;
    let n_deletes = (n_changes as f64 * 0.15).max(1.0) as usize;
    let n_inserts = (n_changes as f64 * 0.15).max(1.0) as usize;

    vec![
        format!(
            "UPDATE src SET amount = amount + 1
             WHERE id IN (
                 SELECT id FROM src ORDER BY random() LIMIT {n_updates}
             )"
        ),
        format!(
            "DELETE FROM src
             WHERE id IN (
                 SELECT id FROM src ORDER BY random() LIMIT {n_deletes}
             )"
        ),
        format!(
            "INSERT INTO src (region, category, amount, score)
             SELECT
                 CASE (i % 5)
                     WHEN 0 THEN 'north' WHEN 1 THEN 'south'
                     WHEN 2 THEN 'east' WHEN 3 THEN 'west' ELSE 'central'
                 END,
                 CASE (i % 4) WHEN 0 THEN 'A' WHEN 1 THEN 'B' WHEN 2 THEN 'C' ELSE 'D' END,
                 (random() * 10000)::int,
                 (random() * 100)::int
             FROM generate_series(1, {n_inserts}) AS s(i)"
        ),
    ]
}

/// Execute all change statements for a benchmark cycle.
async fn apply_changes(db: &E2eDb, table_size: usize, change_pct: f64) {
    for stmt in apply_changes_stmts(table_size, change_pct) {
        db.execute(&stmt).await;
    }
}

// ── Query complexity definitions ───────────────────────────────────────

/// Benchmark scenario: a named query template + whether it needs a join table.
struct QueryScenario {
    name: &'static str,
    query: &'static str,
    needs_dim: bool,
}

fn query_scenarios() -> Vec<QueryScenario> {
    vec![
        QueryScenario {
            name: "scan",
            query: "SELECT id, region, category, amount, score FROM src",
            needs_dim: false,
        },
        QueryScenario {
            name: "filter",
            query: "SELECT id, region, amount FROM src WHERE amount > 5000",
            needs_dim: false,
        },
        QueryScenario {
            name: "aggregate",
            query: "SELECT region, SUM(amount) AS total, COUNT(*) AS cnt FROM src GROUP BY region",
            needs_dim: false,
        },
        QueryScenario {
            name: "join",
            query: "SELECT s.id, s.region, s.amount, d.region_name \
                    FROM src s INNER JOIN dim d ON s.region = d.region",
            needs_dim: true,
        },
        QueryScenario {
            name: "join_agg",
            query: "SELECT d.region_name, SUM(s.amount) AS total, COUNT(*) AS cnt \
                    FROM src s INNER JOIN dim d ON s.region = d.region \
                    GROUP BY d.region_name",
            needs_dim: true,
        },
    ]
}

// ── Result reporting ───────────────────────────────────────────────────

/// Per-phase timing extracted from `[PGS_PROFILE]` log lines.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ProfileData {
    decision_ms: f64,
    generate_ms: f64,
    merge_ms: f64,
    cleanup_ms: f64,
    total_ms: f64,
    affected: i64,
    path: String,
}

/// A single benchmark measurement.
#[derive(Clone)]
struct BenchResult {
    scenario: String,
    table_size: usize,
    change_pct: f64,
    mode: String,
    cycle: usize,
    refresh_ms: f64,
    st_row_count: i64,
    profile: Option<ProfileData>,
}

/// Compute a percentile from a sorted slice using linear interpolation.
fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = pct / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = rank - rank.floor();
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Extract the last `[PGS_PROFILE]` line from docker container logs.
async fn extract_last_profile(container_id: &str) -> Option<ProfileData> {
    let output = tokio::process::Command::new("docker")
        .args(["logs", "--tail", "50", container_id])
        .output()
        .await
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().rev().find(|l| l.contains("[PGS_PROFILE]"))?;
    parse_profile_line(line)
}

/// Parse a `[PGS_PROFILE]` log line into structured data.
fn parse_profile_line(line: &str) -> Option<ProfileData> {
    // Format: [PGS_PROFILE] decision=X.XXms generate+build=X.XXms
    //         merge_exec=X.XXms cleanup=X.XXms total=X.XXms
    //         affected=N mode=INCR path=cache_hit
    let extract_ms = |key: &str| -> Option<f64> {
        let prefix = format!("{}=", key);
        let start = line.find(&prefix)? + prefix.len();
        let rest = &line[start..];
        let end = rest.find("ms")?;
        rest[..end].parse().ok()
    };
    let extract_int = |key: &str| -> Option<i64> {
        let prefix = format!("{}=", key);
        let start = line.find(&prefix)? + prefix.len();
        let rest = &line[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    let extract_str = |key: &str| -> Option<String> {
        let prefix = format!("{}=", key);
        let start = line.find(&prefix)? + prefix.len();
        let rest = &line[start..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    };
    Some(ProfileData {
        decision_ms: extract_ms("decision")?,
        generate_ms: extract_ms("generate+build")?,
        merge_ms: extract_ms("merge_exec")?,
        cleanup_ms: extract_ms("cleanup")?,
        total_ms: extract_ms("total")?,
        affected: extract_int("affected")?,
        path: extract_str("path")?,
    })
}

fn print_results_table(results: &[BenchResult]) {
    println!();
    println!(
        "╔══════════════════════════════════════════════════════════════════════════════════════╗"
    );
    println!("║                    pg_stream Refresh Benchmark Results                      ║");
    println!(
        "╠════════════╤══════════╤════════╤═════════════╤═══════╤════════════╤═════════════════╣"
    );
    println!(
        "║ Scenario   │ Rows     │ Chg %  │ Mode        │ Cycle │ Refresh ms │ ST Rows         ║"
    );
    println!(
        "╠════════════╪══════════╪════════╪═════════════╪═══════╪════════════╪═════════════════╣"
    );

    for r in results {
        println!(
            "║ {:10} │ {:>8} │ {:>5.0}% │ {:11} │ {:>5} │ {:>10.1} │ {:>15} ║",
            r.scenario,
            r.table_size,
            r.change_pct * 100.0,
            r.mode,
            r.cycle,
            r.refresh_ms,
            r.st_row_count,
        );
    }
    println!(
        "╚════════════╧══════════╧════════╧═════════════╧═══════╧════════════╧═════════════════╝"
    );
    println!();

    // Print summary: avg refresh time per (scenario, size, rate, mode)
    print_summary(results);
}

/// Benchmark summary grouped by scenario key.
type SummaryMap = std::collections::BTreeMap<(String, usize, String), (Vec<f64>, Vec<f64>)>;

fn print_summary(results: &[BenchResult]) {
    println!(
        "┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐"
    );
    println!(
        "│                                    Summary (avg ms per cycle)                                                          │"
    );
    println!(
        "├────────────┬──────────┬────────┬─────────────────┬──────────────────────┬─────────┬─────────┬─────────┬─────────┤"
    );
    println!(
        "│ Scenario   │ Rows     │ Chg %  │ FULL avg ms     │ INCR avg ms          │ INCR c1 │ INCR 2+ │ INCR med│ INCR P95│"
    );
    println!(
        "├────────────┼──────────┼────────┼─────────────────┼──────────────────────┼─────────┼─────────┼─────────┼─────────┤"
    );

    // Group results by (scenario, size, rate)
    let mut groups: SummaryMap = std::collections::BTreeMap::new();

    for r in results {
        let key = (
            r.scenario.clone(),
            r.table_size,
            format!("{:.0}%", r.change_pct * 100.0),
        );
        let entry = groups.entry(key).or_insert_with(|| (vec![], vec![]));
        if r.mode == "FULL" {
            entry.0.push(r.refresh_ms);
        } else {
            entry.1.push(r.refresh_ms);
        }
    }

    for ((scenario, size, rate), (full_times, inc_times)) in &groups {
        let full_avg = if full_times.is_empty() {
            0.0
        } else {
            full_times.iter().sum::<f64>() / full_times.len() as f64
        };
        let inc_avg = if inc_times.is_empty() {
            0.0
        } else {
            inc_times.iter().sum::<f64>() / inc_times.len() as f64
        };
        let speedup = if inc_avg > 0.0 {
            format!("{:.1}x", full_avg / inc_avg)
        } else {
            "N/A".to_string()
        };

        // Compute cycle-1 vs cycle-2+ breakdown for DIFFERENTIAL
        let (c1_str, c2n_str) = if inc_times.len() >= 2 {
            let c1 = inc_times[0];
            let c2n_avg = inc_times[1..].iter().sum::<f64>() / (inc_times.len() - 1) as f64;
            (format!("{c1:>7.1}"), format!("{c2n_avg:>7.1}"))
        } else if inc_times.len() == 1 {
            (format!("{:>7.1}", inc_times[0]), "    N/A".to_string())
        } else {
            ("    N/A".to_string(), "    N/A".to_string())
        };

        // Compute median for DIFFERENTIAL
        let inc_median_str = if !inc_times.is_empty() {
            let mut sorted = inc_times.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = if sorted.len() % 2 == 0 {
                (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
            } else {
                sorted[sorted.len() / 2]
            };
            format!("{median:>7.1}")
        } else {
            "    N/A".to_string()
        };

        // Compute P95 for DIFFERENTIAL
        let inc_p95_str = if !inc_times.is_empty() {
            let mut sorted = inc_times.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let p95 = percentile(&sorted, 95.0);
            format!("{p95:>7.1}")
        } else {
            "    N/A".to_string()
        };

        println!(
            "│ {:10} │ {:>8} │ {:>6} │ {:>10.1}       │ {:>10.1} ({:>6}) │ {} │ {} │ {} │ {} │",
            scenario,
            size,
            rate,
            full_avg,
            inc_avg,
            speedup,
            c1_str,
            c2n_str,
            inc_median_str,
            inc_p95_str,
        );
    }

    println!(
        "└────────────┴──────────┴────────┴─────────────────┴──────────────────────┴─────────┴─────────┴─────────┴─────────┘"
    );
    println!();

    // Print per-phase timing breakdown for DIFFERENTIAL results
    print_phase_breakdown(results);
}

/// Per-phase timing breakdown for DIFFERENTIAL refreshes.
///
/// Extracts `[PGS_PROFILE]` data from results and displays average
/// decision / generate+build / merge / cleanup breakdown per scenario.
fn print_phase_breakdown(results: &[BenchResult]) {
    // Collect profile data grouped by (scenario, size, rate)
    let mut groups: std::collections::BTreeMap<(String, usize, String), Vec<&ProfileData>> =
        std::collections::BTreeMap::new();

    for r in results {
        if r.mode == "DIFFERENTIAL"
            && let Some(ref p) = r.profile
        {
            let key = (
                r.scenario.clone(),
                r.table_size,
                format!("{:.0}%", r.change_pct * 100.0),
            );
            groups.entry(key).or_default().push(p);
        }
    }

    if groups.is_empty() {
        return;
    }

    println!(
        "┌──────────────────────────────────────────────────────────────────────────────────────────────────────┐"
    );
    println!(
        "│                     Per-Phase Timing Breakdown (DIFFERENTIAL avg ms)                                 │"
    );
    println!(
        "├────────────┬──────────┬────────┬──────────┬───────────────┬───────────┬──────────┬──────────────────┤"
    );
    println!(
        "│ Scenario   │ Rows     │ Chg %  │ Decision │ Gen+Build     │ Merge     │ Cleanup  │ Path             │"
    );
    println!(
        "├────────────┼──────────┼────────┼──────────┼───────────────┼───────────┼──────────┼──────────────────┤"
    );

    for ((scenario, size, rate), profiles) in &groups {
        let n = profiles.len() as f64;
        let avg =
            |f: fn(&ProfileData) -> f64| -> f64 { profiles.iter().map(|p| f(p)).sum::<f64>() / n };

        let decision = avg(|p| p.decision_ms);
        let generate = avg(|p| p.generate_ms);
        let merge = avg(|p| p.merge_ms);
        let cleanup = avg(|p| p.cleanup_ms);

        // Determine dominant path (most common)
        let hit_count = profiles.iter().filter(|p| p.path == "cache_hit").count();
        let path = if hit_count > profiles.len() / 2 {
            "cache_hit"
        } else {
            "cache_miss"
        };

        println!(
            "│ {:10} │ {:>8} │ {:>6} │ {:>8.2} │ {:>13.2} │ {:>9.2} │ {:>8.2} │ {:16} │",
            scenario, size, rate, decision, generate, merge, cleanup, path,
        );
    }

    println!(
        "└────────────┴──────────┴────────┴──────────┴───────────────┴───────────┴──────────┴──────────────────┘"
    );
    println!();
}

// ── Benchmark runner ───────────────────────────────────────────────────

/// Run one full benchmark for a given (scenario, table_size, change_rate).
///
/// Creates one container with bench-tuned resource constraints, sets up
/// the schema, runs warm-up cycles, and then measures FULL/DIFFERENTIAL
/// refreshes for fair comparison. Captures `[PGS_PROFILE]` data for
/// DIFFERENTIAL cycles.
async fn run_benchmark(
    scenario: &QueryScenario,
    table_size: usize,
    change_pct: f64,
) -> Vec<BenchResult> {
    let db = E2eDb::new_bench().await.with_extension().await;
    let cid = db.container_id().to_string();

    // Create source table
    db.execute(
        "CREATE TABLE src (
             id SERIAL PRIMARY KEY,
             region TEXT NOT NULL,
             category TEXT NOT NULL,
             amount INT NOT NULL,
             score INT NOT NULL
         )",
    )
    .await;

    // Populate source
    db.execute(&bulk_insert_single(table_size)).await;

    // Create dimension table if needed
    if scenario.needs_dim {
        db.execute(create_join_table()).await;
        db.execute(populate_dim()).await;
    }

    // ANALYZE for stable query plans
    db.execute("ANALYZE src").await;
    if scenario.needs_dim {
        db.execute("ANALYZE dim").await;
    }

    let mut results = Vec::new();

    // ── FULL mode benchmark ────────────────────────────────────────
    let full_pgs_name = format!("bench_{}_full", scenario.name);
    db.create_st(&full_pgs_name, scenario.query, "1m", "FULL")
        .await;

    // Warm-up cycles (throw-away, not measured)
    for _ in 0..WARMUP_CYCLES {
        apply_changes(&db, table_size, change_pct).await;
        db.execute("ANALYZE src").await;
        db.refresh_st(&full_pgs_name).await;
    }

    // Measured cycles
    for cycle in 1..=CYCLES {
        apply_changes(&db, table_size, change_pct).await;
        db.execute("ANALYZE src").await;

        let start = Instant::now();
        db.refresh_st(&full_pgs_name).await;
        let elapsed = start.elapsed();

        let row_count = db.count(&format!("public.{full_pgs_name}")).await;

        results.push(BenchResult {
            scenario: scenario.name.to_string(),
            table_size,
            change_pct,
            mode: "FULL".to_string(),
            cycle,
            refresh_ms: elapsed.as_secs_f64() * 1000.0,
            st_row_count: row_count,
            profile: None,
        });
    }

    db.drop_st(&full_pgs_name).await;

    // ── Re-populate source for DIFFERENTIAL (to have same starting point) ──
    db.execute("TRUNCATE src RESTART IDENTITY").await;
    db.execute(&bulk_insert_single(table_size)).await;
    db.execute("ANALYZE src").await;

    // ── DIFFERENTIAL mode benchmark ─────────────────────────────────
    let inc_pgs_name = format!("bench_{}_inc", scenario.name);
    db.create_st(&inc_pgs_name, scenario.query, "1m", "DIFFERENTIAL")
        .await;

    // Warm-up cycles (throw-away, not measured)
    for _ in 0..WARMUP_CYCLES {
        apply_changes(&db, table_size, change_pct).await;
        db.execute("ANALYZE src").await;
        db.refresh_st(&inc_pgs_name).await;
    }

    // Measured cycles with profile capture
    for cycle in 1..=CYCLES {
        apply_changes(&db, table_size, change_pct).await;
        db.execute("ANALYZE src").await;

        let start = Instant::now();
        db.refresh_st(&inc_pgs_name).await;
        let elapsed = start.elapsed();

        let row_count = db.count(&format!("public.{inc_pgs_name}")).await;

        // Capture [PGS_PROFILE] from container logs
        let profile = extract_last_profile(&cid).await;

        results.push(BenchResult {
            scenario: scenario.name.to_string(),
            table_size,
            change_pct,
            mode: "DIFFERENTIAL".to_string(),
            cycle,
            refresh_ms: elapsed.as_secs_f64() * 1000.0,
            st_row_count: row_count,
            profile,
        });
    }

    results
}

// ── Individual benchmark tests ─────────────────────────────────────────
//
// Each test is #[ignore] so it doesn't run in normal CI.
// Run all benches: cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture

#[tokio::test]
#[ignore]
async fn bench_scan_10k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0]; // scan
    let results = run_benchmark(s, 10_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_scan_10k_10pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0];
    let results = run_benchmark(s, 10_000, 0.10).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_scan_10k_50pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0];
    let results = run_benchmark(s, 10_000, 0.50).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_filter_10k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[1]; // filter
    let results = run_benchmark(s, 10_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_aggregate_10k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[2]; // aggregate
    let results = run_benchmark(s, 10_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_join_10k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[3]; // join
    let results = run_benchmark(s, 10_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_join_agg_10k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[4]; // join_agg
    let results = run_benchmark(s, 10_000, 0.01).await;
    print_results_table(&results);
}

// ── 100K row benchmarks ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn bench_scan_100k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0];
    let results = run_benchmark(s, 100_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_scan_100k_10pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0];
    let results = run_benchmark(s, 100_000, 0.10).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_scan_100k_50pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[0];
    let results = run_benchmark(s, 100_000, 0.50).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_aggregate_100k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[2];
    let results = run_benchmark(s, 100_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_aggregate_100k_10pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[2];
    let results = run_benchmark(s, 100_000, 0.10).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_join_agg_100k_1pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[4];
    let results = run_benchmark(s, 100_000, 0.01).await;
    print_results_table(&results);
}

#[tokio::test]
#[ignore]
async fn bench_join_agg_100k_10pct() {
    let scenarios = query_scenarios();
    let s = &scenarios[4];
    let results = run_benchmark(s, 100_000, 0.10).await;
    print_results_table(&results);
}

// ── Full matrix benchmark ──────────────────────────────────────────────
//
// Runs ALL combinations of dimensions: 5 queries × 2 sizes × 3 rates = 30 runs.
// Each run does CYCLES full + CYCLES differential refreshes.
// Expect ~15-30 minutes depending on hardware.

#[tokio::test]
#[ignore]
async fn bench_full_matrix() {
    let scenarios = query_scenarios();
    let mut all_results = Vec::new();

    for table_size in TABLE_SIZES {
        for change_pct in CHANGE_RATES {
            for scenario in &scenarios {
                eprintln!(
                    "▶ Benchmarking: {} | {}rows | {:.0}% changes ...",
                    scenario.name,
                    table_size,
                    change_pct * 100.0,
                );
                let results = run_benchmark(scenario, *table_size, *change_pct).await;
                all_results.extend(results);
            }
        }
    }

    print_results_table(&all_results);
}

// ── NO_DATA refresh latency benchmark ──────────────────────────────────

#[tokio::test]
#[ignore]
async fn bench_no_data_refresh_latency() {
    let db = E2eDb::new_bench().await.with_extension().await;

    db.execute("CREATE TABLE src_nd (id SERIAL PRIMARY KEY, val INT NOT NULL)")
        .await;
    db.execute("INSERT INTO src_nd (val) SELECT i FROM generate_series(1, 10000) AS s(i)")
        .await;

    db.create_st("nd_st", "SELECT id, val FROM src_nd", "1m", "DIFFERENTIAL")
        .await;

    // No changes → refresh should be near-zero cost
    let mut times = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        db.refresh_st("nd_st").await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }

    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let max_ms = times.iter().cloned().fold(0.0f64, f64::max);

    println!();
    println!("┌──────────────────────────────────────────────┐");
    println!("│ NO_DATA Refresh Latency (10 iterations)      │");
    println!("├──────────────────────────────────────────────┤");
    println!("│ Avg: {:>8.2} ms                             │", avg);
    println!("│ Max: {:>8.2} ms                             │", max_ms);
    println!("│ Target: < 10 ms                              │");
    println!(
        "│ Status: {}                              │",
        if avg < 10.0 {
            "✅ PASS"
        } else {
            "⚠️ SLOW"
        }
    );
    println!("└──────────────────────────────────────────────┘");
    println!();
}
