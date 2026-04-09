use std::path::Path;

use crate::db::open_engine;
use crate::output::OutputFormat;

#[derive(clap::Args)]
pub struct OutcomeCountsArgs {
    /// Fact ID to query outcome counts for
    #[arg(long)]
    fact_id: i64,
}

pub fn run(db: &Path, args: &OutcomeCountsArgs, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;
    let counts = engine.get_outcome_counts(args.fact_id)?;

    match format {
        OutputFormat::Json => {
            crate::output::print_json(&serde_json::json!({
                "fact_id": args.fact_id,
                "positive": counts.positive,
                "negative": counts.negative,
                "neutral": counts.neutral,
            }))?;
        }
        OutputFormat::Table => {
            println!("Outcome counts for fact {}:", args.fact_id);
            println!("  positive: {}", counts.positive);
            println!("  negative: {}", counts.negative);
            println!("  neutral:  {}", counts.neutral);
        }
        OutputFormat::Plain => {
            println!(
                "+{} -{} ~{}",
                counts.positive, counts.negative, counts.neutral
            );
        }
    }

    Ok(())
}
