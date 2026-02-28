//! E2E tests for FULL JOIN differential correctness (F18: G6.2).
//!
//! Validates that FULL JOIN produces correct differential results when:
//! - Left-side rows inserted/deleted (affecting NULL-padded right side)
//! - Right-side rows inserted/deleted (affecting NULL-padded left side)
//! - Join key changes on either side
//! - Both sides change simultaneously
//! - NULL join keys on either side (F26: G1.6)
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F18): Add FULL JOIN differential E2E tests.
// TODO(F26): Add FULL JOIN NULL key E2E tests.
// Test scenarios:
//   1. FULL JOIN with matching rows → INSERT on left → verify new matched row
//   2. FULL JOIN → DELETE from right → verify left row with NULL right columns
//   3. FULL JOIN with no initial matches → INSERT match → verify join
//   4. FULL JOIN with NULL keys → verify NULLs handled correctly
//   5. FULL JOIN → UPDATE join key → verify row moves between matched/unmatched
