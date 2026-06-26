# ADR 0017: Cross-layer faithful-metric & derived-structure parity policy

**Status:** Accepted (2026-06-26)
**Date:** 2026-06-26
**Scope:** Cross-layer — **Memory** layer and **Knowledge** layer

> **Redundant shared document.** This ADR is authored **identically** in both
> repositories and defines a **single shared policy** with which both layers maintain
> **complete parity**:
>
> - Memory layer: <https://github.com/dutiona/memory-engine>
>   (`docs/design/adr/0017-cross-layer-faithful-metric-and-derived-structure-parity-policy.md`)
> - Knowledge layer: <https://github.com/dutiona/knowledge-base>
>   (`docs/design/adr/0017-cross-layer-faithful-metric-and-derived-structure-parity-policy.md`)
>
> A change to this policy MUST be applied to **both** copies in lockstep. Neither copy is
> authoritative over the other; they are mirrors of one contract.

This ADR is the **metric-level sibling of ADR-0015**. ADR-0015 pins **embedding
identity** — _which model's space a vector lives in_. ADR-0017 pins the **faithful
metric and the derived structures fitted on top of that space** — _how similarity is
computed within a space, and how every structure that inherits that similarity stays
fresh and parity-safe_. ADR-0015 makes two vectors comparable in principle; ADR-0017
makes the comparison itself a metric rather than a biased dot product, and binds the
two layers to compute it the same way when they co-host.

## Context

Both layers compute similarity between stored vectors and rank/retrieve/cluster on the
result. Today **both layers compute raw cosine everywhere** — there is no de-biasing,
no hubness correction, no spectral or transport operator in either codebase. Raw cosine
on a contextual embedding model is **not a faithful metric**: the model's representation
space is **anisotropic** — vectors are squeezed into a narrow cone, a few dominant
directions inflate every pairwise cosine, and a small set of **hub** points appear in
everyone's top-k regardless of relevance (Ethayarajh 2019; the anisotropy a textbook
whitening removes; the hubness phenomenon corrected by CSLS, Conneau-Lample et al. 2018).
The All-but-the-Top (ABT) `method` of §1 is one of the two whitening forms `W` may take;
its provenance is being confirmed in the E0 PoC before it enters a paper (see References).
The consequence is the same class of failure ADR-0015 removed at the identity level — **numerically valid,
semantically corrupt retrieval** — but it is _invisible_ to ADR-0015: two stores can
carry **identical identity tuples** and still rank differently because they de-bias the
space differently (or one doesn't de-bias at all).

The Knowledge layer is RAG-scoped: it cares about retrieval precision/recall, de-biased
similarity, hubness, query understanding, novelty/abstention, corpus
clustering/routing, cross-document contradiction and transfer, and dedup. It does **not**
model memory dynamics. Within that scope, every structure the layer builds on top of
similarity — `auto_relate.py`'s kNN similarity graph (brute-force, raw cosine, `0.82`
absolute cutoff), `folder_summaries.py`'s mean-pool centroids (with
`FOLDER_BOOST_FACTOR=1.15`), the search ranker, any future clustering/routing — is only
as faithful as the metric underneath it. A biased metric silently corrupts the graph
edges, the cluster boundaries, and the routing decisions that ride on it.

Two further forces make this a **cross-layer** contract rather than a KB-local choice:

1. **Co-hosting with a shared model.** When both layers run the same embedding model
   (Ollama `bge-m3` today; Qwen possible), they share the model's anisotropy. ADR-0015
   already declares they will exchange and co-host embeddings. A vector authored in one
   layer and read in the other MUST be compared under the **same** de-biasing map, or
   the comparison crosses two different linear maps and **stops being a metric** — a
   silent-corruption mode that ADR-0015's identity check cannot catch because the
   de-biasing map is not in the identity tuple.

2. **Derived structures and freshness.** Whitening, CSLS caches, similarity graphs,
   cluster labels, spectral bases, and any future diffusion/transport artifact are all
   **functions of the corpus state and the fit parameters**. They go stale on ingest,
   expiry, consolidation, model-swap — and, critically, on a **re-fit of the de-biasing
   map with no model change at all**. Without a disciplined freshness rule a derived
   structure "looks fresh, is corrupt."

This ADR leads **by the ideal**: it pins the faithful-metric form and the
derived-structure contract _first_, independent of what KB ships today (raw cosine,
zero de-biasing operators). The gap between the ideal and the current code is a
**gap-analysis concern** (see Consequences), not a constraint on the policy.

## Decision

### 1. Canonical faithful-metric identity

Within a single embedding space (identity tuple per ADR-0015), the **faithful
similarity** between two raw vectors `x, y` is **normatively** defined as the cosine
similarity of their de-biased, whitened images, corrected for hubness by CSLS:

```
faithful_sim(x, y) = CSLS_k( W (x − mu),  W (y − mu) )
```

The **sequence is fixed and ordered**: **mean-center → whiten → CSLS**. Each step is a
prerequisite for the next — CSLS hubness statistics computed on an un-whitened
anisotropic space measure the anisotropy, not the hubness, and a whitening fitted on a
non-centered cloud absorbs the mean into its rotation. The form is normative; the
**fit parameters are recorded per space** (they may differ across spaces, never across
the form):

| Field (concept) | Meaning                                                                                               |
| --------------- | ----------------------------------------------------------------------------------------------------- |
| `method`        | De-biasing method: `zca` (ZCA whitening) or `abt` (All-but-the-Top). The two legitimate forms of `W`. |
| `p`             | Number of top principal components removed/whitened (ABT `p`, or whitening rank cutoff).              |
| `csls_k`        | Neighborhood size `k` for the CSLS local-mean-density hubness correction.                             |
| `mu`            | The per-space mean vector subtracted before whitening (the mean-center step).                         |
| `W`             | The whitening matrix applied after centering (the whiten step).                                       |

Raw cosine is the **degenerate case** `method = none`, `mu = 0`, `W = I`, CSLS off — and
is explicitly **not** a faithful metric; it is named only so the policy can describe
KB's current state as a point on the same axis it intends to leave.

### 2. The `projection_matrices` table contract

The fitted de-biasing map is **persisted, versioned, and addressable** in a
`projection_matrices` table. This table is to the **whitened space** exactly what
`embed_spaces` is to **identity**: the single normative record of what makes the
space's metric faithful. Field **names are normative** (to prevent drift between the two
layers, mirroring ADR-0015 §1):

| Field              | Type          | Meaning                                                                                                   |
| ------------------ | ------------- | --------------------------------------------------------------------------------------------------------- |
| `space_id`         | id            | The whitened-space row this fit defines (the de-biased space the metric reads).                           |
| `raw_space_id`     | id            | The `embed_spaces` identity-tuple row whose raw vectors this fit was computed from.                       |
| `method`           | string        | `zca` \| `abt` (see §1). Normative vocabulary.                                                            |
| `p`                | integer       | Components removed/whitened (§1).                                                                         |
| `mu_blob`          | blob          | Serialized mean vector `mu` (§1).                                                                         |
| `matrix_blob`      | blob          | Serialized whitening matrix `W` (§1).                                                                     |
| `csls_k`           | integer       | CSLS neighborhood size `k` (§1).                                                                          |
| `fitted_at`        | timestamp     | When this fit was computed.                                                                               |
| `source_row_count` | integer       | Number of raw vectors the fit was estimated from (statistical sufficiency / RMT guard input).             |
| `fingerprint`      | string (hash) | The content fingerprint of the fit (see §3) — the freshness key every derived structure is keyed against. |

A space with a faithful metric has **exactly one current `projection_matrices` row**.
`(method, p, csls_k)` are the parameters that, together with `mu_blob`/`matrix_blob`,
fully reproduce `faithful_sim`. A consumer that has the raw vectors and this row can
recompute the metric deterministically; a consumer that has only the raw vectors and a
**different** row computes a **different metric** — which is the divergence §5 governs.

### 3. Freshness, epochs, and the derived-structure registry

Every structure fitted on top of the faithful metric — `W`/`mu` themselves, CSLS
caches, the kNN similarity graph, cluster labels, spectral bases, diffusion coordinates,
any future transport artifact — is **derived state** and MUST be invalidated when the
state it was fitted on changes. Two mechanisms are normative:

- **`SpaceEpochCounter`** — a monotonically increasing `u64` stored as a `config` row
  per space, **bumped INSIDE the reconstruction/promote transaction** (the same atomic
  swap ADR-0015 §4 governs). The bump rides the promote tx so that "epoch advanced" and
  "vectors/identity swapped" are the same atomic fact — there is no window in which a
  consumer can read a new epoch against old vectors or vice versa.

- **`DerivedStructureRegistry`** — a registry of every materialized derived structure,
  carrying both the **metric fingerprint** it was built against and a **`dirty_since`
  marker** flipped by the ingest/expire notifier on **every** corpus write. A structure is
  **fresh iff (its stored fingerprint equals the space's current fingerprint) AND (it has
  not been dirtied by a write since it was materialized)**; otherwise it is stale and MUST
  be refused or rematerialized before use. The two gates are orthogonal — the fingerprint
  catches a changed _metric_, the dirty marker catches changed _corpus content_ — and
  **both** are required.

The **metric fingerprint** is **not the epoch alone**. It is:

```
fingerprint = hash{ epoch, W_version, csls_k }
```

**Why epoch-alone is insufficient.** The epoch advances only on a reconstruction/promote
(model-swap or vector swap). But the faithful metric can change with **no reconstruction
at all**: **re-tuning `W`** (re-fitting whitening with a different `method`/`p`, or
re-estimating it on a grown corpus) produces a new linear map while the **epoch is
unchanged** — the identity tuple never moved. A derived structure keyed on epoch alone
would read as fresh against a metric that has silently shifted underneath it: **"looks
fresh, is corrupt"** — the exact silent-corruption signature this whole policy family
exists to kill, relocated from identity (ADR-0015) to metric. Including `W_version` and
`csls_k` closes that hole: any change to the de-biasing map or the hubness parameter flips
the fingerprint, even when the epoch holds.

**Corpus content is a separate axis — deliberately _not_ in the fingerprint.** An
incremental ingest leaves `epoch`/`W_version`/`csls_k` unchanged yet still stales a kNN
graph or CSLS cache built before it. Folding a coarse `row_count_bucket` into the
fingerprint does **not** fix this — an ingest within the same bucket still reads fresh —
so content-freshness is the `dirty_since` marker's job (flipped on every write), which is
why the freshness rule is the **two-gate AND** above, not a fingerprint check alone.
Material corpus _growth_ is handled not by invalidation but by a separate **W-refit
policy**: when growth crosses a threshold `W` is re-fit, which bumps `W_version` and so
flips the fingerprint through the normal path.

### 4. Composition with ADR-0015 and reconstruction

This ADR sits **strictly downstream of ADR-0015**:

- **`W` is undefined across identity tuples.** A whitening matrix is fitted on the raw
  cloud of **one** identity tuple. It is meaningless to apply `W` fitted on space A's
  vectors to space B's vectors (different model, different geometry). ADR-0015's
  hard-reject on identity mismatch therefore **also fences the metric**: you can never
  reach the faithful-metric path with a cross-identity vector.

- **A 0015 reconstruction atomically invalidates the metric.** When ADR-0015 §4
  performs a reconstruction (re-embed under a new identity, atomic swap of identity +
  vectors), it invalidates **`W`, `mu`, the CSLS cache, and every derived structure** in
  one motion — they were all fitted on the now-replaced vectors. The
  **`SpaceEpochCounter` bump rides that same promote transaction** (§3), so the
  invalidation is atomic with the swap. A reconstruction therefore obsoletes the
  `projection_matrices` row and every `DerivedStructureRegistry` entry for that space;
  a re-fit of `W`/`mu` against the new vectors is part of completing the reconstruction.

- **In KB, this composes with `embed_swap.py`.** KB's per-space `vec0` reconstruction
  (the multi-space analogue of ME's same-/different-dim reconstruction, #623/#742) is the
  transaction that MUST bump the epoch and obsolete the `projection_matrices` row.
  In ME, the `fact_vectors` promote (#623) / different-dim reopen (#742) is the
  corresponding boundary. Neither layer may swap vectors without, in the same
  transaction, advancing the epoch and marking the metric and its derived structures
  for re-fit.

### 5. The shared-fit obligation and the divergence risk

For two stores that **share an identity tuple and are meant to co-host or exchange
vectors** (the cloud-shared-DB end-state), the faithful metric MUST be computed from
**one and the same `W`/`mu` blob loaded by both layers** — a **shared fit**. This is the
ideal and the only clean guarantee.

The failure mode if the layers fit independently: a vector **authored under `W_KB` and
read under `W_ME`** is compared across **two different linear maps**. The result is a
number, but it is **not a metric** — `faithful_sim` is only a metric when both operands
pass through the same map. Worse, the means diverge by construction: `mu_KB ≠ mu_ME`,
because KB's corpus is papers/notes/code and ME's is memories — the two clouds have
different centroids, so even "the same method, same `p`" yields different `mu` and
different `W`. This is **ADR-0015's silent-corruption mode relocated to the metric**, and
it is **invisible to ADR-0015's mismatch check**: `W` and `mu` are **not** part of the
identity tuple, so the identity check passes while the metric is quietly wrong.

A **pinned-parameter** scheme (both layers agree on `method`, `p`, `csls_k`, and the
receiver re-whitens locally) is a **migration bridge, not a clean guarantee**: it
removes the method/rank divergence but leaves a **residual `mu`-offset error** (the two
centroids still differ), so cross-layer comparisons carry a systematic bias proportional
to `mu_KB − mu_ME`. It is acceptable as a transitional state on the road to a shared
blob; it is **not** the end-state. The end-state is one `projection_matrices` row (one
`W`, one `mu`) loaded by both layers.

### 6. Scope condition (when the parity obligation binds)

The parity obligation of §5 binds **if and only if** two stores **(a) share an identity
tuple** (same model/provider/dim per ADR-0015) **and (b) are meant to be co-hosted or
exchanged** (the cloud-shared-DB end-state). If the two stores use **different models**,
they are **different spaces** — `W` is undefined across them (§4), there is no shared
metric to keep parity over, and the obligation is **moot**. Two same-model stores that
are never exchanged may diverge harmlessly. The contract is **not** a blanket "always
whiten identically"; it is "**when you share a space and will compare across it, share
the fit.**"

### 7. Quantize-after-whiten

When a space's vectors are quantized (int8, product quantization) for storage or speed,
quantization MUST be applied **after** whitening, to the **de-biased** vectors — **never
to the raw anisotropic vectors**. Quantizing the raw space allocates code/bucket budget
to the dominant anisotropic directions that whitening exists to suppress, **re-introducing
the very anisotropy `W` removed** and corrupting the faithful metric the quantized
vectors are supposed to serve. The `element_type` reserved-`int8` path in the ADR-0015
identity tuple is therefore a **post-whitening** representation: the quantizer's
calibration set is `W (x − mu)`, not `x`.

### 8. Fitted-basis parity (recommended rider)

Beyond `W`/`mu`, several second-wave structures are **fitted bases that are functions of
the shared model geometry** rather than of one layer's local data: **LEACE concept-erasure
axes** (Belrose et al. 2023; Ravfogel et al. 2020), **dictionary-learning atoms**
(Olshausen-Field 1996; Mairal et al. 2010), and the **Laplacian eigenbasis** / spectral
basis (Ng-Jordan-Weiss 2001; von Luxburg 2007). Because these bases are determined by the
geometry of the **shared whitened space**, sharing them across layers is
**parity-recommended** for the same reason §5 mandates sharing `W`: a concept-erased or
spectrally-projected vector compared across two different bases is, again, not a metric.

The distinction is sharp: the **basis** (the LEACE projection `P`, the atom dictionary,
the eigenbasis) is a function of shared geometry and is **parity-recommended**; the
**per-layer point labels/assignments** (which cluster a given memory falls in, which
atom a given paper activates) are **local** — they depend on each layer's own points and
need **not** match. Share the axes; keep the assignments local.

## Consequences

- **Form is pinned; KB ships none of it today.** The faithful-metric form, the
  `projection_matrices` contract, and the freshness rule are normative **now**; the
  current KB code computes **raw cosine everywhere** with **zero** de-biasing/CSLS/derived
  structures. This ADR is the **target**, and the gap to it is the gap-analysis: the E0
  faithful-metric substrate (whitening `W` + `projection_matrices` + CSLS) must land, and
  `auto_relate.py`, `folder_summaries.py`, and the search ranker must be **re-pointed at
  the whitened space**. Until then, KB is at the degenerate `method = none` point of §1 —
  honestly named, not silently assumed faithful.

- **Parity is invisible to ADR-0015 — so it is a separate, explicit obligation.**
  Identical identity tuples do **not** imply identical metrics. `W` and `mu` are outside
  the identity tuple by design, so the ADR-0015 mismatch check **cannot** detect a
  metric divergence. Co-hosting safety therefore requires this ADR's shared-fit rule
  **in addition to** ADR-0015's identity rule, not instead of it.

- **"Looks fresh, is corrupt" is the headline failure.** The epoch-alone trap (§3) is the
  single most likely way to ship a silent corruption under this policy: a re-tuned `W`
  with an unchanged epoch. The `W_version`/`csls_k` terms in the fingerprint, **plus** the
  `dirty_since` marker for corpus content, exist solely to make that failure impossible;
  collapsing the two-gate rule back to a fingerprint-only (or epoch-only) check re-opens it.

- **Parity is a maintenance obligation.** Both repos carry this file and must change it
  in lockstep (mirroring ADR-0015). CI/audit in each repo SHOULD assert the two copies
  match, and SHOULD assert that any cross-layer co-hosted space loads a **single**
  `projection_matrices` row rather than two independent fits.

- **Pinned-parameter is a bridge with a known residual.** Where a shared blob is not yet
  feasible, pinned `(method, p, csls_k)` with a local re-whiten is a sanctioned
  transitional state — provided the residual `mu`-offset error (§5) is documented and the
  migration to a shared blob is tracked, not abandoned.

- **Quantization ordering is load-bearing, not an optimization detail.** The
  quantize-after-whiten clause (§7) is a correctness constraint: a quantized raw space is
  a corrupt metric, and the corruption survives every downstream operator.

## References

- ADR-0015: _Cross-layer embedding-identity & mismatch parity policy_ — the identity-level
  sibling of this ADR; this document is its metric-level extension
  (`docs/design/adr/0015-cross-layer-embedding-identity-policy.md`, shared verbatim across
  both repositories).
- Operator taxonomy: note 34, _Geometric memory operator taxonomy_
  (`autonomous-agent-project/raw/landscape/34-geometric-memory-operator-taxonomy.md`) —
  the faithful-metric ground layer (anisotropy-correction + CSLS) and the second-wave
  operators (LEACE, dictionary atoms, spectral clustering) that ride the whitened space.
- Data-structure landscape: note 35, _Memory data-structure landscape_
  (`autonomous-agent-project/raw/landscape/35-memory-data-structure-landscape.md`) — the
  representation-space inventory (whitened/isotropic as universal prerequisite, the
  `projection_matrices` table, the `DerivedStructureRegistry` / `SpaceEpochCounter` /
  content-fingerprint freshness machinery, decoupled by compute tier).
- Ethayarajh (2019). _How Contextual are Contextualized Word Representations?_
  EMNLP-IJCNLP 2019, 55–65 (ACL D19-1006). _(Anisotropy of contextual embeddings —
  the bias `W` removes.)_
- Mu & Viswanath (2018). _All-but-the-Top: Simple and Effective Postprocessing for
  Word Representations._ ICLR 2018. _(The ABT `method` of §1.)_
  `<WARNING! missing provenance>`: citation unconfirmed — on note 34's suspect blocklist;
  validate in E0 PoC #1 before this reference is relied on in a paper. The anisotropy claim
  in Context rests on Ethayarajh (2019) + textbook whitening, not on this entry.
- Conneau, Lample, Ranzato, Denoyer, Jégou (2018). _Word Translation Without Parallel
  Data._ ICLR 2018, arXiv:1710.04087. _(CSLS hubness correction — the `csls_k` step.)_
- Ravfogel, Elazar, Gonen, Twiton, Goldberg (2020). _Null It Out: Iterative Nullspace
  Projection (INLP)._ ACL 2020, 7237–7256. _(Concept-erasure basis — §8 parity rider.)_
- Belrose, et al. (2023). _LEACE: Perfect Linear Concept Erasure in Closed Form._
  NeurIPS 2023, arXiv:2306.03819. _(Closed-form erasure basis — §8 parity rider.)_
- Olshausen, Field (1996). _Emergence of Simple-Cell Receptive Field Properties by
  Learning a Sparse Code for Natural Images._ Nature 381(6583):607–609; and Mairal, Bach,
  Ponce, Sapiro (2010). _Online Learning for Matrix Factorization and Sparse Coding._
  JMLR 11:19–60. _(Dictionary-learning atoms — §8 parity rider.)_
- Ng, Jordan, Weiss (2001). _On Spectral Clustering: Analysis and an Algorithm._
  NeurIPS 2001, 849–856; and von Luxburg (2007). _A Tutorial on Spectral Clustering._
  Stat. Comput. 17:395–416. _(Laplacian eigenbasis — §8 parity rider.)_
- Knowledge-layer current state: `auto_relate.py` (raw-cosine kNN), `folder_summaries.py`
  (mean-pool centroid), `embed_swap.py` (per-space `vec0` reconstruction), the
  `embed_spaces` registry (<https://github.com/dutiona/knowledge-base>).
- Memory-layer current state: same-/different-dim reconstruction (#623/#742) and the
  geometric programme epic family (<https://github.com/dutiona/memory-engine>).
