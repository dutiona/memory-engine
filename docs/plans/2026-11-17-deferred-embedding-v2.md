# Store memory now, embed it later when the embedder is down (#1021)

Successor plan (v2). The predecessor `docs/plans/2026-11-17-deferred-embedding.md` (SHA-256
`248b4a410b09f0631ad4264e92ea58b0170adafbea503840136a5785af2806a9`) was approved on #1079. The owner's answers at
approval changed one decision: an over-length fact is chunked, never truncated, for every provider. Everything
else is the approved plan, unchanged except where chunking reaches it: two added Constraints (the owner's answers,
the successor brief) and an annotation on the over-length acceptance bullet, the Outcome, decision D1, item 5, item
10's over-length test and harness, the tables, Documentation, and Open questions 7, 10 and 13.

## Outcome

`remember` writes its fact at once and never waits on the embedder. The fact is an ordinary `facts` row: it has
its ID (the receipt, #1022's `set_level` and #1023's `forget` act on it at once), and full-text search finds it
immediately. It has no vector yet: its `embedding` is the zero-length BLOB `X''`, and a row in the new persistent
`embed_queue` table (schema v16) tracks it. One drain in the server embeds the queue one input at a time. A
refused connection makes the queue wait and touches no item. A 401 or 403 pauses the queue, touches no item and
raises the alarm. A 400, 413 or 422 fails only its item. A timeout, a 5xx or any other answer backs off only its
item.
Waits are capped, and retries go on however long the outage lasts. A fact longer than the embedder's limit is
split into consecutive chunks that rejoin exactly, each within the limit as the server counts it (2,048 tokens
with the end-of-sequence token it appends, so 2,047 tokens of text, for the served model); each chunk is embedded
and the fact keeps one vector, the chunks' pooled vector (D1). No vector reader ever sees a fact without a vector: not vector search, the HNSW index or
its sidecar, dedup and clustering, the dream cycle, dormant resonance, reconstruction, or export and restore.
`recall` gives its query embed 500 ms, then answers from full-text search, flagged with ADR-0020 §10's cause.
`memory-engine-cli status` reports the queue through a read-only SQLite connection and never builds the engine.
Postgres is unchanged; the new state is listed on #634.

## Constraints

- "No write ever fails for lack of the embedder. Recall falls back to flagged full-text search within its budget, and vectors appear once the embedder returns, however long the outage."
- "Embedder down: fact writes succeed and wait in a persistent deferred-embedding queue. Recall returns FTS results flagged `degraded`, and a fact remembered during the outage is found by that FTS recall during the outage."
- "about 46 files read a fact's vector and `facts.embedding` is `NOT NULL` (`ddl.rs:28`), so the ticket's plan picks how a not-yet-embedded fact is stored, and no placeholder vector is ever visible to vector search, the HNSW index, dedup or consolidation (tested)."
- "The drain is rate-limited and yields to interactive queries (recall, SessionStart), so a post-outage backlog never pushes them into the degraded fallback."
- "After an outage longer than the backoff cap, the queue drains and every item has its vector."
- "a 401 or 403 pauses the whole queue, with no item state change, and `status` shows the alarm; a 400, 413 or 422 fails only that item, with its error; backoff is per item, so a 5xx that keeps recurring for one item never holds up the others."
- "An input longer than ADR-0015's maximum is embedded truncated, never failed or retried forever (dutiona/infra#77's recorded response, else a conservative character cap)." Superseded at plan approval by the owner's answer 2 below: chunked, not truncated.
- "Embedder hanging: recall answers from FTS, flagged, within ADR-0020's recall time limit."
- "`memory-engine-cli status`, run through `podman exec`, shows queue depth, the oldest item's age, the failed count, the pause state, and the times of the last embed success and the last embed error. It opens SQLite read-only with a busy timeout and never constructs the engine, which could migrate the store and become a second writer. The read-only flag is asserted."
- "New tables are listed on #634, and the parity proof runs or the PR states why it doesn't apply."
- Spec decision 8: "Connection errors, timeouts and 5xx responses are transient: they are retried with capped backoff for as long as it takes, however long the outage." "A 401 or 403 is a configuration error: the whole queue pauses and `status` raises an alarm; no item is touched." "Only a 400, 413 or 422 marks a single item failed, with the error recorded."
- KB is out of scope: it fails loudly and keeps no queue.
- Owner, at plan approval (#1079), verbatim: "I agree with the 3 defaults. 1. yes we can reconcile & back embed the memories in queue once the embedder endpoint is back online for instance. 2. instead of truncation, I4d go with chunking. There's already a massive amount of chunking logic in KB, it's necessary for long articles ingestion. As our facts & memories will be text, we can get a minimal subset of that logic and adapt it on our engine. 3. I agree"
- Orchestrator's brief for the successor: chunking replaces truncation for every provider; port a minimal subset of KB's text chunking (`src/knowledge_base/chunking.py` and knowledge-base#604's split-at-ingest that rejoins exactly, PR knowledge-base#615); put the chunk-to-vector mapping to the owner as an explicit decision; "Chunk with the server-counted limit, 2,047 tokens of text."
- ADR-0020 §6: "Full-text search finds it at once; vector search finds it once it is embedded (#1021)." and "The embedding drain is rate-limited and yields to interactive queries. The rate is set from the embedder's measured throughput (#1021)."
- ADR-0020 §8: "A read-only command (`explain`, `inspect`, `stats`, `query`, `dump`, `outcome-counts`, `status`) takes no lock and may run against the live store: the read-only open never migrates."
- ADR-0020 §10: "Recall embed time limit | 500 ms, for `recall` and for the context build | Full-text search, flagged as degraded"; `cause = "embedder down" / "embedder slow" / "embedder key rejected" / "context timed out"`.
- ADR-0015: "Truncation is outside this ADR. Embedding an input longer than `max_input_tokens` is each layer's ingest concern". This plan does not edit ADR-0015.
- dutiona/infra#77: an input is embedded when the server counts at most 2,048 tokens; the server appends one end-of-sequence token to text, so text holds at most 2,047. One token more is HTTP 400, verbatim: `{"error":{"code":400,"message":"request (2049 tokens) exceeds the available context size (2049 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":2049,"n_ctx":2049}}`. One over-length element fails a whole batch request.
- dutiona/infra#77: one embedding input in flight, shared by ME and KB. Three in flight drained no faster (2.77 vs 2.80 chunks/s of 541 tokens under load) and tripled a query's p95. A query during a one-input drain under load: p95 175.0 ms (130-token items), 120.4 ms (256-token), 320.9 ms (541-token). While the model loads, every path answers 503.
- Owner's ruling: one input in flight and no other policy is enough. Record it; invent no stricter rule.
- Owner's latency ruling: no latency cap, timeout or gate from a guess; a tool call must never hang for seconds.
- After a Mac reboot the embedder is unreachable until a GUI login (accepted): degraded mode holds for hours.
- SessionStart's context build is #1027. This ticket provides the bounded query embed and its cause, and tests `recall` only.
- #1022 and #1023 run in parallel and act on fact IDs, a fact just remembered included. #1023 touches `ddl.rs` and `me-ingest/src/lib.rs`.
- AGENTS.md rule 8: no LLM or network dependency in the core crates; intelligence comes through consumer traits. Rules 2, 4, 9, 10: TDD, atomic commits, no `unwrap()` in library code, rustdoc with `# Errors` and `# Examples` on public items.
- The repository is public: no home paths, host or tailnet names, addresses or tokens in commits, the PR or this plan.
- PR: `Closes #1021` and `Refs dutiona/infra#37`, never a bare `#37`. The owner merges with `/squash-merge`; the worker never merges. Branch commits carry no co-author line.

## Implementation sequence

Every `cargo` command runs from the worktree root, unpiped, as `$H cargo …`: `$H` is the build machine's
heavy-build lock wrapper with `CARGO_BUILD_JOBS=16` (defined outside this repository), so one heavy build runs at a
time.

**Representation (decision 1).** A fact without a vector is a `facts` row with `embedding = X''` plus one
`embed_queue` row. `X''` satisfies `NOT NULL` without a table rebuild, and it can never decode as a vector:
`deserialize_embedding` stays strict, so an SQL vector reader that misses its guard fails loudly with
`EmbeddingDimension` instead of scoring a placeholder. Only the generic hydration `row_to_fact` maps `X''` to an
empty `Vec`; a Rust consumer of a hydrated `Fact.embedding` that missed its guard would score it 0 silently
(`cosine_similarity` zips), which is why every such consumer's input list is guarded in SQL (items 2 and 3) and
tested. Every vector reader filters with one predicate, `HAS_VECTOR = "length(embedding) > 0"`. Rejected:
a placeholder vector (scored, indexed and merged by construction); a side table for pending facts (breaks the
receipt ID, FTS over `content='facts'`, and #1022/#1023's actions); a nullable column with
`Option<Vec<f32>>` (rebuilds `facts` with its FTS triggers and cascading children, and retypes about 46 files).
The zero-compromise structure, every space's vectors in their own relation, is Open question 1.

**Decision D1, for the owner: how a chunked fact maps to vectors.** Both options chunk the same way (item 5).

| | A. One vector per chunk | B. One pooled vector per fact (recommended) |
|---|---|---|
| Storage | a new `fact_chunk_vectors (fact_id, space_id, ord, embedding)` table beside `facts.embedding`, keyed by space as `fact_vectors` is, so reconstruction stages and swaps it too | `facts.embedding` holds the token-weighted mean of the chunk vectors, L2-normalised; no new table |
| Recall | a chunked fact scores by its best-matching chunk: a detail deep in a long final message is found precisely | a chunked fact scores by its pooled vector: a detail that is a small part of a long fact is diluted in the vector channel; hybrid recall's full-text channel still finds its words |
| Readers | every vector reader in the guard table must also read chunk vectors or deliberately use a representative one; brute-force and HNSW must agree; export, restore, reconstruction, Postgres (#634) and the dump format gain the table | none changes: one vector per fact stays the invariant every reader already relies on |
| Size of this PR | a second representation through every guarded reader, the drain's settle and the staged reconstruction: roughly the PR again | one new module and the counting client in the embed crate (item 5; about 150 lines estimated) |
| Later | it can be added behind B without a migration of B's data: re-embedding chunked facts fills the new table; Open question 1's vectors-in-their-own-relation follow-up is where it lands naturally | B forecloses nothing |

Recommended: B. It keeps one vector per fact, so this PR stays one change, and every guard and test above stays as
it is. Its cost falls only on facts longer than 2,047 tokens (remembered facts are statements; long ones are final
messages and compaction summaries from #1025), and #1029's recall log is the data that would show whether A is
worth its size. This plan implements B; choosing A at approval needs a successor plan.

**Where each concern lives.**

| Concern | Home |
|---|---|
| Failure vocabulary `EmbedderError` | `me-types` (L0): the client produces it, the queue consumes it; no network type crosses the trait boundary |
| Failure classification, token counting, chunking and pooling | `memory-engine-embed`: only the HTTP client knows statuses and the server's `/tokenize`; the split itself is a pure function there (`chunk.rs`) |
| `X''`, the queue SQL, every vector-reader guard | `me-backend-sqlite`, behind four new `FactGraph` methods |
| Queue rules: outcome → write, backoff | `me-ingest` + facade (`engine/embed_queue.rs`), unit-tested with a scripted provider |
| Drain loop: wake, sleep, one in flight, shutdown fence | `memory-engine-mcp/src/drain.rs`, spawned by `http::serve` |
| Bounded query embed and its cause | `memory-engine-mcp/src/embedding.rs`; #1027 reuses it |
| `status` | facade free function over the backend SQL; `memory-engine-cli status` formats it |

**Queue-wide state** lives in five `config` keys (the `last_consolidated_at` precedent, no extra table), named
once as constants in `crates/me-backend-sqlite/src/store/embed_queue.rs`: `embed_queue_paused_since`,
`embed_queue_pause_error`, `embed_last_success_at`, `embed_last_error_at`, `embed_last_error`.

- [ ] **0. Worktree and plan.** `git worktree list --porcelain` lists `.worktrees/feat-1021-deferred-embedding`
  on `refs/heads/feat/1021-deferred-embedding` with a `locked` line. The plan issue is published once, by
  `/finish-pr`'s plan archiver, before approval. After approval, commit this plan and
  its review artifacts; the drafts directory is deleted, never committed. No executable behaviour.
  Commit: `docs(plans): plan the deferred-embedding queue (#1021)`.

- [ ] **1. Schema v16: the queue table, no reader or writer yet.**
  Files: `crates/me-backend-sqlite/src/store/schema/{ddl.rs,migrations.rs,mod.rs,tests.rs}`, the insta
  snapshots `schema_v11` and `schema_v11_migration_chain`, the dump goldens
  `snapshot_empty_engine_dump` (me-backend-sqlite) and `snapshot_populated_engine_dump` (memory-engine).
  Red: `migrate_v15_to_v16_adds_the_embedding_queue` (a `simulate_v15` store drops the table and index and sets
  15; after `migrate` both exist at 16), `migrate_v15_to_v16_is_idempotent_on_a_current_schema`,
  `fresh_vs_migrated_embed_queue_converge` (normalized `sqlite_master.sql`, as
  `fresh_vs_migrated_fact_vectors_converge`), `an_embed_queue_row_goes_with_its_fact` (deleting the fact deletes
  the row). `$H cargo test -p me-backend-sqlite --all-features store::schema` fails.
  Change: in `TABLES_DDL`/`INDEXES_DDL` and frozen in `migrate_v15_to_v16`, appended to `MIGRATIONS` as
  `(migrations::migrate_v15_to_v16, false)`; `CURRENT_SCHEMA_VERSION = 16`; `IF NOT EXISTS` keeps the step
  re-runnable for the rolled-back fixtures; `all_nine_indexes_created` expects 30; fix the "(v11→…→v15)" comment
  this makes stale.
  ```sql
  CREATE TABLE IF NOT EXISTS embed_queue (
      fact_id         INTEGER PRIMARY KEY REFERENCES facts(id) ON DELETE CASCADE,
      enqueued_at     TEXT    NOT NULL,
      attempts        INTEGER NOT NULL DEFAULT 0,
      next_attempt_at TEXT    NOT NULL,
      last_error      TEXT,
      failed_at       TEXT
  );
  CREATE INDEX IF NOT EXISTS idx_embed_queue_due ON embed_queue(next_attempt_at) WHERE failed_at IS NULL;
  ```
  Green: the same command, then `$H cargo test -p me-backend-sqlite --all-features` and
  `$H cargo test -p memory-engine --all-features snapshot_` pass after `INSTA_UPDATE=always` rewrites the four
  snapshots and `git diff` shows only the table, its index and `15` → `16`.
  Commit: `feat(storage): schema v16 adds the deferred-embedding queue`.

- [ ] **2. Search path: nothing scores, indexes or snapshots a fact without a vector.**
  Files: `crates/me-backend-sqlite/src/store/facts.rs` (`pub(crate) const HAS_VECTOR`, `row_to_fact`),
  `crates/me-backend-sqlite/src/search/{vector.rs,ann.rs}`, `crates/me-backend-sqlite/src/snapshot.rs`,
  `crates/me-backend-sqlite/src/test_util.rs` (`seed_without_vector(conn, content) -> i64`: a raw `X''` row and its
  queue row), rustdoc of `Fact::embedding` in `crates/me-types/src/types/facts.rs` ("Empty while the fact waits in
  the deferred-embedding queue (#1021); no vector reader returns such a fact; otherwise exactly the store's
  dimension").
  Rules: `vector_search_filtered` reads `WHERE {HAS_VECTOR} AND ({filter})` (the one core of
  `SearchIndex::vector_search` and the HNSW fallback); `ann.rs` `build_inner`, `to_snapshot` and
  `fetch_candidate_embeddings` add `AND {HAS_VECTOR}` (the last so the invariant never depends on the index being
  fresh). `DbFingerprint` (`crates/me-types/src/types/snapshot.rs`) gains `#[serde(default)] pub
  vectorless_fact_count: i64`, which `read_fingerprint` fills with `COUNT(*) FROM facts WHERE length(embedding) =
  0`: filling a vector lowers it, and only a deferred insert, which also raises `max_fact_id`, raises it, so no
  combination of fill, expiry and insert leaves an older sidecar matching. A sidecar written before v16 decodes
  the field as 0 and still matches a store with no waiting fact, so no existing sidecar is invalidated.
  Red: `a_fact_without_a_vector_reads_back_empty` (get, get_facts, list_active, for_each),
  `vector_search_never_scores_a_fact_without_a_vector`, `lexical_finds_a_fact_without_a_vector_and_vector_skips_it`
  (`sqlite/search_index.rs`), under `ann`: `hnsw_build_and_snapshot_skip_a_fact_without_a_vector`,
  `fetch_candidate_embeddings_skips_a_fact_without_a_vector`; `filling_a_fact_without_a_vector_changes_the_fingerprint`
  (it also expires another fact in the same step, the transition that would collide on counts alone).
  `$H cargo test -p me-backend-sqlite --all-features without_a_vector` fails with
  `EmbeddingDimension { expected: 4, actual: 0 }`.
  Green: the same command; `$H cargo test -p me-backend-sqlite --all-features` passes.
  Commit: `feat(storage)!: keep a fact without a vector out of vector search, the HNSW index and its sidecar` (breaking:
  `Fact::embedding` may be empty and `DbFingerprint` gains a public field).

- [ ] **3. Maintenance paths skip a fact without a vector.**
  Files and rules (the `skipped_vectorless` flag also touches `crates/me-types/src/types/consolidation.rs` and
  `crates/me-consolidate/src/pipeline.rs`, which carry the snapshot into the plan):
  - `crates/me-backend-sqlite/src/store/facts.rs`: `list_dormant` adds `AND {HAS_VECTOR}`;
    `list_undreamt_in_period` passes `HAS_VECTOR` in its `extra_conditions` (the fact stays unmarked, so the next
    cycle after its embedding takes it); new `list_active_with_vector()` and `count_active_with_vector()`.
  - `crates/me-backend-sqlite/src/consolidation/mod.rs`: `load_snapshot_capped` caps and loads with the two new
    methods (dedup, cluster and global read nothing else), and in the same read records on the `Snapshot`
    `skipped_vectorless: bool`, whether any active fact had no vector, waiting or failed. The plan carries it, and
    `apply_plan` advances `last_consolidated_at` only when it is false AND, inside its own write transaction, no
    active fact is without a vector. Rule: the watermark never advances while a vectorless fact exists, so a
    fact that had no vector when the watermark last moved sits above it and is compared as new at the first run
    after it gets its vector, whichever path gives it one (the drain, a restore's re-queue, a reconstruction): a
    fact embedded between the load and the apply is caught by the snapshot flag; the apply-time check is cheap
    insurance for a deferred insert backdated (`opts.t_created`) between the two. A failed item holds the
    watermark until it is erased or reconstructed (Open question 7). Not covered, as for `add_fact` today: a fact
    backdated below an existing watermark, or committed by a concurrent writer after the load with an earlier
    `t_created` (Open question 14).
  - `crates/me-backend-sqlite/src/store/fact_vectors.rs::promote_space`: the retain step copies only
    `HAS_VECTOR` rows; after the swap, `DELETE FROM embed_queue` in the same transaction (the completeness gate
    gave every fact a vector; backfill embeds a waiting or failed fact from its content like any other). A
    failed item whose content the new provider also rejects aborts the backfill, as any provider error does (Open
    question 7).
  - `crates/memory-engine/src/inspect/restore.rs`: `validate_snapshot` accepts a fact embedding of length 0 or
    `embed_dim` (summaries unchanged); `restore_snapshot_into`'s transaction re-queues every restored fact with
    an empty embedding (`enqueued_at = next_attempt_at = now`). The dump writes `[]` already.
  - Already guarded, no change: `me-archive` search skips an empty embedding; Promote
    (`apply_cycle_deltas_atomic`, `promote_with_lineage`) refuses a source whose width is wrong; a hard delete
    cascades the queue row.
  Red: `list_dormant_skips_a_fact_without_a_vector`, `list_undreamt_in_period_skips_a_fact_without_a_vector`,
  `load_snapshot_skips_a_fact_without_a_vector`, `the_watermark_holds_while_a_fact_without_a_vector_waits` (a
  waiting fact, a failed fact, one embedded between the load and the apply, one inserted between them),
  `promote_retains_no_fact_without_a_vector_and_empties_the_queue`; facade
  `a_waiting_fact_that_failed_is_compared_as_new_once_embedded` (failed → consolidation, watermark held → each of
  reconstruction, and dump → restore → settle → consolidation: dedup merges it with its older duplicate);
  `a_waiting_fact_round_trips_through_dump_and_restore` (it is queued again; a 3-wide embedding is still
  refused), `consolidation_ignores_a_waiting_fact` (same content as an embedded fact: both stay active),
  `a_dream_cycle_never_selects_a_waiting_fact`. `$H cargo test -p me-backend-sqlite --all-features without_a_vector`
  and `$H cargo test -p memory-engine --all-features waiting_fact` fail.
  Green: both; `$H cargo test -p me-consolidate`, `$H cargo test -p me-cognitive` and
  `$H cargo test -p memory-engine --all-features` pass.
  Commit: `feat(storage): dedup, the dream cycle, resonance, promote and restore skip a fact without a vector`.

- [ ] **4. The embed client says what a retry can do.**
  Files: `crates/me-types/src/error.rs`, `crates/memory-engine/src/lib.rs` (flat error re-export,
  `reexports_are_accessible`), `crates/memory-engine-embed/src/http.rs`,
  `crates/memory-engine-embed/tests/http_integration.rs`.
  Change:
  ```rust
  #[derive(Debug, thiserror::Error)]
  #[non_exhaustive]
  pub enum EmbedderError {
      /// The request never reached the server: reqwest's `is_connect()` (refused, DNS, TLS). A timeout, even one
      /// that ends during connect, is `Failed`: reqwest's blocking client reports it as a request timeout.
      #[error("{0}")] Unreachable(String),
      /// 401 or 403.
      #[error("{0}")] KeyRejected(String),
      /// 400, 413 or 422: this input will not be embedded as sent.
      #[error("{0}")] InputRejected(String),
      /// No usable answer: a timeout, a reset after sending, a broken body, any other non-2xx.
      #[error("{0}")] Failed(String),
  }
  // MemoryError gains:
  #[error("embedder: {0}")] Embedder(#[from] EmbedderError),
  ```
  `send_request` maps each case. Only a request that never reached the server is `Unreachable`: no item can be at
  fault, so the queue waits without touching one. A timeout, a reset or a crash after sending may be the input's
  doing, so it is `Failed` and backs off only its item (a poison input never holds up the others). The texts stay today's (`embedding HTTP request failed: …`,
  `embedding endpoint returned {status}: {body}`, body capped at 1,000 bytes), so the CLI's
  `unreachable_embedder_*` assertions hold. A 2xx the client cannot use keeps its current error.
  `EmbeddingProvider::embed`'s rustdoc states the contract: return `MemoryError::Embedder` so the queue tells the
  cases apart; any other error counts as `Failed`.
  Red: `a_status_maps_to_its_embedder_error` (400, 401, 403, 404, 413, 422, 500, 503), `a_closed_port_is_unreachable`,
  `an_answer_slower_than_the_timeout_is_failed` (mock delay 3 s, `timeout_secs` 1); the two
  `*_http_500_propagates_as_internal_error` tests become `*_is_failed`; `embedder_error_keeps_its_message` in
  `me-types`. `$H cargo test -p memory-engine-embed --test http_integration` fails to compile.
  Green: it passes; `$H cargo test -p me-types` and `$H cargo test -p memory-engine-cli --test cli_integration`
  pass. Commit: `feat(embed): tell an unreachable embedder, a rejected key and a rejected input apart`.

- [ ] **5. An over-length input is chunked, and the fact keeps one pooled vector.**
  Files: `crates/memory-engine-embed/src/{chunk.rs (new),http.rs,lib.rs}`,
  `crates/memory-engine-embed/tests/http_integration.rs`.
  Change, ported from knowledge-base (`embeddings.py` `split_to_limit`/`_fit`, PR knowledge-base#615, merge
  `e95f0620`), the subset this engine needs (plain text; none of `chunking.py`'s markdown, table, page or
  Python-AST logic):
  ```rust
  /// `text` as consecutive slices that each count at most `limit`, each with its count; joined, they are `text`.
  pub fn split_to_limit<'t>(text: &'t str, limit: usize,
      count: &mut dyn FnMut(&str) -> Result<usize, MemoryError>) -> Result<Vec<(&'t str, usize)>, MemoryError>;
  ```
  Split rules, as KB's: when `count(text) <= limit`, the text is one piece. Otherwise the pieces but the last aim
  at an even share of the content (each plus `count("")`, the tokens the server adds to every input), the last
  aims at `limit`. Each cut gallops from its aim (in characters, mapped to byte offsets), then bisects, and is only
  ever a length that was counted; the count returned with a piece is the one counted for it (KB's cut does not
  always end on a count, the port records one). A cut moves back to just after the last line end, else the last
  whitespace, when that keeps at least half the piece. Cuts fall on character boundaries. `limit <= count("")`, or
  a first character that alone does not fit, is `InputRejected` ("not even one character fits"): the only way a
  long input still fails when a limit is known.
  The limit: the served `max_input_tokens` (ADR-0015 §1, `with_served_artifact`), 2,048 as the server counts for
  the deployed model, so 2,047 tokens of text. With no declared limit, the server's own: when llama-server answers
  the self-identifying 400 (`error.type == "exceed_context_size_error"` with integers `n_prompt_tokens` and `n_ctx`
  C, dutiona/infra#77's recorded body), the input is chunked with limit C − 1 (the most the server accepts), so no
  long input fails for want of a declared limit. Any other 400, 413 or 422 is never read as a length signal.
  Counting, as the server counts: for provider `llama.cpp`, or once that recorded 400 has come back,
  `POST <endpoint without its trailing /v1/embeddings>/tokenize` (KB's base rule, which keeps a path prefix) with
  `{"content": …, "add_special": true, "parse_special": true}` and the API key, `len(tokens)`. On b12048 with the
  pinned GGUF that count includes the appended EOS and equals `usage.prompt_tokens` (knowledge-base#615's live
  check: `hello` counts 2 with `add_special`, 1 without). A `/tokenize` that is not there (400, 404, 405, 422, or a
  body without a `tokens` list, KB's own test) falls back to the byte bound below with one `warn`: chunks get
  finer, nothing fails. Its other failures classify like an embed call (item 4). For any other provider the count
  is the conservative bound `UTF-8 bytes + 2` (a token covers at least one byte; 2 for the special tokens
  BERT-style models add). No count is made when `bytes + 2 <= limit`, so short facts cost no extra request
  (recall's queries, at most 1,024 bytes plus the instruction, never split).
  Pooling (decision D1, B): `embed` embeds each piece in order, one input in flight, applies MRL truncation per
  piece if configured, and returns their mean weighted by each piece's count, L2-normalised; a pooled vector with a
  zero or non-finite norm is an error, as `mrl_truncate`'s. The stored content is never cut. `embed_batch` screens
  each text: one that splits goes through `embed` alone, the rest stay one batch request; on the recorded 400 the
  batch falls back to `embed` per text (the answer does not say which is long). A counted 400 after a fitting
  count (the count and the server disagree) is `InputRejected` with the server's body.
  Accepted cost: a fact near the 1 MiB content limit (`MAX_PAYLOAD_BYTES`) is about 125 pieces, about 2,000
  counting requests (sub-millisecond each, measured in knowledge-base#615) and 125 sequential embeds in one drain
  step; a transient failure on any piece retries the whole fact.
  Red (`chunk.rs` units, with a word-counting closure, KB's cases, plus a proptest over random text and limits:
  joined pieces equal the text, every count ≤ limit, no empty piece, every cut on a char boundary):
  `a_text_that_fits_comes_back_whole`, `pieces_rejoin_exactly_with_newlines_indentation_and_trailing_whitespace`,
  `a_non_monotone_count_still_yields_verified_pieces`, `a_cut_keeps_at_least_half_a_piece`,
  `a_cut_prefers_the_end_of_a_line`, `no_piece_is_a_stray_tail`, `a_character_too_big_for_the_even_share_still_fits`,
  `a_long_word_after_a_fitting_boundary_is_cut_inside_the_word`, `nothing_fits_is_input_rejected`,
  `cuts_fall_on_character_boundaries`, `chunk_invariants_hold` (proptest); `http_integration`:
  `over_length_input_is_chunked_by_the_servers_count` (a `/tokenize` mock counting words plus 1, an embed mock
  refusing inputs over the limit with the recorded 400: a 3-chunk input sends three fitting embed requests, and the
  vector is the normalised weighted mean of the three), `over_length_short_input_is_never_counted`,
  `over_length_without_tokenize_uses_the_byte_bound` (provider `tei` with M),
  `over_length_missing_tokenize_falls_back_to_the_byte_bound` (404),
  `over_length_without_a_declared_limit_is_chunked_by_the_recorded_answer`,
  `over_length_in_a_batch_is_chunked_alone`, `over_length_recorded_answer_in_a_batch_falls_back_per_text`,
  `over_length_tokenize_401_is_key_rejected`, `over_length_counted_400_after_a_fitting_count_is_input_rejected`,
  `over_length_pooling_applies_mrl_per_piece`, `over_length_rule_leaves_another_400_alone`.
  `$H cargo test -p memory-engine-embed chunk` and
  `$H cargo test -p memory-engine-embed --test http_integration over_length` fail (no `chunk` module).
  Green: `$H cargo test -p memory-engine-embed` passes. The rustdoc of `HttpEmbeddingProvider::embed` ("via one
  HTTP POST") and its `# Errors` change with it.
  Commit: `feat(embed): chunk an over-length input by the server's own count and pool its vectors`.

- [ ] **6. Recall answers from flagged full-text search when the query embed fails or passes 500 ms.**
  Files: `crates/memory-engine-mcp/src/embedding.rs`, `crates/memory-engine-mcp/src/tools/recall.rs`,
  `crates/memory-engine-mcp/tests/degraded_embedder.rs` (new), `crates/memory-engine-mcp/tests/common/mod.rs`
  (`mount_embedder(query, document)`: the query mock matches `body_string_contains("Instruct:")` at a higher
  priority than the document mock), `crates/memory-engine-mcp/tests/lean_surface.rs`.
  Change:
  ```rust
  /// ADR-0020 §10: past this, `recall` (and #1027's context build) answers from full-text search, flagged.
  pub const RECALL_EMBED_LIMIT: Duration = Duration::from_millis(500);
  /// Why a read fell back; `as_str` is ADR-0020 §10's cause.
  pub enum Degraded { EmbedderDown, EmbedderSlow, EmbedderKeyRejected }
  pub async fn embed_query_bounded(provider: &Arc<HttpEmbeddingProvider>, text: String) -> Result<Vec<f32>, Degraded>;
  ```
  `tokio::time::timeout(RECALL_EMBED_LIMIT, spawn_blocking(embed_query))`: elapsed → `EmbedderSlow`;
  `Embedder(KeyRejected)` → `EmbedderKeyRejected`; any other error or a join error → `EmbedderDown`. A blocking
  call left behind ends at the provider's own timeout and its result is dropped. On `Err(cause)` recall answers
  `{"search":"full-text only","degraded":"<cause>","results":[…]}`, or `"results":[]` when the query has no word
  (the engine never gets a query with neither text nor vector). `degraded` appears only after a fallback. With no
  embedder configured the answer stays `full-text only` without `degraded`: configuration, not a fallback.
  Red: `recall_answers_from_full_text_when_the_embedder_is_down` (503: a seeded fact found, `"embedder down"`),
  `recall_answers_within_the_limit_when_the_embedder_hangs` (query delay 3 s: answered in under 1 s,
  `"embedder slow"`), `recall_names_a_rejected_key` (401 and 403), `a_wordless_recall_with_the_embedder_down_lists_nothing`,
  `a_healthy_embedder_is_never_flagged`; `lean_surface`'s `a_recall_whose_embedder_fails_is_a_recorded_error`
  becomes `a_recall_whose_embedder_fails_answers_from_full_text` (its record carries `fact_ids`).
  `$H cargo test -p memory-engine-mcp --test degraded_embedder` fails: recall errors, or waits out the hang.
  Green: `$H cargo test -p memory-engine-mcp` passes.
  Commit: `feat(mcp): recall answers from flagged full-text search when the embedder is down or slow`.

- [ ] **7. Store a fact now and queue its vector (engine API; the server does not use it yet).**
  Files: `crates/me-storage/src/graph.rs`, `crates/me-backend-sqlite/src/store/{embed_queue.rs (new),facts.rs,mod.rs}`,
  `crates/me-backend-sqlite/src/sqlite/graph.rs`, `crates/me-ingest/src/lib.rs`,
  `crates/memory-engine/src/engine/{ingest.rs,mod.rs}`.
  Change:
  ```rust
  // FactGraph, section "deferred embedding (#1021)"
  /// Insert `fact` with no vector (its `embedding` must be empty) and queue it, in one transaction.
  async fn insert_fact_deferred(&self, fact: &NewFact, now: DateTime<Utc>) -> Result<i64>;
  // me-ingest
  pub async fn add_fact_deferred(ctx: MemoryCtx<'_>, scope_tree: &RwLock<ScopeTree>, req: &AddFactRequest) -> Result<i64>;
  // facade; notifies `embed_work` after the commit
  pub async fn add_fact_deferred(&self, req: &AddFactRequest) -> Result<i64>;
  pub async fn embed_work_arrived(&self); // awaits the engine's `tokio::sync::Notify`
  ```
  The request-to-`NewFact` half of `insert_fact_with_embedding` (defaults, pin, scope resolution) becomes
  `prepare_new_fact`, shared by the three single-fact paths (`add_fact`, `add_fact_precomputed`,
  `add_fact_deferred`), behaviour-preserving, so a check #1023 adds there covers the deferred path too; the batch
  paths keep their own savepoint-scoped build. The deferred path runs `add_fact`'s gates (`ensure_open`,
  `ensure_writable`, validation), takes no classifier (`opts.pinned`, else false), records no identity (there is no
  vector) and fires no HNSW notify; the facade calls `notify_one` after the commit (its stored permit means a drain
  that is not yet waiting still wakes). `FactStore::insert` keeps refusing an empty embedding; it and the new
  `insert_without_vector` share one private row insert, so `insert_fact_deferred` and restore are the only
  producers of `X''`. `FactStore::insert_or_reinforce` (bootstrap reinforces by content hash) leaves a matched
  waiting row as it is, queued, for the drain to embed: filling it there would bypass the identity stamp and the
  index, which the bootstrap batch path only runs for new rows. `SqliteBackend::insert_or_reinforce_fact` then
  fires its `ann` notify only when the returned row holds a vector, so the index never gets a vector the row lacks.
  Red: `a_deferred_insert_writes_the_row_and_its_queue_entry_together`, `a_deferred_insert_that_fails_leaves_nothing`
  (a test trigger aborts the queue insert: no fact row); facade `add_fact_deferred_needs_no_embedder` (in-memory
  engine: the ID reads back empty, a text query finds it, a vector query neither errors nor returns it, no identity
  recorded, `embed_work_arrived` resolves), `add_fact_deferred_validates_like_add_fact` (read-only, oversized
  content, importance out of range); backend `reinforcing_a_deferred_fact_leaves_it_queued_and_unindexed` (`ann`; the
  bootstrap batch with every fact matching a waiting one records no identity and leaves every row queued). `$H cargo test -p
  memory-engine --all-features add_fact_deferred` fails to compile.
  Green: it passes; `$H cargo test -p me-backend-sqlite --all-features deferred` passes; the facade method has a
  doctest that adds a fact and finds it by full text. Commit: `feat(ingest): store a fact now and queue its vector`.

- [ ] **8. The drain's two steps and its retry rules.**
  Files: `crates/me-types/src/types/{embed_queue.rs (new),mod.rs}`, `crates/me-storage/src/graph.rs`,
  `crates/me-backend-sqlite/src/store/embed_queue.rs`, `crates/me-backend-sqlite/src/sqlite/graph.rs`,
  `crates/memory-engine/src/engine/{embed_queue.rs (new),mod.rs}`, `crates/memory-engine/src/lib.rs`.
  Change:
  ```rust
  // me-types
  pub struct PendingEmbedding { pub fact_id: i64, pub content: String, pub attempts: u32 }
  pub enum NextEmbedding { Due(PendingEmbedding), NotBefore(DateTime<Utc>), Idle }
  pub enum EmbeddingFailure { Retry { at: DateTime<Utc> }, Fail, Hold { paused: bool } }
  // FactGraph
  async fn next_embedding(&self, now: DateTime<Utc>) -> Result<NextEmbedding>;
  async fn complete_embedding(&self, fact_id: i64, embedding: &[f32], fingerprint: &EmbeddingFingerprint,
                              expected_dim: usize, now: DateTime<Utc>) -> Result<()>;
  async fn record_embedding_failure(&self, fact_id: i64, error: &str, failure: EmbeddingFailure,
                                    now: DateTime<Utc>) -> Result<()>;
  // facade, engine/embed_queue.rs, re-exported at the root
  #[derive(Clone, Copy, Debug)] pub struct Backoff { pub base: Duration, pub cap: Duration }
  impl Backoff { pub const DEFAULT: Self; /* 1 s, 60 s */ pub fn delay(self, n: u32) -> Duration; /* 0 → 0, else min(cap, base·2^(n−1)), saturating */ }
  pub enum Settled { Embedded, Rejected, Retrying, Unreachable, KeyRejected }
  pub async fn next_embedding(&self) -> Result<NextEmbedding>;
  pub async fn settle_embedding(&self, item: &PendingEmbedding, result: Result<Vec<f32>>,
                                fingerprint: &EmbeddingFingerprint, backoff: Backoff) -> Result<Settled>;
  ```
  SQL (one transaction per write):
  - `next_embedding`: the row with `failed_at IS NULL` and the smallest `(next_attempt_at, fact_id)`, joined to
    `facts.content`: `Due` when `next_attempt_at <= now`, else `NotBefore`; `Idle` when none waits.
  - `complete_embedding`: refuse a vector not `expected_dim` wide; `record_if_absent(fingerprint)` (#613, rejects a
    mismatch per #614); `UPDATE facts SET embedding = ? WHERE id = ? AND length(embedding) = 0` (never over a
    vector); delete the queue row; set `embed_last_success_at`; delete both pause keys. After the commit, under
    `ann`, `hnsw_notify_insert` when a row was written and is active, with `insert_fact_atomic`'s carve-out: an
    `IndexInconsistent` there means the write committed, so `settle_embedding` reports `Embedded` and logs it,
    never retries. A fact that already has a vector, or is gone, only loses its queue row.
  - `record_embedding_failure`: `Retry` sets `attempts + 1`, `next_attempt_at = at`, `last_error`; `Fail` sets
    `attempts + 1`, `failed_at = now`, `last_error`; `Hold` changes no queue row. `Retry` and `Fail` (an answer
    came back, so the key works) delete both pause keys; `Hold { paused: true }` keeps the first
    `embed_queue_paused_since` (`INSERT OR IGNORE`) and sets `embed_queue_pause_error`. Every failure sets
    `embed_last_error_at` and `embed_last_error`.
  `settle_embedding` (`ensure_open`, `ensure_writable`) maps `result`:

  | Result | Write | Settled |
  |---|---|---|
  | a vector | `complete_embedding`; a vector the store refuses (width, identity) becomes `Retry` with that error | `Embedded` (`Retrying` if refused) |
  | `Embedder(Unreachable)` | `Hold { paused: false }` | `Unreachable` |
  | `Embedder(KeyRejected)` | `Hold { paused: true }` | `KeyRejected` |
  | `Embedder(InputRejected)` | `Fail` | `Rejected` |
  | any other error (5xx, another status, an unusable body) | `Retry { at: now + backoff.delay(attempts + 1) }` | `Retrying` |

  Red (facade, file-backed engine, a scripted provider): `an_embedded_result_fills_the_vector_and_leaves_the_queue`
  (a vector query finds it; identity recorded; success time set), `a_key_rejection_pauses_and_touches_no_item`
  (a later success clears the pause), `an_input_rejection_fails_only_that_item`, `an_unreachable_embedder_touches_no_item`,
  `a_failed_answer_backs_off_only_that_item` (another item is `Due` first), `a_failed_answer_clears_the_pause`,
  `a_vector_is_never_written_over_another`, `a_foreign_identity_is_refused_and_the_item_kept`,
  `an_index_inconsistency_after_the_commit_is_embedded` (`ann`),
  `an_item_schedule_survives_a_reopen`, `backoff_doubles_from_base_to_cap` (`DEFAULT`: 0, 1, 2, 4, 8, 16, 32, 60,
  60 s; `u32::MAX` → 60 s); under `ann`: `a_filled_vector_joins_the_hnsw_index`.
  `$H cargo test -p memory-engine --all-features embed_queue` fails to compile.
  Green: it passes; `$H cargo test -p me-backend-sqlite --all-features embed_queue` passes; one runnable example
  (`add_fact_deferred` → `next_embedding` → `settle_embedding` on an in-memory engine).
  Commit: `feat(core): the deferred-embedding drain's steps and retry rules`.

- [ ] **9. `status` reports the queue read-only, without the engine.**
  Files: `crates/me-types/src/types/embed_queue.rs` (`EmbedQueueStatus { waiting, oldest_enqueued_at, failed,
  paused_since, pause_error, last_success_at, last_error_at, last_error }`), `crates/me-backend-sqlite/src/store/embed_queue.rs`
  (`read_status(conn)`), `crates/memory-engine/src/{lib.rs,engine/embed_queue.rs}`,
  `crates/memory-engine-cli/src/{main.rs,commands/mod.rs,commands/status.rs (new)}`,
  `crates/memory-engine-cli/tests/{status.rs (new),store_lock.rs}`.
  Change: `pub fn read_embed_queue_status(db: &Path) -> Result<EmbedQueueStatus>`, re-exported at the root. It
  opens with `open_connection_read_only` (`SQLITE_OPEN_READ_ONLY`, `busy_timeout = 5000`), returns
  `Internal("status needs a read-only connection")` unless `conn.is_readonly(DatabaseName::Main)?`, runs
  `validate_schema_version` (it never migrates; below 16 the error names the migration), then `read_status`. The
  CLI's `status` is synchronous like `schema`, builds no `MemoryEngine` and takes no `StoreLock`. It prints error
  texts with control characters escaped (an embedder's body reaches the terminal). Output:
  ```text
  embedding queue: 3 waiting, oldest 2h; 1 failed
  paused: no                              | paused: since <RFC 3339>: <error>
  last embed success: <RFC 3339>          | never
  last embed error: <RFC 3339>: <error>   | never
  ```
  `--format json`: `{"waiting","oldest_age_secs","failed","paused_since","pause_error","last_success_at","last_error_at","last_error"}`,
  `null` when absent; `--format plain`: the same keys as `key=value`. The age uses ADR-0020 §10's largest whole
  unit (`m`, `h`, `d`). It exits 0 in every queue state.
  Red: `status_reports_the_queue` (states made through `add_fact_deferred` and `settle_embedding`: one embedded,
  one rejected, one waiting, the queue paused by a 401; table and JSON), `status_runs_beside_a_writer_and_changes_nothing`
  (a held `StoreLock` and an open `BEGIN IMMEDIATE`: exit 0; the file's bytes unchanged),
  `status_never_migrates_an_older_store` (v15: exit 1 naming 15 and 16; still 15, no `embed_queue`, bytes
  unchanged), `read_only_commands_run_while_the_store_is_held` gains `status`; facade
  `the_status_connection_is_read_only` (`is_readonly` true; a write through it fails with `SQLITE_READONLY`).
  Also: `peek_embed_dim_from_db` (`crates/memory-engine-cli/src/db.rs`), which every engine-opening read command
  calls, no longer says "add a fact first" for a store whose facts all wait (no identity before the first vector):
  its error names that state and points to `status`; test `a_store_whose_facts_all_wait_names_the_queue`.
  `$H cargo test -p memory-engine-cli --test status` fails: no `status` subcommand.
  Green: `$H cargo test -p memory-engine-cli` and `$H cargo test -p memory-engine --all-features read_only` pass.
  Commit: `feat(cli): status reports the embedding queue read-only, without opening the engine`.

- [ ] **10. `remember` stores at once and a one-in-flight drain embeds the queue.**
  Files: `crates/memory-engine-mcp/src/{drain.rs (new),lib.rs,server.rs,http.rs,tools/remember.rs}`,
  `crates/memory-engine-mcp/tests/{common/mod.rs,degraded_embedder.rs,lean_surface.rs,http_shutdown.rs}`.
  Change:
  - `remember` calls `ctx.engine.add_fact_deferred(&request)` and keeps its receipt. It no longer needs an
    embedder, so the `no embedder is configured` refusal goes (Open question 2).
  - `drain::run(engine: Arc<MemoryEngine>, provider: Arc<dyn EmbeddingProvider>, fence: Arc<tokio::sync::RwLock<()>>, backoff: Backoff)`
    loops, keeping `streak`, the count of consecutive outcomes that embedded nothing (`Unreachable`, `KeyRejected`,
    `Retrying`):
    - `Idle` → await `embed_work_arrived`. `NotBefore(t)` → the sooner of a wake and `t`.
    - `Due(item)` → one `spawn_blocking(embed)`: the only input in flight, and the drain's whole rate rule. Then
      take `fence.read()` and `settle_embedding`.
    - `Embedded` or `Rejected` reset `streak`. `Unreachable` or `KeyRejected` (the item stays due) sleep
      `delay(streak)`. `Retrying` (only that item is backed off) sleeps `delay(streak − 1)`: one failing item never
      slows the others, and a whole embedder answering 503 is probed at most once per `delay`. A wake never
      shortens these sleeps. A store error logs a `warn` and sleeps `backoff.cap`. Every branch leaves the item
      not due or sleeps, so none spins.
  - `http::serve` spawns the drain when the server has an embedder; right after it takes the `in_flight` write
    fence, before `flush_snapshot`, it aborts the drain's `JoinHandle` and awaits it: a settle in progress finishes
    first, none commits after the flush, and the drain's `Arc<MemoryEngine>` is dropped before the flush. An embed
    still in flight is retried at the next start.
  - `MemoryMcpServer` gains `embed_backoff: Backoff` (`Backoff::DEFAULT`) and `#[must_use] pub fn
    with_embed_backoff(self, Backoff) -> Self`, for the tests' sub-second schedule; `new` keeps its signature
    (the fuzz target calls it). `crates/memory-engine-mcp/clippy.toml` disallows `MemoryEngine::add_fact`
    ("ADR-0020 §6, #1021: a tool call never waits on the embedder; store with add_fact_deferred"), and the batch
    entries' reasons say "add_fact_deferred"; `identity_startup.rs`'s store stamping allows it locally.
  - Harness: `TestServer` defaults to `Backoff { base: 10 ms, cap: 100 ms }` and gains `start_unreachable` (a
    reserved, closed loopback port), `bring_embedder_up` (`MockServer::builder().listener(..)` on that port),
    `restart` (same store), `queue(id) -> (attempts, next_attempt_at, failed_at, last_error)`, `has_vector(id)`,
    `wait_drained`.
  - Existing tests: `every_answer_starts_with_its_call_id` (remember without an embedder succeeds);
    `recall_embeds_with_the_query_instruction_and_remember_without` waits for the drain;
    `a_remember_whose_fact_write_fails_leaves_an_unnamed_record` fails the write with a `BEFORE INSERT ON facts`
    trigger; `http_shutdown`'s in-flight tests hold the call with a `recall` whose query takes 400 ms.
  Red (`degraded_embedder.rs`, through `remember` and `recall` over HTTP):
  `a_fact_remembered_while_the_embedder_is_down_is_stored_and_found` (closed port: receipt, queue row with
  `attempts` 0, no vector; recall finds it, `"embedder down"`), `a_pending_fact_never_breaks_a_hybrid_recall`
  (queries 200, documents 503: `hybrid`, no error, found through full text),
  `a_backlog_drains_one_input_at_a_time_and_recall_stays_hybrid` (20 waiting; documents answer after 100 ms,
  queries at once: every recall meanwhile is `hybrid` without `degraded`, and no two document requests overlap),
  `an_outage_longer_than_the_cap_ends_with_every_vector` (closed port for more than three caps, rows untouched;
  then `bring_embedder_up`: every fact has a `DIM`-wide vector, nothing failed),
  `a_rejected_key_pauses_the_queue_and_touches_no_item` (401, then 403: rows unchanged, the pause keys set; then
  200: drained, pause cleared), `a_rejected_input_fails_only_that_item` (400, 413, 422 for content holding
  `poison`: the others embed; the poison item is failed with an error naming the status),
  `a_recurring_5xx_for_one_item_never_holds_up_the_others`, `a_poison_item_that_times_out_never_holds_up_the_others`
  (`timeout_secs` 1; one content answers after 3 s: the others get their vectors, it waits with `attempts` ≥ 1),
  `a_failing_embedder_is_not_stormed` (503 for every
  input, 10 items: at most 25 requests in 1 s), `an_over_length_memory_is_chunked_and_gets_one_vector` (a harness
  constructor with provider `llama.cpp`, a declared `max_input_tokens` of 64 and a `/tokenize` responder: a 300-byte
  memory ends with one `DIM`-wide vector after its fitting embed requests, its content whole),
  `the_queue_survives_a_restart`; `drain.rs` unit `never_two_inputs_in_flight` (a counting provider: at most 1);
  `http_shutdown`: `a_drain_write_never_lands_after_the_stop` (documents answer after 2 s: `stop()` returns in
  under 1.5 s with the sidecar written; 2.5 s later the fact still waits). Timing assertions are upper bounds only.
  `$H cargo test -p memory-engine-mcp --test degraded_embedder` fails: `remember` errors while the embedder is down.
  Green: `$H cargo test -p memory-engine-mcp` and
  `$H cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings` pass.
  Commit: `feat(mcp)!: remember stores at once and a one-in-flight drain embeds the queue` (breaking: `remember`
  no longer refuses without an embedder).

- [ ] **11. The process never waits for an embed call that hangs.**
  Files: `crates/memory-engine-mcp/src/main.rs`, `crates/memory-engine-mcp/tests/{server_process.rs,common/mod.rs}`
  (`ServerProcess::tool(machine, name, args) -> Value`).
  Red: `starts_serves_and_exits_beside_a_silent_embedder` (the embed URL is a bound loopback listener that never
  answers, `timeout_secs` 30: the binary becomes ready, `remember` answers `stored …`, `recall` answers
  `"embedder slow"` in under 1.5 s, and SIGTERM exits 0 within 10 s). `$H cargo test -p memory-engine-mcp --test
  server_process silent` fails: dropping `#[tokio::main]`'s runtime waits for the drain's blocking call until the
  provider's timeout.
  Change: `main` builds the multi-thread runtime itself, `block_on`s today's body (renamed `run`), then calls
  `runtime.shutdown_background()`: the flush has run inside `serve`, and nothing left in the blocking pool can
  write.
  Green: `$H cargo test -p memory-engine-mcp --test server_process --test http_shutdown` passes.
  Commit: `fix(mcp): never wait for an embed call that hangs when the server stops`.

- [ ] **12. Documentation** (the Documentation section). No executable behaviour. Check:
  `uv run sphinx-build -b html docs docs/_build` adds no warning, and Verification's two `cargo doc` lines pass.
  Commit: `docs: the deferred-embedding queue, degraded recall, status and the drain's rate`.

- [ ] **13. Boy-scout: stale notes in `docs/reference/cli-inspector.md`.** Line 260 says `open_engine()` may
  migrate and that a read-only open "is tracked in #103"; #103 is closed and `open_engine` opens
  `.read_only(true)`. Lines 123 and 266 cite #80 (closed) and an `#[ignore]` test the CLI crate no longer has.
  Correct them to what the code does. Check: `grep -n "#103\|#80\|#\[ignore\]" docs/reference/cli-inspector.md`
  prints nothing. Commit: `docs(cli): correct the stale read-only-open and import notes`.

- [ ] **14. #634 and the parity proof (no commit).** Comment on #634 with
  `gh issue comment 634 --repo dutiona/memory-engine --body-file <file outside the repository>`:

  | State | SQLite (v16) | Proposed Postgres |
  |---|---|---|
  | A fact without a vector | `facts.embedding = X''`, the column stays `NOT NULL` | `facts.embedding IS NULL` (a `vector(n)` cannot be empty), so its `NOT NULL` goes |
  | `embed_queue.fact_id` | `INTEGER PRIMARY KEY REFERENCES facts(id) ON DELETE CASCADE` | `BIGINT PRIMARY KEY REFERENCES facts(id) ON DELETE CASCADE` |
  | `enqueued_at`, `next_attempt_at` | `TEXT NOT NULL` (RFC 3339, UTC) | `TIMESTAMPTZ NOT NULL` |
  | `attempts` | `INTEGER NOT NULL DEFAULT 0` | `INTEGER NOT NULL DEFAULT 0` |
  | `last_error` | `TEXT` | `TEXT` |
  | `failed_at` | `TEXT` | `TIMESTAMPTZ` |
  | `idx_embed_queue_due` | `(next_attempt_at) WHERE failed_at IS NULL` | the same partial index |
  | `config` keys | the five names above | the same keys |

  It also names the four `FactGraph` methods and the readers that must exclude a fact without a vector. Run
  `$H cargo test -p me-backend-postgres --features test-util -- --ignored` (testcontainer): every test passes. The
  PR states this proves Postgres unchanged, not matching: `PgBackend` implements no `FactGraph` yet (#634), so the
  new methods have nothing to refuse there.

- [ ] **15. Rebase and reconcile with #1022 and #1023.** `git fetch origin && git rebase origin/main`. If #1023
  landed a v16 migration, renumber this one (function, `MIGRATIONS` slot, `CURRENT_SCHEMA_VERSION`, tests,
  goldens, the policy paragraph, the #634 comment). If #1023 guards the add-fact path against a tombstone, it
  must cover `add_fact_deferred` through `prepare_new_fact`; repeat its test through `remember`. If #1022 changed
  `remember`'s request (level, project key), the deferred call keeps it. Rerun Verification in full. Commit, only
  if something changed: `fix(ingest): the deferred write honours #1023's tombstone`.

- [ ] **16. PR.** `gh pr create` titled `feat(mcp)!: store memory now, embed it later when the embedder is down`.
  The body: `Classification: Feature (researched) (user-confirmed)` and the other route lines; the representation
  decision and the guard table (Testing); the acceptance map; the Verification commands with their results; the
  #634 comment link and the parity statement; the plan issue; the open questions with their defaults;
  `Collateral findings`; `Closes #1021` and `Refs dutiona/infra#37`. No commit.

- [ ] **17. `/super-review`, Full gate, then `/finish-pr` to READY.** Each finding is fixed in its own commit or
  answered with evidence; Verification is rerun after the last fix. No CI run stands in for the local matrix.

- [ ] **18. Merge.** The owner merges with `/squash-merge`; the worker never merges.

## Documentation

- `docs/design/adr/0020-harness-neutral-lifecycle-api.md` §6: replace "The rate is set from the embedder's
  measured throughput (#1021)." with: "The rate is one embedding input in flight, shared by ME and KB, and no
  other limit (owner's decision, from dutiona/infra#77's measurements): three in flight drained no faster (2.77
  vs 2.80 chunks/s of 541 tokens under load) and tripled a query's p95, while during a one-input drain under load
  a query's p95 was 175.0 ms (130-token items), 120.4 ms (256-token) and 320.9 ms (541-token), inside §10's
  500 ms."
- `docs/reference/mcp-server.md`:
  - `recall`: the `degraded` key, its three causes, the 500 ms limit, the wordless case.
  - `remember`: stored at once whatever the embedder's state; full-text search finds it at once, vector search
    once the drain has embedded it. The "needs an embedder" sentence goes.
  - A new "Embedding queue" subsection: one input in flight, woken by each write; no connection made → the queue
    waits, no item changes (a refused connection, DNS or TLS failure); 401/403 → paused, re-probed on the backoff schedule, cleared by the next answer;
    400/413/422 → that item fails with its error; a timeout, a reset or any other answer → only that item backs off;
    1 s doubling to 60 s; a fact over the served `max_input_tokens` → chunked into pieces that rejoin exactly and
    embedded as one pooled vector (counted by llama.cpp's `/tokenize`, else by a byte bound); the queue survives a restart;
    checking it with
    `podman exec <container> memory-engine-cli --db <db> status`.
  - "Shutdown and health": the drain stops at the fence; an embed in flight is abandoned and retried at the next
    start; the process never waits for it. The architecture list gains `drain.rs`.
- `docs/reference/cli-inspector.md`: a `status` section (fields, the three formats, read-only, no lock, never
  migrates, the below-16 refusal, the `podman exec` form); `status` joins the commands that never take the lock.
- `docs/design/schema-evolution-policy.md`: a "v15→v16 (#1021)" paragraph: an additive table plus the data
  convention `X''` = no vector yet, the readers it guards, `IF NOT EXISTS`, 30 indexes.
- `docs/reference/crate-layout.md`: `engine::embed_queue` (deferred write and settle), the server's `drain.rs`
  and `memory-engine-embed`'s `chunk.rs`.
- `docs/reference/mcp-server.md`'s identity section ("declared, never probed"): `max_input_tokens` also sets the
  chunk limit.
- `docs/design/design-choices.md`: decision D1 as approved (one pooled vector per fact) and why, with option A as
  the recorded alternative.
- `docs/reference/api.md`: `add_fact_deferred`, `next_embedding`, `settle_embedding`, `embed_work_arrived`,
  `read_embed_queue_status`, `Backoff`, `Settled`, `EmbedderError`.
- `docs/design/architecture-overview.md`: the "Add Fact" step says the vector comes at once (`add_fact`) or from
  the queue (`add_fact_deferred`).
- Rustdoc with `# Errors` and `# Examples` on every new public item; `Fact::embedding` and
  `DbFingerprint::vectorless_fact_count` state the new meaning.

## Testing

TDD per item: each test is written first and seen failing with its item's red command. HTTP-level tests use the
in-process `http::serve` with a wiremock embedder (`crates/memory-engine-mcp/tests/common/mod.rs`); the
process-level one uses the real binary. Timing tests assert upper bounds only. Unit tests stay below the seam
where the rule lives (storage SQL, settle rules, the drain's streak), integration tests prove each acceptance item
end to end. Excluded: a live embedder (`qwen_integration` stays opt-in) and a Postgres data path (none exists
before #634).

| Acceptance | Tests | Item |
|---|---|---|
| 1 writes succeed into a persistent queue; FTS recall finds the fact, flagged | `a_fact_remembered_while_the_embedder_is_down_is_stored_and_found`, `the_queue_survives_a_restart`, `add_fact_deferred_needs_no_embedder` | 7, 10 |
| 1 no placeholder visible | the guard table below, then `a_pending_fact_never_breaks_a_hybrid_recall` | 2, 3, 10 |
| 2 rate-limited, yields to recall | `never_two_inputs_in_flight`, `a_backlog_drains_one_input_at_a_time_and_recall_stays_hybrid` (these prove ME's side: the drain never holds two inputs, and recall never queues behind the drain in ME; the embedder's side, a query's latency beside a one-input drain, is dutiona/infra#77's measurement on the real embedder, inside the 500 ms limit) | 10 |
| 3 outage longer than the cap | `an_outage_longer_than_the_cap_ends_with_every_vector`, `backoff_doubles_from_base_to_cap` | 8, 10 |
| 4 pause, fail, per-item backoff | `a_status_maps_to_its_embedder_error`, the `settle_embedding` tests, `a_rejected_key_pauses_the_queue_and_touches_no_item`, `a_rejected_input_fails_only_that_item`, `a_recurring_5xx_for_one_item_never_holds_up_the_others`, `a_poison_item_that_times_out_never_holds_up_the_others`, `a_failing_embedder_is_not_stormed`, `status_reports_the_queue` | 4, 8, 9, 10 |
| 5 over-length input (chunked, by the owner's decision, instead of truncated) | the eleven `chunk.rs` tests, the eleven `over_length_*` tests, `an_over_length_memory_is_chunked_and_gets_one_vector` | 5, 10 |
| 6 hanging embedder | `recall_answers_within_the_limit_when_the_embedder_hangs`, `starts_serves_and_exits_beside_a_silent_embedder` | 6, 11 |
| 7 `status` | `status_reports_the_queue`, `status_runs_beside_a_writer_and_changes_nothing`, `status_never_migrates_an_older_store`, `the_status_connection_is_read_only`; the container smoke | 9 |
| 8 #634, parity | the #634 comment; the `--ignored` Postgres suite | 14 |

| Vector reader | Guard | Test |
|---|---|---|
| Brute-force vector search (`search/vector.rs`) | `HAS_VECTOR AND (filter)` | `vector_search_never_scores_a_fact_without_a_vector`, `lexical_finds_a_fact_without_a_vector_and_vector_skips_it` |
| HNSW build, rebuild, sidecar and re-rank fetch (`ann.rs`) | `AND HAS_VECTOR` | `hnsw_build_and_snapshot_skip_a_fact_without_a_vector`, `fetch_candidate_embeddings_skips_a_fact_without_a_vector` |
| HNSW incremental notify | none on the deferred insert; on `complete_embedding` when active | `a_filled_vector_joins_the_hnsw_index` |
| Sidecar freshness (`snapshot.rs::read_fingerprint`) | new `vectorless_fact_count` | `filling_a_fact_without_a_vector_changes_the_fingerprint` |
| Dedup, cluster, global (`consolidation/mod.rs`) | `list_active_with_vector`, `count_active_with_vector`; the watermark never passes a fact without a vector (snapshot flag plus apply-time check) | `load_snapshot_skips_a_fact_without_a_vector`, `the_watermark_holds_while_a_fact_without_a_vector_waits`, `consolidation_ignores_a_waiting_fact`, `a_waiting_fact_that_failed_is_compared_as_new_once_embedded` |
| Dream cycle (`list_undreamt_in_period`) | extra condition | `list_undreamt_in_period_skips_a_fact_without_a_vector`, `a_dream_cycle_never_selects_a_waiting_fact` |
| Dormant resonance (`list_dormant`) | `AND HAS_VECTOR` | `list_dormant_skips_a_fact_without_a_vector` |
| Reconstruction (`fact_vectors.rs::promote_space`, backfill) | retains real vectors only, empties the queue; a long text in a batch is chunked on its own, by the declared limit or the recorded answer | `promote_retains_no_fact_without_a_vector_and_empties_the_queue`, `over_length_in_a_batch_is_chunked_alone` |
| Export and restore (`inspect/restore.rs`) | dump writes `[]`; restore accepts 0 or `embed_dim` and re-queues | `a_waiting_fact_round_trips_through_dump_and_restore` |
| Cold archive (`me-archive`) | already skips an empty embedding; a hard delete cascades | existing tests, `an_embed_queue_row_goes_with_its_fact` |
| Hydration (`row_to_fact`: get, lists, FTS, resume, explain) | `X''` → empty `embedding` | `a_fact_without_a_vector_reads_back_empty` |

Regression suites that stay green, changed only where an item says so: `crates/memory-engine-mcp/tests/*`,
`crates/memory-engine-embed/tests/*`, `crates/memory-engine-cli/tests/*`, the schema migration proptests, and the
`ann` suites in `me-backend-sqlite`.

## Verification

Run on the host, unpiped, each to completion; CI is dispatch-only and stands in for none of it. Each command exits
0 and every test summary reports 0 failed.

```bash
# $H: the build machine's heavy-build lock wrapper with CARGO_BUILD_JOBS=16 (outside this repository)
$H cargo build --workspace --locked
cargo fmt --all --check
$H cargo clippy --workspace --all-targets --all-features -- -D warnings
$H cargo build --workspace --all-features
$H cargo test --workspace --all-features
$H cargo check -p memory-engine
$H cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
$H cargo doc --no-deps -p memory-engine
$H cargo doc --no-deps -p memory-engine --all-features
$H cargo deny check
$H cargo +nightly fuzz build                                             # the facade API grows
$H cargo +1.89 build --workspace --tests --examples                      # MSRV: new std API use
$H cargo +1.89 build --workspace --tests --examples --all-features
$H cargo test -p me-backend-postgres --features test-util -- --ignored  # parity proof, item 14
uv run sphinx-build -b html docs docs/_build
```

- Container smoke for acceptance 7, under the same lock: `podman build --memory=32g -t memory-engine:1021 .`; run
  it with a token-hash file, a user, a store volume, the embedding dimension and an embed URL on a closed port;
  `remember` one fact over `/mcp`; `podman exec <container> memory-engine-cli --db <store path> status` prints
  `1 waiting`, `0 failed`, `paused: no` and a last embed error, while the server keeps serving and holding its
  lock. Remove the container and the image afterwards.
- Plugin gate: N/A, `plugins/memory-engine/` is untouched.
- Snapshot changes (item 1) are accepted only after reading their diff.

## Open questions

1. DEFAULTED: a fact without a vector is `X''` plus a queue row, guarded below the port. The zero-compromise
   structure moves the active vector out of `Fact` into its own relation, where "not embedded" is "no row"; it
   amends ADR-0015 §4 in lockstep with knowledge-base, rebuilds `facts`, changes the export formats and touches
   about 45 files. It is reported to the owner as a follow-up to decide on, not filed by this ticket; this plan
   does not change for it.
2. DEFAULTED: `remember` with no embedder configured stores and queues (the outcome: no write fails for lack of the
   embedder); the facts get vectors once an embedder is configured, and `status` shows them waiting meanwhile.
   Two of three drafts kept #1020's refusal instead; to keep it, restore the check in `remember` and its test.
3. DEFAULTED: backoff 1 s doubling to a 60 s cap. An outage of hours costs about one probe a minute; vectors land
   at most a minute after the embedder returns. A policy value, not a latency cap.
4. DEFAULTED: only a request that never reached the server (connect refused, DNS, TLS) makes the queue wait and
   touches no item, since no item can be at fault. A timeout (also a host that drops packets, as an offline
   network peer does: reqwest reports it as a request timeout), a reset or a crash after sending is treated like a
   5xx, per item, so a poison input never holds up the others; the drain's streak sleep bounds how often a dead or
   hanging embedder is probed. Both are the spec's "retried with capped backoff". No connect timeout is added: it
   would be a value from a guess. Accepted risk: an input that crashes the embedder is retried about once a minute
   for as long as it stays queued (the spec forbids an attempt cap); `status` shows its error.
5. DEFAULTED: answers outside the spec's lists (404, 429, other 4xx, 3xx, an unusable 2xx, a vector the store
   refuses) back off only their item and show in `status`; nothing is lost or paused without a rule.
6. DEFAULTED: the pause is probed on the backoff schedule and cleared by the next answered request; it survives a
   restart in `status` until that answer. `status` exits 0 in every state; the alarm is in its output.
7. DEFAULTED: a fact forgotten (#1023) or expired while it waits is still embedded, so no reader needs a "never
   embedded" case. A failed (400/413/422) item stays full-text only; a reconstruction re-embeds it, and if the new
   provider rejects its content too, the reconstruction aborts as on any provider error (a limitation: before this
   PR such a fact could not exist; with llama.cpp and chunking a failed item is not expected). A failed item
   holds the consolidation watermark until it is erased (#1023) or reconstructed. Meanwhile every consolidation
   run compares every fact newer than the held watermark as new: an all-pairs dedup pass, the work of a first
   consolidation, up to `MAX_FACTS_FOR_DEDUP` (50,000) on every run. Bounded, CLI-only (consolidation is off in the
   server) and not expected (no failed item is expected with llama.cpp and chunking); never a missed fact.
8. DEFAULTED: `record_activity`'s promotion keeps embedding inline: it has no live caller since #1020.
9. DEFAULTED: the recall call record gains no `degraded` field; ADR-0020 §9's model-recall record is #1029's.
10. DEFAULTED: nothing records that a fact's vector is pooled from chunks; whether it was can be recomputed by
   counting its content, which is what option A of D1 would do if chosen later.
11. DEFAULTED: schema v16 is this PR's; whichever of #1021 and #1023 merges second renumbers (item 15).
12. Settled by the owner, not reopened (a reviewer asked for a shared limit and priority at the embedder): the
   drain's rule is one input in flight and no other policy. ME does not schedule recall against the drain or KB,
   and does not limit a recall's query embed beyond §10's 500 ms; a call abandoned at 500 ms ends at the
   provider's own timeout. dutiona/infra#77 measured a query beside a one-input drain under load at p95 320.9 ms
   or less.
13. DEFAULTED: the chunk limit is the declared `max_input_tokens` (the deployment declares 2,048), else
   llama-server's own `n_ctx` from its recorded over-length answer. A provider that declares no limit and is not
   llama-server is sent the whole text, and its over-length rejection fails the item: there is nothing to chunk by,
   and a fixed limit would be a guessed value. Providers other than llama.cpp count by the conservative byte bound,
   so they chunk more finely than they need to: a quality cost, never a failure.
14. Reported as a follow-up, not fixed here (a reviewer raised it as HIGH): dedup selects "new" facts by
   `t_created > last_consolidated_at`, so a fact backdated below the watermark, or committed by a concurrent writer
   after the snapshot with an earlier `t_created`, is never compared as new. It is the same for `add_fact` today
   and outside the deployed workflow: consolidation runs only from the CLI, which holds the store lock (no
   concurrent writer), and `remember` never backdates. The fix (eligibility by a monotonic marker such as the fact
   ID) changes consolidation's contract and belongs in its own issue.
