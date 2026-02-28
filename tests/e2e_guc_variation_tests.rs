//! E2E tests for GUC variation coverage (F23: G6.7).
//!
//! Validates that non-default GUC settings produce correct behavior.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F23): Add GUC variation E2E tests.
// Test scenarios:
//   1. pg_trickle.block_source_ddl = true → ALTER TABLE should be blocked
//   2. pg_trickle.use_prepared_statements = true → verify PREPARE/EXECUTE cycle
//   3. pg_trickle.merge_planner_hints = false → verify refresh works without hints
//   4. pg_trickle.cleanup_use_truncate = true → verify TRUNCATE cleanup path
//   5. pg_trickle.merge_strategy = 'auto' vs explicit settings
