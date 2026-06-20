use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use memory_engine_embed::HttpEmbeddingProvider;
use serde::{Deserialize, Serialize};

use crate::db::{open_engine_writable, open_engine_writable_with_dim};
use crate::output::{OutputFormat, print_json};

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
pub struct BatchIngestArgs {
    /// JSONL input file (use `-` for stdin)
    #[arg(long)]
    file: PathBuf,

    /// OpenAI-compatible embedding endpoint URL (e.g. `http://localhost:11434/v1/embeddings`)
    #[arg(long, env = "MEMORY_ENGINE_EMBED_URL")]
    embed_url: String,

    /// Embedding model name
    #[arg(long, env = "MEMORY_ENGINE_EMBED_MODEL")]
    embed_model: String,

    /// Bearer API key for the embedding endpoint
    #[arg(long, env = "MEMORY_ENGINE_EMBED_API_KEY")]
    embed_api_key: Option<String>,

    /// Facts per transaction batch (default: 100)
    #[arg(long, default_value = "100")]
    batch_size: usize,

    /// HTTP timeout in seconds for embedding calls
    #[arg(long, default_value = "30")]
    embed_timeout: u64,

    /// Create a new database (requires --embed-dim)
    #[arg(long)]
    create: bool,

    /// Embedding dimension (required with --create)
    #[arg(long)]
    embed_dim: Option<usize>,

    /// Default scope path for all ingested facts
    #[arg(long)]
    scope: Option<String>,
}

// ---------------------------------------------------------------------------
// JSONL deserialization types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JsonlFactType {
    Episodic,
    Semantic,
    Procedural,
}

impl From<JsonlFactType> for FactType {
    fn from(jft: JsonlFactType) -> Self {
        match jft {
            JsonlFactType::Episodic => Self::Episodic,
            JsonlFactType::Semantic => Self::Semantic,
            JsonlFactType::Procedural => Self::Procedural,
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonlFact {
    content: String,
    fact_type: JsonlFactType,
    #[serde(default)]
    source_event_id: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    importance: Option<f64>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    t_valid: Option<DateTime<Utc>>,
    #[serde(default)]
    t_invalid: Option<DateTime<Utc>>,
    #[serde(default)]
    pinned: Option<bool>,
    #[serde(default)]
    t_created: Option<DateTime<Utc>>,
    #[serde(default)]
    last_accessed: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Input validation (replicates MCP boundary checks)
// ---------------------------------------------------------------------------

fn validate_jsonl_fact(fact: &JsonlFact) -> Result<(), String> {
    if let Some(imp) = fact.importance
        && !(0.0..=1.0).contains(&imp)
    {
        return Err(format!("importance {imp} out of range [0, 1]"));
    }
    if let (Some(tv), Some(ti)) = (fact.t_valid, fact.t_invalid)
        && tv >= ti
    {
        return Err(format!("t_valid ({tv}) must be before t_invalid ({ti})"));
    }
    Ok(())
}

fn jsonl_to_request(fact: JsonlFact, default_scope: Option<&str>) -> AddFactRequest {
    let opts = AddFactOptions {
        importance: fact.importance,
        metadata: fact.metadata,
        t_valid: fact.t_valid,
        t_invalid: fact.t_invalid,
        pinned: fact.pinned,
        t_created: fact.t_created,
        last_accessed: fact.last_accessed,
    };
    AddFactRequest {
        content: fact.content,
        fact_type: fact.fact_type.into(),
        source_event_id: fact.source_event_id,
        scope: fact.scope.or_else(|| default_scope.map(str::to_owned)),
        opts: Some(opts),
    }
}

// ---------------------------------------------------------------------------
// Summary output
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct IngestSummary {
    pub(crate) total_ingested: usize,
    pub(crate) total_skipped: usize,
    pub(crate) failed_batches: usize,
    pub(crate) elapsed_secs: f64,
}

fn print_summary(summary: &IngestSummary, format: OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => print_json(summary)?,
        OutputFormat::Table => {
            println!("Batch ingest complete:");
            println!("  ingested:       {}", summary.total_ingested);
            println!("  skipped:        {}", summary.total_skipped);
            println!("  failed batches: {}", summary.failed_batches);
            println!("  elapsed:        {:.2}s", summary.elapsed_secs);
        }
        OutputFormat::Plain => {
            println!(
                "ingested={} skipped={} failed_batches={} elapsed_secs={:.2}",
                summary.total_ingested,
                summary.total_skipped,
                summary.failed_batches,
                summary.elapsed_secs,
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core ingestion logic (testable — accepts reader + embedder)
// ---------------------------------------------------------------------------

/// Maximum bytes read for a single JSONL record. A line longer than this is
/// skipped (and the reader resynced to the next line) rather than buffered, so a
/// single unterminated or pathologically long line cannot force an unbounded
/// `String` allocation (CWE-400 / CWE-770 / CWE-789). 8 MiB is far above any
/// realistic fact-as-JSON record while still bounding the worst case.
const MAX_LINE_BYTES: u64 = 8 << 20; // 8 MiB

/// Ingest facts from a byte stream of newline-delimited JSON (JSONL) records.
///
/// Each line must deserialize as a JSONL fact record. Malformed lines, lines
/// that fail validation, and lines exceeding [`MAX_LINE_BYTES`] are skipped
/// (counted in `total_skipped`) rather than aborting the run. Valid records are
/// embedded and inserted in transactions of `batch_size`; a batch the engine
/// rejects is counted as skipped plus one `failed_batches`, and ingestion
/// continues. Progress is reported on stderr unless `format` is JSON.
///
/// `default_scope` is applied to records that omit a `scope` field; a per-record
/// `scope` in the JSONL always takes precedence.
///
/// # Errors
///
/// Returns an error when the call ingested zero facts while at least one line was
/// skipped — i.e. every parseable record failed validation or batching. A
/// wholesale failure is surfaced rather than reported as a successful no-op.
pub fn ingest_from_reader(
    engine: &MemoryEngine,
    reader: impl Read,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    default_scope: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<IngestSummary> {
    let start = Instant::now();
    let mut buf = BufReader::new(reader);

    let mut total_ingested: usize = 0;
    let mut total_skipped: usize = 0;
    let mut failed_batches: usize = 0;
    let mut batch: Vec<AddFactRequest> = Vec::with_capacity(batch_size);
    let mut line_no: usize = 0;
    let mut line = String::new();

    loop {
        line.clear();
        // Cap each line read so a single oversized or unterminated record cannot
        // force an unbounded `String` allocation (CWE-400/770/789). Reading
        // `MAX_LINE_BYTES + 1` lets us detect a line that ran past the cap.
        let n = match buf.by_ref().take(MAX_LINE_BYTES + 1).read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) => {
                line_no += 1;
                eprintln!("warning: line {line_no}: read error: {e}");
                total_skipped += 1;
                continue;
            }
        };
        line_no += 1;

        if n as u64 > MAX_LINE_BYTES {
            eprintln!("warning: line {line_no}: exceeds {MAX_LINE_BYTES} bytes, skipping");
            total_skipped += 1;
            // Resync to the next record without buffering the rest of the line.
            if let Err(e) = buf.skip_until(b'\n') {
                eprintln!("warning: line {line_no}: read error while skipping: {e}");
                break;
            }
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let fact: JsonlFact = match serde_json::from_str(trimmed) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("warning: line {line_no}: parse error: {e}");
                total_skipped += 1;
                continue;
            }
        };

        if let Err(e) = validate_jsonl_fact(&fact) {
            eprintln!("warning: line {line_no}: validation error: {e}");
            total_skipped += 1;
            continue;
        }

        batch.push(jsonl_to_request(fact, default_scope));

        if batch.len() >= batch_size {
            let flushed = flush_batch(
                engine,
                &mut batch,
                embedder,
                &format!("batch at line {line_no}"),
                &mut total_ingested,
                &mut total_skipped,
                &mut failed_batches,
            );
            if flushed {
                eprint_progress(total_ingested, total_skipped, &start, format);
            }
        }
    }

    // Flush remaining partial batch (no-op if empty).
    flush_batch(
        engine,
        &mut batch,
        embedder,
        "final batch",
        &mut total_ingested,
        &mut total_skipped,
        &mut failed_batches,
    );

    let summary = IngestSummary {
        total_ingested,
        total_skipped,
        failed_batches,
        elapsed_secs: start.elapsed().as_secs_f64(),
    };

    if total_ingested == 0 && total_skipped > 0 {
        anyhow::bail!(
            "no facts ingested ({total_skipped} skipped, {failed_batches} failed batches)"
        );
    }

    Ok(summary)
}

fn eprint_progress(ingested: usize, skipped: usize, start: &Instant, format: OutputFormat) {
    // Suppress progress noise when JSON output is expected on stdout
    if format == OutputFormat::Json {
        return;
    }
    let elapsed = start.elapsed().as_secs_f64();
    eprint!("\r  ingested: {ingested}  skipped: {skipped}  elapsed: {elapsed:.1}s");
}

/// Flush the accumulated `batch` to the engine, updating the running counters in
/// place, and clear it. Returns `true` when the batch was ingested, `false` when
/// the engine rejected it (the batch is counted as skipped + one failed batch).
///
/// `context` is the location phrase used in the failure warning, e.g.
/// `"batch at line 42"` or `"final batch"`. An empty batch is a no-op.
fn flush_batch(
    engine: &MemoryEngine,
    batch: &mut Vec<AddFactRequest>,
    embedder: &dyn EmbeddingProvider,
    context: &str,
    total_ingested: &mut usize,
    total_skipped: &mut usize,
    failed_batches: &mut usize,
) -> bool {
    if batch.is_empty() {
        return true;
    }
    let chunk_size = batch.len();
    let ingested = match engine.add_facts_batch(batch.as_slice(), embedder, None) {
        Ok(ids) => {
            *total_ingested += ids.len();
            true
        }
        Err(e) => {
            eprintln!("warning: {context}: {e}");
            *total_skipped += chunk_size;
            *failed_batches += 1;
            false
        }
    };
    batch.clear();
    ingested
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Maximum accepted `--batch-size`. Values above this would allocate an unbounded
/// `Vec` from operator-supplied input, enabling a trivial OOM denial-of-service.
const MAX_BATCH_SIZE: usize = 10_000;

pub fn run(db: &Path, args: &BatchIngestArgs, format: OutputFormat) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.batch_size > 0 && args.batch_size <= MAX_BATCH_SIZE,
        "--batch-size must be between 1 and {MAX_BATCH_SIZE}, got {}",
        args.batch_size,
    );

    // Open or create engine
    let engine = if args.create {
        let embed_dim = args
            .embed_dim
            .ok_or_else(|| anyhow::anyhow!("--embed-dim is required when using --create"))?;
        anyhow::ensure!(
            !db.exists(),
            "database {} already exists — remove it first or omit --create",
            db.display()
        );
        MemoryEngine::builder(embed_dim)
            .path(db.to_path_buf())
            .build()?
    } else if let Some(dim) = args.embed_dim {
        // Existing DB: accept an explicit --embed-dim so a never-embedded store (no
        // recorded identity yet under #613) is still writable. The engine's open path
        // rejects a mismatch against any recorded identity.
        open_engine_writable_with_dim(db, dim)?
    } else {
        open_engine_writable(db)?
    };

    // Build embedding provider
    let embedder = HttpEmbeddingProvider::new(
        args.embed_url.clone(),
        args.embed_model.clone(),
        "ollama".to_string(), // TODO(#618): provider should come from config/CLI
        args.embed_api_key.clone(),
        engine.embed_dim(),
        args.embed_timeout,
    )
    .map_err(|e| anyhow::anyhow!("failed to create embedding provider: {e}"))?;

    // Open input — bind stdin before locking to extend lifetime
    let stdin = std::io::stdin();
    let reader: Box<dyn Read> = if args.file.as_os_str() == "-" {
        Box::new(stdin.lock())
    } else {
        Box::new(
            std::fs::File::open(&args.file)
                .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", args.file.display()))?,
        )
    };

    let summary = ingest_from_reader(
        &engine,
        reader,
        &embedder,
        args.batch_size,
        args.scope.as_deref(),
        format,
    )?;

    // Clear progress line before final output
    if format != OutputFormat::Json {
        eprintln!();
    }

    print_summary(&summary, format)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_fact_deserialize_lowercase() {
        let line = r#"{"content":"hello","fact_type":"episodic"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert!(matches!(fact.fact_type, JsonlFactType::Episodic));

        let line = r#"{"content":"hello","fact_type":"semantic"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert!(matches!(fact.fact_type, JsonlFactType::Semantic));

        let line = r#"{"content":"hello","fact_type":"procedural"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert!(matches!(fact.fact_type, JsonlFactType::Procedural));
    }

    #[test]
    fn jsonl_fact_minimal_fields() {
        let line = r#"{"content":"Paris is in France","fact_type":"semantic"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert_eq!(fact.content, "Paris is in France");
        assert!(fact.importance.is_none());
        assert!(fact.t_valid.is_none());
        assert!(fact.metadata.is_none());
        assert!(fact.scope.is_none());
    }

    #[test]
    fn jsonl_fact_all_fields() {
        let line = r#"{
            "content": "User moved to Istanbul",
            "fact_type": "episodic",
            "t_valid": "2026-03-01T00:00:00Z",
            "t_invalid": "2026-06-01T00:00:00Z",
            "importance": 0.7,
            "metadata": {"source": "beam-conv-3"},
            "scope": "project/beam",
            "pinned": true
        }"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert_eq!(fact.content, "User moved to Istanbul");
        assert!(matches!(fact.fact_type, JsonlFactType::Episodic));
        assert_eq!(fact.importance, Some(0.7));
        assert!(fact.t_valid.is_some());
        assert!(fact.t_invalid.is_some());
        assert_eq!(fact.pinned, Some(true));
        assert_eq!(fact.scope, Some("project/beam".into()));
    }

    #[test]
    fn jsonl_fact_invalid_fact_type() {
        let line = r#"{"content":"hello","fact_type":"unknown"}"#;
        let result: Result<JsonlFact, _> = serde_json::from_str(line);
        assert!(result.is_err());
    }

    #[test]
    fn jsonl_fact_to_request_mapping() {
        let line = r#"{"content":"test","fact_type":"semantic","importance":0.8,"t_valid":"2026-01-01T00:00:00Z"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        let req = jsonl_to_request(fact, None);
        assert_eq!(req.content, "test");
        assert_eq!(req.fact_type, FactType::Semantic);
        let opts = req.opts.unwrap();
        assert_eq!(opts.importance, Some(0.8));
        assert!(opts.t_valid.is_some());
    }

    #[test]
    fn validate_importance_out_of_range() {
        let fact = JsonlFact {
            content: "test".into(),
            fact_type: JsonlFactType::Semantic,
            source_event_id: None,
            scope: None,
            importance: Some(1.5),
            metadata: None,
            t_valid: None,
            t_invalid: None,
            pinned: None,
            t_created: None,
            last_accessed: None,
        };
        assert!(validate_jsonl_fact(&fact).is_err());
    }

    #[test]
    fn validate_temporal_consistency() {
        let t1 = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let t2 = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let fact = JsonlFact {
            content: "test".into(),
            fact_type: JsonlFactType::Semantic,
            source_event_id: None,
            scope: None,
            importance: None,
            metadata: None,
            t_valid: Some(t1),
            t_invalid: Some(t2), // t_invalid before t_valid
            pinned: None,
            t_created: None,
            last_accessed: None,
        };
        assert!(validate_jsonl_fact(&fact).is_err());
    }

    #[test]
    fn validate_valid_fact_passes() {
        let fact = JsonlFact {
            content: "test".into(),
            fact_type: JsonlFactType::Semantic,
            source_event_id: None,
            scope: None,
            importance: Some(0.5),
            metadata: None,
            t_valid: None,
            t_invalid: None,
            pinned: None,
            t_created: None,
            last_accessed: None,
        };
        assert!(validate_jsonl_fact(&fact).is_ok());
    }

    #[test]
    fn default_scope_applied() {
        let line = r#"{"content":"test","fact_type":"semantic"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        let req = jsonl_to_request(fact, Some("project/beam"));
        assert_eq!(req.scope, Some("project/beam".into()));
    }

    #[test]
    fn explicit_scope_overrides_default() {
        let line = r#"{"content":"test","fact_type":"semantic","scope":"custom"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        let req = jsonl_to_request(fact, Some("project/beam"));
        assert_eq!(req.scope, Some("custom".into()));
    }

    #[test]
    fn ingest_from_reader_with_fake_embedder() {
        struct FakeEmbed;
        impl EmbeddingProvider for FakeEmbed {
            fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
            }
            fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
                memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = r#"{"content":"fact one","fact_type":"semantic"}
{"content":"fact two","fact_type":"episodic","importance":0.8}
"#;
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &FakeEmbed,
            100,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 2);
        assert_eq!(summary.total_skipped, 0);
        assert_eq!(summary.failed_batches, 0);
    }

    #[test]
    fn ingest_skips_malformed_lines() {
        struct FakeEmbed;
        impl EmbeddingProvider for FakeEmbed {
            fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
            }
            fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
                memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = r#"{"content":"good","fact_type":"semantic"}
not valid json
{"content":"also good","fact_type":"procedural"}
"#;
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &FakeEmbed,
            100,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 2);
        assert_eq!(summary.total_skipped, 1);
    }

    #[test]
    fn ingest_skips_invalid_importance() {
        struct FakeEmbed;
        impl EmbeddingProvider for FakeEmbed {
            fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
            }
            fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
                memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = r#"{"content":"good","fact_type":"semantic","importance":0.5}
{"content":"bad","fact_type":"semantic","importance":2.0}
"#;
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &FakeEmbed,
            100,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 1);
        assert_eq!(summary.total_skipped, 1);
    }

    #[test]
    fn ingest_empty_input() {
        struct FakeEmbed;
        impl EmbeddingProvider for FakeEmbed {
            fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
                Ok(vec![0.1, 0.2, 0.3, 0.4])
            }
            fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
                memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        let summary =
            ingest_from_reader(&engine, &b""[..], &FakeEmbed, 100, None, OutputFormat::Json);
        // Empty input = 0 ingested, 0 skipped → returns Ok (not an error)
        let s = summary.unwrap();
        assert_eq!(s.total_ingested, 0);
        assert_eq!(s.total_skipped, 0);
    }

    // --- per-line size cap (#408) + flush/error-path coverage (#431, #432) ---

    struct CapEmbed;
    impl EmbeddingProvider for CapEmbed {
        fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
            memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    #[test]
    fn skips_oversized_line_then_resumes() {
        // A single record larger than the per-line cap must be dropped at the read
        // boundary and ingestion must resync to the next record. Without the cap the
        // giant line is read whole and handed to the engine, whose content-size
        // limit rejects it; sharing a batch with the valid record, that failure
        // poisons the whole batch and the call bails (verified RED: "no facts
        // ingested"). With the cap the oversized line never reaches the engine, so
        // the valid record ingests cleanly.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let huge = "x".repeat(9 * 1024 * 1024); // > MAX_LINE_BYTES (8 MiB)
        let input = format!(
            "{{\"content\":\"{huge}\",\"fact_type\":\"semantic\"}}\n\
             {{\"content\":\"ok\",\"fact_type\":\"semantic\"}}\n"
        );
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &CapEmbed,
            100,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 1, "only the normal record ingests");
        assert!(
            summary.total_skipped >= 1,
            "the oversized line must be skipped"
        );
    }

    #[test]
    fn oversized_unterminated_final_line_at_eof() {
        // An oversized line that is also the LAST line and has no trailing newline
        // must be skipped cleanly: skip_until hits EOF and returns Ok, the loop
        // ends, and the earlier valid record is unaffected. Guards against a hang
        // or panic on the EOF-mid-skip path.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let huge = "x".repeat(9 * 1024 * 1024); // > MAX_LINE_BYTES, no closing brace, no \n
        let input =
            format!("{{\"content\":\"ok\",\"fact_type\":\"semantic\"}}\n{{\"content\":\"{huge}");
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &CapEmbed,
            100,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 1, "the valid first record ingests");
        assert!(
            summary.total_skipped >= 1,
            "the oversized final line is skipped"
        );
    }

    #[test]
    fn mid_stream_flush_with_batch_size_one() {
        // batch_size=1 forces a flush after every record, exercising the mid-stream
        // flush path — not just the final partial flush the other tests hit.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = "{\"content\":\"a\",\"fact_type\":\"semantic\"}\n\
                     {\"content\":\"b\",\"fact_type\":\"episodic\"}\n\
                     {\"content\":\"c\",\"fact_type\":\"procedural\"}\n";
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &CapEmbed,
            1,
            None,
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(summary.total_ingested, 3);
        assert_eq!(summary.total_skipped, 0);
        assert_eq!(summary.failed_batches, 0);
    }

    #[test]
    fn all_bad_lines_returns_error() {
        // 0 ingested AND >0 skipped → the function bails rather than reporting a
        // successful no-op. The empty-input test does not hit this (skipped is 0).
        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = "not json\nalso not json\n";
        let result = ingest_from_reader(
            &engine,
            input.as_bytes(),
            &CapEmbed,
            100,
            None,
            OutputFormat::Json,
        );
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("no facts ingested"), "got: {msg}");
    }
}
