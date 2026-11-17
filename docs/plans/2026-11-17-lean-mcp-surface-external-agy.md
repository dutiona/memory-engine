# Antigravity plan review (agy, gemini-3.8-flash-high), rounds 1-9

Each round's accepted review.md, verbatim, from conversation `a4926d37-085b-4424-a342-fca348638e0e`. Round 9 reviewed the final plan revision.

---

## Round 1

Michaël, here is the implementation plan review for #1020:

### Altitude and Duplication Assessment

Altitude report SHA-256: 0fe1139efd9071601dd3249d19834efc0c2004e0807b745b9622787518021d46

The plan is well calibrated at 457 lines across 14 top-level tasks. Fenced volume is restricted to verification commands, and literal strings are confined to exact model-facing contracts (tool descriptions, receipt formats, and instructions). The scope appropriately balances replacing 26 MCP tools with two lean tools, introducing the advisory store lock, and wiring the CLI `prune` subcommand across five crates without speculative abstraction or duplicate validators. Structural completeness is satisfied with dedicated Documentation, Testing, and Verification sections.

---

### Findings

[Antigravity Review] [BLOCKER] Section D7 (lines 111-112), Item 4 (lines 225-231), and Item 13 (lines 358-362): The plan assumes `std::fs::File::try_lock` is stable in Rust 1.89 with no dependencies (`TryLockError::WouldBlock -> StoreLockError::Held { lock }`). The Rust standard library does not provide `try_lock()` on `std::fs::File` in Rust 1.89 (nor in any released version; file locking has never been stabilized in `std`), and `std::fs` has no `TryLockError` type. Attempting `file.try_lock()` fails compilation with `E0599`. Furthermore, Item 13 drops `libc` from `crates/memory-engine/Cargo.toml` as "unused", removing the exact Unix dependency required to call `libc::flock(fd, LOCK_EX | LOCK_NB)` without adding a third-party dependency. Item 4 must implement advisory file locking via a platform API (e.g., retaining `libc::flock` on Unix and `LockFileEx` on Windows) or explicitly introduce an audited locking crate, and Item 13 must not drop `libc`.

[Antigravity Review] [HIGH] Section D9 (lines 141-144) and Item 11 (lines 331-334): The plan adds `crates/memory-engine-mcp/clippy.toml` configuring `disallowed-methods` for `MemoryEngine::{consolidate, run_dream_cycle, run_dream_cycle_guarded, apply_cycle_report}` and expects `cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings` to fail. However, `clippy::disallowed_methods` belongs to the `restriction` lint category, which is `allow` by default. Neither workspace `Cargo.toml` nor `crates/memory-engine-mcp/Cargo.toml` enables `disallowed_methods` under `[lints.clippy]` (unlike `unwrap_used = "deny"` which was explicitly configured). Without declaring `disallowed_methods = "deny"` in `crates/memory-engine-mcp/Cargo.toml` (or `#![deny(clippy::disallowed_methods)]` in its crate roots `src/lib.rs` and `src/main.rs`), Clippy will silently ignore `clippy.toml` and stay green regardless of whether the configuration file was read.

[Antigravity Review] [MEDIUM] Section D1 (lines 69-70) and Item 2 (lines 212-214): The plan claims that `resolve_query_scope_ids already feeds the id set into FactFilter.scope_ids for the FTS, brute-force vector and HNSW channels`. In `crates/me-backend-sqlite/src/search/ann.rs`, `HnswStrategy` does not take `FactFilter` (it implements `VectorSearchStrategy::search` which accepts `scope_ids: Option<&[i64]>`). Furthermore, the underlying `hnsw` crate does not support filtered traversal: Phase 1 retrieves nearest neighbors unconstrained (`dest_len = overfetch.min(n_items)`), and Phase 2 post-filters by `scope_ids` in `fetch_candidate_embeddings`, widening `overfetch` up to 3 times before falling back to brute force if fewer than `limit` candidates survive. The plan should accurately document that HNSW candidate collection is unconstrained and relies on post-filtering and brute-force fallback for C16 conformance, rather than claiming pre-top-k filtering inside the HNSW channel itself.

[Antigravity Review] [LOW] Section D10 (line 152) and Item 11 (lines 345-348): In `fuzz/fuzz_targets/dispatch.rs`, the target is updated to drive `MemoryMcpServer::dispatch(&self, machine: &Machine, ...)` and obtains a `Machine` by creating a temporary file on disk and loading it via `TokenTable::load`. In `crates/memory-engine-mcp/src/auth.rs`, `TokenTable::parse` is `pub(crate)` and `Machine` has private fields with no public constructor, forcing the fuzz harness to perform disk I/O to initialize authentication. Making `TokenTable::parse` public (or exposing a `test-util` constructor for `Machine`) would allow the fuzz harness to initialize completely in memory without temporary files.

[Antigravity Review] [LOW] Section D12 (line 175): The plan cites `search/fts.rs:99` for the FTS5 error catch; following the Wave 2 crate layout reorg, this file is located at `crates/me-backend-sqlite/src/search/fts.rs:99`.

REVIEW COMPLETE


---

## Round 2

Michaël, here is the Round 2 review of the implementation plan delta for #1020:

### Altitude and Duplication Assessment

Altitude report SHA-256: 8b02f1364778780a93be48f540870450b4f68471a43341d226cc8b7948d6dade

Revision 2 of the plan (531 lines, 15 implementation items) remains well-scoped and disciplined. The delta strictly addresses Round 1 findings:
- Clarifies the HNSW candidate channel mechanics and its alignment with ADR-0021 §5 step 3 (D1).
- Adds a startup RNG probe to fail early if OS entropy is unavailable (D5).
- Introduces D13 and Item 5 to allow live SQLite export from a read-only engine without locking, keeping live backups fully operational.
- Acquires the store lock before peeking the database dimension in CLI commands while checking `path.is_file()` first to avoid stray lock files (D7, Item 6).
- Establishes durable call-record ordering with `source_event_id` linkage (D11, Item 10) and deterministic SQLite trigger tests.
- Handles empty/punctuation-only full-text queries gracefully (D12, Item 11).
- Strengthens clippy guard verification (D9, Item 12) and cleans up unneeded dev-dependencies (D10).

The delta introduces no unnecessary abstractions, respects all binding ADR contracts and scope fences, and contains complete Documentation, Testing, and Verification sections.

---

[Antigravity Review] LGTM

REVIEW COMPLETE


---

## Round 3

Michaël, here is the Round 3 review of the implementation plan delta for #1020:

### Altitude and Duplication Assessment

Altitude report SHA-256: bf837855a816354f561857548b69a55ad4b4965697d436d84c6ae6b281034ffe

Revision 3 of the plan (552 lines, 15 implementation items) remains tightly focused. The delta strictly addresses Round 2 feedback:
- Formalizes the exact call record durability guarantees and link mechanics (D11, Item 10), reading the outcome from the fact side via `source_event_id`, and adds test `a_remember_whose_fact_write_fails_leaves_an_unnamed_record`.
- Caps FTS5 query expansion at the first 64 distinct terms (`MAX_FTS_TERMS = 64`, D12, Item 11) to prevent long queries from generating expressions FTS5 rejects, verified with a 5,000-word query test.
- Caps `recall.project` at 4,096 bytes without echoing oversized values in error responses (D10, Item 9).
- Single-sources `MAX_PAYLOAD_BYTES` by re-exporting it from the facade root (`memory_engine::MAX_PAYLOAD_BYTES`, D10, Item 9).
- Hardens the read-only SQLite export test in Item 5 using `BEGIN IMMEDIATE` on a side connection, and cleans up the stale comments across `db.rs` and `schema.rs`.
- Accurately documents the read-only schema version check in `migrate` as an intentional design exception to the lock-before-read rule (D7).

The delta is cohesive, clean, and introduces no regressions or unnecessary complexity.

---

[Antigravity Review] LGTM

REVIEW COMPLETE


---

## Round 4

Michaël,

Here is my Round 4 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r4.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 4, plan SHA-256 `e9eacdf0b97d89a0e3e4ea4f152e9ed55d3c15d02826c258183a4b9e8892a0b3`) inspected and verified.
Altitude report SHA-256: 3ce919be6fac5f4052d6a6881c27d1492563d10e231b5b9cd4dbf20c34d25a03

### Delta Assessment (Revision 3 $\to$ Revision 4)
1. **Unknown `project` error and call record capture cap (D10, Items 9 & 10):**
   - The 4,096-byte refusal on `recall.project` has been removed. Client-facing responses now return the unknown `project` key in full, in strict adherence to ADR-0020 §8 ("an error that names the value").
   - Internally, the call record's `error` field is capped at 512 bytes on a character boundary pursuant to ADR-0020 §6, ensuring large unknown keys cannot cause an event log append failure.
   - Accompanying tests in Item 9 (`an_oversized_unknown_project_is_named_in_full`) and Item 10 (record `error` field truncated to 512 bytes) verify both boundaries.

2. **Recall full-text term limit refusal (D12, Item 11):**
   - Silent truncation of queries to 64 terms has been eliminated.
   - `recall` now cleanly accepts queries with up to 256 distinct terms (`MAX_RECALL_TERMS = 256`) and searches all terms without dropping words. Queries exceeding 256 terms are explicitly rejected with an informative error stating the term count and limit.
   - Accompanying tests in Item 11 (`a_query_at_the_term_limit_finds_its_last_word` matching the 256th word; `a_query_over_the_term_limit_is_refused` rejecting 257 distinct words) enforce this behavior without regressions.

The revision 4 delta resolves all outstanding concerns cleanly and aligns precisely with ADR-0020 and ADR-0021.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Round 5

Michaël,

Here is my Round 5 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r5.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 5, plan SHA-256 `c3f3ac9e30c20981bfd566cd8501fb0a5d87edbb48a1058eeb47f9252c8cab79`) inspected and verified.
Altitude report SHA-256: bce9d20ee5dcd0bdceaa98c61fd931e5ec1094c99834eb0afe1f06307d2ea2f4

### Delta Assessment (Revision 4 $\to$ Revision 5)
1. **Call record error truncation & #1023 hand-off (D11):**
   - The rule that every call record's `error` field is truncated to 512 bytes on a character boundary (per ADR-0020 §6 capture cap) is now stated universally. This protects the event log write against arbitrary client inputs (unknown project names, oversized serde `unknown field` keys, or syntax errors) while keeping the full error message in the client reply.
   - The contract hand-off to #1023 explicitly notes that a future `erase` operation must preserve the call record's status as erased rather than misreporting it as a failed remember.
2. **Case-insensitive FTS term deduplication (D12):**
   - Term deduplication in `fts_terms` is now specified as case-insensitive, aligning with FTS5's `unicode61` tokenizer behavior and avoiding redundant duplicate tokens in the generated `OR` expression.
3. **Test harness seed & concrete over-long key (Item 9):**
   - In test harness seeding, using `add_fact_precomputed` with the configured provider's own `fingerprint()` ensures consistent store identity initialization without fingerprint mismatch.
   - Specifying a concrete 300-byte test key for C5 verifies proper rejection when `project:<label>` exceeds 256 bytes.

All updates are clean, robust, and introduce no regressions.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Round 6

Michaël,

Here is my Round 6 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r6.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 6, plan SHA-256 `c589db1f3ba0a2757f31741b2b5ed588a1eb82d0c6b5b0edc8d044acf50f13e5`) inspected and verified.
Altitude report SHA-256: 68ee8b89da133dc5fd87b627ef8dc7a6bb285f60df80eea5971c253c0f3b385e

### Delta Assessment (Revision 5 $\to$ Revision 6)
- **Unicode alphanumeric term segmentation (D12, Item 11):**
  - Defining each full-text term as a maximal run of Unicode alphanumerics (`char::is_alphanumeric`) ensures that extracted terms contain no internal separators (such as `_`, NBSP, or em-dashes) that the `unicode61` tokenizer splits on.
  - This prevents quoted terms from degrading into multi-token phrase queries, eliminating the superlinear phrase-matching performance blowup observed on large underscore- or separator-joined strings.
  - Because each term is now guaranteed to map to a single atomic FTS5 token, the 256-term limit (`MAX_RECALL_TERMS = 256`) strictly and reliably bounds SQLite query execution cost.
  - Non-ASCII letters (e.g. `été`) remain single intact terms, snake_case identifiers split cleanly into constituent words (`memory_query` $\to$ `"memory" OR "query"`), and repeated segments deduplicate case-insensitively (e.g., `lock_lock_…` deduplicates to `"lock"`).
  - Accompanying test specifications in Item 11 (`a_long_underscore_run_is_cheap`, separator checks, and diacritics tests) rigorously verify these boundaries.

The delta resolves the phrase-matching vulnerability cleanly with zero regressions.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Round 7

Michaël,

Here is my Round 7 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r7.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 7, plan SHA-256 `4c947d14cc67456f067d60ad3ff076b2f06baff3e619eb30ead470635de7d8ba`) inspected and verified.
Altitude report SHA-256: 31793d810bdbb85f996dc49c3f3be5fd6dea9fa3ce6e00396e0b745e4507852f

### Delta Assessment (Revision 6 $\to$ Revision 7)
- **Bounded query size replacing fragile tokenizer parity (D10, D12, Items 9 & 11):**
  - Capping `recall.query` at 4,096 bytes (`MAX_RECALL_QUERY_BYTES = 4096`) decouples FTS5 execution safety from subtle discrepancies between Rust's `char::is_alphanumeric` and SQLite `unicode61` tokenization rules (such as U+0345 or circled alphanumerics).
  - At the 4,096-byte ceiling, worst-case queries (e.g. repeated phrase segments or combining-character sequences) execute in $\le 0.34$ seconds on SQLite 3.46.1 over 20,000 rows.
  - This simplification removes the redundant 256-term count limit and avoids re-exporting `MAX_PAYLOAD_BYTES` from the facade (`crates/memory-engine/src/lib.rs`), keeping the facade surface lean.
  - Case-insensitive term deduplication is retained, and syntax sanitization via `char::is_alphanumeric` serves purely for FTS5 syntax safety.
  - Accompanying tests in Item 9 (`an_oversized_query_is_refused` at 4,097 bytes) and Item 11 (`worst_case_queries_at_the_cap_are_answered` and `a_query_whose_last_word_matches_is_found` at 4,096 bytes) thoroughly validate the query boundaries.

The design is cleaner, simpler, and fully robust against worst-case tokenizer divergences.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Round 8

Michaël,

Here is my Round 8 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r8.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 8, plan SHA-256 `deb26a4be7e417062797a24937b2f1adcdaef5270cc9ae638184069d318d250d`) inspected and verified.
Altitude report SHA-256: b750a3ab4021eabd587f19c89f3f57408e67115f227825f101bfcc6b237f4038

### Delta Assessment (Revision 7 $\to$ Revision 8)
- **Empirical worst-case documentation & residual acceptance (D12, "For Michaël" #2):**
  - Section D12 now documents re-measured worst-case latencies under realistic backend constraints (`ORDER BY bm25 LIMIT 30` over 20,000 rows) at the 4,096-byte ceiling: 0.99 s for a 2,048-segment phrase (`a_a_…`) and 1.81 s for 516 distinct diacritic spellings of one word.
  - The residual cost (1–2 s for maliciously crafted inputs from authenticated callers, versus milliseconds for natural language questions) is accepted for this epic, with the option for a tighter cap surfaced explicitly for you in "For Michaël" item 2.
- **Model tool description transparency (Item 9):**
  - The exact model-facing description for `recall` now explicitly informs the model: *"Queries over 4,096 bytes are refused."*

The adjustments are purely empirical calibrations and doc refinements, preserving full contract integrity.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Round 9

Michaël,

Here is my Round 9 review of the implementation plan for `dutiona/memory-engine#1020` based on the delta in `.review/plan-delta-r9.diff` and the updated candidate in `.review/plan.md`.

### Altitude Checkpoint Acknowledgement
Altitude report at `.review/altitude.md` (revision 9, plan SHA-256 `63615f8813ae874586fba9ae0be975f7d517ea2e5e6a61c78d4feaa4677b4dc1`) inspected and verified.
Altitude report SHA-256: 91d6ae10c23f1303faa5959c49aa691043368aa0ce10331b8a293fe8e7d0fcce

### Delta Assessment (Revision 8 $\to$ Revision 9)
- **Removal of self-referential file path (Item 1):**
  - Item 1 now reads: *"Commit this plan and its review artifacts under `docs/plans/`"*, avoiding a literal local path reference that would trigger the plan-issue archiver's refusal.
  - This is a non-behavioral text edit that ensures clean archiving when filed to GitHub issues.

The plan is complete, fully adjudicated across all rounds, and ready for execution.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

## Resolution

- R1 [BLOCKER] `File::try_lock` not stable → refuted: std's `library/std/src/fs.rs` carries `#[stable(feature = "file_lock", since = "1.89.0")]` on `try_lock`, and 1.89 is the MSRV; `libc` is unused in `crates/memory-engine/src`, so item 14 stands.
- R1 [HIGH] `disallowed_methods` allow-by-default → the workspace denies `clippy::all`; item 12's red step proves the lint fires before anything is deleted, with a crate-root `#![deny(clippy::disallowed_methods)]` fallback (D9).
- R1 [MEDIUM] HNSW post-filter wording → D1 rewritten to describe the channel exactly.
- R1 [LOW] fuzz harness disk I/O → kept (one temporary file at init instead of widening the public auth API). R1 [LOW] path qualification → fixed.
- R2-R9: LGTM.
