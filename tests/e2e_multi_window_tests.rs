//! E2E tests for multi-partition window function correctness (F22: G6.6).
//!
//! Validates that queries with multiple window functions over different
//! PARTITION BY keys produce correct differential results.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F22): Add multi-partition window E2E tests.
// Test scenarios:
//   1. SELECT ROW_NUMBER() OVER (PARTITION BY a), SUM(x) OVER (PARTITION BY b)
//      → INSERT row → verify both windows update correctly
//   2. Multiple RANK() functions with different orderings
//   3. Window functions with ROWS BETWEEN frame clauses
