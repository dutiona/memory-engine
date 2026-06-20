# Multi-model review — resolution (#629 A1 plan)

Reviewers: clean-slate subagent (general-purpose), Codex (one-shot, read-only), agy "Gemini 3.1 Pro (High)" (one-shot). `advisor()` tool unavailable in this environment → substituted by the subagent + two-model pass per the super-plan "skip to one-shot review" guidance. No advisor artifact fabricated.

## Findings & resolutions

- **[BLOCKER · subagent] D1 "drop for_each → list_all" silently regresses O(1) streaming.** FIXED — object-safe `for_each_X(&mut (dyn FnMut(X)->Result<()> + Send))` preserves streaming; `list_all_X` kept only where it exists today. (Both Codex+agy confirmed the new shape compiles + the `+Send` is correct.)
- **[BLOCKER · Codex] Object-safety proof insufficient (vtable-forms ≠ callable under async_trait's hidden `Self: Sync`).** FIXED — traits carry `: Send + Sync` (already); P1 + Testing now add a `#[cfg(feature="async")] #[tokio::test]` that `.await`s a method through `Arc<dyn StorageBackend>`, run in the `--all-features` gate.
- **[BLOCKER · Codex] SearchIndex `Vec<i64>` not faithful — scores surfaced end-to-end (SearchResult.score:f64, hybrid.rs:50; CLI query.rs:113; MCP depth.rs:96; single-channel builds Vec<(i64,f64)>).** RESOLVED (user chose scored) — SearchIndex returns `Vec<(i64, f64)>` (scored, best-first). RRF still fuses by rank (lock intent preserved); single-channel score surfacing preserved; doc caveat: scores are backend-native, not cross-comparable.
- **[BLOCKER · agy / HIGH · Codex] SearchIndex missing `fts_count_expired` probe (engine query.rs:302).** FIXED — added `lexical_count_expired(&self, query, filter) -> Result<usize>`.
- **[MEDIUM · Codex] Config-on-the-port is LIVE (embedding_meta load/store/record_if_absent/require_present called in engine open/write/promotion).** FIXED — promoted the typed embedding-fingerprint surface onto `SchemaManager`; generic config stays backend-private.
- **[LOW · Codex] SessionStore ex-#[cfg(test)] reads = main trim candidate.** KEPT with justification (the #632 conformance suite needs through-trait read-backs; #[cfg(test)] trait methods fork the vtable).
- **[LOW · agy] MCP typed arm framed as "silent degradation".** CORRECTED — wildcard already produces correct output; the typed arm is explicit/greppable/future-proof, uses `err.to_string()` (no double prefix).
- **Positive confirmations (de-risking):** blanket `impl<T> StorageBackend for T` coherent (no overlap); ColdStorage-separate keeps the umbrella feature-invariant; relocation types are impl-free plain structs; MemoryError `#[non_exhaustive]` makes the new variant workspace-safe; full ~90-method surface + one-file-per-trait justified; P1-spike-first sequencing sound.

## Outstanding
None — all findings resolved; SearchIndex decided (scored `Vec<(i64, f64)>`).

## Code review (post-implementation, on PR #648)

Reviewers: 3 adversarial subagents (transcription-fidelity, object-safety/API, scope/leakage) + agy "Gemini 3.1 Pro (High)" on the actual diff. Codex was in a ~5h rate-limit window (per [[feedback_review_under_budget]], used agy rather than dropping external review).

**Subagents:** all clean. Transcription FAITHFUL (85 methods, counts 49/7/13/10 exact, zero divergences); object-safety sound (Arc<dyn> + async-through-dyn callability proven); scope clean (no driver leak, RRF untouched, in-lane diff). One reviewer surfaced 18 clippy pedantic/nursery warnings the P7 gate had **cached-masked** (lesson: force-recompile clippy before trusting a clean grep).

**agy findings & dispositions:**
- **[HIGH] `lexical_count_expired` took `&FactFilter` but ignored `temporal`/`ids`/`pinned`/`metadata` (contract trap).** FIXED — reverted to the faithful `fts_count_expired` signature (`fact_type` + `scope_ids` explicit params). My FactFilter "uniformization" was the editorialization that introduced the trap.
- **[LOW] `vector_search` empty/wrong-length embedding undocumented.** FIXED — documented as an `EmbeddingDimension` error.
- **[HIGH] `record_embedding_fingerprint_if_absent(expected_dim)` "leaks validation into the port".** DECLINED (kept faithful). It is a 1:1 transcription of `embedding_meta::record_if_absent` (user-frozen "transcribe the full surface" decision). Removing `expected_dim` would force the engine's existing call sites (#631) to change and re-validate — *more* churn + behavior-change risk — to satisfy a separation-of-concerns preference. The dim-check is a documented backend contract #630 wraps verbatim. A future "move validation engine-side" refinement can be a follow-up if desired.
- **[MEDIUM] `require_embedding_fingerprint_present` is thin engine policy.** DECLINED (kept faithful), same rationale — it transcribes `embedding_meta::require_present`, a real engine call site; keeping it 1:1 minimizes #631 friction.

The two declined items are surfaced to the maintainer (they touch the port-boundary philosophy); both are trivial follow-up changes if the maintainer prefers the cleaner boundary over faithful transcription.

## Clippy gate lesson
`cargo clippy | grep warning` returns nothing when the build is CACHED (it only re-emits on a fresh compile). Always `touch` the changed files (or `cargo clean -p <crate>`) before trusting a clean clippy grep — the cache masked 18 real warnings in the P7 gate.
