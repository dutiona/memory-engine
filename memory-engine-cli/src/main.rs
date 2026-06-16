mod commands;
mod db;
mod embedding;
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
    long_about = "memory-engine-cli is a command-line tool for memory-engine databases.\n\n\
        It provides inspection (stats, query, explain), data portability (export/import),\n\
        and bulk ingestion (batch-ingest) for agent memory.",
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
    /// Search facts by text (hybrid FTS5 + vector search)
    Query(commands::query::QueryArgs),
    /// Export engine state to file (JSON, `SQLite`, compressed)
    Export(commands::export::ExportArgs),
    /// Import a JSON snapshot into a new database
    Import(commands::import::ImportArgs),
    /// Dump facts or events to stdout (debug/inspection)
    Dump(commands::dump::DumpArgs),
    /// Add a fact to the database with pre-computed embedding
    AddFact(commands::add_fact::AddFactArgs),
    /// Ingest facts from a JSONL file via an embedding API
    BatchIngest(commands::batch_ingest::BatchIngestArgs),
    /// Import historical memory from session `.jsonl` and/or native `.md` dirs
    Bootstrap(commands::bootstrap::BootstrapArgs),
    /// Record an outcome signal (positive/negative/neutral) for a fact
    RecordOutcome(commands::record_outcome::RecordOutcomeArgs),
    /// Show aggregated outcome counts for a fact
    OutcomeCounts(commands::outcome_counts::OutcomeCountsArgs),
    /// Apply pending schema migrations (use --check for a dry-run)
    Migrate(commands::migrate::MigrateArgs),
    /// Report the database schema version vs the binary's `CURRENT_SCHEMA_VERSION`
    Schema,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Most commands return `Result<()>` (mapped to SUCCESS); `migrate`/`schema` own
    // their exit code (non-zero = pending/mismatch status, not an error).
    let result: anyhow::Result<ExitCode> = match cli.command {
        Commands::Stats => commands::stats::run(&cli.db, cli.format).map(|()| ExitCode::SUCCESS),
        Commands::Inspect { id } => {
            commands::inspect::run(&cli.db, id, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::Explain { id } => {
            commands::explain::run(&cli.db, id, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::Query(ref args) => {
            commands::query::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::Export(ref args) => {
            commands::export::run(&cli.db, args).map(|()| ExitCode::SUCCESS)
        }
        Commands::Import(ref args) => {
            commands::import::run(&cli.db, args).map(|()| ExitCode::SUCCESS)
        }
        Commands::Dump(ref args) => {
            commands::dump::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::AddFact(ref args) => {
            commands::add_fact::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::BatchIngest(ref args) => {
            commands::batch_ingest::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::Bootstrap(ref args) => {
            commands::bootstrap::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::RecordOutcome(ref args) => {
            commands::record_outcome::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::OutcomeCounts(ref args) => {
            commands::outcome_counts::run(&cli.db, args, cli.format).map(|()| ExitCode::SUCCESS)
        }
        Commands::Migrate(ref args) => commands::migrate::run(&cli.db, args, cli.format),
        Commands::Schema => commands::schema::run(&cli.db, cli.format),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
