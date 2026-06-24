use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use serde::{Deserialize, Serialize};

use crate::commands::embedding_args::EmbeddingArgs;
use crate::commands::types::deserialize_fact_type;
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

    /// Embedding provider config (shared with query/bootstrap; #619). `--embed-url` +
    /// `--embed-model` are required here. Documents are embedded via `embed_batch`.
    #[command(flatten)]
    embed: EmbeddingArgs,

    /// Facts per transaction batch (default: 100)
    #[arg(long, default_value = "100")]
    batch_size: usize,

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
struct JsonlFact {
    content: String,
    #[serde(deserialize_with = "deserialize_fact_type")]
    fact_type: FactType,
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
        fact_type: fact.fact_type,
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
/// embedded and inserted in transactions of `batch_size` via the partial-success
/// engine path (#663): a record the engine rejects (e.g. oversized content) is
/// counted as skipped individually, leaving its batch-mates ingested. Only a
/// **batch-level** failure (embedder error or atomic-insert rollback) skips the
/// whole batch and bumps `failed_batches`. Ingestion continues either way.
/// Progress is reported on stderr unless `format` is JSON.
///
/// `default_scope` is applied to records that omit a `scope` field; a per-record
/// `scope` in the JSONL always takes precedence.
///
/// # Errors
///
/// Returns an error when the call ingested zero facts while at least one line was
/// skipped for any reason (parse error, validation failure, oversized line, read
/// error, or batch rejection). A wholesale failure is surfaced rather than
/// reported as a successful no-op.
pub async fn ingest_from_reader(
    engine: &MemoryEngine,
    reader: impl Read,
    embedder: std::sync::Arc<dyn EmbeddingProvider>,
    batch_size: usize,
    default_scope: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<IngestSummary> {
    let start = Instant::now();
    let mut buf = BufReader::new(reader);

    let mut tally = BatchTally::default();
    let mut batch: Vec<AddFactRequest> = Vec::with_capacity(batch_size);
    // Parallel to `batch`: the source JSONL line of each batched record, so a
    // per-record skip warning can name the offending line (#663 / #727 review).
    let mut batch_lines: Vec<usize> = Vec::with_capacity(batch_size);
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
                // A read error on the byte stream is terminal — retrying would
                // spin on a persistent error (CWE-835). Stop and flush whatever
                // was already batched, consistent with the skip_until-error path.
                line_no += 1;
                eprintln!("warning: line {line_no}: read error, stopping: {e}");
                tally.skipped += 1;
                break;
            }
        };
        line_no += 1;

        if n as u64 > MAX_LINE_BYTES {
            eprintln!("warning: line {line_no}: exceeds {MAX_LINE_BYTES} bytes, skipping");
            tally.skipped += 1;
            // Resync to the next record — but ONLY if read_line stopped at the byte
            // cap mid-line (no terminator captured). If the line already ends in
            // '\n', read_line consumed the whole record including its terminator, so
            // skipping again would swallow the *following* record.
            if !line.ends_with('\n')
                && let Err(e) = buf.skip_until(b'\n')
            {
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
                tally.skipped += 1;
                continue;
            }
        };

        if let Err(e) = validate_jsonl_fact(&fact) {
            eprintln!("warning: line {line_no}: validation error: {e}");
            tally.skipped += 1;
            continue;
        }

        batch.push(jsonl_to_request(fact, default_scope));
        batch_lines.push(line_no);

        if batch.len() >= batch_size {
            let flushed = flush_batch(
                engine,
                &mut batch,
                &mut batch_lines,
                &embedder,
                &format!("batch at line {line_no}"),
                &mut tally,
            )
            .await;
            if flushed {
                eprint_progress(tally.ingested, tally.skipped, &start, format);
            }
        }
    }

    // Flush remaining partial batch (no-op if empty).
    flush_batch(
        engine,
        &mut batch,
        &mut batch_lines,
        &embedder,
        "final batch",
        &mut tally,
    )
    .await;

    let summary = IngestSummary {
        total_ingested: tally.ingested,
        total_skipped: tally.skipped,
        failed_batches: tally.failed_batches,
        elapsed_secs: start.elapsed().as_secs_f64(),
    };

    if tally.ingested == 0 && tally.skipped > 0 {
        anyhow::bail!(
            "no facts ingested ({} skipped, {} failed batches)",
            tally.skipped,
            tally.failed_batches
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

/// Running tallies threaded through [`flush_batch`] and folded into the final
/// [`IngestSummary`]. Grouped into one struct so `flush_batch` stays within
/// clippy's argument-count budget.
#[derive(Default)]
struct BatchTally {
    ingested: usize,
    skipped: usize,
    failed_batches: usize,
}

/// Flush the accumulated `batch` to the engine, updating `tally` in place, and
/// clear it. Returns `true` when the batch made progress, `false` when the engine
/// rejected the whole batch (counted as skipped + one failed batch).
///
/// `context` is the location phrase used in the failure warning, e.g.
/// `"batch at line 42"` or `"final batch"`. An empty batch is a no-op.
async fn flush_batch(
    engine: &MemoryEngine,
    batch: &mut Vec<AddFactRequest>,
    batch_lines: &mut Vec<usize>,
    embedder: &std::sync::Arc<dyn EmbeddingProvider>,
    context: &str,
    tally: &mut BatchTally,
) -> bool {
    if batch.is_empty() {
        return true;
    }
    let chunk_size = batch.len();
    // Partial-success ingest (#663): a single invalid record (e.g. content in the
    // 1–8 MiB band that passes the CLI line cap but exceeds the engine's payload
    // limit) is skipped individually instead of poisoning its whole batch.
    let progressed = match engine
        .add_facts_batch_partial(batch.as_slice(), embedder.clone(), None)
        .await
    {
        Ok(results) => {
            // `results` is positional with `batch` (hence `batch_lines`), so each
            // rejection is reported against its own source JSONL line, matching the
            // CLI's other `warning: line N: …` diagnostics.
            let mut inserted = 0usize;
            for (src_line, result) in batch_lines.iter().zip(&results) {
                match result {
                    Ok(_) => inserted += 1,
                    Err(err) => eprintln!("warning: line {src_line}: skipped (engine): {err}"),
                }
            }
            tally.ingested += inserted;
            tally.skipped += chunk_size - inserted;
            true
        }
        Err(e) => {
            // Batch-level failure (embedder error or atomic-insert rollback): the
            // whole batch could not be persisted.
            eprintln!("warning: {context}: {e}");
            tally.skipped += chunk_size;
            tally.failed_batches += 1;
            false
        }
    };
    batch.clear();
    batch_lines.clear();
    progressed
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Maximum accepted `--batch-size`. Values above this would allocate an unbounded
/// `Vec` from operator-supplied input, enabling a trivial OOM denial-of-service.
const MAX_BATCH_SIZE: usize = 10_000;

// The future streams from a `Box<dyn Read>` (the `-` branch yields a `!Send` `StdinLock`)
// held across the per-batch `add_fact().await`, so it is intentionally `!Send`. Making the
// reader `+ Send` would force buffering all of stdin up front, defeating the streaming
// design (and the OOM guard it exists for). `run` is only ever awaited inline on the
// single-threaded `#[tokio::main]` entrypoint — never spawned — so `Send` is not required.
#[allow(
    clippy::future_not_send,
    reason = "awaited inline on #[tokio::main]; streams a !Send StdinLock"
)]
pub async fn run(db: &Path, args: &BatchIngestArgs, format: OutputFormat) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.batch_size > 0 && args.batch_size <= MAX_BATCH_SIZE,
        "--batch-size must be between 1 and {MAX_BATCH_SIZE}, got {}",
        args.batch_size,
    );

    // Open or create the engine, plus its embedding provider (shared config —
    // provider/MRL identity per #619).
    //
    // On --create the embed dim is known up front (--embed-dim), so the embedder is
    // validated BEFORE the DB file is created: a misconfigured embedder must not
    // leave an orphan empty database behind (#681). On the open paths the dim is
    // only known after opening, but opening does not create a file, so building the
    // embedder afterwards carries no orphan risk.
    let (mut engine, embedder) = if args.create {
        let embed_dim = args
            .embed_dim
            .ok_or_else(|| anyhow::anyhow!("--embed-dim is required when using --create"))?;
        anyhow::ensure!(
            !db.exists(),
            "database {} already exists — remove it first or omit --create",
            db.display()
        );
        let embedder = args.embed.build_required(embed_dim)?;
        let engine = MemoryEngine::builder(embed_dim)
            .path(db.to_path_buf())
            .build()?;
        (engine, embedder)
    } else {
        let engine = if let Some(dim) = args.embed_dim {
            // Existing DB: accept an explicit --embed-dim so a never-embedded store (no
            // recorded identity yet under #613) is still writable. The engine's open path
            // rejects a mismatch against any recorded identity.
            open_engine_writable_with_dim(db, dim)?
        } else {
            open_engine_writable(db)?
        };
        let embedder = args.embed.build_required(engine.embed_dim())?;
        (engine, embedder)
    };

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
        std::sync::Arc::new(embedder),
        args.batch_size,
        args.scope.as_deref(),
        format,
    )
    .await?;

    // Clear progress line before final output
    if format != OutputFormat::Json {
        eprintln!();
    }

    print_summary(&summary, format)?;
    // Flush the sidecar snapshot before the engine drops (#728 review C).
    engine.close().await?;
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
        assert!(matches!(fact.fact_type, FactType::Episodic));

        let line = r#"{"content":"hello","fact_type":"semantic"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert!(matches!(fact.fact_type, FactType::Semantic));

        let line = r#"{"content":"hello","fact_type":"procedural"}"#;
        let fact: JsonlFact = serde_json::from_str(line).unwrap();
        assert!(matches!(fact.fact_type, FactType::Procedural));
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
        assert!(matches!(fact.fact_type, FactType::Episodic));
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
        assert_eq!(req.fact_type, memory_engine::types::FactType::Semantic);
        let opts = req.opts.unwrap();
        assert_eq!(opts.importance, Some(0.8));
        assert!(opts.t_valid.is_some());
    }

    #[test]
    fn validate_importance_out_of_range() {
        let fact = JsonlFact {
            content: "test".into(),
            fact_type: FactType::Semantic,
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
            fact_type: FactType::Semantic,
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
            fact_type: FactType::Semantic,
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

    #[tokio::test]
    async fn ingest_from_reader_with_fake_embedder() {
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 2);
        assert_eq!(summary.total_skipped, 0);
        assert_eq!(summary.failed_batches, 0);
    }

    #[tokio::test]
    async fn ingest_skips_malformed_lines() {
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 2);
        assert_eq!(summary.total_skipped, 1);
    }

    #[tokio::test]
    async fn ingest_skips_invalid_importance() {
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 1);
        assert_eq!(summary.total_skipped, 1);
    }

    #[tokio::test]
    async fn ingest_empty_input() {
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
        let summary = ingest_from_reader(
            &engine,
            &b""[..],
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await;
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

    #[tokio::test]
    async fn skips_oversized_line_then_resumes() {
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
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 1, "only the normal record ingests");
        assert_eq!(
            summary.total_skipped, 1,
            "exactly the oversized line is skipped"
        );
    }

    #[tokio::test]
    async fn oversized_unterminated_final_line_at_eof() {
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
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 1, "the valid first record ingests");
        assert_eq!(
            summary.total_skipped, 1,
            "exactly the oversized final line is skipped"
        );
    }

    #[tokio::test]
    async fn cap_sized_line_does_not_eat_following_record() {
        // Regression for the resync off-by-one: a line whose bytes (incl. its '\n')
        // total exactly MAX_LINE_BYTES + 1 is read in full — read_line consumes the
        // terminating '\n' — yet n > MAX flags it oversized. An unconditional
        // skip_until would then drain the *next* line. The record after a cap-sized
        // line must survive.
        const OVERHEAD: usize = "{\"content\":\"\",\"fact_type\":\"semantic\"}".len();
        let cap = usize::try_from(MAX_LINE_BYTES).unwrap();
        let pad = "x".repeat(cap - OVERHEAD);
        let cap_line = format!("{{\"content\":\"{pad}\",\"fact_type\":\"semantic\"}}");
        assert_eq!(
            cap_line.len(),
            cap,
            "cap_line must be exactly MAX_LINE_BYTES"
        );

        let engine = MemoryEngine::builder(4).build().unwrap();
        let input =
            format!("{cap_line}\n{{\"content\":\"survivor\",\"fact_type\":\"semantic\"}}\n");
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(
            summary.total_ingested, 1,
            "the record after a cap-sized line must not be swallowed by resync"
        );
    }

    #[tokio::test]
    async fn mid_stream_flush_with_batch_size_one() {
        // batch_size=1 forces a flush after every record, exercising the mid-stream
        // flush path — not just the final partial flush the other tests hit.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = "{\"content\":\"a\",\"fact_type\":\"semantic\"}\n\
                     {\"content\":\"b\",\"fact_type\":\"episodic\"}\n\
                     {\"content\":\"c\",\"fact_type\":\"procedural\"}\n";
        let summary = ingest_from_reader(
            &engine,
            input.as_bytes(),
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            1,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_ingested, 3);
        assert_eq!(summary.total_skipped, 0);
        assert_eq!(summary.failed_batches, 0);
    }

    #[tokio::test]
    async fn create_with_misconfigured_embedder_leaves_no_orphan_db() {
        // #681: a --create run whose embedder is misconfigured (url without model →
        // build_required errors) must fail BEFORE the DB file is created, so no orphan
        // empty database is left behind for the next run to trip over.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("orphan.db");
        let args = BatchIngestArgs {
            file: PathBuf::from("-"),
            embed: EmbeddingArgs {
                embed_url: Some("http://127.0.0.1:0/v1/embeddings".into()),
                embed_model: None, // partial config → build_required errors
                embed_provider: "ollama".into(),
                embed_api_key: None,
                native_dim: None,
                query_instruction: None,
                mrl_dim: None,
                embed_timeout: 5,
            },
            batch_size: 100,
            create: true,
            embed_dim: Some(4),
            scope: None,
        };
        let result = run(&db, &args, OutputFormat::Json).await;
        assert!(result.is_err(), "misconfigured embedder must error");
        assert!(
            !db.exists(),
            "no orphan DB file may be created when the embedder fails to build"
        );
    }

    #[tokio::test]
    async fn all_bad_lines_returns_error() {
        // 0 ingested AND >0 skipped → the function bails rather than reporting a
        // successful no-op. The empty-input test does not hit this (skipped is 0).
        let engine = MemoryEngine::builder(4).build().unwrap();
        let input = "not json\nalso not json\n";
        let result = ingest_from_reader(
            &engine,
            input.as_bytes(),
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            100,
            None,
            OutputFormat::Json,
        )
        .await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("no facts ingested"), "got: {msg}");
    }

    #[test]
    fn ingest_from_reader_never_panics() {
        use proptest::prelude::*;

        // Fuzz the JSONL ingest path (#433): the --file argument is the trust
        // boundary, so no input may panic or hang — only ever Ok(summary) or an Err
        // (e.g. the all-bad-lines bail). Two strategies are mixed so both the early
        // parse-reject path AND the deeper machine are exercised:
        //  - raw arbitrary bytes (mostly hit serde's parse error), and
        //  - structurally-valid-ish JSONL documents (reach validate_jsonl_fact —
        //    incl. importance out of [0,1] — jsonl_to_request, batching, and
        //    flush_batch, since batch_size=8 forces mid-stream flushes).
        let valid_line = (
            "[a-z ]{0,40}",
            prop_oneof!["episodic", "semantic", "procedural"],
            proptest::option::of(-2.0f64..2.0),
        )
            .prop_map(|(content, ft, importance)| {
                let mut obj = serde_json::json!({ "content": content, "fact_type": ft });
                if let Some(i) = importance {
                    obj["importance"] = serde_json::json!(i);
                }
                obj.to_string()
            });
        let jsonl_doc = proptest::collection::vec(prop_oneof![valid_line, "[^\n]{0,40}"], 0..16)
            .prop_map(|lines| lines.join("\n").into_bytes());

        // Built once and reused across cases; ingest_from_reader only appends, so
        // cases cannot interfere. The fn under test is async now, so each case is
        // driven to completion on a current-thread runtime built once for the test.
        let engine = MemoryEngine::builder(4).build().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        proptest::proptest!(|(data in prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..4096),
            jsonl_doc,
        ])| {
            let _ = rt.block_on(ingest_from_reader(
                &engine,
                data.as_slice(),
                std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                8,
                None,
                OutputFormat::Json,
            ));
        });
    }

    #[tokio::test]
    async fn persistent_read_error_breaks_instead_of_looping() {
        // CWE-835 (#664): a reader that errors on every read must make the ingest
        // loop BREAK (a byte-stream read error is terminal), not `continue` and
        // spin forever. The reader panics if polled more than a few times, so a
        // regression to the looping behaviour fails the test instead of hanging it.
        struct AlwaysErr {
            reads: usize,
        }
        impl std::io::Read for AlwaysErr {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    self.reads < 3,
                    "ingest looped on a persistent read error instead of breaking"
                );
                self.reads += 1;
                Err(std::io::Error::other("simulated persistent read error"))
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        // 0 ingested + 1 skipped → the all-bad bail; the point is it RETURNS.
        let err = ingest_from_reader(
            &engine,
            AlwaysErr { reads: 0 },
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            8,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no facts ingested"), "got: {err}");
    }

    #[tokio::test]
    async fn read_error_after_a_valid_line_still_flushes_it() {
        // The `break` must not discard records read BEFORE the terminal read error:
        // the partial batch is flushed on the way out. A reader that yields one
        // valid JSONL line and then errors must still ingest that line.
        struct LineThenErr {
            remaining: &'static [u8],
        }
        impl std::io::Read for LineThenErr {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.remaining.is_empty() {
                    return Err(std::io::Error::other("read error after the line"));
                }
                let n = self.remaining.len().min(buf.len());
                buf[..n].copy_from_slice(&self.remaining[..n]);
                self.remaining = &self.remaining[n..];
                Ok(n)
            }
        }

        let engine = MemoryEngine::builder(4).build().unwrap();
        let reader = LineThenErr {
            remaining: b"{\"content\":\"ok\",\"fact_type\":\"semantic\"}\n",
        };
        let summary = ingest_from_reader(
            &engine,
            reader,
            std::sync::Arc::new(CapEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            8,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(
            summary.total_ingested, 1,
            "the valid line must still be flushed"
        );
        assert_eq!(
            summary.total_skipped, 1,
            "the terminal read error counts as 1"
        );
    }
}
