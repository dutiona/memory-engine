# Bootstrapping Sessions

The bootstrap pipeline parses Claude Code JSONL session logs and imports noteworthy episodes (bug fixes, decisions, conventions, learnings) as historical facts. This solves the cold-start problem: a fresh memory engine starts with no knowledge, but past session logs contain valuable procedural and semantic information that can be recovered.

## Quick start

```rust
# use std::io::Cursor;
# use memory_engine::{MemoryEngine, MemoryError};
# use memory_engine::bootstrap::{KeywordExtractor, BootstrapConfig};
# use memory_engine::EmbeddingProvider;
#
# struct MyEmbedder;
# impl EmbeddingProvider for MyEmbedder {
#     fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
#         Ok(vec![0.0; 384])
#     }
# }
#
let engine = MemoryEngine::open_memory(384)?;
let extractor = KeywordExtractor;
let config = BootstrapConfig::default();

// Bootstrap a single session log
let jsonl_data = std::fs::read("~/.claude/projects/.../session.jsonl")?;
let reader = Cursor::new(jsonl_data);
let report = engine.bootstrap_session(
    reader,
    &MyEmbedder,
    &extractor,
    &config,
    None, // no auto-pin classifier
)?;

println!("Processed {} sessions, created {} facts",
    report.sessions_processed, report.facts_created);
```

To bootstrap an entire directory of session logs at once:

```rust
# use std::path::Path;
# use memory_engine::{MemoryEngine, MemoryError};
# use memory_engine::bootstrap::{KeywordExtractor, BootstrapConfig};
# use memory_engine::EmbeddingProvider;
#
# struct MyEmbedder;
# impl EmbeddingProvider for MyEmbedder {
#     fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
#         Ok(vec![0.0; 384])
#     }
# }
#
# let engine = MemoryEngine::open_memory(384)?;
# let extractor = KeywordExtractor;
# let config = BootstrapConfig::default();
let report = engine.bootstrap_directory(
    Path::new("~/.claude/projects/my-project/sessions/"),
    &MyEmbedder,
    &extractor,
    &config,
    None,
)?;
```

`bootstrap_directory` discovers top-level `*.jsonl` files (skipping subdirectories like `subagents/`) and processes each independently. Individual session failures are logged and skipped -- they do not abort the batch.

## Writing a custom `SessionExtractor`

The default `KeywordExtractor` maps episode categories and outcomes to fact types using hard-coded rules. For higher-quality extraction, implement the `SessionExtractor` trait with an LLM:

```rust
# use memory_engine::error::Result;
# use memory_engine::types::FactType;
# use memory_engine::bootstrap::extract::{
#     SessionExtractor, ExtractedFact, CandidateEpisode, EpisodeCategory,
# };
# use memory_engine::bootstrap::outcome::SessionOutcome;
#
struct LlmExtractor {
    client: reqwest::blocking::Client,
    api_key: String,
}

impl SessionExtractor for LlmExtractor {
    fn extract(
        &self,
        episode: &CandidateEpisode,
        outcome: &SessionOutcome,
    ) -> Result<Vec<ExtractedFact>> {
        // Build a prompt from episode.turns, episode.category,
        // and the session outcome, then call your LLM API.
        // Return one or more ExtractedFact with content, fact_type,
        // importance, category, and metadata.
        todo!("call LLM")
    }
}
```

The extractor receives a `CandidateEpisode` (pre-filtered turns with matched keywords and a category) and the session's heuristic `SessionOutcome` (Success, Failure, or Indeterminate). It returns a `Vec<ExtractedFact>` -- one candidate episode can produce multiple facts.

## Configuration

`BootstrapConfig` controls the pipeline:

```rust
# use memory_engine::bootstrap::BootstrapConfig;
let config = BootstrapConfig {
    scope: Some("project:my-app".into()),
    max_turns: 100,
    skip_existing: true, // default
};
```

| Field           | Type              | Default | Description                                                                                                  |
| --------------- | ----------------- | ------- | ------------------------------------------------------------------------------------------------------------ |
| `scope`         | `Option<String>`  | `None`  | Scope path for ingested facts (e.g., `"project:my-app"`). `None` uses the root scope.                       |
| `max_turns`     | `usize`           | `0`     | Maximum turns to process per session. `0` means no limit.                                                    |
| `skip_existing` | `bool`            | `true`  | Skip sessions that have already been bootstrapped (idempotency). Set to `false` to allow duplicate imports. |

## Reading reports

`bootstrap_session` and `bootstrap_directory` return a `BootstrapReport`:

| Field                  | Type              | Description                                           |
| ---------------------- | ----------------- | ----------------------------------------------------- |
| `sessions_processed`   | `usize`           | Sessions successfully bootstrapped.                   |
| `sessions_skipped`     | `usize`           | Sessions skipped due to idempotency check.            |
| `entries_parsed`       | `usize`           | Raw JSONL entries parsed.                             |
| `entries_malformed`    | `usize`           | Entries that could not be parsed (skipped with warn). |
| `turns_reconstructed`  | `usize`           | User-assistant turn pairs reconstructed.              |
| `candidates_found`     | `usize`           | Episodes that matched keyword pre-filter.             |
| `facts_created`        | `usize`           | Facts inserted into the engine.                       |
| `events_ingested`      | `usize`           | Marker events inserted (one per session).             |
| `outcome_counts`       | `OutcomeCounts`   | Breakdown by session outcome (success/failure/indeterminate). |
| `category_counts`      | `CategoryCounts`  | Breakdown by episode category (bug/decision/convention/learning). |
| `prewarm_metrics`      | `PrewarmMetrics`  | Cold-start quality metrics.                           |

### `PrewarmMetrics`

Tracks the composition and quality of bootstrapped facts for cold-start analysis:

| Field              | Type    | Description                                    |
| ------------------ | ------- | ---------------------------------------------- |
| `episodic_count`   | `usize` | Number of episodic facts created.              |
| `semantic_count`   | `usize` | Number of semantic facts created.              |
| `procedural_count` | `usize` | Number of procedural facts created.            |
| `avg_importance`   | `f64`   | Weighted average importance across all facts.  |

Use `total_count()` to get the sum of all three fact-type counts.

## Idempotency

With `skip_existing: true` (the default), the pipeline checks for a marker event with `source="bootstrap"` and a matching `session_id` before processing. If one exists, the session is skipped and counted in `report.sessions_skipped`.

Each successfully bootstrapped session inserts exactly one marker event of type `SystemEvent` with a payload containing the `session_id`. This serves as the idempotency anchor: re-running bootstrap on the same session logs is a no-op.

Setting `skip_existing: false` disables this check and allows duplicate imports of the same session.

## Pipeline overview

The bootstrap pipeline runs in five stages:

1. **Parse** -- Read JSONL entries from the session log. Malformed lines are skipped with a `tracing::warn`.
2. **Reconstruct** -- Pair user and assistant entries into `ConversationTurn`s using UUID linkage with sequential fallback. Noise entries (progress, queue operations, file history snapshots) are filtered out.
3. **Classify** -- Determine the session outcome (Success, Failure, Indeterminate) from heuristic signals: git commits, passing tests, error loops, interruptions, and user sentiment.
4. **Filter** -- Apply keyword pre-filter to surface candidate episodes in four categories: Bug, Decision, Convention, and Learning.
5. **Extract** -- Pass each candidate episode to the `SessionExtractor` to produce facts, then ingest them with backdated `t_created` and `last_accessed` timestamps for correct Ebbinghaus decay.

The entire ingest phase (marker event + facts) runs within a SQLite savepoint for crash safety. On failure, the savepoint is rolled back with no side effects.

## Limitations

- **English-only heuristics.** Keyword matching for episode classification and outcome detection is hard-coded in English. Non-English sessions will produce fewer (or zero) candidate episodes.
- **No subagent file processing.** `bootstrap_directory` skips subdirectories. Session logs from subagent invocations are not processed.
- **Synthetic fixtures only.** The test suite uses hand-crafted JSONL fixtures, not real session logs. The parser handles the known Claude Code JSONL format but may need updates as the format evolves.
- **Keyword extractor quality ceiling.** The default `KeywordExtractor` produces one fact per candidate episode using simple category-to-type mapping. For production use, consider implementing `SessionExtractor` with an LLM for higher-quality, multi-fact extraction.
