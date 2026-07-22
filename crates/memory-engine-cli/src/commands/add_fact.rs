use std::path::Path;

use chrono::{DateTime, Utc};
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};

use crate::commands::types::CliFactType;
use crate::db::open_engine_writable_with_dim;
use crate::embedding::PassthroughEmbedder;
use crate::output::{self, OutputFormat, parse_datetime};

#[derive(clap::Args)]
pub struct AddFactArgs {
    /// The fact text to store
    #[arg(long)]
    content: String,

    /// Fact type (episodic, semantic, procedural; case-insensitive)
    #[arg(long, value_enum, ignore_case = true)]
    fact_type: CliFactType,

    /// Pre-computed embedding as a JSON array of floats (e.g., "[0.1, 0.2, 0.3]")
    #[arg(long)]
    embedding: String,

    /// Real-world validity start (RFC 3339, e.g., "2026-03-01T00:00:00Z")
    #[arg(long, value_parser = parse_datetime)]
    t_valid: Option<DateTime<Utc>>,

    /// Real-world validity end (RFC 3339)
    #[arg(long, value_parser = parse_datetime)]
    t_invalid: Option<DateTime<Utc>>,

    /// Base importance prior in [0, 1] (default: 0.5). The static seed for the
    /// computed `importance_score`, exposed as `--base-importance`.
    #[arg(long)]
    base_importance: Option<f64>,

    /// Scope path (e.g., "project/sub"). Auto-creates missing segments. Default: root
    #[arg(long)]
    scope: Option<String>,

    /// Arbitrary JSON metadata (must be a JSON object, e.g., '{"source":"beam"}')
    #[arg(long)]
    metadata: Option<String>,

    /// Pin this fact (unforgettable — exempt from decay)
    #[arg(long)]
    pinned: bool,

    /// Link to an existing event ID in the database
    #[arg(long)]
    source_event_id: Option<i64>,
}

pub async fn run(db: &Path, args: &AddFactArgs, format: OutputFormat) -> anyhow::Result<()> {
    // Parse embedding
    let embedding: Vec<f32> = serde_json::from_str(&args.embedding)
        .map_err(|e| anyhow::anyhow!("invalid embedding JSON: {e}"))?;
    anyhow::ensure!(!embedding.is_empty(), "embedding must not be empty");

    // Validate importance
    if let Some(imp) = args.base_importance {
        anyhow::ensure!(
            (0.0..=1.0).contains(&imp),
            "base_importance must be in [0, 1], got {imp}"
        );
    }

    // Validate temporal consistency
    if let (Some(tv), Some(ti)) = (args.t_valid, args.t_invalid) {
        anyhow::ensure!(tv < ti, "t-valid ({tv}) must be before t-invalid ({ti})");
    }

    // Parse and validate metadata (explicit null treated as absent)
    let metadata = match &args.metadata {
        Some(raw) => {
            let val: serde_json::Value = serde_json::from_str(raw)
                .map_err(|e| anyhow::anyhow!("invalid metadata JSON: {e}"))?;
            if val.is_null() {
                None
            } else {
                anyhow::ensure!(
                    val.is_object(),
                    "metadata must be a JSON object, got {}",
                    match &val {
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Bool(_) => "boolean",
                        serde_json::Value::Null | serde_json::Value::Object(_) => unreachable!(),
                    }
                );
                Some(val)
            }
        }
        None => None,
    };

    // Derive the engine dimension from the supplied embedding rather than peeking
    // the database: this works on a freshly-created, never-embedded store (which
    // has no recorded dimension under #613). If the store was previously embedded
    // at a different dimension, the engine's open path rejects the mismatch.
    let mut engine = open_engine_writable_with_dim(db, embedding.len())?;
    let fact_type: FactType = args.fact_type.into();

    let req = AddFactRequest {
        content: args.content.clone(),
        fact_type,
        source_event_id: args.source_event_id,
        scope: args.scope.clone(),
        opts: Some(AddFactOptions {
            base_importance: args.base_importance,
            metadata,
            t_valid: args.t_valid,
            t_invalid: args.t_invalid,
            pinned: if args.pinned { Some(true) } else { None },
            ..Default::default()
        }),
    };

    let embedder = PassthroughEmbedder::new(embedding);
    let fact_id = engine
        .add_fact(&req, std::sync::Arc::new(embedder), None)
        .await?;

    match format {
        OutputFormat::Json => {
            let fact = engine.get_fact(fact_id).await?;
            output::print_json(&fact)?;
        }
        OutputFormat::Table => {
            let base_importance = args.base_importance.unwrap_or(0.5);
            eprintln!(
                "Created fact {fact_id} ({fact_type}, base_importance={base_importance:.2}{})",
                if args.pinned { ", pinned" } else { "" }
            );
        }
        OutputFormat::Plain => {
            println!("{fact_id}");
        }
    }

    // Persist the in-memory projections to the sidecar snapshot before the engine
    // drops, so the next open does not rebuild the HNSW index from the DB (#728 review C).
    engine.close().await?;
    Ok(())
}
