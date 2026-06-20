I have reviewed the plan against the actual codebase. The plan is exceptionally rigorous, well-reasoned, and correctly identifies complex Rust boundaries (like the feature-gating impact on trait object vtables). The TDD sequencing and scope discipline are highly sound.

There is exactly one blocking omission regarding the search trait, and one high-priority architectural consideration regarding the streaming shape.

Here are the detailed findings based on your questions:

**1. OBJECT SAFETY: [HIGH]**
*Plan section: D1 `for_each<F>` reshape*
- **Will it compile?** Yes. A mutable reference to a trait object (`&mut (dyn FnMut(X) -> Result<()> + Send)`) does not violate object safety. The `+ Send` bound correctly ensures the future returned by `#[async_trait]` remains `Send`.
- **The catch:** Because `FnMut` is a *synchronous* closure, the caller cannot use `.await` inside it (e.g., streaming rows directly to a network socket). Furthermore, if the implementation uses an async backend (like `tokio-postgres` in the future), executing a slow/blocking synchronous closure inside the `for_each_X` loop will block the async executor thread.
- **Cleaner shape:** The idiomatic object-safe async streaming shape in Rust is returning a stream: `async fn stream_X(&self) -> Result<futures::stream::BoxStream<'_, Result<X>>>`. This allows the caller to use `while let Some(x) = stream.next().await {}`, keeping control flow natural and asynchronous on both sides of the seam. If you prefer not to pull in the `futures` crate, the synchronous callback is acceptable but its blocking limitations should be heavily documented.

**2. COMPLETENESS: [BLOCKER]**
*Plan section: `SearchIndex` trait*
- The plan is missing **`fts_count_expired`**.
- In the current code (`src/engine/query.rs:302`), the engine calls `crate::search::fts::fts_count_expired` to populate `diagnostics.expired_matches` for text queries.
- The plan's `SearchIndex` trait only provides `lexical_search` and `vector_search`. Since `lexical_search` takes a `limit: usize` and returns `Vec<i64>`, it cannot be used to efficiently count *all* matching expired facts.
- **Fix:** You must add `async fn lexical_count_expired(&self, query: &str, filter: &FactFilter) -> Result<usize>` to the `SearchIndex` trait, otherwise #631 (engine wiring) will be unable to satisfy the `include_expired_probe` logic.

**3. ERROR WIRING: [LOW]**
*Plan section: P2 (MCP typed arm)*
- The typed-arm fix at `memory-engine-mcp/src/error.rs:69` compiles and is safe, but it is functionally redundant.
- The plan proposes adding: `MemoryError::Storage(e) => ErrorData::internal_error(format!("storage error: {e}"), None)`.
- The fallback wildcard arm is: `other => ErrorData::internal_error(other.to_string(), None)`.
- Because `MemoryError::Storage`'s own `Display` implementation (`#[error("storage error: {0}")]`) already prepends `"storage error: "`, `other.to_string()` produces the exact same output. It does not "silently degrade". Keeping the typed arm is fine for explicit tracking, but you can safely simplify it to `ErrorData::internal_error(err.to_string(), None)` without duplicating the prefix.

**4. RELOCATION: [LOW]**
*Plan section: Type relocation*
- Moving `EventFilter`, `FactScoringRow`, and `SessionFact` to `src/types.rs` is completely safe. Codebase verification confirms they are plain data structs with public fields. There is no hidden coupling, tightly-bound trait implementations, or restrictive visibilities that would break when moved behind `pub use` shims.

**5. BLANKET IMPL: [LOW]**
*Plan section: `StorageBackend` blanket impl*
- `impl<T> StorageBackend for T where T: FactGraph + ...` is perfectly coherent and standard Rust. Because `StorageBackend` is a local trait with no other blanket implementations, there is zero coherence or overlap risk. It correctly forces backends to construct the 6 composite traits.

**6. FEATURE GATES: [LOW]**
*Plan section: D4 `ColdStorage` supertrait*
- Keeping `ColdStorage` as a separate `#[cfg(feature="archive")]` trait—rather than a supertrait of `StorageBackend`—is an excellent architectural decision. It keeps the vtable and memory layout of `StorageBackend` identical across builds, guaranteeing that `Arc<dyn StorageBackend>` remains feature-invariant. `--all-features` and default will stay green.

**7. SCOPE / OVER-ENGINEERING: [LOW]**
*Plan section: The contract*
- The ~90-method full surface is fully justified for a "traits only" PR. By transcribing the full surface before writing any backend implementation, you force the compiler to prove object-safety across the entire contract. Segregating them into multiple domain-specific files avoids a monolithic trait file and maintains readability.

**8. TDD FEASIBILITY: [LOW]**
*Plan section: TDD execution order*
- The P1-spike-first sequence is exceptionally sound. Verifying the `Arc<dyn StorageBackend>` coercion compiles on a 2-method skeleton first avoids wasting time transcribing 90 methods if a fundamental object-safety violation existed. The subsequent phase progression minimizes risk at every step.

If you add `lexical_count_expired` to the `SearchIndex` trait, the plan is robust and ready for execution.

REVIEW COMPLETE
