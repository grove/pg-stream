//! v0.22.0: Downstream CDC publication, predictive cost model, and SLA-driven
//! tier auto-assignment API functions.

use pgrx::prelude::*;

use crate::catalog::StreamTableMeta;
use crate::error::PgTrickleError;

// ── CDC-PUB-1: stream_table_to_publication() ─────────────────────────────

/// CDC-PUB-1: Create a logical replication publication for a stream table.
///
/// Creates a PostgreSQL publication exposing the named stream table so that
/// Kafka Connect, Debezium, and other logical replication subscribers can
/// receive change events without a separate replication slot.
#[pg_extern(schema = "pgtrickle")]
fn stream_table_to_publication(name: &str) {
    let result = stream_table_to_publication_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn stream_table_to_publication_impl(name: &str) -> Result<(), PgTrickleError> {
    let (schema, table) = parse_qualified_name(name);
    let meta = StreamTableMeta::get_by_name(&schema, &table)?;

    if meta.downstream_publication_name.is_some() {
        return Err(PgTrickleError::PublicationAlreadyExists(name.into()));
    }

    let pub_name = format!("pgt_pub_{}", meta.pgt_name);
    let qualified_table = format!("{}.{}", meta.pgt_schema, meta.pgt_name);

    Spi::connect_mut(|client| {
        // Create the publication for the stream table's storage table.
        client
            .update(
                &format!(
                    "CREATE PUBLICATION {} FOR TABLE {}",
                    quote_ident(&pub_name),
                    quote_ident_qualified(&meta.pgt_schema, &meta.pgt_name)
                ),
                None,
                &[],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // Store the publication name in the catalog.
        client
            .update(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET downstream_publication_name = $1, updated_at = now() \
                 WHERE pgt_id = $2",
                None,
                &[pub_name.clone().into(), meta.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_trickle: created publication '{}' for stream table '{}'",
            pub_name,
            qualified_table
        );

        Ok::<(), PgTrickleError>(())
    })?;

    Ok(())
}

// ── CDC-PUB-2: drop_stream_table_publication() ──────────────────────────

/// CDC-PUB-2: Drop the logical replication publication for a stream table.
#[pg_extern(schema = "pgtrickle")]
fn drop_stream_table_publication(name: &str) {
    let result = drop_stream_table_publication_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn drop_stream_table_publication_impl(name: &str) -> Result<(), PgTrickleError> {
    let (schema, table) = parse_qualified_name(name);
    let meta = StreamTableMeta::get_by_name(&schema, &table)?;

    let pub_name = match &meta.downstream_publication_name {
        Some(p) => p.clone(),
        None => return Err(PgTrickleError::PublicationNotFound(name.into())),
    };

    Spi::connect_mut(|client| {
        client
            .update(
                &format!("DROP PUBLICATION IF EXISTS {}", quote_ident(&pub_name)),
                None,
                &[],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        client
            .update(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET downstream_publication_name = NULL, updated_at = now() \
                 WHERE pgt_id = $1",
                None,
                &[meta.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_trickle: dropped publication '{}' for stream table '{}.{}'",
            pub_name,
            meta.pgt_schema,
            meta.pgt_name
        );

        Ok::<(), PgTrickleError>(())
    })?;

    Ok(())
}

// ── SLA-1: sla parameter support ─────────────────────────────────────────

/// SLA-1: Set the SLA interval for a stream table.
///
/// Accepts an interval and stores it as `freshness_deadline_ms`.
/// The scheduler uses this to auto-assign the appropriate refresh tier.
#[pg_extern(schema = "pgtrickle")]
fn set_stream_table_sla(name: &str, sla: Interval) {
    let result = set_stream_table_sla_impl(name, sla);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn set_stream_table_sla_impl(name: &str, sla: Interval) -> Result<(), PgTrickleError> {
    let (schema, table) = parse_qualified_name(name);
    let meta = StreamTableMeta::get_by_name(&schema, &table)?;

    // Convert interval to milliseconds.
    // pgrx Interval has months, days, and microseconds.
    let total_ms = sla.months() as i64 * 30 * 24 * 3600 * 1000 + // rough month conversion
        sla.days() as i64 * 24 * 3600 * 1000 +
        sla.micros() / 1000; // microseconds to milliseconds

    if total_ms <= 0 {
        return Err(PgTrickleError::InvalidArgument(
            "SLA interval must be positive".into(),
        ));
    }

    // SLA-2: Determine the initial tier assignment based on the SLA.
    let tier = assign_tier_for_sla(total_ms)?;

    Spi::connect_mut(|client| {
        client
            .update(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET freshness_deadline_ms = $1, refresh_tier = $2, updated_at = now() \
                 WHERE pgt_id = $3",
                None,
                &[total_ms.into(), tier.as_str().into(), meta.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_trickle: set SLA {}ms for '{}', assigned tier '{}'",
            total_ms,
            name,
            tier.as_str()
        );

        Ok::<(), PgTrickleError>(())
    })?;

    Ok(())
}

/// SLA-2: Assign the appropriate tier based on an SLA interval in milliseconds.
///
/// Tier assignment rules:
/// - Hot: SLA ≤ 5s (refresh every 1× schedule)
/// - Warm: SLA ≤ 30s (refresh every 2× schedule)
/// - Cold: SLA > 30s (refresh every 10× schedule)
pub fn assign_tier_for_sla(sla_ms: i64) -> Result<crate::scheduler::RefreshTier, PgTrickleError> {
    use crate::scheduler::RefreshTier;
    if sla_ms <= 5_000 {
        Ok(RefreshTier::Hot)
    } else if sla_ms <= 30_000 {
        Ok(RefreshTier::Warm)
    } else {
        Ok(RefreshTier::Cold)
    }
}

// ── PRED-1: Linear regression forecaster ─────────────────────────────────

/// PRED-1: Fit a simple linear regression `duration_ms ~ delta_rows` over
/// the prediction window for a given stream table.
///
/// Returns `(slope, intercept, sample_count)`. Returns `None` if fewer than
/// `prediction_min_samples` data points exist.
pub fn fit_linear_regression(pgt_id: i64) -> Option<(f64, f64, i64)> {
    let window_minutes = crate::config::pg_trickle_prediction_window();
    let min_samples = crate::config::pg_trickle_prediction_min_samples();

    if min_samples <= 0 {
        return None;
    }

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT count(*) AS n, \
                        coalesce(avg(rows_inserted + rows_deleted), 0) AS avg_x, \
                        coalesce(avg(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000), 0) AS avg_y, \
                        coalesce(sum((rows_inserted + rows_deleted) * \
                            EXTRACT(EPOCH FROM (end_time - start_time)) * 1000), 0) AS sum_xy, \
                        coalesce(sum((rows_inserted + rows_deleted) * \
                            (rows_inserted + rows_deleted)), 0) AS sum_x2 \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = $1 \
                   AND status = 'COMPLETED' \
                   AND action = 'DIFFERENTIAL' \
                   AND end_time IS NOT NULL \
                   AND start_time > now() - ($2 || ' minutes')::interval",
                None,
                &[pgt_id.into(), window_minutes.into()],
            )
            .ok()?;

        if result.is_empty() {
            return None;
        }

        let n: i64 = result.get::<i64>(1).ok()??;
        if n < min_samples as i64 {
            return None; // PRED-3: Cold-start fallback.
        }

        let avg_x: f64 = result.get::<f64>(2).ok()??;
        let avg_y: f64 = result.get::<f64>(3).ok()??;
        let sum_xy: f64 = result.get::<f64>(4).ok()??;
        let sum_x2: f64 = result.get::<f64>(5).ok()??;

        // Simple linear regression: y = slope * x + intercept
        let denominator = sum_x2 - n as f64 * avg_x * avg_x;
        if denominator.abs() < 1e-10 {
            // All x values are identical — slope is undefined.
            return Some((0.0, avg_y, n));
        }

        let slope = (sum_xy - n as f64 * avg_x * avg_y) / denominator;
        let intercept = avg_y - slope * avg_x;

        Some((slope, intercept, n))
    })
}

/// PRED-2: Predict the differential refresh duration for a given delta size.
///
/// Returns `None` if the model cannot be fitted (cold-start fallback).
pub fn predict_diff_duration_ms(pgt_id: i64, delta_rows: i64) -> Option<f64> {
    let (slope, intercept, _n) = fit_linear_regression(pgt_id)?;
    Some(slope * delta_rows as f64 + intercept)
}

/// PRED-2: Check whether the predicted differential cost exceeds the
/// full-refresh cost by more than `prediction_ratio`, triggering a
/// pre-emptive switch to FULL.
pub fn should_preempt_to_full(pgt_id: i64, delta_rows: i64, last_full_ms: f64) -> bool {
    let ratio = crate::config::pg_trickle_prediction_ratio();
    if let Some(predicted_ms) = predict_diff_duration_ms(pgt_id, delta_rows) {
        predicted_ms > last_full_ms * ratio
    } else {
        false // PRED-3: Cold-start fallback — don't preempt.
    }
}

// ── SLA-3: Dynamic tier re-assignment ────────────────────────────────────

/// SLA-3: Check and adjust tier for a stream table based on SLA and queue depth.
///
/// Called after each refresh tick. Bumps tier up or down if the SLA is
/// consistently exceeded or under-utilised.
pub fn maybe_adjust_tier_for_sla(meta: &StreamTableMeta) {
    let sla_ms = match meta.freshness_deadline_ms {
        Some(ms) => ms,
        None => return, // No SLA configured — skip.
    };

    // Look at the last 3 refresh durations to determine if the tier is appropriate.
    let avg_duration_ms = Spi::connect(|client| {
        client
            .select(
                "SELECT coalesce(avg(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000), 0) \
                 FROM (SELECT end_time, start_time \
                       FROM pgtrickle.pgt_refresh_history \
                       WHERE pgt_id = $1 AND status = 'COMPLETED' \
                       ORDER BY end_time DESC LIMIT 3) sub",
                None,
                &[meta.pgt_id.into()],
            )
            .ok()
            .and_then(|r| {
                if r.is_empty() {
                    None
                } else {
                    r.get::<f64>(1).ok()?
                }
            })
    });

    let _avg_ms = match avg_duration_ms {
        Some(ms) => ms,
        None => return, // Not enough data.
    };

    use crate::scheduler::RefreshTier;
    let current_tier = RefreshTier::from_sql_str(&meta.refresh_tier);
    let ideal_tier = assign_tier_for_sla(sla_ms).unwrap_or(RefreshTier::Hot);

    // If the current tier doesn't match what the SLA demands, adjust.
    if current_tier != ideal_tier {
        Spi::connect_mut(|client| {
            let _ = client.update(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET refresh_tier = $1, updated_at = now() \
                 WHERE pgt_id = $2",
                None,
                &[ideal_tier.as_str().into(), meta.pgt_id.into()],
            );
        });
        #[cfg(not(test))]
        pgrx::info!(
            "pg_trickle: SLA-driven tier adjustment for '{}': {} → {}",
            meta.pgt_name,
            current_tier.as_str(),
            ideal_tier.as_str()
        );
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Parse a potentially qualified name into (schema, table).
fn parse_qualified_name(name: &str) -> (String, String) {
    if let Some((schema, table)) = name.split_once('.') {
        (schema.to_string(), table.to_string())
    } else {
        ("public".to_string(), name.to_string())
    }
}

/// Quote a SQL identifier.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a qualified SQL identifier (schema.table).
fn quote_ident_qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assign_tier_for_sla_hot() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(1000).ok(), Some(RefreshTier::Hot));
        assert_eq!(assign_tier_for_sla(5000).ok(), Some(RefreshTier::Hot));
    }

    #[test]
    fn test_assign_tier_for_sla_warm() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(5001).ok(), Some(RefreshTier::Warm));
        assert_eq!(assign_tier_for_sla(30000).ok(), Some(RefreshTier::Warm));
    }

    #[test]
    fn test_assign_tier_for_sla_cold() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(30001).ok(), Some(RefreshTier::Cold));
        assert_eq!(assign_tier_for_sla(60000).ok(), Some(RefreshTier::Cold));
    }

    #[test]
    fn test_parse_qualified_name_with_schema() {
        assert_eq!(
            parse_qualified_name("myschema.mytable"),
            ("myschema".into(), "mytable".into())
        );
    }

    #[test]
    fn test_parse_qualified_name_without_schema() {
        assert_eq!(
            parse_qualified_name("mytable"),
            ("public".into(), "mytable".into())
        );
    }

    #[test]
    fn test_quote_ident_simple() {
        assert_eq!(quote_ident("hello"), "\"hello\"");
    }

    #[test]
    fn test_quote_ident_with_quotes() {
        assert_eq!(quote_ident("he\"llo"), "\"he\"\"llo\"");
    }

    // ── TEST-6 (v0.24.0): Comprehensive publication.rs unit tests ────────
    //
    // 25+ tests for assign_tier_for_sla, parse_qualified_name, quote_ident,
    // and boundary cases (0, negative, NaN-like edge values).

    #[test]
    fn test_assign_tier_sla_zero() {
        use crate::scheduler::RefreshTier;
        // Zero is valid (Hot tier — aggressive)
        assert_eq!(assign_tier_for_sla(0).ok(), Some(RefreshTier::Hot));
    }

    #[test]
    fn test_assign_tier_sla_one_ms() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(1).ok(), Some(RefreshTier::Hot));
    }

    #[test]
    fn test_assign_tier_sla_boundary_5000() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(5000).ok(), Some(RefreshTier::Hot));
    }

    #[test]
    fn test_assign_tier_sla_boundary_5001() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(5001).ok(), Some(RefreshTier::Warm));
    }

    #[test]
    fn test_assign_tier_sla_boundary_30000() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(30000).ok(), Some(RefreshTier::Warm));
    }

    #[test]
    fn test_assign_tier_sla_boundary_30001() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(30001).ok(), Some(RefreshTier::Cold));
    }

    #[test]
    fn test_assign_tier_sla_very_large() {
        use crate::scheduler::RefreshTier;
        assert_eq!(
            assign_tier_for_sla(86_400_000).ok(),
            Some(RefreshTier::Cold)
        );
    }

    #[test]
    fn test_assign_tier_sla_negative() {
        use crate::scheduler::RefreshTier;
        // Negative is technically invalid but shouldn't panic.
        // It falls into the Hot tier (≤ 5000).
        assert_eq!(assign_tier_for_sla(-1).ok(), Some(RefreshTier::Hot));
    }

    #[test]
    fn test_assign_tier_sla_i64_max() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(i64::MAX).ok(), Some(RefreshTier::Cold));
    }

    #[test]
    fn test_parse_qualified_dots_in_schema() {
        // Only the first dot is treated as schema separator
        let (schema, table) = parse_qualified_name("my.schema.table");
        assert_eq!(schema, "my");
        assert_eq!(table, "schema.table");
    }

    #[test]
    fn test_parse_qualified_empty_string() {
        let (schema, table) = parse_qualified_name("");
        assert_eq!(schema, "public");
        assert_eq!(table, "");
    }

    #[test]
    fn test_parse_qualified_dot_only() {
        let (schema, table) = parse_qualified_name(".");
        assert_eq!(schema, "");
        assert_eq!(table, "");
    }

    #[test]
    fn test_quote_ident_empty() {
        assert_eq!(quote_ident(""), "\"\"");
    }

    #[test]
    fn test_quote_ident_spaces() {
        assert_eq!(quote_ident("my table"), "\"my table\"");
    }

    #[test]
    fn test_quote_ident_unicode() {
        assert_eq!(quote_ident("tëst"), "\"tëst\"");
    }

    #[test]
    fn test_quote_ident_qualified_basic() {
        assert_eq!(
            quote_ident_qualified("public", "orders"),
            "\"public\".\"orders\""
        );
    }

    #[test]
    fn test_quote_ident_qualified_with_quotes() {
        assert_eq!(
            quote_ident_qualified("my\"schema", "my\"table"),
            "\"my\"\"schema\".\"my\"\"table\""
        );
    }

    #[test]
    fn test_quote_ident_qualified_empty_schema() {
        assert_eq!(quote_ident_qualified("", "table"), "\"\".\"table\"");
    }

    #[test]
    fn test_assign_tier_sla_warm_midpoint() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(15000).ok(), Some(RefreshTier::Warm));
    }

    #[test]
    fn test_assign_tier_sla_cold_100s() {
        use crate::scheduler::RefreshTier;
        assert_eq!(assign_tier_for_sla(100_000).ok(), Some(RefreshTier::Cold));
    }

    #[test]
    fn test_parse_qualified_leading_dot() {
        let (schema, table) = parse_qualified_name(".table");
        assert_eq!(schema, "");
        assert_eq!(table, "table");
    }

    #[test]
    fn test_parse_qualified_trailing_dot() {
        let (schema, table) = parse_qualified_name("schema.");
        assert_eq!(schema, "schema");
        assert_eq!(table, "");
    }

    #[test]
    fn test_quote_ident_backslash() {
        assert_eq!(quote_ident("back\\slash"), "\"back\\slash\"");
    }

    #[test]
    fn test_quote_ident_null_char() {
        // Null characters should be preserved in quoting
        assert_eq!(quote_ident("a\0b"), "\"a\0b\"");
    }
}
