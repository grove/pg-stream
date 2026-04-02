use clap::Parser;
use serde::Serialize;
use tokio_postgres::Client;

use crate::cli::OutputFormat;
use crate::error::CliError;
use crate::output;

#[derive(Parser)]
pub struct ListArgs {
    /// Output format
    #[arg(long, short, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,
}

#[derive(Serialize)]
pub struct StreamTableRow {
    pub name: String,
    pub schema: String,
    pub status: String,
    pub refresh_mode: String,
    pub is_populated: bool,
    pub consecutive_errors: i64,
    pub schedule: Option<String>,
    pub staleness: Option<String>,
}

pub async fn execute(client: &Client, args: &ListArgs) -> Result<(), CliError> {
    let rows = client
        .query(
            "SELECT
                s.pgt_name::text,
                s.pgt_schema::text,
                s.status::text,
                s.refresh_mode::text,
                s.is_populated,
                COALESCE(s.consecutive_errors, 0)::bigint,
                s.schedule::text,
                s.staleness_secs
             FROM pgtrickle.st_refresh_stats() s
             ORDER BY s.pgt_schema, s.pgt_name",
            &[],
        )
        .await
        .map_err(|e| CliError::Query(e.to_string()))?;

    let items: Vec<StreamTableRow> = rows
        .iter()
        .map(|row| {
            let staleness_secs: Option<f64> = row.get(7);
            StreamTableRow {
                name: row.get(0),
                schema: row.get(1),
                status: row.get(2),
                refresh_mode: row.get(3),
                is_populated: row.get(4),
                consecutive_errors: row.get(5),
                schedule: row.get(6),
                staleness: staleness_secs.map(|s| format!("{s:.1}s")),
            }
        })
        .collect();

    output::print_output(
        args.format,
        &items,
        &[
            "Name",
            "Schema",
            "Status",
            "Mode",
            "Populated",
            "Errors",
            "Schedule",
            "Staleness",
        ],
        |row| {
            vec![
                row.name.clone(),
                row.schema.clone(),
                row.status.clone(),
                row.refresh_mode.clone(),
                if row.is_populated { "yes" } else { "no" }.to_string(),
                row.consecutive_errors.to_string(),
                row.schedule.clone().unwrap_or_default(),
                row.staleness.clone().unwrap_or_default(),
            ]
        },
    )
}
