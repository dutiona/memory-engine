I have independently verified the trait definitions and cross-cutting types. The internal reviewers are correct on transcription fidelity, object-safety, and scope boundaries — the `async_trait` setup is sound, the blanket `StorageBackend` implementation is correct, and no dialect details leak into the types. 

However, there are several semantic mismatches and boundary leaks that violate the "clean, behavior-neutral" goal and will cause friction for #630 and #631. 

Here are my findings:

**[HIGH] `SearchIndex::lexical_count_expired` breaks the `FactFilter` contract**
* **File:** `src/storage/search_index.rs:49`
* **Issue:** The signature takes a full `FactFilter`, but the documentation explicitly notes that "`filter.temporal` is ignored" because the expired predicate is baked in. This is a severe contract trap. A caller passing `FactFilter::new().temporal(TemporalFilter::AsOf(t))` will have their temporal constraint silently dropped. Furthermore, if the underlying implementation also ignores `ids`, `pinned`, or `metadata` (since this is a narrow diagnostic probe), passing a `FactFilter` is highly misleading.
* **Fix:** Either change the signature to take exactly the parameters it respects (e.g., `fact_type: Option<FactType>, scope_ids: Option<&[i64]>`), or add an `ExpiredOnly` variant to `TemporalFilter` so `lexical_count` can use the filter faithfully without dropping fields.

**[HIGH] `SchemaManager::record_embedding_fingerprint_if_absent` leaks engine validation logic into the port boundary**
* **File:** `src/storage/schema.rs:48-52`
* **Issue:** The method takes `expected_dim: usize` and validates that `candidate.dim == expected_dim` (returning `MemoryError::EmbeddingDimension` if they mismatch). This pushes business logic down to the storage backend. The engine must validate `embed_dim` anyway on a read-only open (when it uses `load_` instead of this method), and it already knows `expected_dim` when building the `candidate`. Forcing every backend implementation to include boilerplate `if candidate.dim != expected_dim { return Err(...) }` violates the separation of concerns. 
* **Fix:** Remove `expected_dim`. The signature should be `async fn record_embedding_fingerprint_if_absent(&self, candidate: &EmbeddingFingerprint) -> Result<EmbeddingFingerprint>;`. The engine should pre-validate the `candidate` and post-validate the returned `stored` fingerprint if it was already present.

**[MEDIUM] `SchemaManager::require_embedding_fingerprint_present` is redundant engine policy**
* **File:** `src/storage/schema.rs:53-54`
* **Issue:** This method requires the backend to implement the equivalent of `if self.load_embedding_fingerprint().await?.is_none() { return Err(...) }`. This is trivial engine-layer policy ("the store must have a fingerprint to be valid") being pushed down to the infrastructure port.
* **Fix:** Remove this method from the trait entirely. The engine should simply call `load_embedding_fingerprint()` during its open sequence and return the error itself if `None` is yielded.

**[LOW] `SearchIndex::vector_search` lacks an empty-input contract**
* **File:** `src/storage/search_index.rs:38-44`
* **Issue:** The documentation for `lexical_search` explicitly defines its edge-case behavior ("A malformed query yields an empty result, not an error"). However, `vector_search` omits what happens if the `embedding` slice is empty (`&[]`). Should backends panic, return an error, or yield an empty vector? This ambiguity will lead to divergent backend behavior.
* **Fix:** Document whether an empty `embedding` slice yields `Ok(vec![])` or an error, so implementers don't have to guess.

Aside from these boundary gaps, the trait abstraction provides a solid foundation. 

REVIEW COMPLETE
