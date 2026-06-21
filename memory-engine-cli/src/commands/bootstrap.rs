//! `bootstrap` subcommand — load historical memory into the store from the
//! read-only history backbone, idempotently and with backdated timestamps.
//!
//! Two source kinds, either or both per invocation:
//!   - `--jsonl-dir` — Claude/Codex/Gemini session `.jsonl` transcripts (AAP #53).
//!   - `--memory-dir` — native `.md` memory files (`MEMORY.md` + fact files).
//!
//! Both paths are **redaction-gated** (#45/#51 — runs before any write, no
//! bypass flag), **dedup-with-reinforced** (#520), and **backdated** (#521).
//! Sources are opened read-only and never modified.

use std::path::{Path, PathBuf};

use memory_engine::MemoryEngine;
use memory_engine::bootstrap::{
    BootstrapConfig, BootstrapReport, KeywordExtractor, load_secret_denylist,
};
use memory_engine::traits::EmbeddingProvider;

use crate::commands::embedding_args::EmbeddingArgs;
use crate::db::{open_engine_writable, open_engine_writable_with_dim};
use crate::output::{OutputFormat, print_json};

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
pub struct BootstrapArgs {
    /// Directory of session `.jsonl` transcripts to import (recursive; skips `subagents/`).
    #[arg(long)]
    jsonl_dir: Option<PathBuf>,

    /// Directory of native `.md` memory files to import (recursive).
    #[arg(long)]
    memory_dir: Option<PathBuf>,

    /// Embedding provider config (shared with query/batch-ingest; #619). `--embed-url` +
    /// `--embed-model` are required here. Documents are embedded via `embed` / `embed_batch`.
    #[command(flatten)]
    embed: EmbeddingArgs,

    /// Default scope path for all imported facts
    #[arg(long)]
    scope: Option<String>,

    /// Maximum turns per session (`.jsonl` path only). `0` = no limit.
    #[arg(long, default_value = "0")]
    max_turns: usize,

    /// Re-process already-bootstrapped sessions instead of skipping them
    /// (`.jsonl` path). Off by default; idempotency normally skips on the
    /// session marker. With this set, re-runs reinforce instead of skipping.
    #[arg(long)]
    reprocess: bool,

    /// Create a new database (requires `--embed-dim`)
    #[arg(long)]
    create: bool,

    /// Embedding dimension (required with `--create`)
    #[arg(long)]
    embed_dim: Option<usize>,
}

// ---------------------------------------------------------------------------
// Report output
// ---------------------------------------------------------------------------

fn print_report(report: &BootstrapReport, format: OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => print_json(report)?,
        OutputFormat::Table => {
            println!("Bootstrap complete:");
            println!("  sessions processed: {}", report.sessions_processed);
            println!("  sessions skipped:   {}", report.sessions_skipped);
            println!("  memory files:       {}", report.memory_files_parsed);
            println!("  memory skipped:     {}", report.memory_files_skipped);
            println!("  facts created:      {}", report.facts_created);
            println!("  facts reinforced:   {}", report.facts_reinforced);
            println!("  secrets redacted:   {}", report.secrets_redacted);
        }
        OutputFormat::Plain => {
            println!(
                "sessions_processed={} sessions_skipped={} memory_files_parsed={} \
                 memory_files_skipped={} facts_created={} facts_reinforced={} secrets_redacted={}",
                report.sessions_processed,
                report.sessions_skipped,
                report.memory_files_parsed,
                report.memory_files_skipped,
                report.facts_created,
                report.facts_reinforced,
                report.secrets_redacted,
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core logic (testable — accepts engine + embedder + config)
// ---------------------------------------------------------------------------

/// Run the bootstrap import over whichever source dirs are provided, merging
/// both reports. The `.jsonl` path uses the no-LLM [`KeywordExtractor`]; both
/// paths share the redaction/dedup/backdating semantics in `config`.
///
/// # Errors
///
/// Propagates engine errors (embedding, DB, traversal) and bails if neither
/// `--jsonl-dir` nor `--memory-dir` was given.
pub async fn run_bootstrap(
    engine: &MemoryEngine,
    jsonl_dir: Option<&Path>,
    memory_dir: Option<&Path>,
    embedder: std::sync::Arc<dyn EmbeddingProvider>,
    config: &BootstrapConfig,
) -> anyhow::Result<BootstrapReport> {
    anyhow::ensure!(
        jsonl_dir.is_some() || memory_dir.is_some(),
        "nothing to do: pass --jsonl-dir and/or --memory-dir"
    );

    let mut report = BootstrapReport::default();

    if let Some(dir) = jsonl_dir {
        anyhow::ensure!(
            dir.is_dir(),
            "--jsonl-dir is not a directory: {}",
            dir.display()
        );
        let sub = engine
            .bootstrap_directory(
                dir,
                embedder.clone(),
                std::sync::Arc::new(KeywordExtractor)
                    as std::sync::Arc<dyn memory_engine::bootstrap::SessionExtractor>,
                config,
                None,
            )
            .await?;
        report.merge(&sub);
    }

    if let Some(dir) = memory_dir {
        anyhow::ensure!(
            dir.is_dir(),
            "--memory-dir is not a directory: {}",
            dir.display()
        );
        let sub = engine
            .bootstrap_memory_directory(dir, embedder.clone(), config, None)
            .await?;
        report.merge(&sub);
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(db: &Path, args: &BootstrapArgs, format: OutputFormat) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.jsonl_dir.is_some() || args.memory_dir.is_some(),
        "pass --jsonl-dir and/or --memory-dir"
    );

    // Open or create the engine, plus its embedding provider (shared config —
    // provider/MRL identity per #619).
    //
    // On --create the embed dim is known up front (--embed-dim), so the embedder is
    // validated BEFORE the DB file is created: a misconfigured embedder must not
    // leave an orphan empty database behind (#681). On the open paths the dim is
    // only known after opening, but opening does not create a file, so building the
    // embedder afterwards carries no orphan risk.
    let (engine, embedder) = if args.create {
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

    // Author-seeded denylist (#51). Loud about how many literals loaded so an
    // unset env var (→ signatures-only) is never silently mistaken for "active".
    let denylist = load_secret_denylist()
        .map_err(|e| anyhow::anyhow!("failed to load secret denylist: {e}"))?;
    eprintln!(
        "redaction: signatures + {} author-seeded denylist literal(s)",
        denylist.len()
    );

    let config = BootstrapConfig {
        scope: args.scope.clone(),
        max_turns: args.max_turns,
        skip_existing: !args.reprocess,
        redact: true, // no bypass in normal CLI operation (#51)
        denylist,
        // #293 hostile-input caps (max_session_bytes / max_entries): no CLI flag
        // exposes them, so inherit the library defaults. This is what wires the
        // caps into the CLI `bootstrap` command — the real untrusted-input entry
        // point — rather than leaving it uncapped.
        ..BootstrapConfig::default()
    };

    let report = run_bootstrap(
        &engine,
        args.jsonl_dir.as_deref(),
        args.memory_dir.as_deref(),
        std::sync::Arc::new(embedder),
        &config,
    )
    .await?;

    print_report(&report, format)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Zero-vector embedder (dim 4) — no network.
    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }
        fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
            memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    const PLANTED: &str = "AKIAIOSFODNN7EXAMPLE";

    fn write_session(dir: &Path) {
        // A minimal Claude-Code JSONL session whose assistant turn trips the
        // keyword pre-filter ("root cause", "fix", "tests pass") and plants a
        // secret to prove redaction on the .jsonl path too.
        let jsonl = format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "user", "sessionId": "s1", "timestamp": "2024-02-01T10:00:00Z",
                "uuid": "s1-0", "parentUuid": serde_json::Value::Null,
                "message": {"role": "user", "content": [{"type": "text", "text": "Fix the off-by-one in parser.rs"}]}
            }),
            serde_json::json!({
                "type": "assistant", "sessionId": "s1", "timestamp": "2024-02-01T10:00:30Z",
                "uuid": "s1-1", "parentUuid": "s1-0",
                "message": {"role": "assistant", "content": [{"type": "text",
                    "text": format!("Found the root cause and applied the fix; tests pass. Leaked {PLANTED} here.")}]}
            }),
        );
        fs::write(dir.join("s1.jsonl"), jsonl).unwrap();
    }

    fn write_memory(dir: &Path) {
        fs::write(
            dir.join("pref.md"),
            "---\nmetadata:\n  type: user\n---\nThe user prefers Conventional Commits.\n",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn run_bootstrap_both_dirs_redacts_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_dir = tmp.path().join("jsonl");
        let memory_dir = tmp.path().join("md");
        fs::create_dir_all(&jsonl_dir).unwrap();
        fs::create_dir_all(&memory_dir).unwrap();
        write_session(&jsonl_dir);
        write_memory(&memory_dir);

        let engine = MemoryEngine::builder(4).build().unwrap();
        let config = BootstrapConfig::default();
        let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(FakeEmbed);

        let report = run_bootstrap(
            &engine,
            Some(&jsonl_dir),
            Some(&memory_dir),
            embedder.clone(),
            &config,
        )
        .await
        .unwrap();

        assert_eq!(report.sessions_processed, 1, "one jsonl session imported");
        assert_eq!(report.memory_files_parsed, 1, "one md memory imported");
        assert!(
            report.facts_created >= 2,
            "at least the session fact + the memory fact"
        );
        assert!(
            report.secrets_redacted >= 1,
            "the planted secret was redacted"
        );

        // The secret is nowhere in the store.
        for f in engine.list_active_facts(None).await.unwrap() {
            assert!(
                !f.content.contains(PLANTED),
                "secret leaked: {:?}",
                f.content
            );
        }

        // Idempotency: re-run creates nothing new.
        let report2 = run_bootstrap(
            &engine,
            Some(&jsonl_dir),
            Some(&memory_dir),
            embedder.clone(),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(report2.facts_created, 0, "re-run creates 0 facts");
        // jsonl session is skipped on the marker; the md memory reinforces.
        assert_eq!(report2.sessions_skipped, 1);
        assert_eq!(report2.facts_reinforced, 1, "the md memory reinforces");
    }

    #[tokio::test]
    async fn create_with_misconfigured_embedder_leaves_no_orphan_db() {
        // #681: a --create run whose embedder is misconfigured (url without model →
        // build_required errors) must fail BEFORE the DB file is created, so no orphan
        // empty database is left behind for the next run to trip over.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("orphan.db");
        let jsonl = tmp.path().join("jsonl");
        fs::create_dir_all(&jsonl).unwrap();
        let args = BootstrapArgs {
            jsonl_dir: Some(jsonl),
            memory_dir: None,
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
            scope: None,
            max_turns: 0,
            reprocess: false,
            create: true,
            embed_dim: Some(4),
        };
        let result = run(&db, &args, OutputFormat::Json).await;
        assert!(result.is_err(), "misconfigured embedder must error");
        assert!(
            !db.exists(),
            "no orphan DB file may be created when the embedder fails to build"
        );
    }

    #[tokio::test]
    async fn run_bootstrap_requires_a_source() {
        let engine = MemoryEngine::builder(4).build().unwrap();
        let config = BootstrapConfig::default();
        let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(FakeEmbed);
        let err = run_bootstrap(&engine, None, None, embedder, &config)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nothing to do"));
    }
}
