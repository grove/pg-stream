//! E2E tests for multi-cycle refresh correctness (F24: G6.8).
//!
//! Validates that the delta template cache, prepared statement cache,
//! deferred cleanup, and adaptive threshold logic work correctly across
//! multiple refresh cycles.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F24): Add multi-cycle refresh E2E tests.
// Test scenarios:
//   1. DML → refresh → DML → refresh → assert (aggregate)
//   2. DML → refresh → DML → refresh → assert (join)
//   3. DML → refresh → DML → refresh → assert (window function)
//   4. Verify prepared statement cache survives multiple cycles
//   5. Verify adaptive threshold adjusts after high-ratio changes
