# Codex plan review (headless, config-default model gpt-6.1-terra), rounds 1-9

Each round's final message, verbatim, from the reviewer's thread `01a1e978-bf51-76e3-91a5-abada28e523f`. Round 9 reviewed the final plan revision. Link targets are made repository-relative (the review tree's absolute prefix removed); nothing else is changed.

---

## Round 1

I’ll read the review instructions and inspect the specified evidence, Michaël.
The plan’s main risks are in the existing search and event-log paths. I’m checking those against the ADRs before finalizing the findings.
Michaël, **the plan needs revision before approval**. Confidence is high in the first two findings.

Altitude report SHA-256: 0fe1139efd9071601dd3249d19834efc0c2004e0807b745b9622787518021d46

The altitude report’s assessment is reasonable for the core issue: replacing the surface, enforcing the lock, and moving pruning require changes across the named crates. The report also correctly identifies D11 and D12 as separable. Their inclusion needs an explicit decision; it should not be treated as an automatic override of the scope fence. The plan contains Documentation, Testing, and Verification sections.

- **[BLOCKER] D1 / item 2: HNSW does not filter before candidate selection.** The plan says passing `FactFilter.scope_ids` into HNSW satisfies ADR-0021’s pre-top-k rule. [ann.rs](crates/me-backend-sqlite/src/search/ann.rs:418) first selects global nearest neighbors and only then filters them by scope at line 432. Widening and brute-force fallback can make the proposed 50-hidden-fact test pass without enforcing the contract. Use a scope-constrained candidate strategy, or route scoped searches through the already filtered brute-force path, and test the actual selection boundary.

- **[BLOCKER] D11 / item 9: the proposed call event cannot replay a remembered fact.** Its payload contains only call, tool, machine, and fact IDs, and it is appended *after* the handler. Existing `add_fact` inserts the fact separately ([me-ingest](crates/me-ingest/src/lib.rs:216)). Replaying only the log therefore cannot reconstruct the content or filing, contrary to ADR-0021 §12; a crash or record-write failure also leaves a successful fact without its capture context. The plan explicitly returns success after record failure. Specify a replayable, durable write with the required atomicity and a replay/failure test, or hold deployment until the responsible follow-up supplies it.

- **[HIGH] D5: RNG failure violates the response contract.** The proposed JSON-RPC internal error has no call line, while ADR-0020 §8 requires failures of the model tools to be tool results headed by a call ID. Resolve the inability to allocate an ID before exposing the tool, or specify another contract-compliant failure strategy and test it.

- **[HIGH] D12 / item 10: punctuation-only queries can become unfiltered store reads.** The plan drops the FTS channel when `fts_terms` yields no terms. With no embedder, both text and embedding are absent; [MemoryQuery](crates/me-types/src/types/memory_query.rs:25) defines that as a non-search query returning active facts by importance. `recall("?!")` can consequently return unrelated memories. Explicitly return an empty result or reject such a query, and cover the no-embedder case.

- **[MEDIUM] D7 / item 5: the lock acquisition order does not guarantee the stated refusal.** `open_engine_writable` currently peeks at the database before its writable helper is called ([db.rs](crates/memory-engine-cli/src/db.rs:154)). Acquiring only inside the latter lets a missing, malformed, or concurrently changing database fail before the held-lock refusal. Specify acquisition before the peek and retain the same guard through engine close; test this ordering.

- **[MEDIUM] Documentation item 12: the proposed dream-cycle replacement is misleading.** The plan tells readers to run the dream cycle with `memory-engine-cli consolidate`; those are distinct operations. Document the actual available maintenance operation or state that the dream cycle is unavailable through the server.

The raw FTS5 `MATCH` diagnosis is supported by [fts.rs](crates/me-backend-sqlite/src/search/fts.rs:67). `getrandom` 0.3 is in the lockfile, and the installed Rust source marks `File::try_lock` stable in 1.89. The CLI has the additional writable creation and restore paths that D7 identifies. I did not run tests, builds, gates, or fuzzing, as instructed; the clippy guard and revised fuzz target therefore remain implementation-time verification items.

REVIEW COMPLETE

---

## Round 2

Michaël, the revision needs two changes before approval. Confidence is high.

The refreshed altitude report matches the new plan digest. Its added live-export work has a concrete purpose; D11 and D12 remain separable decisions for you.

Altitude report SHA-256: 8b02f1364778780a93be48f540870450b4f68471a43341d226cc8b7948d6dade

- **[HIGH] D11 / item 10: the new ordering cannot deliver its complete-log guarantee on failure.** If appending a call record fails, the plan returns an error with a call ID, but that call has no record. If the record succeeds and the subsequent fact write fails, the already-written record has neither the promised error nor an explicit outcome. This contradicts “one event per call” and the claim that no answer exists without its record. Specify a durable failure strategy and how a failed post-record write is represented; test both paths, including a failure after the record was appended.

- **[MEDIUM] D12 / item 11: the new query cap still permits an FTS expression too deep for SQLite.** A permitted query containing thousands of short words becomes thousands of `OR` terms. SQLite can reject that expression for excessive depth, and the existing [FTS handler](crates/me-backend-sqlite/src/search/fts.rs:87) converts the error to an empty result. A natural-language recall can therefore silently miss matching facts even after the proposed fix. Bound or restructure the generated expression and test a query exceeding SQLite’s expression-depth limit.

I reviewed the delta and its cited code read-only. I did not run tests, builds, gates, or fuzzing.

REVIEW COMPLETE

---

## Round 3

Michaël, the delta has two new issues. Confidence is high.

The refreshed altitude report matches the plan digest and acknowledges the added limits and failure semantics. Its overall disposition remains reasonable, subject to the findings below.

Altitude report SHA-256: bf837855a816354f561857548b69a55ad4b4965697d436d84c6ae6b281034ffe

- **[HIGH] D10 / item 9: the new project-length limit conflicts with the binding error contract.** ADR-0020 §8 requires an unknown `project` value to produce an error that names that value. The new 4,096-byte limit explicitly refuses longer values without echoing them. Resolve this with a contract-compatible response or an approved ADR change; the current plan cannot satisfy both.

- **[MEDIUM] D12 / item 11: truncating to the first 64 terms silently discards the rest of a valid query.** With no embedder, a query whose only matching word is term 65 returns no results, although the query is accepted and reported as full-text search. The new long-query test places the matching word first, so it misses this case. Reject queries exceeding the term limit with a clear error, or search all terms in bounded batches; add a test whose only match occurs after term 64.

This was a read-only delta review; I did not run tests, builds, gates, or fuzzing.

REVIEW COMPLETE

---

## Round 4

LGTM for the delta, Michaël. Confidence is high.

The refreshed altitude report matches the plan digest. The changes address the two round-3 findings: unknown project values are named in full, and queries over the term limit are refused rather than silently truncated.

Altitude report SHA-256: 3ce919be6fac5f4052d6a6881c27d1492563d10e231b5b9cd4dbf20c34d25a03

This was a read-only review; I did not run tests, builds, gates, or fuzzing.

REVIEW COMPLETE

---

## Round 5

LGTM for the delta, Michaël. Confidence is high.

The refreshed altitude report matches the plan digest. The changes clarify record truncation and the erasure handoff, and tighten the query and test setup.

Altitude report SHA-256: bce9d20ee5dcd0bdceaa98c61fd931e5ec1094c99834eb0afe1f06307d2ea2f4

This was a read-only review; I did not run tests, builds, gates, or fuzzing.

REVIEW COMPLETE

---

## Round 6

Michaël, the delta needs one correction. Confidence is high.

The refreshed altitude report matches the plan digest; its assessment of the plan’s overall size remains reasonable.

Altitude report SHA-256: 68ee8b89da133dc5fd87b627ef8dc7a6bb285f60df80eea5971c253c0f3b385e

- **[MEDIUM] D12 / item 11: Unicode alphanumeric runs are not necessarily single `unicode61` tokens.** For example, U+0345 is Unicode Alphabetic, but a read-only in-memory SQLite probe tokenized `alpha\u0345beta` as two terms, `alpha` and `beta`. A long sequence joined by that character can therefore pass the 256-run limit as one quoted run while becoming a large FTS phrase—the same cost problem the delta aims to close. Count using the actual SQLite tokenizer, or use a demonstrably compatible bounded tokenization rule; add a regression case for U+0345.

I ran only the small in-memory tokenizer probe; I did not run project tests, builds, gates, benchmarks, or fuzzing.

REVIEW COMPLETE

---

## Round 7

LGTM for the delta, Michaël. Confidence is high.

The refreshed altitude report matches the plan digest. The 4,096-byte limit no longer relies on Rust and SQLite classifying every character alike, and the added cases cover the previously identified U+0345 scenario.

Altitude report SHA-256: 31793d810bdbb85f996dc49c3f3be5fd6dea9fa3ce6e00396e0b745e4507852f

This was a read-only review; I did not run tests, builds, gates, benchmarks, or fuzzing.

REVIEW COMPLETE

---

## Round 8

LGTM for the delta, Michaël. Confidence is high.

The refreshed altitude report matches the plan digest. The revision states the measured residual cost and adds the query limit to the model-facing description.

Altitude report SHA-256: b750a3ab4021eabd587f19c89f3f57408e67115f227825f101bfcc6b237f4038

This was a read-only review; I did not run tests, builds, gates, benchmarks, or fuzzing.

REVIEW COMPLETE

---

## Round 9

LGTM for the one-line delta, Michaël. Confidence is high.

The refreshed altitude report matches the plan digest. The wording preserves the instruction to commit the plan and review artifacts.

Altitude report SHA-256: 91d6ae10c23f1303faa5959c49aa691043368aa0ce10331b8a293fe8e7d0fcce

This was a read-only review; I did not run tests, builds, gates, or fuzzing.

REVIEW COMPLETE

---

## Resolution

- R1 [BLOCKER] HNSW post-filter → not a defect: the HNSW channel drops out-of-set candidates inside the channel, widens, then falls back to the filtered brute-force path; that is ADR-0021 §5 step 3's "vector search with its widening". D1 now describes it exactly (the server builds without `ann`).
- R1 [BLOCKER] call event cannot replay a fact → content replay is not required (ADR-0021 §12: re-filing changes the filing only, never content); the durability gap was fixed by D11's ordering (record first for a valid remember, `source_event_id` link, failed record write fails the call).
- R1 [HIGH] RNG failure → startup probe (D5); post-probe residual accepted as unreachable.
- R1 [HIGH] punctuation-only query → empty `full-text only` / `vector only`, never a non-search listing (D12, test).
- R1 [MEDIUM] lock before the peek → D7, tests. R1 [MEDIUM] dream-cycle wording → `consolidate --backend dream-cycle`.
- R2 [HIGH] failure semantics → D11 states the exact guarantee and defines the remember outcome from the fact side; test for a fact write failing after its record.
- R2 [MEDIUM] FTS expression too deep → did not reproduce (100,000 OR terms parse on SQLite 3.46.1); the cost concern it implied was handled in R3-R7.
- R3 [HIGH] project refusal vs "names the value" → refusal removed; unknown values named in full; only the record's `error` is cut to 512 bytes.
- R3 [MEDIUM] silent 64-term truncation → removed; R6 [MEDIUM] run class wider than `unicode61` → cost bounded by a 4,096-byte query cap instead (D10, D12), with measurements.
- R4, R5, R7, R8, R9: LGTM.
