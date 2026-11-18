//! `unforget` — reverse a forget (#1023, ADR-0020 §8).
//!
//! It returns every fact the forget retracted to the state it had before. It writes the
//! store, so it takes the store lock and refuses while the server runs.

use std::path::Path;

use memory_engine::types::UnforgetOutcome;

use crate::db::open_engine_writable;
use crate::output::OutputFormat;

pub async fn run(db: &Path, id: i64, format: OutputFormat) -> anyhow::Result<()> {
    let mut engine = open_engine_writable(db)?;
    let outcome = engine.unforget_fact(id).await?;
    // The restore is committed whatever close does: say what came back before its error.
    let closed = engine.close().await;
    let then = closed
        .as_ref()
        .err()
        .map(|e| format!("; closing the store then failed: {e}"))
        .unwrap_or_default();
    let facts = match outcome {
        UnforgetOutcome::Restored { facts } => facts,
        UnforgetOutcome::NotForgotten => anyhow::bail!("fact {id} is not forgotten{then}"),
        UnforgetOutcome::NotFound => anyhow::bail!("fact {id} not found{then}"),
    };
    match format {
        OutputFormat::Json => crate::output::print_json(&serde_json::json!({
            "restored": facts
                .iter()
                .map(|(fact_id, active)| serde_json::json!({ "fact_id": fact_id, "active": active }))
                .collect::<Vec<_>>(),
        }))?,
        OutputFormat::Table => {
            for (fact_id, active) in &facts {
                if *active {
                    println!("unforgot fact {fact_id}: back in recall");
                } else {
                    println!(
                        "unforgot fact {fact_id}: still out of recall (it was expired or invalid \
                         before the forget)"
                    );
                }
            }
        }
        OutputFormat::Plain => {
            for (fact_id, active) in &facts {
                println!("fact_id={fact_id} active={active}");
            }
        }
    }
    closed?;
    Ok(())
}
