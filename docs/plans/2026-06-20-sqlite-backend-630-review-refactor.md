# Clean-slate review — #630 `SqliteBackend` plan, REFACTOR-SAFETY / BEHAVIOR-PRESERVATION lens

Reviewer: clean-slate, no prior context. Target plan: `docs/plans/2026-06-20-sqlite-backend-630.md`.
Verification basis: the real code in this checkout (file:line citations below).

The plan's whole safety claim is **zero behavior change**, and the structural trap it
correctly names is real: the engine is **not** rewired in #630, so **no existing test
exercises `SqliteBackend`**. A green suite proves nothing unless the new parity tests
_independently_ pin behavior. My job was to find a way drift slips past those parity
tests. I found one BLOCKER (HNSW build/dispatch policy lives in the engine, not the
backend, and the differential proof is structurally blind to it), plus several HIGH/MEDIUM
gaps. Most of the plan's load-bearing claims (D4, the empty-slice quirk, marker_key,
NotFound, the f32→f64 widening direction, ReadOnly preservation) **check out against the
code** — I confirm those explicitly so the author isn't second-guessing the parts that are
right.

---

## Findings

### [BLOCKER] B1 — HNSW build + dispatch policy lives in the **engine** (`search_config`), not the backend; D3/H9 cannot see the divergence

**Plan:** §1 D3 ("`SqliteBackend` **owns** its `HnswStrategy` … built `from_db` at
construction … `vector_search` dispatches HNSW-vs-brute"); struct §2 lines 57–66; H9 §4
("Brute≡HNSW recall on exact-small corpus; insert/expire reflected"); T8 §5.

**Reality:**

- The engine builds HNSW **conditionally on `search_config`** — `engine/mod.rs:264-272`:
  HNSW is constructed **only** when `search_config` is `Some` **and**
  `ann_threshold < usize::MAX`. The default config is `None` (`engine/mod.rs:105`), so a
  default engine — even with `--features ann` compiled in — **never builds HNSW at all**.
- The engine dispatches HNSW vs brute via `should_use_hnsw()` (`engine/mod.rs:370-378`),
  gated on `hnsw.active_count() >= search_config.ann_threshold` (default fallback
  `usize::MAX` ⇒ **never** when no config). `active_vector_strategy()`
  (`engine/query.rs:17-27`) consults that gate per query.

The plan's `SqliteBackend` struct (lines 57–63) holds `hnsw: Option<…>` but carries **no
`search_config`/`ann_threshold`**. So the backend, as designed, will (a) build HNSW
unconditionally at construction and (b) dispatch to HNSW whenever the index is populated —
**both diverge from the engine's config-gated policy.** When #631 rewires the engine onto
this backend, a deployment that today runs pure brute-force (no `search_config`) silently
flips to approximate HNSW search → a recall change. That is precisely the "zero behavior
change" violation #630 exists to prevent, relocated into #631 where the seam is closed and
it cannot be fixed without re-opening it (the very argument the plan uses _for_ doing HNSW
in #630).

**Why H9 is blind to it:** H9 asserts brute ≡ HNSW recall _on an exact-small corpus_. On a
tiny corpus the two strategies return identical results **by construction**, regardless of
_when_ HNSW is built or _at what threshold_ it dispatches. The proof targets recall
equivalence; the drift is in the **build/dispatch policy**, which recall-equivalence
testing cannot observe. This is the cleanest "green test that proves nothing" hole in the
plan — and it sits on the one hazard (H9) the plan flags as a "perf cliff," masking that it
is also a **correctness/behavior** divergence.

**Fix (concrete):**

1. Carry the policy into the backend: add `search_config: Option<SearchConfig>` (or at
   minimum `ann_threshold: usize`) to `SqliteBackend`, and replicate **both** the
   build-condition (`cfg.ann_threshold < usize::MAX`) and the dispatch gate
   (`active_count() >= ann_threshold`, with the `None ⇒ usize::MAX ⇒ never` fallback)
   verbatim from `engine/mod.rs:264-272` + `engine/mod.rs:370-378`. The constructor
   (`with_hnsw`) must build HNSW **only** under the same condition the engine uses, not
   "always at construction."
2. Add a parity test that pins the **dispatch boundary**, not just recall: construct the
   backend with `ann_threshold` set just above and just below `active_count()`, and assert
   the _same dispatch decision_ (HNSW vs brute) the engine's `should_use_hnsw()` makes —
   e.g. by asserting backend `vector_search` ≡ `BruteForce::search` when below threshold,
   and ≡ the HNSW strategy when at/above. Include the `search_config = None ⇒ never HNSW`
   case explicitly.
3. If the author prefers to keep the policy engine-side (defer the gate to #631), then D3's
   "backend **owns** HNSW" framing is wrong and must be restated: the backend exposes
   `vector_search` brute-only (or a strategy injected by #631), and the §1 D3 rationale
   ("deferring relocates an O(log N)→O(N) regression into #631 it could not fix") collapses
   — because the _gate_ is what #631 must port anyway. Pick one; the current plan straddles
   both and the straddle is the bug.

---

### [HIGH] H-a — `SqliteBackend` struct omits `vector_strategy` / `search_config`; the "four-fields-to-one swap" (§2) is actually six-plus fields, and the omission is what hides B1

**Plan:** §2 "`SqliteBackend` absorbs the four SQLite-private fields the engine holds as
siblings today (`pool`, `embed_dim`, `upcaster_registry`, `hnsw`), so #631 is a
four-fields-to-one swap."

**Reality:** the engine struct (`engine/mod.rs:157-167`) holds `pool`, `embed_dim`,
`graph: RwLock<MemoryGraph>`, `scope_tree: RwLock<ScopeTree>`,
`vector_strategy: Box<dyn VectorSearchStrategy>` (the brute-force fallback),
`reranker`, `hnsw_strategy`, **`search_config: Option<SearchConfig>`**, and
`upcaster_registry`. The "four SQLite-private fields" the plan lists drops both
`vector_strategy` and `search_config` — yet `vector_strategy` (`BruteForce`) is the
non-HNSW arm of dispatch and `search_config` is the gate (B1). `graph`/`scope_tree`/
`reranker` legitimately stay engine-side (they are caches / consumer traits), so the plan
is right to exclude _those_ — but it must not exclude `vector_strategy` and `search_config`,
which are intrinsic to the search seam it is moving.

**Fix:** restate §2 to list the _actual_ set the backend must own for the seam
(`pool`, `embed_dim`, `upcaster_registry`, `hnsw`, `vector_strategy` or its equivalent,
`search_config`), and explicitly note which engine fields stay (graph/scope_tree/reranker)
and why. This is the same root cause as B1 surfaced at the design-altitude.

---

### [HIGH] H-b — Collateral #1 (§9) misdescribes the real doc-drift, and the _trait-level_ error menu actually understates the returned variant

**Plan:** §9 collateral #1: "the rustdoc implies `EmbeddingDimension` for
`require_embedding_fingerprint_present`, but the concrete (and #630 impl) returns
`Internal` — reconcile." H4 (§4) lists "`require_*_present` fresh ⇒ `Internal`."

**Reality:**

- The concrete `embedding_meta::require_present` returns `MemoryError::Internal`
  (`store/embedding_meta.rs:128-137`), and its **own** rustdoc correctly says
  `MemoryError::Internal` (`embedding_meta.rs:126-127`). So the claim "the rustdoc implies
  `EmbeddingDimension`" is **not** true of the concrete function's doc.
- The drift is at the **trait** layer: `SchemaManager::require_embedding_fingerprint_present`
  (`storage/schema.rs:53-54`) documents it only as "the open-time identity guard" and the
  trait-level `# Errors` menu (`storage/schema.rs:22-27`) lists `Storage` / `Migration` /
  `EmbeddingDimension` — but **not** `Internal`, which is exactly what the concrete path
  returns. So a backend author reading the trait doc would not expect `Internal`, and H4's
  parity assertion (`Internal`) would _contradict_ the trait's documented error set.

**Why it matters for behavior preservation:** H4 is the error-fidelity hazard. If the plan
asserts `Internal` in tests (correct vs the concrete) while the trait doc forbids it, the
seam's documented contract and its tested behavior disagree — a latent #632-conformance
landmine and a real chance someone "fixes" the test to match the doc and silently changes
the variant. The method name in H4/T6 is also wrong (`require_*_present` →
`require_embedding_fingerprint_present` / underlying `require_present`).

**Fix:** Rewrite collateral #1 to target the _trait_ doc: `storage/schema.rs:22-27` +
`:53-54` must add `MemoryError::Internal` to the documented error set for
`require_embedding_fingerprint_present` (it is the un-stamped-store guard, #614 landmine).
Keep H4 asserting `Internal` (it is correct vs the concrete) but cite the concrete source
of truth (`embedding_meta.rs:128`).

---

### [HIGH] H-c — D2 streaming early-exit "at most one extra row" caveat is under-tested, and the `for_each_streamed` error-surfacing order can swallow a mid-scan SQL error behind a callback error

**Plan:** §3 D2 code + caveat ("on early callback `Err`, the blocking scan produces at most
one extra row before its next `send` fails — acceptable"); H6 catch ("callback `Err` at row
`k` ⇒ same `Err`, exactly `k` rows seen").

**Reality / risk:** the sketch (lines 125–135) does:

```
while let Ok(row) = rx.recv() { cb(row)?; }   // returns callback Err immediately
map_seam_err(handle.await...)                 // only reached if the loop ended Ok
```

On a callback `Err` at row `k`, the function returns the **callback** error via `?` and
**never awaits the join handle** — so a _mid-scan SQL error_ that the blocking scan would
have produced is dropped. Conversely, if the scan errors first, the `send` fails, the loop
ends `Ok` (recv sees a closed channel), and the join handle surfaces the SQL error — good.
The two error sources race, and the plan's H6 catch only pins the callback-error case
("exactly `k` rows seen"). It does **not** pin which error wins when _both_ a callback error
and a scan SQL error occur, nor does it assert the callback-error path is value-identical to
the free-fn's `for_each_*` early-exit (the free fns are synchronous and surface the callback
error directly — there is no join handle to abandon). For a "zero behavior change" claim
this is a real semantic the differential proof must pin, not hand-wave.

**Fix:** Make H6/T1 assert (a) callback `Err` at row `k` ⇒ that exact `Err`, `k` rows seen
(already planned); (b) a forced **mid-scan SQL error with no callback error** ⇒ surfaced as
`Storage(Backend)` (the join-await path); (c) explicitly document that a callback error
takes precedence over a concurrent scan error (callback error wins, scan error dropped) and
confirm that matches the synchronous free-fn semantics (it does — the free fn would also
return the callback error first and never continue the scan). Add the "no hang on early
exit" assertion (drop `rx`, scan's `send` must error and the `spawn_blocking` task must
terminate) — the plan mentions it in T1 prose but it is not in the H6 catch column.

---

### [MEDIUM] M-a — vector-search tie ordering is **non-deterministic** (unstable sort); a naive value-and-order parity test can false-green or flake

**Plan:** H1 catch "Per-dimension parity vs `fts_search`/`vector_search` (value **and**
order)"; §10 "assert results **identical** to the … oracle on a shared fixture (identity
holds because the SQL is reused)."

**Reality:** `vector_search` (`search/vector.rs:122-135`) uses
`select_nth_unstable_by` + `sort_by` with `partial_cmp(...).unwrap_or(Ordering::Equal)`.
For **tied scores** the relative order is **not stable / not deterministic** (unstable
sort, ties compare `Equal`). So "order-identical to the free fn on a shared fixture" is only
well-defined when scores are strictly distinct. A fixture with tied cosine scores (trivial
to hit: two identical embeddings, or the all-`0.1` vectors the existing tests use) can make
the backend and the oracle disagree on tie order across runs — either a flaky parity test,
or (if both happen to agree on one run) a false green that masks a _real_ future reordering.

**Fix:** Either (a) build the H1 vector fixture with strictly-distinct scores so order is
total and parity is meaningful, or (b) compare as **sets** for ties and assert order only on
the strictly-ordered prefix, and document that tie order is an unspecified detail the seam
does not promise. Note: this is _not_ the f32→f64 issue (that one is fine — see C-2); it is
orthogonal and the plan currently conflates "order" into one H1 bullet.

---

### [MEDIUM] M-b — in-memory pool **collapses reads onto the write mutex**; D2's "never blocks the executor" and H8's routing claim need the in-memory caveat stated

**Plan:** §3 "Guard acquired inside the closure"; D2 "never blocks the executor"; H8 "**File-backed**
concurrent-read routing test (in-memory collapses both conns and would mask a mis-route)."

**Reality:** `ConnectionPool::read()` in in-memory mode (`read_pool_size == 0`) acquires the
**write** connection's `Mutex` (`pool/connection_pool.rs:225-230`,
`ReadConn::InMemory(WriteAsReadGuard)`). The plan correctly identifies this for H8 (good —
that claim checks out). But two consequences are unstated: (1) a long-running
`for_each_streamed` scan in in-memory mode holds the _write_ mutex for the whole stream, so
a concurrent `block_write` on the same backend serializes behind it — the cap-1 backpressure
means the stream can be slow (bounded by the async drainer / callback), extending the
write-lock hold. This is _not_ a deadlock but it is a contention behavior the "never blocks
the executor" line glosses. (2) The H8 test is correctly scoped to file-backed — confirm it,
but also assert the in-memory path _works_ (doesn't deadlock) under a read-during-write
pattern, since that is the default test harness mode.

**Fix:** Add one sentence to the D2 rustdoc/§3: "in in-memory mode `pool.read()` takes the
write mutex, so a streamed scan and a concurrent write serialize (no deadlock; bounded by
the cap-1 drain)." Keep H8 file-backed but add an in-memory no-deadlock smoke assertion.

---

### [MEDIUM] M-c — `schema_version()` String→u32 parse failure returns **`Migration`**, not the variant H10/T6 implies; pin it

**Plan:** T6 "`schema_version` (String→u32 parse)"; H10 catch "`string→u32` parse."

**Reality:** the underlying read parses the config string and on failure returns
`MigrationError::Incompatible` → `MemoryError::Migration` (`store/schema/mod.rs:204-206`,
mirrored at `:325-327` for the validate path). The trait `schema_version()` returns
`Result<u32>` (`storage/schema.rs:33`). So a corrupt `schema_version` value surfaces as
`Migration`, and — per D4 — `Migration` is a semantic variant that **passes through**
unchanged (not remapped to `Storage(Backend)`). The plan's H10 "`string→u32` parse" bullet
doesn't say which variant it asserts. For error fidelity (the spirit of H4) it should pin
**`Migration`**, and confirm `map_seam_err` does **not** opacify it (it won't — only
`Database` is remapped; see C-1).

**Fix:** H10/T6: assert a corrupt `schema_version` config value ⇒ `MemoryError::Migration`
(passes through the seam), citing `store/schema/mod.rs:204`.

---

### [LOW] L-a — Documentation/Testing/Verification sections are present and substantive — minor structural nit only

**Confirmed present:** §7 Verification (the full feature-matrix build/test/clippy/fmt + the
`git diff --stat src/engine/` H5 gate + the "no `rusqlite`/`Connection` in `pub`
signatures" grep), §8 Documentation (module rustdoc, crate-layout, CLAUDE.md status, ADR
N/A with rationale, Sphinx N/A with rationale), §10 Testing (positive differential proof,
error-variant fidelity, file-backed routing, feature-matrix, e2e/bench N/A with rationale).
This is a structurally complete plan — no missing section. **Nit:** §6 is "(reserved)" and
empty; either fill or delete to avoid a reader hunting for content. The N/A justifications
are concrete, not hand-wavy (ADR N/A correctly notes the seam ADR is a separate spec
deliverable; bench N/A correctly defers to memarch-bench).

---

### [LOW] L-b — circularity of the differential proof is real but _partially_ acknowledged; tighten the wording

**Plan:** §10 "assert results **identical** to the concrete-store/free-fn oracle on a shared
fixture (identity holds **because the SQL is reused**)."

**Assessment:** the plan is honest that identity holds _because the same SQL runs on both
sides_ — which is exactly the circularity: a bug in the shared SQL passes on both sides. The
plan does **not** fully spell out the residual risk or its mitigation. The residual risk is:
the differential proof catches **wrapper/adapter** bugs (wrong conn selection, dropped
filter dimension, wrong error remap, broken streaming, f32→f64 mistakes, scope-slice
inversion) — i.e. everything the _adapter_ adds — but is **blind to** any bug already in the
delegated SQL/free-fn (those are pre-existing and out of #630's scope) **and** blind to any
divergence in _policy the adapter must re-implement rather than delegate_ (HNSW build/
dispatch — B1 — is the prime example: there is no shared code to make both sides agree, so
"identity because SQL is reused" does not hold there). T2 partially mitigates by adding "a
hand-built SQL oracle for the asserted-absent dimensions," which is the right instinct.

**Fix:** Add one explicit paragraph to §4 or §10: "The differential proof is sound for
delegated behavior (the adapter and oracle share SQL) but **provides no coverage where the
adapter re-implements rather than delegates** — HNSW build/dispatch policy (B1) and any
fail-loud projection in `convert.rs`. Those require an **independent** oracle (the engine's
own `should_use_hnsw`/build condition for HNSW; a hand-built SQL/expected-set for
`convert.rs`), not a self-comparison." This makes the one genuine hole (B1) visible in the
proof strategy itself.

---

## Claims that CHECK OUT (confirmed against code — no change needed)

- **C-1 / D4 error mapping is correct.** `StorageError` doc (`error.rs:345-368`) explicitly
  states driver errors map to an opaque `String` at the seam and "the existing `Database`
  variant stays for `SQLite`-internal use" — so the seam **must** remap `Database` →
  `Storage(Backend)` and pass semantic variants through. The plan's `map_seam_err`
  (lines 83–89) matches **only** `MemoryError::Database(e)` and remaps it; every other
  variant (`NotFound`, `Migration`, `EmbeddingDimension`, `Conflict`, `ReadOnly`,
  `Internal`, `UnsupportedEpoch`, `Pool`, `Serialization`) flows through untouched. This is
  exactly what #629 intends. **No "should-be-NotFound raw Database" leak found:** I checked
  the high-risk methods — `FactStore::get(missing)` returns `NotFound` (`store/facts.rs:228`,
  not a raw `Database`); the importance/access mutators return `NotFound` on zero rows
  affected (`facts.rs:409/425/442/714/732`); `marker_key` returns
  `Conflict(QueryValidation)` (`facts.rs:991-995`). None of these would be wrongly opacified
  by the blanket `Database` remap, because none of them emit a raw `Database` for a semantic
  condition. The D4 remap is safe as written. (The single-point enforcement in `map_seam_err`
  is the right call — it makes per-method omission impossible.)

- **C-2 / f32→f64 widening is order-safe and value-exact.** `SearchIndex` returns
  `Vec<(i64, f64)>` (`storage/search_index.rs:38/50`); the concrete `VectorResult.score` is
  `f32` (`search/vector.rs:13`). The engine's own single-channel path already widens with
  `f64::from(r.score)` (`search/hybrid.rs:196`), which is value-exact (every f32 → f64
  losslessly) and monotonic, so widening cannot reorder. The backend doing the same widening
  is correct. RRF (`hybrid.rs:117`) fuses by **rank only**, so even if scores differed the
  fusion would be invariant — but they don't. The plan's H1 "f32→f64 order" catch targets
  the right thing and will pass. (The _separate_ tie-ordering issue is M-a.)

- **C-3 / the non-uniform `scope_ids` empty-slice contract is preserved by passthrough.**
  `fts_search`/`vector_search`/`fts_count_expired` take `scope_ids: Option<&[i64]>` and
  honor `Some(&[]) ⇒ matches-nothing` vs `None ⇒ filter disabled` — verified by the existing
  tests `fts_search_empty_scope_slice_matches_nothing` (`fts.rs:301-317`) and
  `vector_search_empty_scope_slice_matches_nothing` (`vector.rs:331-353`), and the SQL
  `(?3 IS NULL OR scope_id IN (json_each(?3)))`. `FactFilter.scope_ids: Option<Vec<i64>>`
  uses the **same** `Some(empty)=matches-nothing` convention (`storage/filter.rs:23-27`,
  explicitly "A backend MUST NOT normalize `Some(empty)` to no-filter"). So `convert.rs`
  projecting `filter.scope_ids.as_deref()` into the free fns is faithful, and
  `serialize_scope_ids` (`search/mod.rs:32`) maps `None→None`, `Some(&[])→Some("[]")`
  correctly. The plan's §3 convert.rs claim ("preserved by passing the slice straight
  through to `serialize_scope_ids`") is correct. **Note the trap the plan navigates well:**
  the `FactGraph` `&[i64]` methods use the _opposite_ convention (empty = ALL for 7 of them,
  per `storage/graph.rs:30-47`), so the H2 parametric table (empty-slice meaning per method)
  is genuinely necessary and correctly scoped. Confirmed the doc at `graph.rs:30-47` lists
  the 7 empty=ALL, 3 empty=NONE, 1 `Option` split exactly as H2 says.

- **C-4 / `marker_key` guard.** `list_active_by_metadata_key_recent` rejects a
  non-`[A-Za-z0-9_]+`/empty key with `Conflict(QueryValidation)` in **all** build profiles
  (`store/facts.rs:986-996`), and the key is interpolated (not bound) into the JSON path —
  so H3's reject-set + happy-path parity is the right test and the variant assertion is
  correct.

- **C-5 / ReadOnly preservation via `try_write`.** `ConnectionPool::try_write()` returns
  `MemoryError::ReadOnly` on a read-only pool (`pool/connection_pool.rs:271-274`), and
  `block_write` (plan lines 104–112) uses `try_write()?`, so DD#6 is preserved for free on
  every write method. H7's "each mutator ⇒ `ReadOnly`" is correct. The pool API the plan
  assumes (`read() -> Result<ReadConn>`, `try_write() -> Result<MutexGuard>`) exists exactly
  as used.

- **C-6 / engine stays untouchable & default build is runtime-free.** `MemoryEngine { pool:
ConnectionPool }` by value (`engine/mod.rs:157`); `default = []` (no async/ann) in
  `Cargo.toml`, so gating `storage::sqlite` behind `#[cfg(feature = "async")]` (D1) genuinely
  compiles it out of the default build — zero tokio, confirmed. The concrete stores are
  `&Connection`-scoped and transient (`FactStore<'a> { conn: &'a Connection }`,
  `store/facts.rs:11-12`), which validates the delegation-not-absorption rationale (§1)
  and the "engine untouched" achievability: nothing in the additive `storage/sqlite/*`
  subtree forces an `src/engine/` edit, because the stores it delegates to are constructed
  per-call from a borrowed connection — **except** the HNSW policy (B1), which is the one
  place the backend cannot stay engine-independent without copying `search_config` logic.
  `archive_manifest.list()` orders `BY created_at` ascending = oldest-first (T7 claim
  correct, `store/archive_manifest.rs:65-70`). `migrate` returns `UnsupportedEpoch` for a
  future epoch (`store/schema/mod.rs:215-219`), validating the H10 epoch assertion.

---

## Bottom line

The plan is **strong** on the parts most likely to leak — D4 error remapping, the
non-uniform scope-slice contract, marker_key, NotFound, f32→f64, ReadOnly — all verified
correct against the code, and its differential-proof instinct (ship parity tests in the
same commit; fail-loud `convert.rs`; file-backed for routing) is right. The **one structural
hole** is HNSW (B1/H-a): the build-and-dispatch _policy_ lives in the engine's
`search_config`, the backend struct omits it, and the H9 recall test is constitutionally
blind to the divergence — so #630 can ship green while planting a recall-behavior change for
#631 to detonate. Fix B1 (carry `search_config` + replicate the gate, and pin the dispatch
_boundary_ not just recall) and the "zero behavior change" claim becomes defensible. H-b
(trait-doc error menu omits `Internal`) and M-c (pin `Migration` on schema-version parse)
are cheap error-fidelity tightenings. H-c (streaming error-precedence) and M-a (tie-order
nondeterminism) are real but bounded. The Documentation/Testing/Verification scaffolding is
complete and concrete.

**Severity tally:** 1 BLOCKER (B1), 3 HIGH (H-a, H-b, H-c), 3 MEDIUM (M-a, M-b, M-c),
2 LOW (L-a, L-b). 6 load-bearing claims confirmed correct (C-1…C-6).
