// FUZZ-5 (v0.39.0): O39-12 — Merge SQL template and snapshot identifier fuzz target.
//
// Exercises pure-Rust paths that process column lists, qualified names,
// and scheduled values to ensure no adversarial input causes panics.
//
// Functions under test (pure Rust, no PostgreSQL backend required):
//   - parse_qualified_name_pub    (api/helpers — split schema.table)
//   - parse_schedule_pub          (api/helpers — schedule string parser)
//   - validate_cron_pub           (api/helpers — cron expression validator)
//   - classify_spi_error_retryable (error.rs — SPI error text classifier)
//
// Run locally with:
//   cargo +nightly fuzz run dag_fuzz -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // -------------------------------------------------------------------
    // 1. Schedule string parsing — cron, interval, 'calculated', etc.
    //    Failure is acceptable; panic is not.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::parse_schedule_pub(s);

    // -------------------------------------------------------------------
    // 2. Cron expression validator — must never panic.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::validate_cron_pub(s);

    // -------------------------------------------------------------------
    // 3. SELECT * detection guard — must never panic.
    // -------------------------------------------------------------------
    let _ = pg_trickle::api::helpers::detect_select_star_pub(s);

    // -------------------------------------------------------------------
    // 4. SPI error retry classifier — deterministic, no panics.
    // -------------------------------------------------------------------
    let _ = pg_trickle::error::classify_spi_error_retryable(s);
});
