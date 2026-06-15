use std::path::Path;

use clap::ValueEnum;
use memory_engine::types::Outcome;

use crate::db::open_engine_writable;
use crate::output::OutputFormat;

/// Outcome variant for CLI argument parsing.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum OutcomeArg {
    Positive,
    Negative,
    Neutral,
}

impl From<OutcomeArg> for Outcome {
    fn from(arg: OutcomeArg) -> Self {
        match arg {
            OutcomeArg::Positive => Self::Positive,
            OutcomeArg::Negative => Self::Negative,
            OutcomeArg::Neutral => Self::Neutral,
        }
    }
}

#[derive(clap::Args)]
pub struct RecordOutcomeArgs {
    /// Fact ID to record the outcome for
    #[arg(long)]
    fact_id: i64,

    /// Outcome signal
    #[arg(long, value_enum)]
    outcome: OutcomeArg,
}

pub fn run(db: &Path, args: &RecordOutcomeArgs, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine_writable(db)?;
    let outcome: Outcome = args.outcome.into();

    let event_id = engine.record_outcome(args.fact_id, outcome)?;

    match format {
        OutputFormat::Json => {
            crate::output::print_json(&serde_json::json!({
                "event_id": event_id,
                "fact_id": args.fact_id,
                "outcome": outcome,
            }))?;
        }
        OutputFormat::Table => {
            eprintln!(
                "Recorded {outcome} outcome for fact {} (event {event_id})",
                args.fact_id
            );
        }
        OutputFormat::Plain => {
            println!("{event_id}");
        }
    }

    Ok(())
}
