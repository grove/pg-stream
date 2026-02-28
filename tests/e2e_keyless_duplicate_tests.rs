//! E2E tests for keyless table duplicate row handling (F48: G1.5).
//!
//! Validates that tables without a PRIMARY KEY handle duplicate rows
//! correctly (or at least predictably) during differential refresh.
//! Documents the known limitation where identical rows produce identical
//! pk_hash values.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F48): Add keyless table duplicate row E2E tests.
// Test scenarios:
//   1. INSERT identical rows into keyless table → verify count after refresh
//   2. DELETE one of two identical rows → verify only one removed (known limitation)
//   3. UPDATE one of two identical rows → verify correct row updated
//   4. Keyless table with unique content → verify differential works normally
