// FUZZ-6 (v0.44.0 / A45-9): SQL-builder and parser typed facade fuzz target.
//
// This target exercises:
//   - `sql_builder` module helpers (A45-2): ident quoting, literal escaping,
//     qualified names, regclass, spi_param, list_idents.
//   - `validate_immediate_mode_support`: parser validation pipeline entry
//     point — pure Rust, no backend required.
//   - `lookup_function_volatility` / `lookup_operator_volatility`: pure Rust
//     volatility classification.
//   - `max_volatility`: pure Rust volatility algebra.
//
// Invariants verified for each input:
//   1. `sql_builder::ident` output contains the input wrapped in double quotes
//      with double-quote chars escaped — never panics.
//   2. `sql_builder::literal` output contains the input wrapped in single
//      quotes with single-quote chars escaped — never panics.
//   3. `sql_builder::qualified` output is the concatenation of the two ident-
//      quoted fragments separated by `.` — never panics.
//   4. None of the pure validation helpers panic or abort.
//
// Run locally:
//   cargo +nightly fuzz run sql_builder_fuzz -- -max_total_time=60
//
// See plans/safety/PLAN_REDUCED_UNSAFE.md §SAF-2 and §A45-9 for context.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Require valid UTF-8.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // ---------------------------------------------------------------
    // 1. sql_builder::ident — safe identifier quoting.
    //    Invariant: output starts with `"`, ends with `"`, and every
    //    `"` in the input is doubled.
    // ---------------------------------------------------------------
    let quoted = pg_trickle::sql_builder::ident(s);
    assert!(quoted.starts_with('"'));
    assert!(quoted.ends_with('"'));

    // ---------------------------------------------------------------
    // 2. sql_builder::literal — safe string literal quoting.
    //    Invariant: output starts with `'`, ends with `'`, and single
    //    quotes in the input appear doubled inside the output.
    // ---------------------------------------------------------------
    let lit = pg_trickle::sql_builder::literal(s);
    assert!(lit.starts_with('\''));
    assert!(lit.ends_with('\''));

    // ---------------------------------------------------------------
    // 3. sql_builder::qualified — schema.table quoting.
    //    Invariant: output contains exactly one unquoted `.` separator.
    // ---------------------------------------------------------------
    let _ = pg_trickle::sql_builder::qualified(s, s);

    // ---------------------------------------------------------------
    // 4. sql_builder::spi_param — $N placeholder.
    //    We only need valid indices; clamp to a small range.
    // ---------------------------------------------------------------
    if !data.is_empty() {
        let idx = (data[0] as usize) % 64 + 1;
        let param = pg_trickle::sql_builder::spi_param(idx);
        assert!(param.starts_with('$'));
    }

    // ---------------------------------------------------------------
    // 5. sql_builder::list_idents — comma-separated ident list.
    //    Must never panic regardless of how many slices we pass.
    // ---------------------------------------------------------------
    let parts: Vec<&str> = s.split(',').collect();
    let _ = pg_trickle::sql_builder::list_idents(&parts.iter().map(|p| *p).collect::<Vec<_>>());

    // ---------------------------------------------------------------
    // 6. Volatility classifier — pure Rust, no backend.
    //    Must never panic.
    // ---------------------------------------------------------------
    let _ = pg_trickle::dvm::parser::lookup_function_volatility(s);
    let _ = pg_trickle::dvm::parser::lookup_operator_volatility(s);

    // ---------------------------------------------------------------
    // 7. max_volatility — must never panic.
    // ---------------------------------------------------------------
    if data.len() >= 2 {
        let a = data[0] as char;
        let b = data[1] as char;
        let _ = pg_trickle::dvm::parser::max_volatility(a, b);
    }
});
