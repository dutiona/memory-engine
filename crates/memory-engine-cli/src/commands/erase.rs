//! `erase` — the one maintenance command that deletes (#1023, ADR-0020 §8).
//!
//! It deletes a fact, or every fact and event a machine wrote, then purges the store's files
//! of their earlier copies. It writes the store, so it takes the store lock and refuses
//! while the server runs. Backups keep their copies.

use std::path::Path;

use memory_engine::types::EraseTarget;

use crate::db::open_engine_writable;
use crate::output::OutputFormat;

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
pub struct EraseArgs {
    /// The fact to delete, by id
    #[arg(long)]
    fact: Option<i64>,
    /// The machine whose facts and events to delete, by name
    #[arg(long)]
    machine: Option<String>,
}

pub async fn run(db: &Path, args: &EraseArgs, format: OutputFormat) -> anyhow::Result<()> {
    let target = match (&args.fact, &args.machine) {
        (Some(id), _) => EraseTarget::Fact(*id),
        (None, Some(machine)) => EraseTarget::Machine(machine.clone()),
        (None, None) => unreachable!("clap requires exactly one target"),
    };
    let mut engine = open_engine_writable(db)?;
    let report = engine.erase(&target).await?;
    engine.close().await?;
    match format {
        OutputFormat::Json => crate::output::print_json(&report)?,
        OutputFormat::Table => {
            let counts: Vec<String> = report
                .deleted
                .iter()
                .map(|(kind, n)| format!("{kind}={n}"))
                .collect();
            println!("erased: {}", counts.join(" "));
            if !report.copies.is_empty() {
                println!("copies left: {:?}", report.copies);
            }
            if !report.orphaned.is_empty() {
                println!("orphaned (still expired): {:?}", report.orphaned);
            }
        }
        OutputFormat::Plain => {
            for (kind, n) in &report.deleted {
                println!("{kind}={n}");
            }
        }
    }
    let command = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if let Some(cause) = &report.purge_error {
        anyhow::bail!(
            "erased, not purged: {cause}; free disk space, stop any reader, then run this same \
             command again: memory-engine-cli {command}"
        );
    }
    if let (EraseTarget::Fact(id), true) = (&target, report.erased_facts.is_empty()) {
        anyhow::bail!("fact {id} not found; purged");
    }
    Ok(())
}
