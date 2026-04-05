//! G14-SHC: Catalog-backed cross-backend template cache.
//!
//! Eliminates cold-start latency when new backends connect by persisting
//! delta SQL templates in an UNLOGGED catalog table (`pgtrickle.pgt_template_cache`).
//!
//! ## Cache hierarchy
//!
//! 1. **L1 — thread-local** (`DELTA_TEMPLATE_CACHE` in `dvm/mod.rs`):
//!    In-process HashMap, ~0 ns lookup.  Flushed on `CACHE_GENERATION` bump.
//!
//! 2. **L2 — catalog table** (`pgtrickle.pgt_template_cache`):
//!    Cross-backend persistent cache.  ~1 ms SPI lookup on L1 miss.
//!    Eliminates the ~45 ms DVM parse+differentiate cost for cold backends.
//!
//! Templates are validated by `defining_query_hash` — if the hash doesn't
//! match, the entry is treated as stale and repopulated.

use pgrx::spi::Spi;

use crate::error::PgTrickleError;

/// A template entry loaded from or stored to the catalog cache table.
pub struct CachedTemplate {
    pub delta_sql_template: String,
    pub output_columns: Vec<String>,
    pub source_oids: Vec<u32>,
    pub is_deduplicated: bool,
    pub has_key_changed: bool,
    pub is_all_algebraic: bool,
}

/// Look up a cached delta template from the catalog table.
///
/// Returns `Some(template)` if a valid entry exists for the given `pgt_id`
/// and `defining_query_hash`, or `None` if no entry or hash mismatch.
///
/// Cost: ~1 ms (single SPI SELECT).
pub fn lookup(pgt_id: i64, defining_query_hash: u64) -> Option<CachedTemplate> {
    if !crate::config::pg_trickle_template_cache_enabled() {
        return None;
    }

    let hash_i64 = defining_query_hash as i64;

    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT delta_sql, columns, source_oids, is_dedup, key_changed, all_algebraic \
                 FROM pgtrickle.pgt_template_cache \
                 WHERE pgt_id = $1 AND query_hash = $2",
                None,
                &[pgt_id.into(), hash_i64.into()],
            )
            .ok()?;

        if table.is_empty() {
            return None;
        }

        let row = table.first();

        let delta_sql: String = row.get::<String>(1).ok().flatten()?;
        let columns: Vec<String> = row.get::<Vec<String>>(2).ok().flatten().unwrap_or_default();
        let source_oids_i32: Vec<i32> = row.get::<Vec<i32>>(3).ok().flatten().unwrap_or_default();
        let is_dedup: bool = row.get::<bool>(4).ok().flatten().unwrap_or(false);
        let key_changed: bool = row.get::<bool>(5).ok().flatten().unwrap_or(false);
        let all_algebraic: bool = row.get::<bool>(6).ok().flatten().unwrap_or(false);

        let source_oids: Vec<u32> = source_oids_i32.into_iter().map(|v| v as u32).collect();

        Some(CachedTemplate {
            delta_sql_template: delta_sql,
            output_columns: columns,
            source_oids,
            is_deduplicated: is_dedup,
            has_key_changed: key_changed,
            is_all_algebraic: all_algebraic,
        })
    })
}

/// Store (or update) a delta template in the catalog cache table.
///
/// Uses INSERT ... ON CONFLICT DO UPDATE to handle both first-time
/// population and hash-mismatch updates.
pub fn store(
    pgt_id: i64,
    defining_query_hash: u64,
    template: &CachedTemplate,
) -> Result<(), PgTrickleError> {
    if !crate::config::pg_trickle_template_cache_enabled() {
        return Ok(());
    }

    let hash_i64 = defining_query_hash as i64;
    let source_oids_i32: Vec<i32> = template.source_oids.iter().map(|&v| v as i32).collect();

    Spi::run_with_args(
        "INSERT INTO pgtrickle.pgt_template_cache \
         (pgt_id, query_hash, delta_sql, columns, source_oids, \
          is_dedup, key_changed, all_algebraic, cached_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now()) \
         ON CONFLICT (pgt_id) DO UPDATE SET \
           query_hash = EXCLUDED.query_hash, \
           delta_sql = EXCLUDED.delta_sql, \
           columns = EXCLUDED.columns, \
           source_oids = EXCLUDED.source_oids, \
           is_dedup = EXCLUDED.is_dedup, \
           key_changed = EXCLUDED.key_changed, \
           all_algebraic = EXCLUDED.all_algebraic, \
           cached_at = now()",
        &[
            pgt_id.into(),
            hash_i64.into(),
            template.delta_sql_template.as_str().into(),
            template.output_columns.clone().into(),
            source_oids_i32.into(),
            template.is_deduplicated.into(),
            template.has_key_changed.into(),
            template.is_all_algebraic.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    Ok(())
}

/// Invalidate (remove) a cached template for a specific stream table.
///
/// Called on ALTER QUERY, DROP, or reinitialize.
pub fn invalidate(pgt_id: i64) {
    let _ = Spi::run_with_args(
        "DELETE FROM pgtrickle.pgt_template_cache WHERE pgt_id = $1",
        &[pgt_id.into()],
    );
}

/// Invalidate all cached templates.
///
/// Called when a bulk cache flush is needed (e.g., extension upgrade).
pub fn invalidate_all() {
    let _ = Spi::run("DELETE FROM pgtrickle.pgt_template_cache");
}

/// Check whether the template cache table exists.
///
/// Used to gracefully handle the case where the extension was upgraded
/// but the migration hasn't been applied yet.
pub fn table_exists() -> bool {
    Spi::get_one::<bool>(
        "SELECT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_tables \
           WHERE schemaname = 'pgtrickle' AND tablename = 'pgt_template_cache' \
         )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false)
}
