use clap::{Parser, Subcommand, ValueEnum};

use crate::commands;

/// pg_trickle Terminal User Interface — manage and monitor stream tables.
///
/// Launch with no subcommand for the interactive TUI dashboard.
/// Use subcommands for one-shot CLI operations (scriptable, CI-friendly).
#[derive(Parser)]
#[command(name = "pgtrickle", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[command(flatten)]
    pub connection: ConnectionArgs,
}

#[derive(Parser, Clone)]
pub struct ConnectionArgs {
    /// Full PostgreSQL connection URL (overrides PGHOST/PGPORT/etc.)
    #[arg(long = "url", short = 'U', env = "PGTRICKLE_URL")]
    pub url: Option<String>,

    /// Database host
    #[arg(long, env = "PGHOST")]
    pub host: Option<String>,

    /// Database port
    #[arg(long, env = "PGPORT")]
    pub port: Option<u16>,

    /// Database name
    #[arg(long, env = "PGDATABASE")]
    pub dbname: Option<String>,

    /// Database user
    #[arg(long, env = "PGUSER")]
    pub user: Option<String>,

    /// Database password
    #[arg(long, env = "PGPASSWORD", hide_env_values = true)]
    pub password: Option<String>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// List all stream tables
    List(commands::list::ListArgs),

    /// Show detailed status of a stream table
    Status(commands::status::StatusArgs),

    /// Trigger a manual refresh
    Refresh(commands::refresh::RefreshArgs),

    /// Create a new stream table
    Create(commands::create::CreateArgs),

    /// Drop a stream table
    Drop(commands::drop::DropArgs),

    /// Alter a stream table's settings
    Alter(commands::alter::AlterArgs),

    /// Export stream table DDL
    Export(commands::export::ExportArgs),

    /// Show diagnostics and recommendations
    Diag(commands::diag::DiagArgs),

    /// Show CDC health and buffer sizes
    Cdc(commands::cdc::CdcArgs),

    /// Show dependency graph (ASCII)
    Graph(commands::graph::GraphArgs),

    /// Show or set configuration (GUC parameters)
    Config(commands::config::ConfigArgs),

    /// Run health checks
    Health(commands::health::HealthArgs),

    /// Show worker pool and job queue status
    Workers(commands::workers::WorkersArgs),

    /// Show fuse and circuit breaker status
    Fuse(commands::fuse::FuseArgs),

    /// Show watermark groups and source gating
    Watermarks(commands::watermarks::WatermarksArgs),

    /// Inspect delta SQL, operator tree, or dedup stats
    Explain(commands::explain::ExplainArgs),

    /// Watch stream table status (non-interactive, continuous output)
    Watch(commands::watch::WatchArgs),

    /// Generate shell completion scripts
    Completions(commands::completions::CompletionsArgs),
}

#[derive(Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// Human-readable table output
    #[default]
    Table,
    /// JSON output
    Json,
    /// CSV output
    Csv,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn test_no_subcommand_parses() {
        let cli = Cli::try_parse_from(["pgtrickle"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_list_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "list"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::List(_))));
    }

    #[test]
    fn test_status_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "status", "my_table"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Status(_))));
    }

    #[test]
    fn test_refresh_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "refresh", "my_table"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Refresh(_))));
    }

    #[test]
    fn test_create_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "create", "my_table", "SELECT 1"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Create(_))));
    }

    #[test]
    fn test_drop_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "drop", "my_table"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Drop(_))));
    }

    #[test]
    fn test_health_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "health"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Health(_))));
    }

    #[test]
    fn test_diag_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "diag"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Diag(_))));
    }

    #[test]
    fn test_graph_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "graph"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Graph(_))));
    }

    #[test]
    fn test_workers_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "workers"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Workers(_))));
    }

    #[test]
    fn test_fuse_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "fuse"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Fuse(_))));
    }

    #[test]
    fn test_watermarks_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "watermarks"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Watermarks(_))));
    }

    #[test]
    fn test_cdc_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "cdc"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Cdc(_))));
    }

    #[test]
    fn test_config_subcommand() {
        let cli = Cli::try_parse_from(["pgtrickle", "config"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Config(_))));
    }

    #[test]
    fn test_connection_args_url() {
        let cli = Cli::try_parse_from(["pgtrickle", "--url", "postgres://user:pass@host:5432/db"])
            .unwrap();
        assert_eq!(
            cli.connection.url.as_deref(),
            Some("postgres://user:pass@host:5432/db")
        );
    }

    #[test]
    fn test_connection_args_individual() {
        let cli = Cli::try_parse_from([
            "pgtrickle",
            "--host",
            "myhost",
            "--port",
            "5433",
            "--dbname",
            "mydb",
            "--user",
            "admin",
            "--password",
            "secret",
        ])
        .unwrap();
        assert_eq!(cli.connection.host.as_deref(), Some("myhost"));
        assert_eq!(cli.connection.port, Some(5433));
        assert_eq!(cli.connection.dbname.as_deref(), Some("mydb"));
        assert_eq!(cli.connection.user.as_deref(), Some("admin"));
        assert_eq!(cli.connection.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_connection_args_defaults() {
        let cli = Cli::try_parse_from(["pgtrickle"]).unwrap();
        assert!(cli.connection.url.is_none());
        assert!(cli.connection.host.is_none());
        assert!(cli.connection.port.is_none());
        assert!(cli.connection.dbname.is_none());
        assert!(cli.connection.user.is_none());
        assert!(cli.connection.password.is_none());
    }

    #[test]
    fn test_invalid_subcommand_fails() {
        let result = Cli::try_parse_from(["pgtrickle", "nonexistent"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_output_format_default_is_table() {
        let f = OutputFormat::default();
        assert!(matches!(f, OutputFormat::Table));
    }
}
