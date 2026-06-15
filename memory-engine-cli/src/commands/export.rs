use std::path::{Path, PathBuf};

use memory_engine::inspect_types::DumpFormat;

use crate::db::{open_engine, open_engine_writable};

/// Serialization format for `export`.
///
/// The kebab-cased variant names are the stable CLI value tokens
/// (`json`, `sqlite`, `json-gz`, `json-zst`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ExportFormat {
    /// Plain JSON snapshot (default).
    #[default]
    Json,
    /// `SQLite` database copy (via `VACUUM INTO`).
    Sqlite,
    /// gzip-compressed JSON snapshot.
    #[value(name = "json-gz")]
    JsonGz,
    /// zstd-compressed JSON snapshot.
    #[value(name = "json-zst")]
    JsonZst,
}

#[derive(clap::Args)]
pub struct ExportArgs {
    /// Output file path
    #[arg(short, long)]
    output: PathBuf,

    /// Export format
    #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
    export_format: ExportFormat,
}

pub fn run(db: &Path, args: &ExportArgs) -> anyhow::Result<()> {
    // Only SQLite export needs write access (VACUUM INTO); others are read-only.
    let engine = if args.export_format == ExportFormat::Sqlite {
        open_engine_writable(db)?
    } else {
        open_engine(db)?
    };

    let format = match args.export_format {
        ExportFormat::Json => DumpFormat::Json(args.output.clone()),
        ExportFormat::Sqlite => DumpFormat::Sqlite(args.output.clone()),
        ExportFormat::JsonGz => DumpFormat::JsonGzip(args.output.clone()),
        ExportFormat::JsonZst => DumpFormat::JsonZstd(args.output.clone()),
    };

    engine.dump_state(&format)?;
    eprintln!("Exported to {}", args.output.display());
    Ok(())
}
