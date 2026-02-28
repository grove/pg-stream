//! E2E tests for aggregate function differential correctness (F17: G6.1).
//!
//! Validates that each supported aggregate function produces correct
//! differential results after INSERT, UPDATE, and DELETE operations.
//!
//! The 21 supported aggregate functions that need differential E2E tests:
//! - `sum`, `avg`, `count`, `min`, `max`
//! - `count(DISTINCT ...)`, `sum(DISTINCT ...)`
//! - `array_agg`, `string_agg`
//! - `bool_and`, `bool_or`
//! - `bit_and`, `bit_or`
//! - `every`
//! - `json_agg`, `jsonb_agg`
//! - `json_object_agg`, `jsonb_object_agg`
//! - `percentile_cont`, `percentile_disc` (ordered-set)
//! - `mode` (ordered-set)
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

#[allow(unused_imports)]
use e2e::E2eDb;

// TODO(F17): Add differential E2E tests for each aggregate function.
// Pattern for each test:
//   1. CREATE TABLE with appropriate data types
//   2. INSERT initial data
//   3. CREATE STREAM TABLE with GROUP BY + the aggregate
//   4. Refresh → verify initial values
//   5. INSERT more data → refresh → verify aggregate updates correctly
//   6. DELETE data → refresh → verify aggregate recomputes
//   7. UPDATE data → refresh → verify aggregate reflects changes
