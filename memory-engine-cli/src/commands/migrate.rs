use std::path::Path;
use std::process::ExitCode;

use clap::Args;
use serde::Serialize;

use crate::db::{open_engine_writable, peek_schema_version_from_db};
use crate::output::{self, OutputFormat};

#[derive(Args)]
pub struct MigrateArgs {
    /// Dry-run: report which migrations would run and exit non-zero if any are
    /// pending, without mutating the database.
    #[arg(long)]
    pub check: bool,
}

/// Machine-readable migration report.
#[derive(Serialize)]
struct MigrateReport {
    schema_version: u32,
    current_schema_version: u32,
    /// Schema versions that are pending (would be / were applied): `live+1..=current`.
    pending: Vec<u32>,
    /// Whether migrations were actually applied (false for `--check` or up-to-date).
    migrated: bool,
    /// Whether this was a `--check` dry-run.
    checked: bool,
    /// Whether the DB is NEWER than this binary (forward-incompatible, cannot migrate).
    newer: bool,
}

/// Apply pending migrations to the live database, or (with `--check`) report them
/// without mutating.
///
/// `migrate` reuses the engine's transactional migration chain, which takes a
/// WAL-safe `VACUUM INTO` backup before mutating (see `open_engine_writable`).
///
/// Exit codes (release-gate contract):
/// - up-to-date → `0`
/// - `--check` with pending migrations → non-zero (a status signal, not an error)
/// - migrations applied → `0`
///
/// A genuine migration failure surfaces as `Err` (non-zero with an `error:` line);
/// the transactional chain rolls back and the pre-migration backup remains.
pub fn run(db: &Path, args: &MigrateArgs, format: OutputFormat) -> anyhow::Result<ExitCode> {
    let live = peek_schema_version_from_db(db)?;
    let current = memory_engine::CURRENT_SCHEMA_VERSION;
    // A database from a NEWER binary cannot be migrated forward by this one — the
    // migrate() primitive would reject it. Signal non-zero for both `migrate` and
    // `migrate --check` so a release gate never treats a rollback DB as "nothing to do".
    let newer = live > current;
    let pending: Vec<u32> = if live < current {
        (live + 1..=current).collect()
    } else {
        Vec::new()
    };

    let migrated = if args.check || pending.is_empty() {
        false
    } else {
        // Opening the engine writable runs the migration chain (transactional, with a
        // VACUUM-INTO backup first via the backup_dir set in open_engine_writable).
        let engine = open_engine_writable(db)?;
        drop(engine);
        true
    };

    let report = MigrateReport {
        schema_version: live,
        current_schema_version: current,
        pending: pending.clone(),
        migrated,
        checked: args.check,
        newer,
    };

    match format {
        OutputFormat::Json => output::print_json(&report)?,
        OutputFormat::Table => {
            if newer {
                println!(
                    "database schema_version {live} is NEWER than this binary (CURRENT_SCHEMA_VERSION {current}) — cannot migrate"
                );
            } else if pending.is_empty() {
                println!("no pending migrations: schema_version = {live} (current {current})");
            } else if args.check {
                let list = pending
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("pending migrations: {list} (schema_version {live} -> {current})");
            } else {
                println!(
                    "migrated schema_version {live} -> {current} (WAL-safe backup written next to the database)"
                );
            }
        }
        OutputFormat::Plain => {
            let list = pending
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            println!("schema_version={live}");
            println!("current_schema_version={current}");
            println!("pending={list}");
            println!("migrated={migrated}");
            println!("newer={newer}");
        }
    }

    Ok(if newer {
        // Newer-than-binary DB: forward-incompatible, cannot migrate.
        ExitCode::from(1)
    } else if pending.is_empty() || migrated {
        ExitCode::SUCCESS
    } else {
        // --check with pending migrations.
        ExitCode::from(1)
    })
}
