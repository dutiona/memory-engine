use std::path::Path;

use tabled::{Table, Tabled};

use crate::db::open_engine;
use crate::output::{self, OutputFormat};

#[derive(Tabled)]
struct FactField {
    #[tabled(rename = "Field")]
    field: &'static str,
    #[tabled(rename = "Value")]
    value: String,
}

pub async fn run(db: &Path, fact_id: i64, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;
    let fact = engine.get_fact(fact_id).await?;

    match format {
        OutputFormat::Json => output::print_json(&fact)?,
        OutputFormat::Table => {
            let rows = vec![
                FactField {
                    field: "id",
                    value: fact.id.to_string(),
                },
                FactField {
                    field: "content",
                    value: fact.content.clone(),
                },
                FactField {
                    field: "fact_type",
                    value: format!("{:?}", fact.fact_type),
                },
                FactField {
                    field: "importance",
                    value: format!("{:.4}", fact.importance),
                },
                FactField {
                    field: "importance_score",
                    value: format!("{:.4}", fact.importance_score),
                },
                FactField {
                    field: "is_pinned",
                    value: fact.is_pinned.to_string(),
                },
                FactField {
                    field: "access_count",
                    value: fact.access_count.to_string(),
                },
                FactField {
                    field: "scope_id",
                    value: fact.scope_id.to_string(),
                },
                FactField {
                    field: "t_created",
                    value: fact.t_created.to_rfc3339(),
                },
                FactField {
                    field: "t_expired",
                    value: fact
                        .t_expired
                        .map_or_else(|| "\u{2014}".into(), |t| t.to_rfc3339()),
                },
                FactField {
                    field: "t_valid",
                    value: fact
                        .t_valid
                        .map_or_else(|| "\u{2014}".into(), |t| t.to_rfc3339()),
                },
                FactField {
                    field: "t_invalid",
                    value: fact
                        .t_invalid
                        .map_or_else(|| "\u{2014}".into(), |t| t.to_rfc3339()),
                },
                FactField {
                    field: "surfaced_at",
                    value: fact
                        .surfaced_at
                        .map_or_else(|| "\u{2014}".into(), |t| t.to_rfc3339()),
                },
                FactField {
                    field: "source_event_id",
                    value: fact
                        .source_event_id
                        .map_or_else(|| "\u{2014}".into(), |id| id.to_string()),
                },
                FactField {
                    field: "last_accessed",
                    value: fact.last_accessed.to_rfc3339(),
                },
                FactField {
                    field: "content_hash",
                    value: fact.content_hash.clone(),
                },
                FactField {
                    field: "metadata",
                    value: fact.metadata.to_string(),
                },
            ];
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            println!("{}", fact.content);
        }
    }

    Ok(())
}
