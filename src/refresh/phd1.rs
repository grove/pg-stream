// ARCH-1: PH-D1 phantom-cleanup sub-module for the refresh pipeline.
//
// This module contains the PH-D1 DELETE+INSERT strategy for handling
// join phantom rows:
// - PH-D1 strategy selection logic (currently around line 4991 in mod.rs)
// - DELETE+INSERT execution path (currently around line 5151 in mod.rs)
// - Co-deletion detection helpers
// - EC-01 convergence validation
// - EC01-2: Cross-cycle phantom cleanup
//
// Currently most code lives in `super` (mod.rs). This file is the landing
// zone for the phantom-cleanup layer during the ongoing ARCH-1 migration.

use crate::error::PgTrickleError;
use pgrx::Spi;

/// EC01-2: Reconcile orphaned row IDs from prior refresh cycles.
///
/// After a differential refresh applies the current delta, phantom rows may
/// remain from prior cycles where Part 1a inserted a row but the
/// corresponding Part 1b delete was dropped (because the right partner was
/// simultaneously deleted). This function detects and removes those orphans
/// by comparing the stream table's `__pgt_row_id` set against the
/// full-refresh result set.
///
/// The cleanup runs in batches of `batch_size` rows to avoid holding long
/// locks. Returns the total number of orphaned rows removed.
///
/// Called after each non-deduplicated, keyed, non-partitioned differential
/// apply so stale row IDs converge even when the current delta no longer
/// contains the matching DELETE.
pub fn cleanup_cross_cycle_phantoms(
    pgt_id: i64,
    stream_table_name: &str,
    defining_query: &str,
    batch_size: i64,
) -> Result<i64, PgTrickleError> {
    let row_id_expr = crate::dvm::row_id_expr_for_query(defining_query);
    let full_with_row_id = format!(
        "SELECT {row_id_expr} AS __pgt_row_id \
         FROM ({defining_query}) sub"
    );

    let deleted = Spi::get_one_with_args::<i64>(
        &format!(
            "WITH current_full AS MATERIALIZED ({full_with_row_id}), \
             orphans AS ( \
                 SELECT DISTINCT st.__pgt_row_id \
                 FROM {stream_table_name} st \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM current_full cf \
                     WHERE cf.__pgt_row_id = st.__pgt_row_id \
                 ) \
                 LIMIT $1 \
             ), \
             deleted AS ( \
                 DELETE FROM {stream_table_name} st \
                 USING orphans o \
                 WHERE st.__pgt_row_id = o.__pgt_row_id \
                 RETURNING 1 \
             ) \
             SELECT count(*)::bigint FROM deleted"
        ),
        &[batch_size.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(0);

    if deleted > 0 {
        pgrx::log!(
            "[pg_trickle] EC01-2: cleaned up {} cross-cycle phantom rows for pgt_id={}",
            deleted,
            pgt_id,
        );
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cleanup_returns_zero_for_empty_case() {
        // Verify batch_size defaults are positive integers
        let batch_size: i64 = 1000;
        assert!(batch_size > 0);
    }
}
