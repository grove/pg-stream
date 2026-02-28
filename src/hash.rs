//! xxHash-based row ID generation for stream tables.
//!
//! Row IDs are deterministic 64-bit hashes used to identify rows in
//! incrementally-maintained stream tables.

use pgrx::prelude::*;
use xxhash_rust::xxh64;

/// Compute a 64-bit xxHash row ID from a text representation.
///
/// This function is exposed as a SQL function for use in INSERT statements
/// and delta query generation.
#[pg_extern(schema = "pgtrickle", immutable, parallel_safe)]
fn pg_trickle_hash(input: &str) -> i64 {
    // Use a fixed seed for deterministic hashing
    const SEED: u64 = 0x517cc1b727220a95;
    let hash = xxh64::xxh64(input.as_bytes(), SEED);
    hash as i64
}

/// Compute a row ID by hashing multiple text values.
///
/// Used for composite keys (e.g., join row IDs, group-by keys).
#[pg_extern(schema = "pgtrickle", immutable, parallel_safe)]
fn pg_trickle_hash_multi(inputs: Vec<Option<String>>) -> i64 {
    const SEED: u64 = 0x517cc1b727220a95;

    let mut combined = String::new();
    for (i, input) in inputs.iter().enumerate() {
        if i > 0 {
            combined.push('\x1E'); // record separator
        }
        match input {
            Some(val) => combined.push_str(val),
            None => combined.push_str("\x00NULL\x00"),
        }
    }

    let hash = xxh64::xxh64(combined.as_bytes(), SEED);
    hash as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_determinism() {
        let hash1 = xxh64::xxh64(b"hello world", 0x517cc1b727220a95);
        let hash2 = xxh64::xxh64(b"hello world", 0x517cc1b727220a95);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_different_inputs() {
        let hash1 = xxh64::xxh64(b"hello", 0x517cc1b727220a95);
        let hash2 = xxh64::xxh64(b"world", 0x517cc1b727220a95);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_null_handling_in_multi_hash() {
        let combined1 = "a\x1E\x00NULL\x00\x1Eb";
        let combined2 = "a\x1E\x00NULL\x00\x1Ec";
        let h1 = xxh64::xxh64(combined1.as_bytes(), 0x517cc1b727220a95);
        let h2 = xxh64::xxh64(combined2.as_bytes(), 0x517cc1b727220a95);
        assert_ne!(h1, h2);
    }

    // ── pg_trickle_hash() tests via raw xxh64 (same logic, avoids pg_extern) ──

    #[test]
    fn test_pg_trickle_hash_empty_string() {
        const SEED: u64 = 0x517cc1b727220a95;
        let hash = xxh64::xxh64(b"", SEED);
        // Should produce a valid non-zero hash (xxHash of empty with non-zero seed)
        assert_ne!(hash, 0);
    }

    #[test]
    fn test_pg_trickle_hash_i64_range() {
        // Verify the cast from u64 to i64 doesn't panic
        const SEED: u64 = 0x517cc1b727220a95;
        let hash = xxh64::xxh64(b"test", SEED);
        let _ = hash as i64; // Should not panic
    }

    // ── pg_trickle_hash_multi() separator logic ────────────────────────────

    #[test]
    fn test_multi_hash_separator_prevents_collision() {
        const SEED: u64 = 0x517cc1b727220a95;
        // "ab" + "c" vs "a" + "bc" — record separator should differentiate
        let combined1 = "ab\x1Ec";
        let combined2 = "a\x1Ebc";
        let h1 = xxh64::xxh64(combined1.as_bytes(), SEED);
        let h2 = xxh64::xxh64(combined2.as_bytes(), SEED);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_multi_hash_null_vs_string_null() {
        const SEED: u64 = 0x517cc1b727220a95;
        // None encoded as \x00NULL\x00 vs literal string "NULL"
        let with_null_marker = "\x00NULL\x00";
        let with_string_null = "NULL";
        let h1 = xxh64::xxh64(with_null_marker.as_bytes(), SEED);
        let h2 = xxh64::xxh64(with_string_null.as_bytes(), SEED);
        assert_ne!(
            h1, h2,
            "NULL marker and string 'NULL' should hash differently"
        );
    }
}
