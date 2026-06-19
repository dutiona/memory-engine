# Design Spec — Pluggable `StorageBackend` Abstraction (SQLite + PostgreSQL)

- **Date:** 2026-06-19
- **Status:** Approved (brainstorming) → pending implementation plan
- **Author:** Michaël Roynard
- **Scope:** A+B+C+D (single comprehensive spec, internally phased for incremental delivery)

---

## 1. Context & Motivation

`memory-engine` is an embedded Rust library: it runs **in-process**, exposes the
single `MemoryEngine` facade, and persists everything through **SQLite**
(`rusqlite` 0.34, `bundled-full`). Today there is **no storage abstraction** —
the persistence layer is wired directly to SQLite:

- ~219 inline SQL string literals spread across `src/store/*` and engine modules.
- ~42 files reference `rusqlite` directly.
- The concrete store structs (`FactStore<'a>`, `EdgeStore<'a>`, …) wrap
  `&rusqlite::Connection`; they are **not** traits.
- SQLite-specific features are load-bearing: **FTS5** (`MATCH` + `bm25()` +
  `porter unicode61` + external-content sync triggers), JSON1 (`json_each`,
  `json_type`, `json_extract`), RFC3339-as-TEXT timestamps relied upon for
  **lexicographic == chronological** ordering, WAL pragmas, the read-only
  open path, and a custom read-pool/write-mutex `ConnectionPool`.

### The driving requirement

A prospective adopter standardizes on **PostgreSQL** across their stack and
intends to deploy and scale in the cloud. They do not want to operate a second
database technology (SQLite) alongside Postgres. We want to support pluggable
DB backends so such an adopter can run `memory-engine` on Postgres, while
**SQLite remains the default** in-process backend.

### What the research established (see Appendix A)

- PostgreSQL **has** full-text search (`tsvector`/`tsquery`/GIN), but **not
  BM25**: `ts_rank`/`ts_rank_cd` use **no corpus-level statistics** (no IDF), no
  term-frequency saturation, and length normalization is off by default —
  Postgres docs state "the ranking functions do not use any global information."
- Real BM25 in Postgres requires an **extension** (`pg_search`/ParadeDB,
  `pg_textsearch`/Tiger, `VectorChord-bm25`). **None are installable on managed
  cloud Postgres** (AWS RDS/Aurora, GCP Cloud SQL/AlloyDB, Azure Flexible
  Server) — those allow only a curated allowlist; BM25 extensions have not
  cleared it anywhere mainstream (Neon deprecated `pg_search` for new projects
  2026-03).
- **`pgvector` IS on every managed allowlist** — so server-side vector search is
  available even on stock managed Postgres; **BM25 lexical search is the only
  capability that needs a self-hosted bundle**.
- Our hybrid retrieval merges via **Reciprocal Rank Fusion (RRF)**, which
  consumes **rank order only, not raw scores**. This means a ranking-quality
  difference between `bm25()` and `ts_rank_cd` does **not** break the API
  contract — it is a measurable quality property, not an incompatibility.

### Decision: "meet in the middle"

Support Postgres in **two tiers**, plus SQLite, behind one abstraction:

1. **SQLite** (default, in-process) — `bm25()` baseline.
2. **Postgres, bundled-BM25** — adopter self-hosts our reference
   `postgres + bm25` Docker image (or any Postgres with `pg_search`/
   `pg_textsearch` installed). True BM25 parity with SQLite.
3. **Postgres, stock managed** — RDS/Cloud SQL/Azure with `tsvector` +
   `ts_rank_cd` + `pgvector`. Lexical retrieval is degraded (no IDF); vector
   retrieval is first-class via `pgvector`.

We **do not silently degrade**: an adopter who requires BM25 sets
`LexicalMode::RequireBm25` and gets a **fail-fast at open** if the extension is
absent. We **measure** the degradation (piece D) rather than asserting it.

---

## 2. Goals / Non-Goals

### Goals

- A clean `StorageBackend` trait family that isolates _all_ persistence/dialect
  concerns from the engine. No SQL or driver type leaks past the seam.
- SQLite refactored behind the traits with **zero behavior change** (existing
  test suite stays green — the safety property).
- A Postgres backend (both lexical tiers) implementing the same traits.
- A reference Docker bundle proving Postgres-with-BM25, plus a managed-PG guide.
- A retrieval-quality benchmark quantifying BM25 vs `ts_rank_cd` vs SQLite.
- SQLite-only users pull **zero** Postgres dependencies.

### Non-Goals

- Removing or deprioritizing SQLite. It stays the default, in-process backend.
- A general-purpose JSON-path/predicate query language. `FactFilter` is a
  _closed_ set matching exactly today's query shapes (YAGNI).
- ORM/query-builder adoption. Each backend owns its SQL.
- Cross-backend byte-for-byte lexical parity (impossible: tokenizers differ).
- Multi-writer / distributed coordination beyond what each engine provides.

---

## 3. Decisions Summary (the four forks)

| #   | Decision                | Choice                                                            | Rationale                                                                                                                    |
| --- | ----------------------- | ----------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| 1   | I/O model               | **Async-native** (`async_trait`)                                  | The cloud/async adopter is the reason the PG backend exists; SQLite wraps sync rusqlite in `spawn_blocking` inside its impl. |
| 2   | Trait granularity       | **Bounded-context traits + `StorageBackend` umbrella supertrait** | One handle for the engine; cohesive, independently-mockable units.                                                           |
| 3   | Vector search placement | **Backend responsibility** (returns ranked ids)                   | Lets Postgres use `pgvector` (managed-available); symmetric with lexical; RRF stays engine-side.                             |
| 4   | Spec scope              | **A+B+C+D in one spec**, internally phased                        | User decision; implementation lands incrementally via the issue tree.                                                        |

Supporting decisions: lexical/vector channels return **ranked `Vec<i64>`**
(RRF needs order, not scores); a closed **`FactFilter`** replaces inline JSON1;
timestamps cross the seam as `chrono::DateTime<Utc>` (the lexicographic-TEXT
trick becomes a SQLite-private detail); errors extend the typed `MemoryError`
with a `StorageError` sub-enum.

---

## 4. Section 1 — Module Layout & Trait Family

A new `src/storage/` module holds the persistence **port**. This is an
_infrastructure_ abstraction, deliberately distinct from `src/traits.rs`, which
holds **consumer capability-injection** traits (`EmbeddingProvider`,
`SummaryGenerator`, `Reranker`, …). Conflating the two is how such seams rot.

Seven bounded-context traits, aggregated by one umbrella:

```rust
// src/storage/mod.rs  — internal persistence port (NOT consumer-facing)

/// Core knowledge graph: facts, edges, and the scope hierarchy that partitions
/// them. (today: store/facts.rs, edges.rs, scopes.rs)
#[async_trait] pub trait FactGraph: Send + Sync { /* insert/get/expire/list facts;
    add/list edges; create/resolve scopes; bi-temporal supersession */ }

/// Append-only event log — source of truth. (store/events.rs)
#[async_trait] pub trait EventLog: Send + Sync { /* append; read window; by-session */ }

/// Lexical + vector retrieval. Both return RANKED ids; RRF stays engine-side.
/// (search/fts.rs, vector.rs, ann.rs)
#[async_trait] pub trait SearchIndex: Send + Sync {
    async fn lexical_search(&self, q: &str, f: &FactFilter, k: usize) -> Result<Vec<i64>>;
    async fn vector_search (&self, e: &[f32], f: &FactFilter, k: usize) -> Result<Vec<i64>>;
}

/// Consolidation outputs: cluster/global summaries + wisdom lineage.
/// (store/summaries.rs, lineage.rs)
#[async_trait] pub trait ConsolidationStore: Send + Sync { /* upsert/list summaries; lineage */ }

/// Session & cognitive bookkeeping: activity stream + checkpoints.
/// (store/activities.rs, checkpoints.rs)
#[async_trait] pub trait SessionStore: Send + Sync { /* record activity; checkpoint cursor */ }

/// Lifecycle: open, capability probe, migrate, schema_version. (store/schema/*)
#[async_trait] pub trait SchemaManager: Send + Sync {
    async fn migrate(&self) -> Result<()>;
    async fn schema_version(&self) -> Result<u32>;
    fn capabilities(&self) -> BackendCapabilities;
}

/// Cold storage (.pak). Feature-gated. (store/archive_manifest.rs, archive/pak.rs)
#[cfg(feature = "archive")]
#[async_trait] pub trait ColdStorage: Send + Sync { /* archive/restore/manifest */ }

/// The single handle the engine holds.
pub trait StorageBackend:
    FactGraph + EventLog + SearchIndex + ConsolidationStore + SessionStore + SchemaManager {}
```

### Notes

- **Scopes fold into `FactGraph`** — `scope_id` is an FK on facts/events;
  scopes partition the graph rather than being an independent concern.
- **Lineage sits in `ConsolidationStore`** — it is Phase-5 wisdom provenance, a
  consolidation output.
- **`capabilities()`** is how the BM25-vs-ts_rank tier surfaces _without_
  leaking dialect (consumed by the engine and benchmark D).
- The umbrella gives the engine a single `Arc<dyn StorageBackend>`, while the
  bounded traits are what tests mock in isolation (e.g. a forgetting test mocks
  only `FactGraph`).

---

## 5. Section 2 — The `SearchIndex` Seam in Depth

The trait stays trivial (two methods → ranked ids); all variance hides inside
the impls.

### Lexical tiers (Postgres selects one at `open`)

```rust
// src/storage/postgres/lexical.rs
enum PgLexical {
    Bm25Search,     // pg_search:     WHERE col @@@ $1 ORDER BY paradedb.score(id) DESC
    Bm25TextSearch, // pg_textsearch: BM25 on standard GIN (PostgreSQL-licensed, PG17+)
    TsRankCd,       // stock:         WHERE tsv @@ websearch_to_tsquery($1)
                    //                ORDER BY ts_rank_cd(...) DESC
}

pub enum LexicalMode {
    Auto,        // probe: best BM25 available, else TsRankCd (logs a downgrade warning)
    RequireBm25, // fail-fast at open if no BM25 extension — the "no compromise" guard, opt-in
    StockOnly,   // force TsRankCd even if BM25 present (D benchmark's control arm)
}
```

### Capabilities (dialect-free tier signal)

```rust
pub enum LexicalRanker { Bm25, TsRankCd }

pub struct BackendCapabilities {
    pub lexical_ranker: LexicalRanker, // the "is retrieval degraded?" signal
    pub server_side_vector: bool,      // pgvector / FTS5-rust
    pub true_idf: bool,                // Bm25 ⇒ true, TsRankCd ⇒ false
}
```

### Three design decisions

1. **Query parsing is backend-owned.** The engine passes the **raw user
   string**; each impl parses to its dialect (`websearch_to_tsquery`, FTS5
   `MATCH`, `@@@`). No query syntax ever leaves a backend.
2. **RRF is untouched.** `src/search/hybrid.rs` stays pure engine-side logic over
   two ranked `Vec<i64>`. Because it fuses by rank, the BM25-vs-ts_rank
   difference is invisible to it — the property that lets all tiers satisfy one
   trait.
3. **Vector via `pgvector`** for Postgres (HNSW `vector` index, managed-
   available); brute-force / in-memory HNSW for SQLite. Both return ranked ids.

### Tokenizer parity (the one thing the seam cannot hide)

PG `english` strips stopwords + Snowball/Porter2; FTS5 `porter` keeps stopwords

- Porter1. The matched **row set** differs across backends regardless of
  ranking. Answer: **per-backend golden tests** (each backend asserts its own
  quality, never cross-backend identity), and the D benchmark **quantifies** the
  recall gap. A PG text-search config can optionally disable stopwords to narrow
  it — a tuning knob, not a correctness guarantee.

The `RequireBm25` fail-fast is how the "don't compromise on no-BM25" stance
survives as an explicit, opt-in contract.

---

## 6. Section 3 — Cross-Cutting Types

### `FactFilter` — replaces all inline JSON1 filtering

```rust
pub struct FactFilter {
    pub fact_type: Option<FactType>,
    pub scope_ids: Option<Vec<i64>>,        // was: json_each(?) IN-list
    pub ids:       Option<Vec<i64>>,        // was: id IN (json_each(?))
    pub temporal:  TemporalFilter,          // Active | AsOf(t) | ValidDue(now) | IncludeExpired
    pub pinned:    Option<bool>,
    pub metadata:  Vec<MetadataPredicate>,  // closed set, NOT arbitrary SQL
}

pub enum MetadataPredicate {                // exactly today's shapes — YAGNI on a general lang
    KeyAbsent(String),   // json_type(meta,'$.k') IS NULL   (dream_cycle marker)
    KeyPresent(String),
    KeyEquals(String, serde_json::Value),
}
```

SQLite translates to JSON1; Postgres to `jsonb` operators (`?`, `->>`, `@>`).

### Timestamps — lexicographic trick becomes SQLite-private

The trait deals only in `chrono::DateTime<Utc>`. SQLite serializes to padded
RFC3339 TEXT (preserving the `min`/`max` ordering at `facts.rs:197`); Postgres
uses native `timestamptz`. The string-ordering invariant stops being a
cross-cutting concern a contributor can break from the engine side.

### Error variants — extend typed `MemoryError` (continues the #560 arc)

```rust
pub enum StorageError {
    Backend(String),       // rusqlite::Error / tokio_postgres::Error mapped in, opaque out
    Migration(String),
    CapabilityUnavailable { needed: &'static str, backend: &'static str }, // RequireBm25 fail-fast
    Pool(String),
    Serialization(String), // embedding (de)serialize, json
}
```

### Async / object-safety

`async_trait` is required (not native AFIT): the engine holds
`Arc<dyn StorageBackend>` and native `async fn` in traits is not `dyn`-safe yet.
Cost: one boxed future per call — negligible against a DB round-trip. `Send +
Sync` carries over from the existing trait convention.

---

## 7. Section 4 — Backend Implementations

### `SqliteBackend` — the zero-change refactor (piece A, the risky part)

Existing concrete stores become the SQLite impl, reusing their SQL **verbatim**:

```
store/facts.rs (FactStore<'a>)        ──►  impl FactGraph for SqliteBackend
store/events.rs                       ──►  impl EventLog  for SqliteBackend
search/fts.rs + vector.rs + ann.rs    ──►  impl SearchIndex for SqliteBackend
... etc.
```

- `ConnectionPool` (`src/pool/connection_pool.rs`) stays as `SqliteBackend`'s
  private pool — read-pool + write-mutex + WAL + `read_only` guard survive.
- Async methods wrap sync rusqlite in `spawn_blocking` — the same mechanism the
  `async` feature uses today, moved behind the trait.
- The 11 migrations, `VACUUM INTO` backup, FK-rebuild hack — all preserved.
- **Safety gate:** the entire existing test suite stays green with
  `SqliteBackend` as the only backend. Behavior-preservation is _verified_.

### `PgBackend` — feature-gated, async-native (piece B)

```toml
[features]
backend-sqlite   = ["dep:rusqlite"]                               # default
backend-postgres = ["dep:tokio-postgres", "dep:deadpool-postgres"] # + pgvector support
```

- **Pool:** `deadpool-postgres` over `tokio-postgres`. MVCC ⇒ no single-writer
  dance; the read-pool/write-mutex complexity vanishes.
- **Migrations:** a _fresh_ PG chain (not a port of SQLite's 11). FK-rebuild
  hack disappears (`ALTER TABLE ADD CONSTRAINT` works); FTS sync becomes one
  `GENERATED ALWAYS AS (to_tsvector(...)) STORED` column instead of three
  triggers.
- **Lexical tiers** (§5): probe at open → `pg_search` / `pg_textsearch` /
  `ts_rank_cd`.
- **Vector:** `pgvector` (`vector(dim)` column + HNSW index) — a hard
  requirement of the PG backend (justified: managed-available everywhere).
- **`read_only`:** `default_transaction_read_only` / read-only role —
  application-level, replacing SQLite's file-open flags.

### Wire contract & asymmetry

`chrono::DateTime<Utc>` and `f32` embeddings define the inter-backend contract.
SQLite's hard problems (single-writer concurrency, FK-rebuild migration,
lexicographic-timestamp fragility) are non-problems in Postgres; Postgres's hard
problem (no built-in BM25) is a non-problem in SQLite. The abstraction lets each
backend solve what it is good at — the reason a shared-SQL approach would have
been wrong.

The embedding-fingerprint guard (#626) stays engine-side and backend-agnostic.

### Placement

`backend-postgres` is off by default. Whether `PgBackend` lives in-crate behind
the feature or as a separate `memory-engine-postgres` crate is an implementation
detail for the plan (the workspace already splits cli/mcp/embed).

---

## 8. Section 5 — Docker Bundle (C) & Benchmark (D)

### Piece C — Docker BM25 bundle + deploy docs

```
deploy/postgres-bm25/
  Dockerfile          # two documented bases (license-driven choice):
                      #   FROM paradedb/paradedb           → pg_search (Tantivy BM25)  [AGPL-3.0]
                      #   FROM postgres:17 + pg_textsearch → BM25 on GIN               [PostgreSQL lic.]
  init.sql            # CREATE EXTENSION pg_search|pg_textsearch; CREATE EXTENSION vector;
                      #   + bm25 index + GENERATED tsvector fallback
  docker-compose.yml  # one-command bring-up
  README.md           # + managed-PG guide: "on RDS/Cloud SQL you get the StockOnly tier"
```

**Licensing as a documented choice:** `pg_search` is most mature (real Tantivy
BM25) but **AGPL-3.0**; `pg_textsearch` is **PostgreSQL-licensed** but newer and
PG17+. AGPL here does **not** reach the adopter's application — `memory-engine`
talks to Postgres over the wire protocol ("mere use", not a derivative work);
the AGPL network-clause obligation falls only on whoever operates a _modified_
`pg_search`. But corporate AGPL policy is real, so shipping both bases lets the
adopter's legal posture decide.

### Piece D — retrieval-quality benchmark

```
benches/retrieval_quality/
  corpus + labeled query set (golden relevance judgments)
  harness runs the SAME queries through 4 configs and emits:
    | config                 | recall@10 | nDCG@10 | MRR |
    | SQLite  bm25()         |    ...    |   ...   | ... |  ← baseline
    | PG  pg_search BM25     |    ...    |   ...   | ... |
    | PG  ts_rank_cd (Stock) |    ...    |   ...   | ... |  ← the degradation, measured
    | PG  pgvector (vector)  |    ...    |   ...   | ... |
```

- `StockOnly` lets the ts_rank control arm run on the **same** PG instance as
  the BM25 arm — a clean A/B.
- Runs against `testcontainers`-spun Postgres (the C image) + in-memory SQLite,
  so CI can execute it.
- Metrics are **quality** (recall@k/nDCG/MRR), distinct from existing Criterion
  **speed** benches — separate harness/directory.
- Each row is labeled with `capabilities()` (self-documenting tier).
- Corpus/judgments: a small curated set to start (YAGNI on TREC-scale).
  `~/dev/memarch-bench` is a candidate home; the plan decides in-repo vs there.

---

## 9. Section 6 — Features, Phasing, Testing

### Cargo features

```toml
backend-sqlite   = [...]   # default; rusqlite
backend-postgres = [...]   # tokio-postgres + deadpool + pgvector
# compile_error! if neither enabled — at least one backend required.
ann      # SQLite in-memory HNSW (PG uses pgvector regardless)
archive  # gates ColdStorage on BOTH backends
```

### Internal phasing (A→D)

- **A — abstraction + SQLite (zero behavior change)**
  - A1 trait family + `FactFilter`/`BackendCapabilities`/`StorageError`
  - A2 `SqliteBackend` impl (existing SQL verbatim, behind traits)
  - A3 wire engine to `Arc<dyn StorageBackend>`
  - A4 cross-backend conformance suite + existing-suite-green gate
- **B — Postgres backend**
  - B1 pool + schema + migration chain
  - B2 `FactGraph`/`EventLog`/`ConsolidationStore`/`SessionStore` CRUD
  - B3 `SearchIndex`: lexical tiers + pgvector
  - B4 lexical-mode probe + `RequireBm25` fail-fast
- **C — Docker bundle + deploy docs** (C1 image/compose, C2 managed-PG guide)
- **D — retrieval-quality benchmark**
- **ADR** documenting the seam decision (`docs/design/adr/`)

### Testing strategy

- **A regression gate:** existing suite green with SQLite-behind-traits.
- **Cross-backend conformance suite (A4):** one parameterized battery running
  against `SqliteBackend` always and `PgBackend` via testcontainers when
  `backend-postgres` is on. Encodes the trait _contract_ once (e.g. "expire then
  query Active excludes it"; "bi-temporal AsOf returns the historical row") so
  both backends prove the same semantics.
- **Per-backend golden lexical tests:** tokenizer behavior legitimately differs;
  these stay per-backend, not cross-backend identity.
- **D benchmark:** quality gate / reporting.

---

## 10. Epic + Sub-Issue Tree

| Issue    | Title (Conventional-Commits)                                                         | `type:`  | `area:`   |
| -------- | ------------------------------------------------------------------------------------ | -------- | --------- |
| **Epic** | `feat(storage): pluggable StorageBackend abstraction (SQLite + Postgres)`            | epic     | — (spans) |
| A1       | `feat(storage): define StorageBackend trait family + FactFilter/capabilities/errors` | feature  | storage   |
| A2       | `refactor(storage): implement SqliteBackend behind traits (zero behavior change)`    | refactor | storage   |
| A3       | `refactor(core): wire engine to Arc<dyn StorageBackend>`                             | refactor | core      |
| A4       | `test(qa): cross-backend storage conformance suite`                                  | test     | qa        |
| B1       | `feat(storage): PgBackend pool + schema + migrations`                                | feature  | storage   |
| B2       | `feat(storage): PgBackend graph/event/consolidation/session CRUD`                    | feature  | storage   |
| B3       | `feat(retrieval): PgBackend SearchIndex — lexical tiers + pgvector`                  | feature  | retrieval |
| B4       | `feat(retrieval): lexical-mode probe + RequireBm25 fail-fast`                        | feature  | retrieval |
| C1       | `feat(build): Docker postgres+BM25 reference bundle`                                 | feature  | build     |
| C2       | `docs(docs): Postgres deployment guide (bundle + managed-PG tiers)`                  | docs     | docs      |
| D        | `test(qa): retrieval-quality benchmark across lexical tiers`                         | test     | qa        |
| ADR      | `docs(design): ADR — pluggable storage backend`                                      | docs     | docs      |

Dependency order: **A → B → {C, D}**; the ADR can land alongside A1. All
sub-issues linked to the epic via `addSubIssue` + `replaceParent: true`.

---

## Appendix A — Research: SQLite FTS5 vs PostgreSQL FTS (cited)

### A.1 Ranking math

- **SQLite FTS5 `bm25()`** — Okapi BM25, hard-coded `k1=1.2`, `b=0.75`,
  per-column weights, returns a **negative** score (smaller = better;
  `ORDER BY rank` ascending = best-first).
  Source: <https://www.sqlite.org/fts5.html#the_bm25_function>
- **PostgreSQL `ts_rank` / `ts_rank_cd`** — frequency/position-weighted (A/B/C/D
  labels); `ts_rank_cd` adds cover-density proximity. **No IDF, no TF
  saturation; length normalization off by default** (`normalization` bitmask:
  1=`/(1+log(len))`, 2=`/len`, 8=`/unique`, 16, 32=`/(rank+1)`).
  Docs (verbatim): _"the ranking functions do not use any global information, so
  it is impossible to produce a fair normalization"_ and _"The built-in ranking
  functions are only examples."_
  Source: <https://www.postgresql.org/docs/current/textsearch-controls.html>
- **BM25 vs ts_rank gap:** no corpus-level IDF (rare-term upweighting), no k1
  saturation (ts_rank ≈ linear in TF), optional/off length normalization.

### A.2 RRF (why the gap is tolerable)

RRF score per result = `1/(k + rank)` — uses **rank position only**, discards
raw score. Designed to fuse rankers with incommensurable score scales. A
`ts_rank`-ordered list is just another input ranker; the IDF-driven _order_
differences cause bounded recall loss on rare-term multi-word queries, covered
by the vector channel. (Cormack, Clarke, Büttcher 2009.)

### A.3 Tokenizer parity

- PG `english` config **removes stopwords** (Snowball checks stopwords before
  stemming) and uses **Snowball/Porter2**.
  Source: <https://www.postgresql.org/docs/current/textsearch-dictionaries.html>
- SQLite FTS5 `porter unicode61` **does not remove stopwords** (no built-in
  tokenizer applies a stopword list) and uses **original Porter1**.
  Source: <https://www.sqlite.org/fts5.html#tokenizers>
- ⇒ matched row sets differ; cross-backend identity is impossible.

### A.4 Sync model

- FTS5 external-content tables (`content=`, `content_rowid=`) kept in sync via
  AFTER INSERT/UPDATE/DELETE triggers using the special `'delete'` command;
  mismatched old-text leaves orphaned index entries.
  Source: <https://www.sqlite.org/fts5.html#external_content_tables>
- Postgres `GENERATED ALWAYS AS (to_tsvector(...)) STORED` (PG12+) — engine-
  maintained, cannot diverge; costs a stored tsvector column.
  Source: <https://www.postgresql.org/docs/current/textsearch-tables.html>

### A.5 Managed-cloud BM25 extension availability (2025/2026)

| Extension              | RDS/Aurora  | Cloud SQL | Azure Flex | License                         |
| ---------------------- | ----------- | --------- | ---------- | ------------------------------- |
| pg_search (ParadeDB)   | ❌          | ❌        | ❌         | AGPL-3.0                        |
| VectorChord-bm25       | ❌          | ❌        | ❌         | AGPL / ELv2 (pre-1.0)           |
| pg_textsearch (Tiger)  | ❌          | ❌        | ❌         | PostgreSQL (PG17+)              |
| pg_bestmatch.rs        | ❌          | ❌        | ❌         | Apache-2.0 (preprocessing only) |
| **tsvector + ts_rank** | ✅ built-in | ✅        | ✅         | core                            |
| **pgvector**           | ✅          | ✅        | ✅         | MIT                             |

Sources: AWS Aurora PostgreSQL extensions reference; GCP Cloud SQL extensions
docs; Azure Database for PostgreSQL Flexible Server extensions list; ParadeDB /
TensorChord / TigerData project docs; Neon `pg_search` deprecation note
(2026-03). Structural barrier: managed providers require pre-packaged, vetted
extensions; Tantivy-backed custom index access methods have not cleared that bar.

---

## Appendix B — Affected Files (current → target)

| Current                                               | Becomes                                                 |
| ----------------------------------------------------- | ------------------------------------------------------- |
| `src/store/facts.rs` (93 KB), `edges.rs`, `scopes.rs` | `impl FactGraph` (SQLite)                               |
| `src/store/events.rs`                                 | `impl EventLog` (SQLite)                                |
| `src/store/summaries.rs`, `lineage.rs`                | `impl ConsolidationStore` (SQLite)                      |
| `src/store/activities.rs`, `checkpoints.rs`           | `impl SessionStore` (SQLite)                            |
| `src/store/schema/*`                                  | `impl SchemaManager` (SQLite)                           |
| `src/store/archive_manifest.rs`, `archive/pak.rs`     | `impl ColdStorage` (SQLite, feature)                    |
| `src/search/fts.rs`, `vector.rs`, `ann.rs`            | `impl SearchIndex` (SQLite)                             |
| `src/search/hybrid.rs` (RRF)                          | **unchanged** — engine-side                             |
| `src/pool/connection_pool.rs`                         | `SqliteBackend` private pool                            |
| `src/engine/*` (direct `rusqlite`)                    | call `Arc<dyn StorageBackend>`                          |
| — (new)                                               | `src/storage/{mod,sqlite/*,postgres/*}.rs`              |
| — (new)                                               | `deploy/postgres-bm25/*`, `benches/retrieval_quality/*` |
