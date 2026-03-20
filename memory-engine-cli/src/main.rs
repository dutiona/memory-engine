mod commands;
mod db;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use output::OutputFormat;

/// memory-engine-cli — operator tool for agent memory databases
#[derive(Parser)]
#[command(
    name = "memory-engine-cli",
    version,
    about = "Operator tool for agent memory databases",
    long_about = "memory-engine-cli is a command-line inspector for memory-engine databases.\n\n\
        It provides read-only access to facts, events, statistics, and provenance.\n\
        For data portability, use export/import for JSON snapshots or `SQLite` backups.",
    after_help = "Set MEMORY_ENGINE_DB to avoid passing --db on every invocation."
)]
struct Cli {
    /// Path to the memory-engine `SQLite` database
    #[arg(long, env = "MEMORY_ENGINE_DB")]
    db: PathBuf,

    /// Output format
    #[arg(long, default_value = "table")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show engine statistics
    Stats,
    /// View fact details
    Inspect {
        /// Fact ID
        id: i64,
    },
    /// Explain why a fact is active, forgotten, pinned, or due
    Explain {
        /// Fact ID
        id: i64,
    },
    /// Search facts by text (FTS5 full-text search)
    Query(commands::query::QueryArgs),
    /// Export engine state to file (JSON, `SQLite`, compressed)
    Export(commands::export::ExportArgs),
    /// Import a JSON snapshot into a new database
    Import(commands::import::ImportArgs),
    /// Dump facts or events to stdout (debug/inspection)
    Dump(commands::dump::DumpArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Stats => commands::stats::run(&cli.db, cli.format),
        Commands::Inspect { id } => commands::inspect::run(&cli.db, id, cli.format),
        Commands::Explain { id } => commands::explain::run(&cli.db, id, cli.format),
        Commands::Query(ref args) => commands::query::run(&cli.db, args, cli.format),
        Commands::Export(ref args) => commands::export::run(&cli.db, args),
        Commands::Import(ref args) => commands::import::run(&cli.db, args),
        Commands::Dump(ref args) => commands::dump::run(&cli.db, args, cli.format),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
