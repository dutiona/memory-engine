# ADR 0015: Cross-layer embedding-identity & mismatch parity policy

**Status:** Accepted (2026-06-16)
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
> authoritative over the other; they are mirrors of one contract.

## Context

Both layers store vectors produced by an embedding model alongside the source text the
vector is derived from. An embedding is only meaningful within the vector space of the
**exact model** that produced it. Mixing spaces — querying a store built with model A
using model B — yields cosine similarities that are numerically valid but semantically
meaningless: **silent, undetected retrieval corruption**, the worst RAG failure mode.

Prior state across the two layers:

- **Knowledge layer** already records embedding-space metadata (its `embed_spaces`
  registry) but performs **no model-identity validation** — only a runtime _dimension_
  check at the provider. Dimension is **necessary but not sufficient**: two different
  models at the same dimension pass the length check and corrupt retrieval silently.
- **Memory layer** stored only `embed_dim` and likewise validated only vector length.

Because the layers belong to one cognitive architecture and will eventually exchange and
co-host embeddings, their identity and mismatch policy MUST be identical. Drift between
them reintroduces exactly the silent-corruption risk this ADR removes.

## Decision

### 1. Canonical identity tuple

An embedding store records an **identity tuple** with these fields and semantics in both
layers (field **names** are normative to prevent drift):

| Field                 | Type            | Meaning                                                                                                        |
| --------------------- | --------------- | -------------------------------------------------------------------------------------------------------------- |
| `model`               | string (slug)   | Model identity, e.g. `Qwen/Qwen3-Embedding-0.6B`. Operator-declared; **not** a weight hash (see Consequences). |
| `provider`            | string          | Serving backend, e.g. `tei`, `ollama`, `openai`, `onnx`.                                                       |
| `dim`                 | integer         | Stored vector dimension (post-truncation).                                                                     |
| `matryoshka_base_dim` | integer \| null | Native model dimension before MRL truncation; `null` if untruncated.                                           |
| `element_type`        | string          | Vector element storage type: `float32` (reserved: `int8`).                                                     |

A single-active-space store (Memory layer today) records exactly one identity tuple. A
multi-space store (Knowledge layer; Memory layer Wave 2) records one tuple per space; the
tuple is the **degenerate single case** of the multi-space registry — identical field
names and semantics, so neither layer diverges as it grows into multi-space.

Layer-internal lifecycle fields (e.g. `status`, `table_name`, `chunk_count`,
`chunk_strategy`) are **not** part of the shared contract and remain layer-specific.

### 2. When the identity is recorded

The identity tuple is written on the **first embedding write** to a store and is
thereafter the store's fixed identity until an explicit, deliberate reconstruction
changes it atomically (future work; see §4).

### 3. Mismatch rule

Two identity tuples are **compatible iff they are field-by-field equal**. When a
configured embedding provider's identity does not equal the store's recorded identity,
the operation MUST be **hard-rejected**:

- A typed **error** is returned (Memory layer: `MemoryError::EmbeddingModelMismatch`;
  Knowledge layer: the equivalent typed error). **Never a panic, never a warning that
  proceeds, never silent.**
- Enforcement happens at the **embedding boundary** (ingest and query). An eager check at
  service startup is RECOMMENDED for fail-fast.
- **Pre-computed / caller-supplied embeddings** MUST carry a declared `model` (and the
  rest of the tuple as available) and are subject to the same rule; a bare vector with no
  declared identity is rejected when a store identity exists.

### 4. Reconstruction (forward-looking)

Changing a store's identity (model/dim swap) is only legitimate via an explicit
**reconstruction**: re-embed the stored source text under the new identity, then swap the
identity tuple **atomically** with the vectors and invalidate any cached similarity
artifacts (vector index, similarity edges/relations). Reconstruction is out of scope for
the initial implementation in either layer but, when built, MUST preserve the identity
tuple and mismatch rule above.

## Consequences

- **Slug-level identity, not weight-level.** `model` is operator-declared. It catches
  configuration mistakes (the realistic failure) but **not** a silently re-pulled model
  whose weights changed under the same slug, nor a server (e.g. TEI) that ignores the
  request's model field. A content/weight hash is a future hardening, additive to this
  tuple.
- **Parity is a maintenance obligation.** Both repos carry this file and must change it in
  lockstep. CI/audit in each repo SHOULD assert the two copies match.
- **Knowledge layer reaches parity via follow-up work** (it currently has no mismatch
  detection). Tracked in the `knowledge-base` repository, cross-referenced from this ADR.
- **No data migration in either layer for adoption** where the store has no users; the
  identity tuple is established on first write.

## References

- Memory-layer design: `docs/design/embedding-identity-and-tei-qwen-migration.md`
  (<https://github.com/dutiona/memory-engine>).
- Knowledge-layer prior art: the `embed_spaces` registry and `embed_swap.py`
  reconstruction (<https://github.com/dutiona/knowledge-base>).
