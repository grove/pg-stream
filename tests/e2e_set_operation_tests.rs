//! E2E tests for INTERSECT/EXCEPT differential correctness (F19: G6.3).
//!
//! Validates that INTERSECT and EXCEPT set operations produce correct
//! differential results when rows are inserted/updated/deleted.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F19): Add INTERSECT/EXCEPT differential E2E tests.
// Test scenarios:
//   1. INTERSECT → INSERT matching row → appears in result
//   2. INTERSECT → DELETE one side → row disappears
//   3. EXCEPT → INSERT into right side → row disappears from result
//   4. EXCEPT → DELETE from right side → row appears in result
//   5. INTERSECT ALL / EXCEPT ALL with duplicates
//   6. Multi-way INTERSECT/EXCEPT chains
