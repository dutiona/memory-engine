use std::path::Path;

use tabled::{Table, Tabled};

use crate::db::open_engine;
use crate::output::{self, OutputFormat};

#[derive(Tabled)]
struct StatRow {
    #[tabled(rename = "Category")]
    category: &'static str,
    #[tabled(rename = "Metric")]
    metric: &'static str,
    #[tabled(rename = "Value")]
    value: String,
}

pub fn run(db: &Path, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;
    let stats = engine.statistics()?;

    match format {
        OutputFormat::Json => output::print_json(&stats)?,
        OutputFormat::Table => {
            let rows = vec![
                StatRow {
                    category: "Facts",
                    metric: "total",
                    value: stats.facts.total.to_string(),
                },
                StatRow {
                    category: "Facts",
                    metric: "active",
                    value: stats.facts.active.to_string(),
                },
                StatRow {
                    category: "Facts",
                    metric: "expired",
                    value: stats.facts.expired.to_string(),
                },
                StatRow {
                    category: "Facts",
                    metric: "pinned",
                    value: stats.facts.pinned.to_string(),
                },
                StatRow {
                    category: "Facts",
                    metric: "due",
                    value: stats.facts.due.to_string(),
                },
                StatRow {
                    category: "Edges",
                    metric: "total",
                    value: stats.edges.total.to_string(),
                },
                StatRow {
                    category: "Edges",
                    metric: "active",
                    value: stats.edges.active.to_string(),
                },
                StatRow {
                    category: "Events",
                    metric: "total",
                    value: stats.events.total.to_string(),
                },
                StatRow {
                    category: "Scopes",
                    metric: "total",
                    value: stats.scopes.total.to_string(),
                },
                StatRow {
                    category: "Scopes",
                    metric: "max_depth",
                    value: stats.scopes.max_depth.to_string(),
                },
                StatRow {
                    category: "Summaries",
                    metric: "total",
                    value: stats.summaries.total.to_string(),
                },
                StatRow {
                    category: "Storage",
                    metric: "size_bytes",
                    value: stats.storage.main_db_bytes.to_string(),
                },
                StatRow {
                    category: "Storage",
                    metric: "page_count",
                    value: stats.storage.page_count.to_string(),
                },
            ];
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            println!("facts.total={}", stats.facts.total);
            println!("facts.active={}", stats.facts.active);
            println!("facts.expired={}", stats.facts.expired);
            println!("facts.pinned={}", stats.facts.pinned);
            println!("facts.due={}", stats.facts.due);
            println!("edges.total={}", stats.edges.total);
            println!("edges.active={}", stats.edges.active);
            println!("events.total={}", stats.events.total);
            println!("scopes.total={}", stats.scopes.total);
            println!("scopes.max_depth={}", stats.scopes.max_depth);
            println!("summaries.total={}", stats.summaries.total);
            println!("storage.size_bytes={}", stats.storage.main_db_bytes);
            println!("storage.page_count={}", stats.storage.page_count);
        }
    }

    Ok(())
}
