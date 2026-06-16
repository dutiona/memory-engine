use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use crate::db::peek_schema_version_from_db;
use crate::output::{self, OutputFormat};

/// Machine-readable schema-version report.
#[derive(Serialize)]
struct SchemaReport {
    schema_version: u32,
    current_schema_version: u32,
    matches: bool,
}

/// Report the database's live `schema_version` against the binary's
/// `CURRENT_SCHEMA_VERSION`.
///
/// This is the release-gate **verify hook**: exit `0` when they match, non-zero
/// on any mismatch (the live DB is stale and needs `migrate`, or is from a newer
/// binary). It never mutates the database.
pub fn run(db: &Path, format: OutputFormat) -> anyhow::Result<ExitCode> {
    let live = peek_schema_version_from_db(db)?;
    let current = memory_engine::CURRENT_SCHEMA_VERSION;
    let report = SchemaReport {
        schema_version: live,
        current_schema_version: current,
        matches: live == current,
    };

    match format {
        OutputFormat::Json => output::print_json(&report)?,
        OutputFormat::Table => {
            if report.matches {
                println!("schema up to date: schema_version = {live} (current {current})");
            } else {
                println!(
                    "schema MISMATCH: schema_version = {live}, binary CURRENT_SCHEMA_VERSION = {current}"
                );
            }
        }
        OutputFormat::Plain => {
            println!("schema_version={live}");
            println!("current_schema_version={current}");
            println!("matches={}", report.matches);
        }
    }

    Ok(if report.matches {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
