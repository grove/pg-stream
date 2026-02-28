//! E2E tests for HAVING group transition correctness (F25: G1.4).
//!
//! Validates that HAVING clause filtering produces correct results when
//! group membership changes across refreshes (rows join/leave groups,
//! group aggregates cross HAVING thresholds).
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F25): Add HAVING group transition E2E tests.
// Test scenarios:
//   1. GROUP BY with HAVING count(*) > 2 → INSERT to make group qualify → verify
//   2. DELETE row from qualifying group → group drops below threshold → disappears
//   3. UPDATE row to move between groups → both groups recompute
//   4. HAVING with aggregate expression (e.g., HAVING sum(x) > 100)
