use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use serde::Serialize;

use crate::cli::OutputFormat;
use crate::error::CliError;

/// Format and print output in the requested format.
pub fn print_output<T: Serialize>(
    format: OutputFormat,
    rows: &[T],
    headers: &[&str],
    row_fn: impl Fn(&T) -> Vec<String>,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Table => {
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic);
            table.set_header(headers);
            for row in rows {
                table.add_row(row_fn(row));
            }
            println!("{table}");
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(rows)?;
            println!("{json}");
        }
        OutputFormat::Csv => {
            println!("{}", headers.join(","));
            for row in rows {
                let values = row_fn(row);
                let escaped: Vec<String> = values
                    .iter()
                    .map(|v| {
                        if v.contains(',') || v.contains('"') || v.contains('\n') {
                            format!("\"{}\"", v.replace('"', "\"\""))
                        } else {
                            v.clone()
                        }
                    })
                    .collect();
                println!("{}", escaped.join(","));
            }
        }
    }
    Ok(())
}

/// Print a single-item detail output.
pub fn print_detail<T: Serialize>(
    format: OutputFormat,
    item: &T,
    display_fn: impl FnOnce(&T),
) -> Result<(), CliError> {
    match format {
        OutputFormat::Table => {
            display_fn(item);
        }
        OutputFormat::Json | OutputFormat::Csv => {
            let json = serde_json::to_string_pretty(item)?;
            println!("{json}");
        }
    }
    Ok(())
}

/// Print a simple success message.
pub fn print_success(message: &str) {
    println!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;
    use serde::Serialize;

    #[derive(Serialize)]
    struct TestRow {
        name: String,
        value: i32,
    }

    fn test_rows() -> Vec<TestRow> {
        vec![
            TestRow {
                name: "alpha".to_string(),
                value: 1,
            },
            TestRow {
                name: "beta".to_string(),
                value: 2,
            },
        ]
    }

    fn row_fn(r: &TestRow) -> Vec<String> {
        vec![r.name.clone(), r.value.to_string()]
    }

    #[test]
    fn test_json_output() {
        let rows = test_rows();
        let json = serde_json::to_string_pretty(&rows).unwrap();
        assert!(json.contains("\"alpha\""));
        assert!(json.contains("\"value\": 1"));
    }

    #[test]
    fn test_csv_escaping_with_comma() {
        let rows = vec![TestRow {
            name: "hello,world".to_string(),
            value: 42,
        }];
        let values = row_fn(&rows[0]);
        let escaped: Vec<String> = values
            .iter()
            .map(|v| {
                if v.contains(',') || v.contains('"') || v.contains('\n') {
                    format!("\"{}\"", v.replace('"', "\"\""))
                } else {
                    v.clone()
                }
            })
            .collect();
        assert_eq!(escaped[0], "\"hello,world\"");
        assert_eq!(escaped[1], "42");
    }

    #[test]
    fn test_csv_escaping_with_quotes() {
        let rows = vec![TestRow {
            name: "say \"hello\"".to_string(),
            value: 1,
        }];
        let values = row_fn(&rows[0]);
        let escaped: Vec<String> = values
            .iter()
            .map(|v| {
                if v.contains(',') || v.contains('"') || v.contains('\n') {
                    format!("\"{}\"", v.replace('"', "\"\""))
                } else {
                    v.clone()
                }
            })
            .collect();
        assert_eq!(escaped[0], "\"say \"\"hello\"\"\"");
    }

    #[test]
    fn test_csv_no_escaping_needed() {
        let values = row_fn(&test_rows()[0]);
        let escaped: Vec<String> = values
            .iter()
            .map(|v| {
                if v.contains(',') || v.contains('"') || v.contains('\n') {
                    format!("\"{}\"", v.replace('"', "\"\""))
                } else {
                    v.clone()
                }
            })
            .collect();
        assert_eq!(escaped[0], "alpha");
    }

    #[test]
    fn test_print_output_table_format() {
        // Just verify it doesn't panic — we can't easily capture stdout
        let result = print_output(
            OutputFormat::Table,
            &test_rows(),
            &["Name", "Value"],
            row_fn,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_print_output_json_format() {
        let result = print_output(OutputFormat::Json, &test_rows(), &["Name", "Value"], row_fn);
        assert!(result.is_ok());
    }

    #[test]
    fn test_print_output_csv_format() {
        let result = print_output(OutputFormat::Csv, &test_rows(), &["Name", "Value"], row_fn);
        assert!(result.is_ok());
    }

    #[test]
    fn test_print_detail_json() {
        let row = &test_rows()[0];
        let result = print_detail(OutputFormat::Json, row, |_| {});
        assert!(result.is_ok());
    }

    #[test]
    fn test_print_detail_table() {
        let row = &test_rows()[0];
        let result = print_detail(OutputFormat::Table, row, |_| {});
        assert!(result.is_ok());
    }
}
