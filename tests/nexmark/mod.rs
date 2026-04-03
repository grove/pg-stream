//! Shared Nexmark benchmark helpers.
//!
//! Modelled on the TPC-H `tpch/mod.rs` helpers. Each integration test binary
//! includes this module via `mod nexmark;` and gets access to schema loading,
//! data generation, RF mutation helpers, and the invariant assertion function.

#![allow(dead_code)]

// ── Configuration ───────────────────────────────────────────────────────

/// Number of persons at the default scale.
pub fn nm_persons() -> usize {
    std::env::var("NEXMARK_PERSONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
}

/// Number of auctions at the default scale.
pub fn nm_auctions() -> usize {
    std::env::var("NEXMARK_AUCTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

/// Number of bids at the default scale.
pub fn nm_bids() -> usize {
    std::env::var("NEXMARK_BIDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}

/// Number of refresh cycles per query.
pub fn cycles() -> usize {
    std::env::var("NEXMARK_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

/// Number of bid rows affected per RF cycle.
pub fn rf_count() -> usize {
    std::env::var("NEXMARK_RF_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or((nm_bids() / 20).max(10))
}

// ── SQL file embedding ──────────────────────────────────────────────────

pub const SCHEMA_SQL: &str = include_str!("schema.sql");
pub const DATAGEN_SQL: &str = include_str!("datagen.sql");
pub const RF1_SQL: &str = include_str!("rf1.sql");
pub const RF2_SQL: &str = include_str!("rf2.sql");
pub const RF3_SQL: &str = include_str!("rf3.sql");

// ── Token substitution ──────────────────────────────────────────────────

/// Replace scale tokens in a SQL template.
pub fn substitute_scale(sql: &str) -> String {
    sql.replace("__NM_PERSONS__", &nm_persons().to_string())
        .replace("__NM_AUCTIONS__", &nm_auctions().to_string())
        .replace("__NM_BIDS__", &nm_bids().to_string())
}

/// Replace RF tokens in a mutation SQL template.
pub fn substitute_rf(sql: &str, next_person: usize, next_auction: usize) -> String {
    sql.replace("__RF_COUNT__", &rf_count().to_string())
        .replace("__NM_NEXT_PERSON__", &next_person.to_string())
        .replace("__NM_NEXT_AUCTION__", &next_auction.to_string())
        .replace("__NM_PERSONS__", &nm_persons().to_string())
        .replace("__NM_AUCTIONS__", &nm_auctions().to_string())
}

/// Replace RF count token in deletion/update SQL.
pub fn substitute_rf_simple(sql: &str) -> String {
    sql.replace("__RF_COUNT__", &rf_count().to_string())
}

// ── Database helpers ────────────────────────────────────────────────────

use super::e2e::E2eDb;

/// Execute each semicolon-delimited SQL statement, skipping blanks.
pub async fn exec_sql(db: &E2eDb, sql: &str) {
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

/// Load the Nexmark schema.
pub async fn load_schema(db: &E2eDb) {
    exec_sql(db, SCHEMA_SQL).await;
}

/// Load Nexmark data at the configured scale.
pub async fn load_data(db: &E2eDb) {
    let sql = substitute_scale(DATAGEN_SQL);
    exec_sql(db, &sql).await;
}

/// Current max person ID.
pub async fn max_person_id(db: &E2eDb) -> usize {
    let max: i64 = db
        .query_scalar("SELECT COALESCE(MAX(id), 0)::bigint FROM person")
        .await;
    max as usize
}

/// Current max auction ID.
pub async fn max_auction_id(db: &E2eDb) -> usize {
    let max: i64 = db
        .query_scalar("SELECT COALESCE(MAX(id), 0)::bigint FROM auction")
        .await;
    max as usize
}

/// Apply RF1 (bulk INSERTs: persons + auctions + bids).
pub async fn apply_rf1(db: &E2eDb, next_person: usize, next_auction: usize) {
    let sql = substitute_rf(RF1_SQL, next_person, next_auction);
    exec_sql(db, &sql).await;
}

/// Apply RF2 (bulk DELETEs: bids → auctions → persons).
pub async fn apply_rf2(db: &E2eDb) {
    let sql = substitute_rf_simple(RF2_SQL);
    exec_sql(db, &sql).await;
}

/// Apply RF3 (targeted UPDATEs: prices, reserves, cities).
pub async fn apply_rf3(db: &E2eDb) {
    let sql = substitute_rf_simple(RF3_SQL);
    exec_sql(db, &sql).await;
}

/// Assert a stream table is multiset-equal to its defining query.
///
/// Also checks for negative `__pgt_count` (over-retraction bug).
/// Returns `Ok(())` on match; `Err(description)` on mismatch.
pub async fn assert_invariant(
    db: &E2eDb,
    st_name: &str,
    query: &str,
    qname: &str,
    cycle: usize,
) -> Result<(), String> {
    let st_table = format!("public.{st_name}");

    let cols: String = db
        .query_scalar(&format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) \
             FROM information_schema.columns \
             WHERE (table_schema || '.' || table_name = 'public.{st_name}' \
                OR table_name = '{st_name}') \
             AND left(column_name, 6) <> '__pgt_'"
        ))
        .await;

    // Negative __pgt_count guard.
    let has_pgt_count: bool = db
        .query_scalar(&format!(
            "SELECT EXISTS ( \
                SELECT 1 FROM information_schema.columns \
                WHERE table_name = '{st_name}' \
                  AND column_name = '__pgt_count' \
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
                "NEGATIVE __pgt_count: {qname} cycle {cycle} -- \
                 {neg_count} rows with __pgt_count < 0 (over-retraction bug)"
            ));
        }
    }

    // Multiset equality via symmetric EXCEPT ALL.
    let matches: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS ( \
                (SELECT {cols} FROM {st_table} EXCEPT ALL ({query})) \
                UNION ALL \
                (({query}) EXCEPT ALL SELECT {cols} FROM {st_table}) \
            )"
        ))
        .await;

    if matches {
        return Ok(());
    }

    // Diagnostics on mismatch.
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

    Err(format!(
        "MISMATCH: {qname} cycle {cycle} -- \
         ST has {st_count} rows, query has {q_count}; \
         {extra} extra in ST, {missing} missing from ST"
    ))
}
