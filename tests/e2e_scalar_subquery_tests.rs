//! E2E tests for scalar subquery differential correctness (F20: G6.4).
//!
//! Validates that scalar subqueries in the SELECT list and WHERE clause
//! produce correct differential results when the subquery's underlying
//! data changes.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F20): Add scalar subquery E2E tests.
// Test scenarios:
//   1. SELECT (SELECT max(x) FROM t2) FROM t1 → change t2 → verify update
//   2. WHERE col > (SELECT avg(x) FROM t2) → change t2 → verify filter changes
//   3. Correlated subquery → change outer row → verify result changes
