# Codex plan review (headless, persistent thread; config-default model gpt-6.1-terra)

## Round 1

Michaël, the plan needs revision before implementation. Confidence is high in the first three findings; the fourth is a missing guard whose reachability depends on index state.

The altitude report matches the plan digest. Its level of detail is justified by the cross-crate representation and queue changes; no override is claimed. Documentation, Testing and Verification sections are present.

Altitude report SHA-256: 458a16292ad99c30c4d11dc2b596d4a2effc0ec2b49bfcc9d559dcd1380f80c3

- **[HIGH] Item 3: the consolidation watermark can pass a fact omitted from its snapshot.** The plan checks for waiting rows when applying the plan, but the snapshot is loaded earlier ([plan item 3](.review/plan.md:137); [consolidation/mod.rs](crates/me-backend-sqlite/src/consolidation/mod.rs:90)). If the drain embeds and removes a pending row between those steps, the apply sees no waiting row and advances the watermark. Dedup subsequently selects new facts by `t_created > watermark` ([dedup.rs](crates/me-consolidate/src/pipeline/dedup.rs:83)), so that fact can be missed permanently. The same outcome is possible when a failed item is excluded from `has_waiting` and later recovered by reconstruction. Bind watermark advancement to what the snapshot actually covered, and test both transitions.

- **[HIGH] Items 2 and 8: the proposed sidecar fingerprint can collide after a pending fact is embedded.** The existing fingerprint uses maximum fact ID and active fact count ([snapshot.rs](crates/me-backend-sqlite/src/snapshot.rs:163)); item 2 changes only the count to active facts with vectors. Starting from a sidecar containing active fact A and pending fact B, expire A and embed B before restart. The maximum ID and vector-bearing active count are unchanged, so the old sidecar can be accepted while omitting B. Use a durable vector-change generation or another fingerprint that distinguishes these states, and test this combined transition.

- **[HIGH] Items 6 and 10: one drain request in flight does not establish the required shared limit or priority for interactive queries.** The drain is serialized only against itself, while recall starts an independent blocking request ([recall.rs](crates/memory-engine-mcp/src/tools/recall.rs:106)); timed-out calls continue until the provider timeout under item 6. Thus recall, abandoned recall requests, the drain and KB can overlap at the embedder. The planned mock responds to queries immediately regardless of document load, so it cannot demonstrate that a backlog yields or avoids degraded recall. Define and test an enforceable scheduling contract at the shared resource, including timed-out calls and KB traffic, or obtain an explicit change to the stated shared-limit and yielding requirements.

- **[MEDIUM] Item 2 and the guard table omit an HNSW vector decoder.** `fetch_candidate_embeddings` selects and strictly decodes `facts.embedding` without a length predicate ([ann.rs](crates/me-backend-sqlite/src/search/ann.rs:622)). If an index contains a candidate whose current row is vectorless, this path returns `EmbeddingDimension` instead of excluding it. The plan guards build and snapshot paths but does not name this reader ([plan guard table](.review/plan.md:525)). Add the predicate and a focused candidate-fetch test; this also makes the claimed invariant independent of index freshness.


## Round 2

Michaël, the delta resolves the HNSW candidate-reader finding and the reported fill-plus-expire fingerprint collision. The watermark fix still leaves one recovery path incomplete, and the delta introduces two other gaps. Confidence is high.

The regenerated altitude report matches the revised plan digest. Its increased detail is justified by the round-one corrections; no override is claimed.

Altitude report SHA-256: 26f5f401232430210f6005e8676b0c6407a7c8a27249d3454f8367e06fd5880e

- **[HIGH] Item 7: filling a queued fact during bootstrap can leave its vector unstamped and absent from HNSW.** The new `insert_or_reinforce` behavior fills a vectorless match and removes its queue row ([plan](.review/plan.md:287)). But the bootstrap batch calls that method directly, reports the match as reinforced, stamps identity only when `any_created` is true, and does not notify HNSW ([graph.rs](crates/me-backend-sqlite/src/sqlite/graph.rs:1075), [graph.rs](crates/me-backend-sqlite/src/sqlite/graph.rs:1123)). If every bootstrap fact matches a waiting fact, vectors can be committed without recording their identity; a later provider can then establish a different identity. The live HNSW index also misses the filled facts. Track vector fills separately from new rows, stamp or check identity atomically, and update the index; test the all-reinforced case.

- **[HIGH] Item 3: the watermark correction still loses a failed fact recovered by reconstruction.** The delta explicitly excludes failed queue rows from `skipped_waiting` ([plan](.review/plan.md:148)), so consolidation can advance past the fact’s creation time. If reconstruction later embeds it, dedup selects new facts by creation time and never compares that fact as new against older facts ([dedup.rs](crates/me-consolidate/src/pipeline/dedup.rs:83)). The plan acknowledges this loss as a default, but it is not an owner-approved disposition. Preserve eligibility when a failed fact becomes embedded, and test failed → consolidation → reconstruction → dedup.

- **[MEDIUM] Item 5: the new fallback treats unrelated input rejections as over-length.** For any 400, 413 or 422, the rule retries a long-enough input truncated even when the response does not identify its length as the cause ([plan](.review/plan.md:220)). For example, a server could reject prohibited content near the end with 422 and accept its truncated prefix; the queue would mark the whole stored fact embedded rather than record the specified item failure. Restrict truncation to a recognized length rejection or another reliable length signal, and test a non-length rejection on an input longer than `M − 1` bytes.


## Round 3

Michaël, the delta resolves the bootstrap identity and index issue. The watermark correction remains incomplete in two cases, and the new truncation default narrows an acceptance criterion. Confidence is high.

The regenerated altitude report matches the revised plan digest. Its added detail is justified by the review corrections; no override is claimed.

Altitude report SHA-256: 62ab1e2947fd6bb69244d2bc396c9ef3ea282d8c1f7a3e3bfc121425a5548ef9

- **[HIGH] Item 3: an in-flight consolidation can overwrite reconstruction’s watermark reset.** A consolidation may snapshot a failed fact, for which `skipped_waiting` is false, then compute outside the write lock. Reconstruction can embed the fact and delete `last_consolidated_at`, after which the older consolidation plan applies and advances it again. The fact remains omitted from that plan and is older than the restored watermark. Production consolidation deliberately separates load, compute and apply ([pipeline.rs](crates/me-consolidate/src/pipeline.rs:215)); the proposed reset is in [plan item 3](.review/plan.md:149). Fence the reset against stale consolidation plans and test that interleaving.

- **[HIGH] Item 3: restore is another path by which a failed fact receives a vector.** A dump can contain a failed vectorless fact and a watermark advanced past it. Restore imports the watermark from config ([restore.rs](crates/memory-engine/src/inspect/restore.rs:587)) and the plan requeues every vectorless fact ([plan item 3](.review/plan.md:163)). The drain can then embed it without reconstruction clearing the watermark, contrary to the delta’s “only path” assumption. Clear or otherwise repair the watermark when restoring such facts, and test failed → consolidation → dump/restore → drain → dedup.

- **[HIGH] Item 5: the new default explicitly fails some over-length inputs, contrary to acceptance.** An OpenAI-compatible server returning an unrecognized over-length 400 fails the item ([plan](.review/plan.md:668)); the acceptance criterion requires an over-length input to be embedded truncated, with a conservative cap when the recorded response is unavailable. Avoiding misclassification of unrelated 400s is necessary, but the default does not resolve both requirements. Specify a reliable length signal or conservative strategy, or obtain Michaël’s explicit approval to narrow acceptance.


## Round 4

Michaël, the revised watermark rule still has a gap for backdated facts, and the truncation fallback remains incomplete when the provider does not declare a limit. Confidence is high.

The regenerated altitude report matches the revised plan digest. Its added detail is justified by the review corrections; no override is claimed.

Altitude report SHA-256: 82f101f644b34d432b101c2285893b078cee99b56c246d612bf6aed87d059cfd

- **[HIGH] Item 3: holding the watermark cannot make an already-backdated fact new.** `add_fact_deferred` shares request preparation with `add_fact` ([plan](.review/plan.md:290)), which accepts an explicit `t_created` ([me-ingest/src/lib.rs](crates/me-ingest/src/lib.rs:171)). If that timestamp precedes an existing consolidation watermark, holding the watermark while the fact is vectorless leaves it behind the watermark after embedding. Dedup selects facts strictly newer than the watermark ([dedup.rs](crates/me-consolidate/src/pipeline/dedup.rs:83)), so it misses the fact. The same gap can occur when an insertion starts before a snapshot but commits and is embedded before apply. Track embedding eligibility independently of creation time or safely move the watermark back; test both cases.

- **[HIGH] Item 5: the conservative fallback is absent for a supported provider configuration.** `HttpEmbeddingProvider::new` defaults `served` to `None` ([http.rs](crates/memory-engine-embed/src/http.rs:159)); the revised plan sends an uncapped input in that case and fails its over-length rejection ([plan](.review/plan.md:674)). That still conflicts with the acceptance criterion requiring a conservative cap when the recorded response is unavailable. Define a fallback for an undeclared limit or obtain an explicit narrowing of acceptance.

- **[MEDIUM] Item 5: `M − 2` is invalid for an accepted artifact limit.** `ServedArtifact::from_parts` accepts `max_input_tokens = 1` ([cognitive.rs](crates/me-types/src/types/cognitive.rs:329)), while the new cap subtracts two ([plan](.review/plan.md:230)). This underflows for `M = 1`, and `M = 2` yields an empty prefix. Define and test the boundary behavior instead of assuming `M > 2`.


## Round 5

Michaël, the altitude report matches the revised plan digest, and its added detail is justified by the review dispositions. I found one remaining boundary issue in the delta. Confidence is high.

Altitude report SHA-256: 9a134fa030d4db2f26aa93f9396a237a75407d726159cb37f2ed1a6303db0000

- **[MEDIUM] Item 5: retaining the first character does not guarantee it fits a tiny token limit.** The revised cap keeps a complete first character even when it exceeds `max(M − 2, 1)` bytes ([plan](.review/plan.md:230)). A three-byte character can require multiple byte-level tokens; with `M = 1` or `M = 2`, the server may also add special tokens and reject even that prefix. The proposed test checks what is sent, not whether a server with that token budget can embed it. `ServedArtifact` currently accepts `M = 1` ([cognitive.rs](crates/me-types/src/types/cognitive.rs:329)). Define how an input is handled when no nonempty prefix can fit, and test the server rejection rather than claiming it can never fail for length.


## Round 6

Michaël, the delta addresses `M = 1` and `M = 2`, but the same boundary can occur above them. Confidence is high.

The regenerated altitude report matches the revised plan digest. Its additional detail is justified by the review correction; no override is claimed.

Altitude report SHA-256: cc87729c3b7120f9a9aeadfa327ecad565e8505a43603c4fb22930ec5df3bc60

- **[MEDIUM] Item 5: retaining the first character can exceed the cap when `M > 2`.** With `M = 3`, the byte cap is one, but the rule sends a complete three-byte first character ([plan item 5](.review/plan.md:233)). A byte-level tokenizer can count three tokens for it, and a server adding two special tokens can reject it. Thus the new claim that inputs above `M = 2` never fail for length is not guaranteed. Cover the case where the first character exceeds `M − 2` bytes and state the rejection behavior there too.


## Round 7

Michaël, LGTM for the round-seven delta. The altitude report matches the revised plan digest, and its added detail is justified by the review correction.

Altitude report SHA-256: 3e4141ad1e2d9f0a2adef6d689d0d44418b69e0a4fbbf17527c20d38e71b2b1a

REVIEW COMPLETE