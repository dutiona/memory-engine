//! `consolidate` — run a dream cycle with a selectable backend (#554).
//!
//! This is the seam the efficiency×quality benchmark drives by subprocess: it opens
//! the database writable, runs the chosen consolidation backend through the #209
//! caller-write guard (`run_dream_cycle_guarded`), applies the resulting report, and
//! prints a machine-readable result (deltas, wall time, and — for the LLM backend —
//! the call/token counts).
//!
//! - `--backend dream-cycle` (default): the shipped pure-Rust `DefaultDreamCycle`.
//! - `--backend llm`: an `LlmDreamCycle` driven by an HTTP `DeltaProposer` (Ollama
//!   `/api/generate`) plus an HTTP embedder (the backend embeds its own summaries).

use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use clap::ValueEnum;
use memory_engine::{
    CycleOutcome, DefaultDreamCycle, DreamCycle, LlmDreamCycle, MemoryEngine, SkipReason,
};
use memory_engine_embed::{HttpDeltaProposer, HttpEmbeddingProvider};

use crate::db::{open_engine_writable, peek_embed_dim_from_db};
use crate::output::OutputFormat;

/// Upper bound on #209 drain retries. `consolidate` is a manual force-consolidate, so a
/// deferral from a stale caller-write cursor should be drained (the first guarded call
/// advances the cursor; the next runs). Bounded so a genuinely concurrent writer cannot
/// spin forever — after this many deferrals we report the skip honestly.
const MAX_DRAIN_ATTEMPTS: u32 = 8;

/// Run a cycle through the #209 guard, draining transient caller-write deferrals.
fn run_with_drain(engine: &MemoryEngine, cycle: &dyn DreamCycle) -> anyhow::Result<CycleOutcome> {
    let mut outcome = engine.run_dream_cycle_guarded(cycle)?;
    let mut attempts = 1;
    while attempts < MAX_DRAIN_ATTEMPTS {
        match outcome {
            CycleOutcome::Skipped(SkipReason::CallerWroteFacts { .. }) => {
                outcome = engine.run_dream_cycle_guarded(cycle)?;
                attempts += 1;
            }
            _ => break,
        }
    }
    Ok(outcome)
}

/// Which consolidation backend to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// The shipped deterministic, zero-LLM `DefaultDreamCycle`.
    DreamCycle,
    /// An LLM backend: an HTTP `DeltaProposer` + HTTP embedder.
    Llm,
}

#[derive(clap::Args)]
pub struct ConsolidateArgs {
    /// Consolidation backend to run.
    #[arg(long, value_enum, default_value = "dream-cycle")]
    backend: BackendArg,

    /// LLM `/api/generate` URL (required for `--backend llm`).
    #[arg(long)]
    llm_url: Option<String>,

    /// LLM model name, e.g. `gemma4:26b` (required for `--backend llm`).
    #[arg(long)]
    llm_model: Option<String>,

    /// Embedding endpoint URL — the LLM backend embeds its own summaries
    /// (required for `--backend llm`).
    #[arg(long)]
    embed_url: Option<String>,

    /// Embedding model name (required for `--backend llm`).
    #[arg(long)]
    embed_model: Option<String>,

    /// HTTP timeout (seconds) for the LLM and embedding calls.
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
}

/// Require an arg that is mandatory for the LLM backend, naming it in the error.
fn require<'a>(value: Option<&'a String>, flag: &str) -> anyhow::Result<&'a str> {
    value
        .map(String::as_str)
        .with_context(|| format!("--backend llm requires {flag}"))
}

pub fn run(db: &Path, args: &ConsolidateArgs, format: OutputFormat) -> anyhow::Result<()> {
    let engine = open_engine_writable(db)?;
    let start = Instant::now();

    // Run the selected backend through the #209 guard (draining deferrals). The LLM
    // backend's proposer + embedder are borrowed by the cycle, so build them in a scope
    // that outlives the run; the proposer's token stats are captured regardless of
    // outcome (a failed run still burned LLM calls — fail-loud accounting). Setup errors
    // (missing flags, client build) fail BEFORE any JSON; the consolidate work's own
    // errors (cycle run + apply) are reported IN the JSON below.
    let (run_result, llm_stats): (anyhow::Result<CycleOutcome>, Option<_>) = match args.backend {
        BackendArg::DreamCycle => {
            let cycle = DefaultDreamCycle::with_defaults();
            (run_with_drain(&engine, &cycle), None)
        }
        BackendArg::Llm => {
            let llm_url = require(args.llm_url.as_ref(), "--llm-url")?;
            let llm_model = require(args.llm_model.as_ref(), "--llm-model")?;
            let embed_url = require(args.embed_url.as_ref(), "--embed-url")?;
            let embed_model = require(args.embed_model.as_ref(), "--embed-model")?;
            let dim = peek_embed_dim_from_db(db)?;

            let proposer = HttpDeltaProposer::new(
                llm_url.to_owned(),
                llm_model.to_owned(),
                None,
                args.timeout_secs,
            )?;
            let embedder = HttpEmbeddingProvider::new(
                embed_url.to_owned(),
                embed_model.to_owned(),
                "ollama".to_owned(), // TODO(#618): provider should come from config/CLI
                None,
                dim,
                args.timeout_secs,
            )?;
            let cycle = LlmDreamCycle::new(&proposer, &embedder);
            let result = run_with_drain(&engine, &cycle);
            (result, Some(proposer.stats()))
        }
    };

    // Resolve the outcome. Any error during the consolidate step (cycle run OR apply) is
    // reported IN the JSON (outcome "failed" + error) AND as a non-zero exit — so the
    // benchmark's machine-readable contract holds while the run still fails loud (never a
    // fake 0).
    let mut outcome_label = "skipped";
    let mut applied = serde_json::Value::Null;
    let mut skip_reason = serde_json::Value::Null;
    let mut error_json = serde_json::Value::Null;
    let mut failed = false;
    match run_result {
        Ok(CycleOutcome::Ran(report)) => match engine.apply_cycle_report(&report) {
            Ok(result) => {
                outcome_label = "ran";
                applied = serde_json::to_value(&result)?;
            }
            Err(e) => {
                outcome_label = "failed";
                error_json = serde_json::Value::String(format!("{e:#}"));
                failed = true;
            }
        },
        Ok(CycleOutcome::Skipped(reason)) => skip_reason = serde_json::to_value(reason)?,
        Err(e) => {
            outcome_label = "failed";
            error_json = serde_json::Value::String(format!("{e:#}"));
            failed = true;
        }
    }
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let backend_label = match args.backend {
        BackendArg::DreamCycle => "dream-cycle",
        BackendArg::Llm => "llm",
    };
    let llm_json = llm_stats.map_or(serde_json::Value::Null, |s| {
        serde_json::json!({
            "llm_calls": s.llm_calls,
            "eval_count": s.eval_count,
            "prompt_eval_count": s.prompt_eval_count,
        })
    });
    let report = serde_json::json!({
        "backend": backend_label,
        "outcome": outcome_label,
        "skip_reason": skip_reason,
        "applied": applied,
        "error": error_json,
        "elapsed_ms": elapsed_ms,
        "llm": llm_json,
    });

    match format {
        OutputFormat::Json | OutputFormat::Plain => crate::output::print_json(&report)?,
        OutputFormat::Table => {
            eprintln!("backend={backend_label} outcome={outcome_label} elapsed_ms={elapsed_ms}");
            if let Some(applied) = report.get("applied").filter(|v| !v.is_null()) {
                eprintln!("applied: {applied}");
            }
        }
    }

    // Fail loud AFTER emitting the JSON: the report (with outcome "failed" + error) is
    // on stdout for the harness, and a non-zero exit signals the run is invalid.
    if failed {
        anyhow::bail!("consolidate: the consolidation step failed (see JSON `error` field)");
    }
    Ok(())
}
