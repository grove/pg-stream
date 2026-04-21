// FUZZ-1 (v0.26.0): cargo-fuzz target for the cron parser.
//
// Exercises `validate_cron_pub()` with pathological input strings.
// Guards against DoS (catastrophic backtracking, infinite loops) and
// panics in the cron expression parser.
//
// Run locally with:
//   cargo +nightly fuzz run cron_fuzz -- -max_total_time=60
//
// For 10M iterations:
//   cargo +nightly fuzz run cron_fuzz -- -runs=10000000
//
// Functions under test (pure Rust, no PostgreSQL backend required):
//   - validate_cron_pub  (api/helpers)
//   - parse_schedule_pub (api/helpers)

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Require valid UTF-8 so we can pass &str.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // -------------------------------------------------------------------
    // 1. Cron validation — must never panic, abort, or infinite-loop.
    //    Valid expressions return Ok(()), invalid return Err.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::validate_cron_pub(s);

    // -------------------------------------------------------------------
    // 2. Schedule parsing — cron expressions are a sub-case of schedules.
    //    Must never panic or abort for any UTF-8 input.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::parse_schedule_pub(s);
});
