mod app;
mod cli;
mod commands;
mod connection;
mod error;
mod output;
mod poller;
mod state;
#[cfg(test)]
mod test_fixtures;
mod theme;
mod views;

use std::process::ExitCode;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = <Cli as clap::Parser>::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            match e {
                error::CliError::NotFound(_) => ExitCode::from(2),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

async fn run(cli: Cli) -> Result<(), error::CliError> {
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            // No subcommand → launch interactive TUI
            return app::run(&cli.connection).await;
        }
    };

    let client = connection::connect(&cli.connection).await?;

    match command {
        Commands::List(args) => commands::list::execute(&client, &args).await,
        Commands::Status(args) => commands::status::execute(&client, &args).await,
        Commands::Refresh(args) => commands::refresh::execute(&client, &args).await,
        Commands::Create(args) => commands::create::execute(&client, &args).await,
        Commands::Drop(args) => commands::drop::execute(&client, &args).await,
        Commands::Alter(args) => commands::alter::execute(&client, &args).await,
        Commands::Export(args) => commands::export::execute(&client, &args).await,
        Commands::Diag(args) => commands::diag::execute(&client, &args).await,
        Commands::Cdc(args) => commands::cdc::execute(&client, &args).await,
        Commands::Graph(args) => commands::graph::execute(&client, &args).await,
        Commands::Config(args) => commands::config::execute(&client, &args).await,
        Commands::Health(args) => commands::health::execute(&client, &args).await,
        Commands::Workers(args) => commands::workers::execute(&client, &args).await,
        Commands::Fuse(args) => commands::fuse::execute(&client, &args).await,
        Commands::Watermarks(args) => commands::watermarks::execute(&client, &args).await,
        Commands::Explain(args) => commands::explain::execute(&client, &args).await,
        Commands::Watch(args) => commands::watch::execute(&client, &args).await,
        Commands::Completions(args) => {
            commands::completions::execute(&args);
            Ok(())
        }
    }
}
