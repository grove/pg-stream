//! Centralised safe SQL-building helpers (A45-2).
//!
//! All functions in this module produce SQL fragments that are safe against
//! injection attacks. They are the canonical way to construct dynamic SQL
//! throughout pg_trickle production code.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use crate::sql_builder::{ident, qualified, literal, spi_param, list_idents};
//!
//! let sql = format!(
//!     "SELECT {} FROM {}",
//!     list_idents(&["col_a", "col_b"]),
//!     qualified("public", "my_table"),
//! );
//! ```
//!
//! ## CI Lint Rule
//!
//! A semgrep/grep CI check in `scripts/check_security_definer.sh` flags
//! manual `format!("... '{}'")` patterns that bypass these helpers. New
//! code must not interpolate untrusted values into SQL strings directly.

/// Quote a single PostgreSQL identifier using double-quote escaping.
///
/// Any double-quote characters in `name` are doubled per the SQL standard.
/// This prevents identifier-injection attacks.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::ident;
/// assert_eq!(ident("my_table"),   r#""my_table""#);
/// assert_eq!(ident(r#"bad"name"#), r#""bad""name""#);
/// ```
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Produce a schema-qualified identifier `"schema"."name"`.
///
/// Both parts are individually double-quoted, so schema and table names
/// containing special characters or reserved words are handled safely.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::qualified;
/// assert_eq!(qualified("public", "orders"), r#""public"."orders""#);
/// ```
pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", ident(schema), ident(name))
}

/// Produce a safe SQL string literal by escaping single quotes.
///
/// Single-quote characters in `value` are escaped by doubling them
/// (`'` → `''`). The result is wrapped in single quotes.
///
/// **Never** use this for user-supplied values in SPI calls — use `$N`
/// parameterised queries instead. This function is intended for constant
/// SQL fragments such as status strings or schema names embedded in DDL.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::literal;
/// assert_eq!(literal("hello"),        "'hello'");
/// assert_eq!(literal("it's alive"),   "'it''s alive'");
/// ```
pub fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Produce an OID-to-regclass cast fragment: `<oid>::oid::regclass`.
///
/// This is the canonical way to resolve an OID to a schema-qualified table
/// name at SQL execution time. It is safe because the OID is a numeric
/// value that cannot be injected.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::regclass;
/// assert_eq!(regclass(12345), "12345::oid::regclass");
/// ```
pub fn regclass(oid: u32) -> String {
    format!("{}::oid::regclass", oid)
}

/// Produce a SPI parameter placeholder `$N` (1-indexed).
///
/// Use this instead of hard-coding `$1`, `$2`, etc. in SQL strings to
/// make parameter index management less error-prone.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::spi_param;
/// assert_eq!(spi_param(1), "$1");
/// assert_eq!(spi_param(3), "$3");
/// ```
pub fn spi_param(index: usize) -> String {
    format!("${}", index)
}

/// Produce a comma-separated list of quoted identifiers.
///
/// Convenience wrapper around [`ident`] for column lists and similar
/// constructs.
///
/// # Examples
///
/// ```
/// use pg_trickle::sql_builder::list_idents;
/// assert_eq!(list_idents(&["col_a", "col_b"]), r#""col_a", "col_b""#);
/// assert_eq!(list_idents(&[]),                 "");
/// ```
pub fn list_idents(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| ident(n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Produce a comma-separated list of owned quoted identifiers.
///
/// Same as [`list_idents`] but accepts `&[String]` for ergonomic use with
/// owned collections.
pub fn list_idents_owned(names: &[String]) -> String {
    names
        .iter()
        .map(|n| ident(n))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // T-A45-2: sql_builder unit tests

    #[test]
    fn test_ident_simple() {
        assert_eq!(ident("my_table"), r#""my_table""#);
    }

    #[test]
    fn test_ident_reserved_word() {
        assert_eq!(ident("select"), r#""select""#);
    }

    #[test]
    fn test_ident_with_double_quote() {
        assert_eq!(ident(r#"bad"name"#), r#""bad""name""#);
    }

    #[test]
    fn test_ident_empty() {
        assert_eq!(ident(""), r#""""#);
    }

    #[test]
    fn test_ident_uppercase() {
        assert_eq!(ident("MyTable"), r#""MyTable""#);
    }

    #[test]
    fn test_qualified_simple() {
        assert_eq!(qualified("public", "orders"), r#""public"."orders""#);
    }

    #[test]
    fn test_qualified_special_chars() {
        assert_eq!(
            qualified("my schema", "my table"),
            r#""my schema"."my table""#
        );
    }

    #[test]
    fn test_literal_simple() {
        assert_eq!(literal("hello"), "'hello'");
    }

    #[test]
    fn test_literal_single_quote() {
        assert_eq!(literal("it's alive"), "'it''s alive'");
    }

    #[test]
    fn test_literal_multiple_quotes() {
        assert_eq!(literal("it''s"), "'it''''s'");
    }

    #[test]
    fn test_literal_empty() {
        assert_eq!(literal(""), "''");
    }

    #[test]
    fn test_regclass() {
        assert_eq!(regclass(12345), "12345::oid::regclass");
        assert_eq!(regclass(0), "0::oid::regclass");
    }

    #[test]
    fn test_spi_param() {
        assert_eq!(spi_param(1), "$1");
        assert_eq!(spi_param(3), "$3");
        assert_eq!(spi_param(10), "$10");
    }

    #[test]
    fn test_list_idents_empty() {
        assert_eq!(list_idents(&[]), "");
    }

    #[test]
    fn test_list_idents_single() {
        assert_eq!(list_idents(&["col_a"]), r#""col_a""#);
    }

    #[test]
    fn test_list_idents_multiple() {
        assert_eq!(list_idents(&["col_a", "col_b"]), r#""col_a", "col_b""#);
    }

    #[test]
    fn test_list_idents_with_special() {
        assert_eq!(list_idents(&["col a", r#"col"b"#]), r#""col a", "col""b""#);
    }

    #[test]
    fn test_list_idents_owned() {
        let names = vec!["x".to_string(), "y".to_string()];
        assert_eq!(list_idents_owned(&names), r#""x", "y""#);
    }
}
