//! `prune` — decay pruning as a maintenance command (ADR-0020 §8).
//!
//! It expires (never deletes) facts whose decayed importance falls below a threshold. It
//! writes the store, so it takes the store lock and refuses while the server runs.

use std::collections::HashMap;
use std::path::Path;

use memory_engine::ForgetPolicy;
use memory_engine::types::FactType;

use crate::db::open_engine_writable;
use crate::output::OutputFormat;

#[derive(clap::Args)]
pub struct PruneArgs {
    /// Base Ebbinghaus half-life in days
    #[arg(long)]
    half_life_days: Option<f64>,
    /// Facts whose decayed importance falls below this are expired, in [0, 1]
    #[arg(long)]
    min_importance: Option<f64>,
    /// Weight of the recency (decay) signal
    #[arg(long)]
    recency_weight: Option<f64>,
    /// Weight of the access-frequency signal
    #[arg(long)]
    frequency_weight: Option<f64>,
    /// Weight of the graph-connectivity signal
    #[arg(long)]
    graph_degree_weight: Option<f64>,
    /// Weight of the fact's base importance
    #[arg(long)]
    base_importance_weight: Option<f64>,
    /// Per-type half-life, `TYPE=DAYS` (repeatable), e.g. `episodic=30`
    #[arg(long = "half-life-override", value_name = "TYPE=DAYS", value_parser = parse_override)]
    half_life_overrides: Vec<(FactType, f64)>,
}

fn parse_override(value: &str) -> Result<(FactType, f64), String> {
    let (fact_type, days) = value
        .split_once('=')
        .ok_or_else(|| format!("expected TYPE=DAYS, got {value:?}"))?;
    let fact_type = fact_type.parse::<FactType>().map_err(|e| e.to_string())?;
    let days = days
        .parse::<f64>()
        .map_err(|_| format!("DAYS must be a number, got {days:?}"))?;
    Ok((fact_type, days))
}

fn policy(args: &PruneArgs) -> anyhow::Result<ForgetPolicy> {
    let mut policy = ForgetPolicy::default();
    let fields = [
        (args.half_life_days, &mut policy.half_life_days),
        (args.min_importance, &mut policy.min_importance),
        (args.recency_weight, &mut policy.recency_weight),
        (args.frequency_weight, &mut policy.frequency_weight),
        (args.graph_degree_weight, &mut policy.graph_degree_weight),
        (
            args.base_importance_weight,
            &mut policy.base_importance_weight,
        ),
    ];
    for (value, field) in fields {
        if let Some(value) = value {
            *field = value;
        }
    }
    let mut overrides = HashMap::new();
    for &(fact_type, days) in &args.half_life_overrides {
        anyhow::ensure!(
            overrides.insert(fact_type, days).is_none(),
            "--half-life-override names {fact_type} twice"
        );
    }
    policy.half_life_overrides = overrides;
    policy.validate()?;
    Ok(policy)
}

pub async fn run(db: &Path, args: &PruneArgs, format: OutputFormat) -> anyhow::Result<()> {
    // An invalid policy is refused before the store is touched.
    let policy = policy(args)?;
    let mut engine = open_engine_writable(db)?;
    let stats = engine.forget(&policy).await?;
    engine.close().await?;

    match format {
        OutputFormat::Json => crate::output::print_json(&serde_json::json!({
            "facts_expired": stats.facts_expired,
            "facts_evaluated": stats.facts_evaluated,
        }))?,
        OutputFormat::Table => println!(
            "expired {} of {} facts evaluated",
            stats.facts_expired, stats.facts_evaluated
        ),
        OutputFormat::Plain => {
            println!("facts_expired={}", stats.facts_expired);
            println!("facts_evaluated={}", stats.facts_evaluated);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(overrides: Vec<(FactType, f64)>) -> PruneArgs {
        PruneArgs {
            half_life_days: None,
            min_importance: None,
            recency_weight: None,
            frequency_weight: None,
            graph_degree_weight: None,
            base_importance_weight: None,
            half_life_overrides: overrides,
        }
    }

    #[test]
    fn defaults_are_the_forget_policy_defaults() {
        let built = policy(&args(Vec::new())).unwrap();
        let default = ForgetPolicy::default();
        assert_eq!(built.half_life_days, default.half_life_days);
        assert_eq!(built.min_importance, default.min_importance);
        assert!(built.half_life_overrides.is_empty());
    }

    #[test]
    fn an_override_parses_its_type_and_days() {
        assert_eq!(
            parse_override("episodic=30"),
            Ok((FactType::Episodic, 30.0))
        );
        assert_eq!(
            parse_override("Procedural=365"),
            Ok((FactType::Procedural, 365.0))
        );
    }

    #[test]
    fn a_malformed_override_is_refused() {
        for bad in ["episodic", "wisdom=30", "episodic=soon"] {
            assert!(parse_override(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_repeated_override_is_refused() {
        let err = policy(&args(vec![
            (FactType::Episodic, 30.0),
            (FactType::Episodic, 40.0),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");
    }
}
