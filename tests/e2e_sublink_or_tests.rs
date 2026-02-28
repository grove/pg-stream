//! E2E tests for SubLinks-in-OR differential correctness (F21: G6.5).
//!
//! Validates that EXISTS/IN subqueries combined with OR in WHERE clauses
//! produce correct differential results.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F21): Add SubLinks-in-OR E2E tests.
// Test scenarios:
//   1. WHERE EXISTS(...) OR col > 10 → change subquery data → verify
//   2. WHERE col IN (SELECT ...) OR col = 0 → change subquery → verify
//   3. WHERE NOT EXISTS(...) OR EXISTS(...) → change both → verify
