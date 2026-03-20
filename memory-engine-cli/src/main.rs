mod commands;
mod db;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use output::OutputFormat;

/// memory-engine-cli — operator tool for agent memory databases
#[derive(Parser)]
#[command(name = "memory-engine-cli", version, about)]
struct Cli {
    /// Path to the memory-engine SQLite database
    #[arg(long, global = true, env = "MEMORY_ENGINE_DB")]
    db: PathBuf,

    /// Output format
    #[arg(long, global = true, default_value = "table")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show engine statistics
    Stats,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Stats => commands::stats::run(&cli.db, cli.format),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
