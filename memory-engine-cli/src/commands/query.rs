use std::path::Path;

use chrono::{DateTime, Utc};
use memory_engine::MemoryQuery;
use memory_engine::search::hybrid::MatchType;
use tabled::{Table, Tabled};

use crate::db::open_engine;
use crate::output::{self, OutputFormat, truncate_str};

#[derive(clap::Args)]
pub struct QueryArgs {
    /// Search text (FTS5 full-text search)
    text: String,

    /// Maximum results
    #[arg(long, default_value = "10")]
    limit: usize,

    /// Scope path filter (subtree match)
    #[arg(long)]
    scope: Option<String>,

    /// Filter by fact type (episodic, semantic, procedural)
    #[arg(long)]
    fact_type: Option<String>,

    /// Minimum importance score
    #[arg(long)]
    min_importance: Option<f64>,

    /// Show only pinned facts
    #[arg(long)]
    pinned_only: bool,

    /// Filter by bi-temporal validity (ISO 8601, e.g. 2026-03-25T00:00:00Z).
    /// Returns facts valid at this point in time:
    /// t_valid <= dt AND (t_invalid IS NULL OR t_invalid > dt).
    #[arg(long, value_parser = parse_datetime)]
    valid_at: Option<DateTime<Utc>>,
}

fn parse_datetime(s: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("invalid ISO 8601 datetime: {e}"))
}

#[derive(Tabled)]
struct ResultRow {
    #[tabled(rename = "ID")]
    id: i64,
    #[tabled(rename = "Score")]
    score: String,
    #[tabled(rename = "Match")]
    match_type: String,
    #[tabled(rename = "Type")]
    fact_type: String,
    #[tabled(rename = "Pinned")]
    pinned: &'static str,
    #[tabled(rename = "Valid")]
    t_valid: String,
    #[tabled(rename = "Invalid")]
    t_invalid: String,
    #[tabled(rename = "Content")]
    content: String,
}

fn fmt_optional_dt(dt: Option<&DateTime<Utc>>) -> String {
    match dt {
        Some(t) => t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        None => String::new(),
    }
}

pub fn run(db: &Path, args: &QueryArgs, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;

    let mut query = MemoryQuery::new().text(&args.text).limit(args.limit);

    if let Some(scope) = &args.scope {
        query = query.scope_subtree(scope);
    }

    if let Some(score) = args.min_importance {
        query = query.min_importance_score(score);
    }

    if let Some(ft) = &args.fact_type {
        let fact_type = match ft.to_lowercase().as_str() {
            "episodic" => memory_engine::FactType::Episodic,
            "semantic" => memory_engine::FactType::Semantic,
            "procedural" => memory_engine::FactType::Procedural,
            other => anyhow::bail!(
                "unknown fact type: {other} (expected: episodic, semantic, procedural)"
            ),
        };
        query = query.fact_type(fact_type);
    }

    if args.pinned_only {
        query = query.pinned_only();
    }

    if let Some(dt) = args.valid_at {
        query = query.valid_at(dt);
    }

    let response = engine.execute_query(&query)?;
    let results = response.results;

    match format {
        OutputFormat::Json => output::print_json(&results)?,
        OutputFormat::Table => {
            if results.is_empty() {
                println!("No results.");
                return Ok(());
            }
            let rows: Vec<ResultRow> = results
                .iter()
                .map(|r| ResultRow {
                    id: r.fact.id,
                    score: format!("{:.4}", r.score),
                    match_type: match r.match_type {
                        MatchType::Fts => "FTS".into(),
                        MatchType::Vector => "VEC".into(),
                        MatchType::Both => "BOTH".into(),
                        MatchType::ImportanceRank => "RANK".into(),
                        _ => "?".into(),
                    },
                    fact_type: format!("{:?}", r.fact.fact_type),
                    pinned: if r.fact.is_pinned { "yes" } else { "" },
                    t_valid: fmt_optional_dt(r.fact.t_valid.as_ref()),
                    t_invalid: fmt_optional_dt(r.fact.t_invalid.as_ref()),
                    content: truncate_str(&r.fact.content, 80),
                })
                .collect();
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            for r in &results {
                println!(
                    "{}\t{:.4}\t{}\t{}\t{}",
                    r.fact.id,
                    r.score,
                    fmt_optional_dt(r.fact.t_valid.as_ref()),
                    fmt_optional_dt(r.fact.t_invalid.as_ref()),
                    r.fact.content,
                );
            }
        }
    }

    Ok(())
}
