use std::path::Path;

use memory_engine::inspect_types::ReplayFilter;
use tabled::{Table, Tabled};

use crate::db::open_engine;
use crate::output::{self, OutputFormat, truncate_str};

/// What to dump from the database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DumpTarget {
    /// Active facts only (default).
    #[default]
    Facts,
    /// Event log only.
    Events,
    /// Both facts and events.
    All,
}

#[derive(clap::Args)]
pub struct DumpArgs {
    /// What to dump
    #[arg(value_enum, default_value_t = DumpTarget::Facts)]
    target: DumpTarget,

    /// Maximum items
    #[arg(long, default_value = "100")]
    limit: usize,
}

#[derive(Tabled)]
struct FactRow {
    #[tabled(rename = "ID")]
    id: i64,
    #[tabled(rename = "Type")]
    fact_type: String,
    #[tabled(rename = "Pinned")]
    pinned: &'static str,
    #[tabled(rename = "Score")]
    importance: String,
    #[tabled(rename = "Created")]
    created: String,
    #[tabled(rename = "Content")]
    content: String,
}

#[derive(Tabled)]
struct EventRow {
    #[tabled(rename = "ID")]
    id: i64,
    #[tabled(rename = "Type")]
    event_type: String,
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Timestamp")]
    timestamp: String,
    #[tabled(rename = "Session")]
    session: String,
}

pub async fn run(db: &Path, args: &DumpArgs, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;

    match args.target {
        DumpTarget::Facts => dump_facts(&engine, args.limit, format).await,
        DumpTarget::Events => dump_events(&engine, args.limit, format).await,
        DumpTarget::All => dump_all(&engine, args.limit, format).await,
    }
}

async fn dump_all(
    engine: &memory_engine::MemoryEngine,
    limit: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if format == OutputFormat::Json {
        let facts = engine.list_active_facts(Some(limit)).await?;
        let mut filter = ReplayFilter::default();
        filter.limit = Some(limit);
        let events = engine.replay_events(&filter).await?;
        let combined = serde_json::json!({
            "facts": facts,
            "events": events,
        });
        output::print_json(&combined)?;
    } else {
        dump_facts(engine, limit, format).await?;
        println!();
        dump_events(engine, limit, format).await?;
    }
    Ok(())
}

async fn dump_facts(
    engine: &memory_engine::MemoryEngine,
    limit: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let facts = engine.list_active_facts(Some(limit)).await?;

    match format {
        OutputFormat::Json => output::print_json(&facts)?,
        OutputFormat::Table => {
            let rows: Vec<FactRow> = facts
                .iter()
                .map(|f| FactRow {
                    id: f.id,
                    fact_type: format!("{:?}", f.fact_type),
                    pinned: if f.is_pinned { "yes" } else { "" },
                    importance: format!("{:.2}", f.importance_score),
                    created: f.t_created.format("%Y-%m-%d %H:%M").to_string(),
                    content: truncate_str(&f.content, 60),
                })
                .collect();
            println!("=== Active Facts ({}) ===", facts.len());
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            for f in &facts {
                println!("{}\t{:?}\t{}", f.id, f.fact_type, f.content);
            }
        }
    }
    Ok(())
}

async fn dump_events(
    engine: &memory_engine::MemoryEngine,
    limit: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut filter = ReplayFilter::default();
    filter.limit = Some(limit);
    let events = engine.replay_events(&filter).await?;

    match format {
        OutputFormat::Json => output::print_json(&events)?,
        OutputFormat::Table => {
            let rows: Vec<EventRow> = events
                .iter()
                .map(|e| EventRow {
                    id: e.id,
                    event_type: format!("{:?}", e.event_type),
                    source: e.source.clone(),
                    timestamp: e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    session: e.session_id.clone().unwrap_or_default(),
                })
                .collect();
            println!("=== Events ({}) ===", events.len());
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            for e in &events {
                println!(
                    "{}\t{:?}\t{}\t{}",
                    e.id, e.event_type, e.source, e.timestamp
                );
            }
        }
    }
    Ok(())
}
