# Design: Embedding model identity + TEI/Qwen migration

**Status:** Approved design (2026-06-16)
**Epic:** `epic(embedding): model-identity policy + TEI/Qwen migration`
**Related ADR:** [0015 — Cross-layer embedding-identity & mismatch parity policy](adr/0015-cross-layer-embedding-identity-policy.md)

## Context

The embedding provider is on the **hot path**: every `add_fact` embeds content
([`ingest.rs`](../../src/engine/ingest.rs)), and every query embeds the query text
at the consumer layer ([MCP `tools/mod.rs`](../../memory-engine-mcp/src/tools/mod.rs)).
Today an `all-MiniLM` model is served via an OpenAI/Ollama-compatible
`/v1/embeddings` endpoint, consumed by the sync `HttpEmbeddingProvider`. Two problems
drive this work:

1. **Model fit.** We are moving to `Qwen/Qwen3-Embedding-0.6B` (multilingual + strong
   code retrieval + Matryoshka), served by **HuggingFace Text Embeddings Inference
   (TEI)** — a Rust, embedding-specialized server whose token-based **dynamic batching**
   coalesces the bursts from ~5 concurrent agent sessions into single forward passes.
   Qwen is **asymmetric**: queries take an instruction prefix, documents do not. The
   current `EmbeddingProvider::embed(text)` cannot express that distinction.

2. **Silent model mismatch.** The engine stores only `embed_dim` and validates _vector
   length_. Dimension is **necessary but not sufficient** identity: two different models
   at the same dimension pass the length check and then return vectors from incompatible
   spaces — silently wrong retrieval, the worst RAG failure mode. We need a persisted
   **model identity** and a hard mismatch error.

**No users yet** → no back-compat to preserve, no data migration. We can break any
existing shape cleanly.

## Goals / Non-goals

**In scope (Wave 0 + Wave 1):**

- Persisted embedding **identity fingerprint** + hard mismatch detection (no panic).
- Pre-computed-embedding submissions must declare a `model` that matches the fingerprint.
- Asymmetric embedding trait (`embed_query` / `embed_query_batch`).
- `HttpEmbeddingProvider` extended for TEI/Qwen: query instruction, MRL truncation,
  fingerprint reporting.
- Consumer wiring (MCP/CLI/config) + a gated TEI+Qwen smoke test.

**Out of scope (Wave 2, future):**

- Multiple embeddings per fact / multi-space coexistence. _(The registry **schema** + single-active
  API landed in #622 — schema v12→v13, the `embedding_spaces` table with a `status` enum and a
  partial-unique single-active invariant, `store::embedding_meta` reduced to a facade over it.
  End-to-end coexistence / query-across-spaces / promote-rollback remain Wave 2: #689.)_
- Background embedding reconstruction (shadow space → backfill → atomic promote). _(#623.)_
- Weight-hash identity (slug-level identity only for now).
- TEI `/info` authoritative-identity probe.

## Cross-layer policy (shared contract)

This is a **first-class constraint**: the embedding-identity and mismatch policy MUST
be identical across the **Memory** layer (`memory-engine`) and the **Knowledge** layer
(`knowledge-base`). KB today has a proven fingerprint schema (its `embed_spaces`
registry) but **no mismatch detection**; ME leads on mismatch and **adopts KB's field
names** to avoid drift. KB reaches parity via a separate tracked KB-repo track.

The canonical policy lives in [ADR 0015](adr/0015-cross-layer-embedding-identity-policy.md),
**redundantly authored in both repos**, pointing to both GitHub repositories.

**Identity tuple (canonical, both layers):**

| Field                 | Type          | Meaning                                                                 |
| --------------------- | ------------- | ----------------------------------------------------------------------- |
| `model`               | string (slug) | e.g. `Qwen/Qwen3-Embedding-0.6B` — operator-declared, not a weight hash |
| `provider`            | string        | e.g. `tei`, `ollama`, `openai`                                          |
| `dim`                 | int           | stored vector dimension (post-MRL)                                      |
| `matryoshka_base_dim` | int?          | native dim before MRL truncation; `None` if not truncated               |
| `element_type`        | string        | `float32` (room for `int8` later)                                       |

ME's single-active-space is a **degenerate case** of KB's multi-space registry: same
field names, same semantics, so ME can grow into multi-space (Wave 2) without a schema
divergence to reconcile. KB-internal lifecycle fields (`status`, `table_name`,
`chunk_count`, `chunk_strategy`) stay layer-specific.

**Mismatch rule (canonical, both layers):** two stores are compatible **iff** their
identity tuples are equal; otherwise the operation is **hard-rejected** (error, never
panic, never silent).

## Design

### 1. Embedding identity fingerprint (`area:core`, `area:storage`)

> **Status: delivered.** The type + trait method landed in #612 (`EmbeddingFingerprint` +
> `EmbeddingProvider::fingerprint()`). The persistence landed in **#613** (schema v11→v12):
> the `store::embedding_meta` typed boundary (`load`/`store`/`record_if_absent`) records the
> tuple on the first embedding write, the open path validates dim against it, and the bare
> `embed_dim` config key is dropped. Mismatch **enforcement** remains #614 (the seam
> `record_if_absent` extends).

- New `EmbeddingFingerprint { model, provider, dim, matryoshka_base_dim, element_type }`.
- New trait method `EmbeddingProvider::fingerprint(&self) -> EmbeddingFingerprint` so
  providers declare their identity.
- DB meta stores an `embedding_meta` record (replacing the bare `embed_dim`), written on
  **first embedding write**; `schema_version` bump (no data migration — no users).

### 2. Mismatch detection (`area:core`)

- New `MemoryError::EmbeddingModelMismatch { expected: EmbeddingFingerprint, actual: EmbeddingFingerprint }`.
- Enforced at the embedding boundary (ingest + query) by comparing
  `provider.fingerprint()` against the stored `embedding_meta`. Optional eager check at
  MCP server startup for fail-fast.

### 3. Pre-computed embedding policy (`area:mcp`)

- `memory_add_fact` / `memory_query` pre-computed `embedding` submissions MUST carry a
  declared `model` (and the rest of the tuple as available); the engine checks it against
  the fingerprint and hard-rejects mismatch. Closes the `PassthroughEmbedder` hole where
  a same-dim foreign vector would otherwise slip through.

### 4. Asymmetric embedding trait (`area:core`)

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;                 // document
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> { /* default loop */ }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> { self.embed(text) }          // NEW
    fn embed_query_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {              // NEW
        texts.iter().map(|t| self.embed_query(t)).collect()                            // loop embed_query
    }
    fn fingerprint(&self) -> EmbeddingFingerprint;                                       // NEW
}
```

Defaults make `embed_query` degrade to document semantics, so symmetric providers
(`Mock`, `Hash`, `Passthrough`) are unaffected. **The core never embeds a query** — query
embedding happens only at the consumer layer (MCP/CLI), because `MemoryQuery` takes a
pre-computed vector. So the core's document-only call sites need no change.

**`embed_query_batch` defaults to looping `embed_query`, not delegating to `embed_batch`**
(corrected from an earlier draft during #616 review). This mirrors how `embed_batch` loops
`embed`, and makes the default **correct by construction**: a provider that overrides only
`embed_query` (a query prefix) still gets prefixed batch queries, with no silent leak into
document space — the exact silent-mismatch failure this epic exists to prevent. Native-batch
providers (TEI) override `embed_query_batch` for a single round-trip: correctness is the
default, batch efficiency is the opt-in.

### 5. Provider extension — TEI/Qwen (`area:retrieval`)

`HttpEmbeddingProvider` already speaks OpenAI `/v1/embeddings` = TEI's API verbatim. Add:

- `query_instruction: Option<String>` — Qwen's query-only prefix (default:
  `"Instruct: Given a search query, retrieve relevant memory facts.\nQuery: "`).
  `embed_query*` prepend it; `embed*` (documents) stay prefix-free.
- `mrl_dim: Option<usize>` — when set, truncate the returned vector and **renormalize to
  unit L2**. Lives provider-side so the engine `dim` matches stored vectors.
- `fingerprint()` → `{ model, provider: "tei", dim, matryoshka_base_dim, element_type: "float32" }`.

### 6. Wiring & concurrency

- MCP query path: `embed(text)` → `embed_query(text)`; `add_fact` stays on `embed`.
- CLI: query subcommands → `embed_query`; ingest/bootstrap stay on `embed`/`embed_batch`.
- Config (MCP + CLI): `model`, `provider`, `query_instruction`, `dim`, `mrl_dim`.
- **Keep the sync trait + `spawn_blocking`.** Each session already embeds on its own
  blocking thread with an independent HTTP call; the prior serialization was **server-side**
  (Ollama has no batching). TEI's server-side dynamic batching fixes throughput with no
  client coalescing.

### 7. Migration

**None.** No users → start clean: configure ME with Qwen/TEI, fresh DB adopts the new
fingerprint on first write. The lossless re-embed path (content is stored alongside the
vector — `facts.content` is source-of-truth, `facts.embedding` is derived) is deferred to
Wave 2 as reusable tooling for _future_ model swaps.

## Wave 0 — validation gate (smoke test)

Runs **before** Wave 1, using the _existing_ provider + a _manually_ prefixed query (the
new code does not exist yet). Two orthogonal axes, independently validatable:

**Model axis (no new infra).** Ollama can serve the model today via the existing
`HttpEmbeddingProvider` Ollama path — `ollama pull dengcao/Qwen3-Embedding-0.6B:Q8_0`
(or `:F16`). This lets W0.1 start immediately, decoupled from any TEI standup:

1. The model returns **1024-dim** vectors over the existing endpoint.
2. A hand-prefixed query measurably **out-retrieves** an unprefixed one on a tiny labeled
   set — proof the asymmetry is worth building.

**Server axis (TEI standup).** Orthogonal throughput validation:

3. TEI serves the same model over the OpenAI API and its server-side dynamic batching
   absorbs ~5 concurrent embed bursts with no client change.

The harness it leaves behind is promoted into the Wave 1 gated integration test (which
additionally asserts the prefix is applied by `embed_query` and that a mismatch is
hard-rejected). Note the `Q8_0`/`F16` Ollama quantizations are for _testing_; the
production target is Qwen3-0.6B on TEI.

## Wave breakdown / issue map

- **Wave 0 (gate):** premise-validation spike (TEI+Qwen).
- **Wave 1 (today):** identity fingerprint → mismatch detection → pre-computed policy →
  `embed_query` → TEI/Qwen provider → MCP/CLI wiring → shared ADR → gated smoke test.
  _Guardrail sub-issues land before the swap so the swap is protected as it lands._
- **Wave 2 (future):** multi-embedding registry → background reconstruction
  (shadow→backfill→promote) → HNSW rebuild + similarity-edge invalidation → real-time
  concurrency-safety research.

See the epic for the live sub-issue list and dependency links.

## References

- KB prior art: `embed_spaces` registry + `embed_swap.py` reconstruction
  (`re_embed` / `create_space` / `backfill_space` / `promote_space`) in
  [dutiona/knowledge-base](https://github.com/dutiona/knowledge-base). KB has **no**
  mismatch detection — the parity gap this work closes on the ME side.
- [Qwen3-Embedding-0.6B](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) (Apache-2.0,
  MTEB-Code 75.41, MRL 32–1024, asymmetric query instruction).
- [HuggingFace Text Embeddings Inference](https://github.com/huggingface/text-embeddings-inference)
  (Rust, token-based dynamic batching, OpenAI-compatible `/v1/embeddings`).
- [ADR 0015 — Cross-layer embedding-identity & mismatch parity policy](adr/0015-cross-layer-embedding-identity-policy.md)
