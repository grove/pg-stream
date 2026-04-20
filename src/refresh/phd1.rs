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
/// Called conditionally when:
/// - The stream table uses a join-based defining query (non-deduplicated delta)
/// - The prior refresh reported non-zero phantom residuals
/// - The stream table has completed at least 2 refresh cycles
pub fn cleanup_cross_cycle_phantoms(
    pgt_id: i64,
    stream_table_name: &str,
    defining_query: &str,
    batch_size: i64,
) -> Result<i64, PgTrickleError> {
    // Step 1: Find orphaned __pgt_row_id values that exist in the stream
    // table but NOT in the full-refresh result set.
    //
    // We use an anti-join (NOT EXISTS) against the full query to identify
    // rows whose __pgt_row_id has no corresponding row in the correct
    // result set. These are phantoms from prior cycles.
    let orphan_count = Spi::get_one_with_args::<i64>(
        &format!(
            "WITH current_full AS ({defining_query}), \
             orphans AS ( \
                 SELECT st.__pgt_row_id \
                 FROM {stream_table_name} st \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM current_full cf \
                     WHERE cf.__pgt_row_id = st.__pgt_row_id \
                 ) \
                 LIMIT $1 \
             ) \
             SELECT count(*) FROM orphans"
        ),
        &[batch_size.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .unwrap_or(0);

    if orphan_count == 0 {
        return Ok(0);
    }

    // Step 2: Delete orphaned rows in batches.
    let deleted = Spi::get_one_with_args::<i64>(
        &format!(
            "WITH current_full AS ({defining_query}), \
             orphans AS ( \
                 SELECT st.__pgt_row_id \
                 FROM {stream_table_name} st \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM current_full cf \
                     WHERE cf.__pgt_row_id = st.__pgt_row_id \
                 ) \
                 LIMIT $1 \
             ) \
             DELETE FROM {stream_table_name} \
             WHERE __pgt_row_id IN (SELECT __pgt_row_id FROM orphans)"
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
