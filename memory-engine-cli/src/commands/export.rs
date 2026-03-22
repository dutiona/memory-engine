use std::path::{Path, PathBuf};

use memory_engine::inspect_types::DumpFormat;

use crate::db::open_engine;

#[derive(clap::Args)]
pub struct ExportArgs {
    /// Output file path
    #[arg(short, long)]
    output: PathBuf,

    /// Export format
    #[arg(long, default_value = "json", value_parser = ["json", "sqlite", "json-gz", "json-zst"])]
    export_format: String,
}

pub fn run(db: &Path, args: &ExportArgs) -> anyhow::Result<()> {
    let engine = open_engine(db)?;

    let format = match args.export_format.as_str() {
        "json" => DumpFormat::Json(args.output.clone()),
        "sqlite" => DumpFormat::Sqlite(args.output.clone()),
        "json-gz" => DumpFormat::JsonGzip(args.output.clone()),
        "json-zst" => DumpFormat::JsonZstd(args.output.clone()),
        _ => unreachable!("clap value_parser prevents unknown formats"),
    };

    engine.dump_state(&format)?;
    eprintln!("Exported to {}", args.output.display());
    Ok(())
}
