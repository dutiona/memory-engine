use std::path::Path;

use tabled::{Table, Tabled};

use crate::db::open_engine;
use crate::output::{self, OutputFormat};

#[derive(Tabled)]
struct ExplainRow {
    #[tabled(rename = "Aspect")]
    aspect: &'static str,
    #[tabled(rename = "Detail")]
    detail: String,
}

pub async fn run(db: &Path, fact_id: i64, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine(db)?;
    let explanation = engine.explain_fact(fact_id).await?;

    match format {
        OutputFormat::Json => output::print_json(&explanation)?,
        OutputFormat::Table => {
            let rows = vec![
                ExplainRow {
                    aspect: "fact_id",
                    detail: explanation.fact_id.to_string(),
                },
                ExplainRow {
                    aspect: "state",
                    detail: format!("{:?}", explanation.state),
                },
                ExplainRow {
                    aspect: "scope_path",
                    detail: explanation.scope_path.clone(),
                },
                ExplainRow {
                    aspect: "importance",
                    detail: format!(
                        "base={:.4}  composite={:.4}",
                        explanation.provenance.importance, explanation.provenance.importance_score,
                    ),
                },
                ExplainRow {
                    aspect: "is_pinned",
                    detail: explanation.provenance.is_pinned.to_string(),
                },
                ExplainRow {
                    aspect: "access_count",
                    detail: explanation.provenance.access_count.to_string(),
                },
                ExplainRow {
                    aspect: "source_event",
                    detail: explanation
                        .provenance
                        .source_event_id
                        .map_or_else(|| "\u{2014}".into(), |id| id.to_string()),
                },
                ExplainRow {
                    aspect: "graph",
                    detail: format!(
                        "degree={} component_size={} neighbors={:?}",
                        explanation.graph_context.degree,
                        explanation.graph_context.component_size,
                        explanation.graph_context.neighbor_ids,
                    ),
                },
            ];
            println!("{}", Table::new(rows));
        }
        OutputFormat::Plain => {
            println!("state: {:?}", explanation.state);
            println!("scope: {}", explanation.scope_path);
            println!("pinned: {}", explanation.provenance.is_pinned);
            println!("importance: {:.4}", explanation.provenance.importance_score);
            println!("degree: {}", explanation.graph_context.degree);
        }
    }

    Ok(())
}
