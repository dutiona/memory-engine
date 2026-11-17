# Clean-slate subagent review (3b), round 1

Reviewer: a fresh Claude Opus subagent with no conversation context (read-only).

Not LGTM yet. I found no blocker, one HIGH, four MEDIUM and several LOW findings. The plan is at the right altitude, but the altitude report does not cover the plan revision I reviewed.

Altitude report SHA-256: 458a16292ad99c30c4d11dc2b596d4a2effc0ec2b49bfcc9d559dcd1380f80c3

Plan reviewed: SHA-256 `399d10f1dad4803a7f3b3cfaffa5aab00f4190171cbafdd17a2f5bfe41790b1f`, 615 lines, 54,239 bytes. The plan was rewritten partway through my review: the file is dated 19:07, while the altitude report is dated 19:00. I re-read the whole current file and reviewed that version. The rewrite added the `fetch_candidate_embeddings` guard, `vectorless_fact_count`, `skipped_waiting` and Open question 12. So I am not reporting those three as gaps: the current plan already fixes them.

## Altitude and duplication

- **[MEDIUM] The altitude report covers an older plan.** The report records Plan SHA-256 `012fa70a…`, 598 lines and 52,190 bytes. It should be measured again and bound to the current plan before approval. In my reading its decision to proceed still holds:
  - The only code blocks are signatures, one DDL statement, one output format and the command list. No complete product file is embedded.
  - The two test tables assign tests; they do not restate code.
  - The length comes from a change that crosses eight crates, not from design prose.
  - No override is claimed, and none is needed.
- **[LOW] The rewrite left a contradiction.** Documentation (line 511) still says `DbFingerprint::active_fact_count` "state[s] the new meaning". Item 2 now leaves that field's meaning unchanged and adds `vectorless_fact_count`.

## Findings

**[HIGH] One item that gets no answer stalls the whole queue (item 8's table at line 323, item 10, Open question 4).**
- `Unreachable` becomes `Hold { paused: false }`, which changes no row. `next_embedding` always picks the smallest `(next_attempt_at, fact_id)` (line 306), so the same item comes back first after every sleep.
- An item that itself causes "no answer" is never moved aside. Examples: an input that times out, or one that resets the connection or crashes the embedder.
- Every item behind it waits indefinitely. That breaks acceptance 3 ("every item has its vector") and the intent of acceptance 4 ("never holds up the others").
- Spec decision 8 groups connection errors and timeouts with 5xx: "retried with capped backoff". The plan instead makes them queue-wide.
- A poison input that crashes the shared embedder would also be retried forever, which could restart the embedder about once a minute for ME, KB and recall alike.
- Fix, either of:
  - Only a failure where no request reached the server (reqwest's `is_connect()`, DNS) holds the queue. A timeout or reset after the request was sent becomes a per-item `Retry`.
  - At minimum, `Hold { paused: false }` sets `next_attempt_at = now` so the item moves behind the others, without counting an attempt.
- Add a test: one item whose request is reset while the others embed.

**[MEDIUM] CLI read commands fail on a store whose facts all wait.**
- `open_engine` (`crates/memory-engine-cli/src/db.rs:135`) calls `peek_embed_dim_from_db`. That call stops with "nothing has been embedded — add a fact first" (`db.rs:93`) when the store has no identity.
- Nothing records an identity before the first vector: `verify_embedding_identity` does not write (`crates/memory-engine/src/engine/mod.rs:501`).
- So on a new deployment during an outage, or a server with no embedder (Open question 2), `stats`, `inspect`, `explain`, `query`, `dump` and `export` all fail with a misleading message. `export` is the backup path (`commands/export.rs:40`). `status` is not affected.
- Fix: at least correct the message and document the state; better, let read-only commands accept a known dimension.

**[MEDIUM] A failed item blocks reconstruction (Open question 7, item 3's reconstruction row).**
- Open question 7 says "a reconstruction re-embeds it". But backfill embeds every fact (`crates/me-backend-sqlite/src/store/fact_vectors.rs:33–60`) through `embed_batch` (`crates/memory-engine/src/engine/reconstruct.rs:268`), and any provider error aborts it.
- Item 5 only falls back to one text at a time on the over-length answer.
- A fact whose content the embedder rejects with a 400, 413 or 422 could not exist before this PR. Now it can, and it stops every reconstruction under a provider that rejects it too.
- Fix: either state this as a limitation, or let promote keep failed items as `X''` with their queue rows.

**[MEDIUM] Truncation only works for llama.cpp's over-length answer (item 5).**
- Only the `exceed_context_size_error` body triggers a cut. `KNOWN_PROVIDERS` (`crates/memory-engine-mcp/src/main.rs:162`) also lists `tei` and `openai`.
- On those providers an over-length input gets a different 400 or 413, becomes `InputRejected`, and fails for good. Acceptance 5 says it must be "never failed".
- Fix, either of:
  - When the provider's `served.max_input_tokens` is known, retry once at `max_input_tokens − 2` bytes before failing.
  - Write "llama.cpp only" into the docs and the PR.

**[LOW] Reinforcing a waiting fact is not covered.**
- `insert_or_reinforce` keeps a waiting row's `X''` and throws away the caller's vector (`crates/me-backend-sqlite/src/store/facts.rs:262–280`).
- `insert_or_reinforce_fact` then adds that vector to the HNSW index (`sqlite/graph.rs:185`), which the row does not hold. The caller is `bootstrap_memory_directory`.
- Fix: fill the vector and delete the queue row when the reinforced row has none.

**[LOW] `IndexInconsistent` after `complete_embedding` commits must count as `Embedded`.** Item 8's table maps any refused vector to `Retry`, but this error comes after a successful commit, so it must not be retried (`insert_fact_atomic`'s precedent, `sqlite/graph.rs:765`).

**[LOW] `serve` should await the aborted drain task.** `abort()` alone leaves the task's `Arc<MemoryEngine>` alive until the runtime drops it. The fence never releases (`crates/memory-engine-mcp/src/http.rs:75`), so no write can land after the flush; this is about clean teardown only.

**[LOW] Several red commands do not run the tests they name.**
- Item 2: the `without_a_vector` filter skips `lexical_finds_it_and_vector_skips_it` and `filling_a_vector_changes_the_fingerprint`.
- Item 3: it skips the watermark and promote tests, and the facade's `waiting_fact` filter skips the dump/restore round-trip test.
- Item 5: only 2 of "the five `over_length` tests" (line 529) contain `over_length`.

**[LOW] Some file lists and claims don't match the code.**
- Item 3's `skipped_waiting` also needs `crates/me-types/src/types/consolidation.rs` (lines 23 and 44) and `crates/me-consolidate/src/pipeline.rs` (lines 144 and 205). Neither is listed.
- Item 7 says `prepare_new_fact` is "shared by every add path" (line 268). The batch path builds its own `NewFact` and resolves scopes inside its savepoint (`crates/me-ingest/src/lib.rs:423`).
- `insert_fact_deferred` should reuse `FactStore::insert`'s 17-column INSERT rather than copy it.

**[LOW] "Fails loudly" is only half true (decision 1).**
- It holds for readers that decode the blob. Rust readers of a hydrated `Fact.embedding` would silently score 0, because `cosine_similarity` zips the two vectors (`crates/me-types/src/math.rs:26`). Examples: `dedup.rs:119`, `cluster.rs:195`, `default_impl.rs:169`, `dormant.rs:62`.
- The guards cover today's lists. A `debug_assert_eq!` on the two lengths would protect future readers.
- Promoting a waiting fact chosen from a query result aborts the whole dream cycle. That is a refusal, not a skip.

**[LOW] The public contract change is not flagged as breaking.**
- `Fact.embedding` may now be empty, and a `Reranker` sees that through `SearchResult`.
- `DbFingerprint` and `Snapshot` gain public fields.
- `remember` stops refusing when no embedder is configured.
- The repository's precedent for such changes is `feat(...)!`.

**[LOW] Host-specific paths in a plan that item 0 commits.** Item 0 names a skill script under the home directory, and the intro and Verification name a lock file under a per-user runtime directory. This contradicts the plan's own Constraint (line 44).

**[LOW] `status` prints the embedder's raw error body (up to 1,000 bytes) to the terminal.** Strip control characters before printing.

**[LOW] `docs/reference/api.md` is not updated** for the new facade methods.

**[LOW] Optional: enforce the no-wait rule with clippy.** Adding `MemoryEngine::add_fact` to `crates/memory-engine-mcp/clippy.toml`'s disallowed methods would make "remember never waits on the embedder" checked by clippy.

## Choices I accept

- **Open question 12** (one input in flight, no other limit, an abandoned query embed ends at the provider's timeout): the owner settled this. I don't reopen it.
- **Open question 1**: `X''` instead of moving vectors into their own relation is justified by ADR-0015's lockstep with KB and about 45 files. But no item actually files the issue it says is needed.
- **Open question 7**: a failed item not holding the consolidation watermark is an acceptable cost, since consolidation is off in the server.

## Checked against the code, no finding

- **Representation:** `facts.embedding` is `NOT NULL` (`ddl.rs:28`). `deserialize_embedding` is strict. Brute-force search has a single core (`sqlite/search_index.rs:101`).
- **HNSW:** `ann.rs` line 196 (`build_inner`), 320 (`to_snapshot`) and 624 (`fetch_candidate_embeddings`).
- **Sidecar:** `vectorless_fact_count` blocks fill/expire collisions.
- **Lost wake-ups:** `add_fact_deferred_needs_no_embedder` asserts `embed_work_arrived` resolves after the insert, which only passes with `notify_one`'s stored permit. Naming `notify_one` in the plan would still help.
- **Spin:** no loop branch can spin.
- **Storage plumbing:** foreign keys are on for every connection, so the cascade works. Only SQLite implements `FactGraph`, so Postgres is unaffected.
- **Container smoke:** the image ships `memory-engine-cli` (`Containerfile:26`).
- **Test names:** every existing test name the plan cites exists.

**Structure:** the Documentation, Testing and Verification sections are present. The plugin gate is N/A with a reason. Items 1 to 11 can run in the stated order.

## Resolution

Round 1, applied to the plan (Codex round-1 findings resolved in the same revision, recorded in the review state):

- [HIGH] One unanswered item stalls the queue → item 4 now makes only a request that never reached the server
  (`is_connect()`) `Unreachable`; a timeout, reset or crash after sending is `Failed`, backed off per item. Item 10
  adds `a_poison_item_that_times_out_never_holds_up_the_others`; Open question 4 rewritten.
- [MEDIUM] Altitude report bound to an older plan → the report is regenerated for the revised plan before round 2.
- [MEDIUM] CLI read commands on a store whose facts all wait → item 9 corrects `peek_embed_dim_from_db`'s message
  (names the state, points to `status`) with a test. Accepting a known dimension for read commands is not taken:
  the state exists only before a fresh store's first vector.
- [MEDIUM] A failed item blocks reconstruction → stated as a limitation in item 3 and Open question 7.
- [MEDIUM] Truncation only for llama.cpp → item 5 adds the served `max_input_tokens` byte cap for other servers'
  400/413/422, with `over_length_without_counts_is_cut_to_max_input_tokens`.
- [LOW] Fingerprint doc contradiction → Documentation names `vectorless_fact_count`.
- [LOW] Reinforcing a waiting fact → item 7: `insert_or_reinforce` fills the vector and deletes the queue row;
  test `a_deferred_reinforce_fills_its_vector`.
- [LOW] `IndexInconsistent` after commit → item 8 reports `Embedded`, never retries; test added.
- [LOW] Await the aborted drain → item 10 aborts and awaits the `JoinHandle` before the flush.
- [LOW] Red filters → test names renamed so each red command runs them (`without_a_vector`, `waiting_fact`,
  `over_length_*`).
- [LOW] File lists and claims → item 3 lists `me-types` consolidation and `me-consolidate` pipeline; item 7 says
  `prepare_new_fact` serves the three single-fact paths and `insert_without_vector` shares `insert`'s row insert.
- [LOW] "Fails loudly" half true → the Representation paragraph now says so and why the SQL guards cover Rust
  consumers.
- [LOW] Breaking change → items 2 and 10 commit as `feat(...)!`; the PR title is `feat(mcp)!:`.
- [LOW] Host-specific paths → removed from the plan (`$H` and the archiver are named without paths).
- [LOW] `status` prints raw error bodies → item 9 escapes control characters.
- [LOW] `docs/reference/api.md` → added to Documentation.
- [LOW, optional] clippy guard → taken: `MemoryEngine::add_fact` is disallowed in the MCP crate.
- Accepted choices (Open questions 1, 7, 12) → unchanged; Open question 1 now says the structural follow-up is
  reported to the owner, not filed here.

# Round 2 (delta review, same reviewer)

Altitude report SHA-256: 26f5f401232430210f6005e8676b0c6407a7c8a27249d3454f8367e06fd5880e

No blocker or HIGH against the revision reviewed (`0fbe6246…`, which already carried the Codex round-2 fixes);
three MEDIUM, three LOW:

- [MEDIUM] The altitude report is bound to `53481e13…`, not the current plan; its round-1 provenance names
  `012fa70a…` (598 lines) where this reviewer's round 1 read `399d10f1…` (615 lines). Proceed still holds.
- [MEDIUM] A stalled connect is `Failed`, not `Unreachable`: the provider sets only `.timeout()`, and reqwest
  0.12's blocking client reports a timeout during connect as a request timeout, so `is_connect()` is false. The
  Outcome's "No answer … makes the queue wait" contradicts item 4.
- [MEDIUM] Item 5's M − 1 byte cut is too large for TEI models that add two special tokens; use M − 2 and test
  with two specials. `embed_batch`'s per-text fallback should cover the 413 path too. OpenAI's over-length 400 now
  fails its item: list it as an Open question.
- [LOW] The vector-reader table names six tests the items renamed.
- [LOW] Item 3's `waiting_fact` red filter does not run the new reconstruction test.
- [LOW] A poison input that crashes the embedder is retried about once a minute forever: name it as an accepted
  risk (the spec forbids an attempt cap).
- Checked and holding: the Unreachable/Failed split fixes head-of-line blocking; the 413-only fallback is right;
  the current `insert_or_reinforce` resolution (leave the waiting row queued, notify only with a vector) is safe
  (the earlier "fill it" version would have been HIGH: it bypassed the #614 identity check in bootstrap);
  `IndexInconsistent`, the awaited drain abort, the clippy guard, the CLI message, control-character escaping,
  the watermark clear at promote, breaking markers, removed host paths, `api.md`.

## Resolution

Round 2:

- [MEDIUM] altitude binding → regenerated for the round-3 plan; provenance now names both round-1 digests.
- [MEDIUM] stalled connect → item 4's enum doc and Open question 4 now say a timeout, even during connect, is
  `Failed` (per item); the Outcome says "a refused connection makes the queue wait". No connect timeout is added
  (it would be a guessed value).
- [MEDIUM] TEI cut → M − 2 bytes, tested with a mock that counts two specials; `embed_batch` falls back per text
  on either length answer; Open question 13 names the OpenAI-compatible 400 that still fails its item.
- [LOW] table names → corrected. [LOW] red filter → the test is renamed
  `a_waiting_fact_that_failed_is_compared_as_new_after_reconstruction`. [LOW] crashing poison input → accepted
  risk in Open question 4.

# Round 3 (delta review, same reviewer)

Altitude report SHA-256: 62ab1e2947fd6bb69244d2bc396c9ef3ea282d8c1f7a3e3bfc121425a5548ef9

LGTM against `aeafa241…`: every round-2 resolution and every Codex round-2 resolution holds against the code; the
altitude report is bound to that plan and its figures match (670 lines, 60,516 bytes, 19 tasks, 7 blocks, 75 lines,
4,843 bytes). One [LOW]: the planned mcp-server.md "Embedding queue" bullet says "no connection → the queue waits",
wider than the rule now that a host that drops packets is `Failed` per item.

## Resolution

Round 3: the [LOW] wording is fixed ("no connection made → the queue waits, no item changes (a refused connection,
DNS or TLS failure)"). The same revision resolves Codex's round-3 findings (items 3 and 5, Open questions 7 and
13), which round 4 reviews.

# Round 4 (delta review, same reviewer)

Altitude report SHA-256: 82f101f644b34d432b101c2285893b078cee99b56c246d612bf6aed87d059cfd

LGTM against `571a7b19…`: Codex's three round-3 resolutions hold (the watermark never moves past a vectorless
fact by either check; forgetting a failed fact releases the hold; no status is a length signal; the pre-send cap
meets acceptance 5 for any provider that declares `max_input_tokens`). Three optional [LOW]:
1. The reactive llama.cpp cut is gated on the free-form provider label; apply it whenever the self-identifying
   counted body arrives, keep the label only to choose the pre-send cap.
2. The stated reason for the apply-time check is wrong: the watermark is the snapshot's time, so a fact inserted
   after the load is already above it; the check only matters for a backdated deferred insert. Reword the rule as
   "never advances past an existing vectorless fact".
3. Open question 7 understates the held watermark's cost: a full all-pairs dedup up to `MAX_FACTS_FOR_DEDUP`
   (50,000) per run; still bounded, rare and CLI-only.

## Resolution

Round 4: all three [LOW] taken (item 5: the counted cut applies whatever the label; item 3: the rule reworded,
the apply-time check kept as insurance for a backdated deferred insert; Open question 7 states the cost). The same
revision answers Codex's round 4: the `max(M − 2, 1)` boundary with a first-character floor (taken); the
backdated or concurrent-writer watermark gap, pre-existing for `add_fact` and outside the deployed workflow
(reported as a follow-up, Open question 14); the undeclared-limit provider (arbitrated, Open question 13).

# Round 5 (delta review, same reviewer)

Altitude report SHA-256: 9a134fa030d4db2f26aa93f9396a237a75407d726159cb37f2ed1a6303db0000

LGTM against `be716ed9…`: all three round-4 LOWs fixed, nothing new at any severity. Noted: strictly, "never failed
for length" cannot hold at a declared limit of 1 or 2 when the first character takes several bytes, but no real
model has such a limit. Open question 13's reading of the acceptance matches this reviewer's round-1 reading and
is put to the owner; Open question 14 is properly a follow-up.

## Resolution

Round 5: the tiny-limit note (also Codex's round-5 MEDIUM) is taken: item 5 states that at M = 1 or 2 no prefix is
guaranteed to fit, the first character is sent, and a rejection fails the item; its test now asserts that.

# Round 6 (confirmation, same reviewer)

Altitude report SHA-256: cc87729c3b7120f9a9aeadfa327ecad565e8505a43603c4fb22930ec5df3bc60

LGTM against `89bdce7c…`: the tiny-limit change states the case as it behaves and its test pins it. Optional
wording: the guarantee holds only when M − 2 covers the first character (M ≥ 6 for a 4-byte character), so "above
that" is a touch wide for limits of 3 to 5.

## Resolution

Round 6: the wording is made exact (also Codex's round-6 MEDIUM): "when the first character alone is longer than
the cap (only at a declared M below 6)", and the test covers M = 3.

# Round 7 (final confirmation, same reviewer)

Altitude report SHA-256: 3e4141ad1e2d9f0a2adef6d689d0d44418b69e0a4fbbf17527c20d38e71b2b1a

LGTM against `248b4a41…`: the bound is right (a cap of `max(M − 2, 1)` and at most 4 bytes per character means
the first character can exceed the cap only below M = 6), and the tiny-limit test checks M = 3 at its edge.
Optional, "no further round needed": at M = 1 or 2 even a 1-byte first character has no guarantee of fitting, so
"When M ≤ 2, or the first character alone is longer than M − 2" would make the clause exact.

## Resolution

Round 7: no change. The optional precision point is accepted as written: the plan's "only at a declared M below
6, which no real model has" bound already covers M ≤ 2, the rule's behaviour there (send the first character, a
rejection fails the item) is the same, and the test covers M = 1 and 2.
