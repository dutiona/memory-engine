# ADR 0015: Cross-layer embedding-identity & mismatch parity policy

**Status:** Accepted (2026-06-16); amended 2026-11-16 (dutiona/memory-engine#1017: served artifact, pinned identity, per-store query instructions)
**Date:** 2026-06-16
**Scope:** Cross-layer — **Memory** layer and **Knowledge** layer

> **Redundant shared document.** This ADR is authored **identically** in both
> repositories and defines a **single shared policy** with which both layers maintain
> **complete parity**:
>
> - Memory layer: <https://github.com/dutiona/memory-engine>
>   (`docs/design/adr/0015-cross-layer-embedding-identity-policy.md`)
> - Knowledge layer: <https://github.com/dutiona/knowledge-base>
>   (`docs/design/adr/0015-cross-layer-embedding-identity-policy.md`)
>
> A change to this policy MUST be applied to **both** copies in lockstep. Neither copy is
> authoritative over the other; they are mirrors of one contract. Every issue reference
> below is qualified with its repository, because the two copies are byte-identical.

## Context

Both layers store vectors produced by an embedding model alongside the source text the
vector is derived from. An embedding is only meaningful within the vector space of the
**exact model** that produced it. Mixing spaces — querying a store built with model A
using model B — yields cosine similarities that are numerically valid but semantically
meaningless: **silent, undetected retrieval corruption**, the worst RAG failure mode.

The state before this policy:

- **Knowledge layer** recorded embedding-space metadata (its `embed_spaces` registry) but
  performed **no model-identity validation** — only a runtime _dimension_ check at the
  provider. Dimension is **necessary but not sufficient**: two different models at the same
  dimension pass the length check and corrupt retrieval silently.
- **Memory layer** stored only `embed_dim` and likewise validated only vector length.

A model slug alone does not pin a vector space either. The same slug served from another
file (another quantization), with another pooling or normalization, or with another input
limit gives other vectors. The identity therefore also records the **served artifact**.

Because the layers belong to one cognitive architecture and will eventually exchange and
co-host embeddings, their identity and mismatch policy MUST be identical. Drift between
them reintroduces exactly the silent-corruption risk this ADR removes.

## Decision

### 1. Canonical identity tuple

An embedding store records an **identity tuple** with these fields and semantics in both
layers (field **names** are normative to prevent drift):

| Field                 | Type            | Meaning                                                                                                        |
| --------------------- | --------------- | -------------------------------------------------------------------------------------------------------------- |
| `model`               | string (slug)   | Model identity, e.g. `Qwen/Qwen3-Embedding-0.6B`. Operator-declared.                                            |
| `provider`            | string          | Serving backend, e.g. `llama.cpp`, `tei`, `ollama`, `openai`, `onnx`.                                          |
| `dim`                 | integer         | Stored vector dimension (post-truncation).                                                                     |
| `matryoshka_base_dim` | integer \| null | Native model dimension before MRL truncation; `null` if untruncated.                                           |
| `element_type`        | string          | Vector element storage type: `float32` (reserved: `int8`).                                                     |
| `artifact_sha256`     | string \| null  | SHA-256, as 64 lowercase hex digits, of the single model file the provider serves (for llama.cpp, the GGUF). Operator-declared. |
| `pooling`             | string \| null  | Pooling the server applies, e.g. `last`.                                                                       |
| `normalization`       | string \| null  | Normalization of each output vector, e.g. `l2`.                                                                |
| `max_input_tokens`    | integer \| null | Maximum input length, in tokens, the server embeds.                                                            |

The last four fields are the **served artifact**. A store records all four or none; a
partially recorded served artifact is invalid.

Each store records one tuple per embedding space, with exactly one space active. The
Memory layer's single active space is the degenerate case of the Knowledge layer's
multi-space registry, with identical field names and semantics, so neither layer diverges
as it grows.

Layer-internal lifecycle fields (e.g. `status`, `table_name`, `chunk_count`,
`chunk_strategy`) are **not** part of the shared contract and remain layer-specific.

### 2. When the identity is recorded

The identity tuple, served artifact included, is written on the **first embedding write**
to a store and is thereafter the store's fixed identity until an explicit, deliberate
reconstruction changes it atomically (§4). A blank store records whatever its first write
declares, so a deployment declares every field, and a store refuses a pre-computed first
write whose declared identity differs from its configured provider's.

### 3. Mismatch rule

Two identity tuples are **compatible iff they are field-by-field equal**. An absent served
artifact equals only another absent one. When a configured embedding provider's identity
does not equal the store's recorded identity, the operation MUST be **hard-rejected**:

- A typed **error** is returned (Memory layer: `MemoryError::EmbeddingModelMismatch`;
  Knowledge layer: the equivalent typed error). **Never a panic, never a warning that
  proceeds, never silent.**
- Enforcement happens at the **embedding boundary** (ingest and query), and each store
  also checks at startup and MUST refuse to start when its configured identity differs
  from the recorded one.
- **Pre-computed / caller-supplied embeddings** MUST carry a declared `model` (and the
  rest of the tuple as available) and are subject to the same rule; a bare vector with no
  declared identity is rejected when a store identity exists, and a declaration that omits
  a field the store records does not match it.

### 4. Reconstruction

Changing a store's identity is only legitimate via an explicit **reconstruction**:
re-embed the stored source text under the new identity, then swap the identity tuple
**atomically** with the vectors and invalidate any cached similarity artifacts (vector
index, similarity edges/relations). Changing any served-artifact field (another model
file or quantization, pooling, normalization or maximum input length), or adopting a
served artifact on a store recorded without one, changes the identity and is a
reconstruction. A reconstruction re-embeds with a provider that reports exactly the target
identity. Reconstruction MUST preserve the identity tuple and mismatch rule above.

**Memory layer.** A generalized `fact_vectors(fact_id, space_id, embedding)` table holds
the **non-active** spaces' vectors (a `populating` space during a resumable, crash-safe
background backfill; the previous active space retained after a promote for rollback),
while `facts.embedding` stays the single active serving vector (no read-path change).
The promote is one transaction — a completeness gate _inside_ the transaction,
retain-old, an O(N) copy-swap of the new vectors into `facts.embedding`, then a
demote-then-activate registry flip. **The status flip _is_ the identity swap** (the
active row's tuple is the store's identity), so the identity and vectors swap
atomically. For a **same-dim** swap the in-process vector index (HNSW) is rebuilt before
the reconstruction returns (dutiona/memory-engine#624). **Different-dim** reconstruction
is also implemented (dutiona/memory-engine#742): because the engine's `embed_dim` is
frozen at open, a different-dim promote fences the handle and the consumer reopens the
engine at the new dim, which re-validates and rebuilds the index. Background
reconstruction is dutiona/memory-engine#623; its operator surface is
dutiona/memory-engine#689.

**Knowledge layer.** Each space has its own sqlite-vec (`vec0`) table, registered in
`embed_spaces`. A space goes through create → backfill → promote → deprecate → cleanup
(`src/knowledge_base/embed_swap.py`), with the same atomic-swap semantics in a different
physical layout: promoting a space flips the registry's active row together with the
table that serves queries.

### 5. Pinned identity (both layers)

Both layers serve and record this identity:

| Field                 | Value                                                              |
| --------------------- | ------------------------------------------------------------------ |
| `model`               | `Qwen/Qwen3-Embedding-0.6B`                                        |
| `provider`            | `llama.cpp`                                                        |
| `dim`                 | `1024`                                                             |
| `matryoshka_base_dim` | `null`                                                             |
| `element_type`        | `float32`                                                          |
| `artifact_sha256`     | `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439` |
| `pooling`             | `last`                                                             |
| `normalization`       | `l2`                                                               |
| `max_input_tokens`    | `2048`                                                             |

The served file is `Qwen3-Embedding-0.6B-Q8_0.gguf` (639,150,592 bytes) from the official
Hugging Face repository `Qwen/Qwen3-Embedding-0.6B-GGUF` at commit
`370f27d7550e0def9b39c1f16d3fbaa13aa67728`, the Q8_0 file the 2026-11-02 benchmark used.
The deployment verifies the installed file's SHA-256 against this value. 2,048 tokens is
well above the benchmark's 538-token chunks; dutiona/infra#77 confirms it fits in the
Mac's memory with the chosen `--parallel`.

### 6. Query instructions

Asymmetric models embed queries with an instruction and documents without one. Each store
pins its own instruction, verbatim below, and its embedding client prepends it to every
query automatically; callers never build it. Documents never carry an instruction, so
document vectors stay comparable across stores. The instruction is policy, not identity:
it is not recorded in the store, and changing it re-embeds nothing.

Notation: `\n` below is one line feed (U+000A), never the two characters `\` and `n`, and
each string ends with one space after `Query:`.

| Store           | Query instruction                                                               |
| --------------- | ------------------------------------------------------------------------------- |
| Memory layer    | `Instruct: Given a search query, retrieve relevant memory facts.\nQuery: `      |
| Knowledge layer | `Instruct: Given a search query, retrieve relevant document passages.\nQuery: ` |

## Consequences

- **Declared, not probed.** `model` and `artifact_sha256` are operator-declared, and each
  store compares the declared tuple with the recorded one. The served file's SHA-256 is
  checked at install, not probed at runtime (a canary-vector check is deferred), so a
  server that loads another file than declared, or ignores the request's model field, is
  not detected.
- **Both layers enforce it.** Each records the full tuple, compares it field by field and
  refuses to start on a mismatch.
- **Parity is a maintenance obligation.** Both repositories carry this file and must
  change it in lockstep. Audit in each repository SHOULD assert the two copies match.
- **Schema change, no data migration.** A store recorded before the served artifact
  existed keeps its tuple with the served artifact absent, which does not match a
  configuration that declares one; its operator keeps serving the recorded identity or
  reconstructs (§4). A new store starts blank and records §5 on its first embedding write.
- **Truncation is outside this ADR.** Embedding an input longer than `max_input_tokens` is
  each layer's ingest concern; this ADR only records the limit as identity.

## References

- Memory-layer design: `docs/design/embedding-identity-and-tei-qwen-migration.md`
  (<https://github.com/dutiona/memory-engine>).
- Knowledge-layer prior art: the `embed_spaces` registry and `embed_swap.py`
  reconstruction (<https://github.com/dutiona/knowledge-base>).
- The served artifact: <https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF>, commit
  `370f27d7550e0def9b39c1f16d3fbaa13aa67728`, file `Qwen3-Embedding-0.6B-Q8_0.gguf`.
- The serving deployment: dutiona/infra#77.
