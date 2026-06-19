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
