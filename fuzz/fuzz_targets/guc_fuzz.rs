// FUZZ-2 (v0.26.0): cargo-fuzz target for GUC string→enum coercion paths.
//
// Exercises the pure-Rust normalization functions that convert GUC string
// values to typed enums. These paths are invoked on every scheduler tick,
// so any panic or unexpected behaviour in them would kill the background worker.
//
// Run locally with:
//   cargo +nightly fuzz run guc_fuzz -- -max_total_time=60
//
// For 10M iterations:
//   cargo +nightly fuzz run guc_fuzz -- -runs=10000000
//
// Functions under test (pure Rust, no PostgreSQL backend required):
//   - normalize_frontier_holdback_mode  (config)
//   - validate_cron_pub                 (api/helpers — schedule GUC)
//   - detect_select_star_pub            (api/helpers — query validation)
//   - detect_volatile_functions_pub     (api/helpers — query validation)

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Require valid UTF-8.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // -------------------------------------------------------------------
    // 1. frontier_holdback_mode GUC coercion — must never panic.
    //    Accepts: "none", "xmin", "lsn:<N>", or garbage → defaults.
    // -------------------------------------------------------------------
    let _ = pg_trickle::config::normalize_frontier_holdback_mode(Some(s.to_string()));

    // -------------------------------------------------------------------
    // 2. refresh_mode / cdc_mode validation via schedule parse.
    //    (The schedule GUC accepts interval strings and cron expressions.)
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::parse_schedule_pub(s);

    // -------------------------------------------------------------------
    // 3. Query-level GUC guards — SELECT * and volatile function detection.
    //    Must handle any SQL fragment without panicking.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::detect_select_star_pub(s);
    let _ = pg_trickle::api::helpers::detect_volatile_functions_pub(s);
});
