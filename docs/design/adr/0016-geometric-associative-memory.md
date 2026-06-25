# ADR-0016: Geometric Associative Memory

**Status:** Proposed
**Date:** 2026-06-26
**Scope:** Core engine — retrieval, storage, consolidation, forgetting, reconstruction

## Context

The engine today is **pointwise-metric + low-pass-diffusion only**: cosine similarity via HNSW, FTS5 full-text, and Random-Walk-with-Restart (RWR) spreading activation. It has no de-biased metric, no graph Laplacian, no curvature signal, no distributional or structural operator, and no freshness registry for derived artifacts. Three concrete failures follow from this:

1. **Silent metric bias.** Qwen3-class embeddings are anisotropic — random-pair cosine sits far above zero (Ethayarajh 2019). Every edge weight, and consequently every spectral, curvature, or transport operator built on those raw edges, inherits this bias. Without a whitened derived space, no downstream geometric operator is correct.

2. **No cross-domain transfer.** RWR is a low-pass heat-diffusion filter (Chung 2007; Kondor and Lafferty 2002; Gasteiger et al. 2019 APPNP) — it spreads activation within a connected component but cannot bridge the spectral gap between disconnected domain clusters. The engine therefore cannot detect or exploit structural analogies between, for example, compiler-pass ordering and recipe sequencing. Relation-as-offset (Mikolov et al. 2013) is the canonical candidate replacement; it is rejected: performance on abstract-relation benchmarks is approximately 9–11% on BATS <WARNING! missing provenance>: specific percentage range traces to the operator-taxonomy synthesis document (§3 PROBABLY-NOT), not directly extractable from the cited papers; cite as reported in the survey (Gladkova et al. 2016; Drozd et al. 2016; Rogers et al. 2017). The failure is geometric: cosine offset is a flat zero-curvature tool; on a curved anisotropic manifold the relation vector does not translate.

3. **No derived-structure lifecycle.** The append-only store (soft-delete via `t_expired`) invalidates every cached spectral, curvature, and diffusion artifact on each ingest. There is no freshness registry, no content fingerprint per derived structure, and no scheduler to amortize rebuilds. Derived artifacts therefore either go stale silently or are never persisted.

The operator survey (`34-geometric-memory-operator-taxonomy.md`) and data-structure inventory (`35-memory-data-structure-landscape.md`) identify the complete gap and ground every operator against a verified citation set (`37-geometric-associative-memory-bibliography.md`). This ADR records the architectural commitments that follow.

## Decision

### (a) Whitened/isotropic derived space as the universal correctness prerequisite

A whitened projection of the active Qwen3 embedding space is introduced as a **derived space** registered in the existing `embedding_spaces` table (schema v13, issue #622). The projection matrix `W` is stored in a companion `projection_matrices` table. The whitened space is the **mandatory prerequisite** for every spectral, curvature, and transport operator below: a Laplacian, ORC, or GW coupling built on raw anisotropic cosine edges is biased and yields incorrect results (Ethayarajh 2019). Qwen3 remains the ingest embedding; the whitened space is a derived projection, not a model swap and not a reconstruction event.

The specific post-processing algorithm (mean-centering, All-but-the-Top, or full whitening) is left to the implementing epic pending PoC #1 (anisotropy audit: measure mean random-pair cosine on the live store, sweep correction variants, measure recall@k before and after). All-but-the-Top is a standard technique; its primary citation carries a verification conflict (see `37-geometric-associative-memory-bibliography.md` §7.x) and must be validated empirically before being locked. The `use-now` tier rests on whitening/mean-centering as textbook operations, independently of any single citation; Ethayarajh (2019) grounds the existence of the anisotropy, not the specific correction.

### (b) Persisted kNN similarity-edge graph, DerivedStructureRegistry with content fingerprints, and dream-cycle MaterializationScheduler

The existing typed-edge graph (`edges` table) is too sparse for a connected graph Laplacian. A **persisted kNN similarity-edge graph** (`similarity_edges`, relation tag `similarity_knn`) is introduced at blocking tier, built from whitened-space HNSW top-k neighbors on each ingest and stored as ordinary `edges` rows. This graph must strictly consume whitened vectors, never raw Qwen3 vectors, to avoid propagating the anisotropy bias into every downstream spectral and curvature structure.

A **DerivedStructureRegistry** (one DB row per structure kind × scope × space) tracks every derived artifact — similarity graph, ORC cache, eigenbasis, diffusion kernel, GW coupling cache, etc. — with:

- a `dirty_since` timestamp,
- a `status` field (`fresh` / `dirty` / `rebuilding`),
- a **content fingerprint** — a hash of `{epoch, W-version, sim-graph k, scope-row-count-bucket}` — not merely the space-epoch counter.

The epoch counter (`space_epoch` config row, bumped inside the promote transaction) guards model-swap invalidation. The content fingerprint is required in addition because edge weights change when the whitening matrix `W` or the similarity graph `k` is retuned without a model swap; epoch-only tracking produces "looks fresh, is corrupt" eigenbases and ORC caches, the single most dangerous failure mode.

A **MaterializationScheduler** runs as a dream-cycle sub-step: it claims dirty registry rows (`status = 'rebuilding'`), dispatches CPU-heavy builds via `spawn_blocking`, and flips `status = 'fresh'` on completion. Two startup consistency guards are mandatory: re-dirty any row with `last_computed_at < MAX(facts.t_created)` (crash between commit and notify) and reset `status = 'dirty'` where `status = 'rebuilding'` (crash mid-build).

Heavy structures — Laplacian partial eigendecomposition, diffusion-map coordinates, GW coupling caches, persistence diagrams — are dream-cycle-offline only. The reconstruction seam (#623/#742) triggers a full rebuild of all derived structures on promote, with the Gavish–Donoho effective-rank check (Gavish and Donoho 2014; Marchenko and Pastur 1967) applied before committing the HNSW reindex to confirm the new embedding space is non-degenerate.

### (c) In-area associative recall via RWR/PPR as a heat-diffusion filter with temperature control

The existing RWR spreading activation is retained and reframed as a spectral low-pass heat-diffusion filter (Chung 2007; Kondor and Lafferty 2002; Gasteiger et al. 2019 APPNP). The restart probability `α` is exposed as a **temperature** parameter: high `α` (high restart) = shallow, surface-level diffusion; low `α` (low restart) = wide, deep diffusion reaching distant related memories. This reframing gives a principled tuning knob and — critically — a diagnosis for why RWR cannot bridge domain gaps: the spectral gap `λ₂` of the graph Laplacian quantifies the disconnection between components, and no amount of temperature tuning crosses it.

### (d) Cross-domain transfer via structural operators with engine scaffolding and consumer LLM analogical mapping

Cross-domain transfer requires operators that work without a shared coordinate frame. Four are committed at different tiers:

**Ollivier–Ricci curvature (ORC) for bridge detection.** Per-edge curvature (Ollivier 2009; Ni et al. 2019; Topping et al. 2022): edges with the most-negative ORC values are structurally cross-community bridges. ORC is the cheapest concrete handle on the cross-domain problem and doubles as a graph-rewiring signal. On the ingest hot path, Forman–Ricci curvature (Forman 2003; Sia et al. 2019) is used as the real-time proxy (O(degree), no optimal transport); ORC (Sinkhorn-per-edge) is computed in batch by the MaterializationScheduler. Curvature-protected forgetting follows directly: a memory whose only outgoing edges are negative-curvature bridge edges must not be expired — forgetting it severs the cross-domain reachability that depends on those bridges (research amendment A4).

**Gromov–Wasserstein coordinate-free structural matching.** GW (Mémoli 2011; Alvarez-Melis and Jaakkola 2018; Scetbon et al. 2022) matches two memory regions by their **internal distance matrices**, requiring no shared coordinate frame. The engine computes GW couplings offline in the dream-cycle (Scetbon et al. 2022 low-rank variant; hard cluster-size cap applied via recursive splitting before any cost matrix is formed — the dense O(N²) ground cost is materialized regardless of coupling rank, so cluster size must be bounded). The resulting coupling matrix `T` is persisted as a GW coupling cache. The engine identifies structurally analogous memory pairs; the **consumer LLM performs the analogical mapping**. This division is explicit: the engine provides the structural scaffold (which pairs are geometrically analogous); it does not attempt to generate or evaluate the analogy. GW on small domains (N ≈ 50–100 per domain) is a mandatory PoC gate before the full coupling cache is built (PoC #4 in the operator survey).

**Concept erasure (INLP/LEACE) for relational skeleton exposure.** INLP (Ravfogel et al. 2020) and LEACE (Belrose et al. 2023) strip the domain-identity linear subspace, exposing the relational skeleton that cross-domain analogies depend on. This is the tractable linear complement to GW. Whether domain identity is linearly separable in Qwen3 final embeddings (rather than only in intermediate activations) is an open question that must be resolved by PoC before the concept-erased space is materialized.

**Induced schema bridge-nodes via dictionary learning.** An overcomplete dictionary (Olshausen and Field 1996, 1997; Mairal et al. 2010) induces concept atoms. Two memories in different domains that share atom `k` are structurally related at low cosine distance; the shared atom is materialized as a **bridge node** in the graph (`fact_type = 'schema'`, pinned) with `instantiates` edges to member facts. This delivers in-area abstraction (atoms as induced schemata, F3) and cross-domain bridging (shared atoms as traversable links, F2) from one structure.

### (e) Operator tiering: real-time / blocking / offline-expensive

All operators are assigned to exactly one compute tier:

**Real-time (sub-ms–ms; hot-path safe):** FRC per-edge increment, LID per-node update (reusing HNSW kNN from vector queries; async fire-and-forget for FTS/graph queries), Hawkes intensity update (Ozaki 1979 O(1) recursion), Euler-χ increment from degree deltas, RMT k\* threshold (Marchenko and Pastur 1967; Gavish and Donoho 2014; O(1) given γ), DerivedStructureRegistry dirty-flip, SpaceEpochCounter check. Access logging and Hawkes/LID updates are **async fire-and-forget**, batched into a single deferred write per query off the read path, to avoid serializing reads behind SQLite's single writer.

**Blocking (seconds; `spawn_blocking`):** Whitened space + anisotropy cache (randomized SVD), kNN similarity-edge graph build, ORC batch computation, LEACE/INLP projection, dictionary-learning batch initialization, spectral cluster k-means, sparse CSR adjacency build.

**Offline-expensive (minutes+; dream-cycle only):** Laplacian partial eigendecomposition (Lanczos; Ng et al. 2001; von Luxburg 2007), diffusion-map coordinates (Coifman et al. 2005; Coifman and Lafon 2006), GW coupling cache (Mémoli 2011; Scetbon et al. 2022), pairwise cluster cost matrices (with mandatory recursive cluster-size cap), persistence diagrams (Bauer 2021), Mapper graph (Singh et al. 2007), tangent frames and connection Laplacian (Singer and Wu 2012; offline, cold storage, processed per-landmark patch — never global in RAM at full `d`).

All heavy structures are **dream-cycle-amortized** and **full-rebuilt on reconstruction** (model swap or dim change via #623/#742).

## Alternatives Considered

### Relation-offset / analogy arithmetic for cross-domain transfer

The canonical "king − man + woman" arithmetic (Mikolov et al. 2013) applies cosine offset as a relation transport operator. **Rejected.** Performance on abstract semantic relations reaches approximately 9–11% on the BATS benchmark <WARNING! missing provenance>: specific percentage range traces to the operator-taxonomy synthesis document (§3 PROBABLY-NOT), not directly extractable from the cited papers; cite as reported in the survey (Gladkova et al. 2016; Drozd et al. 2016; Rogers et al. 2017). The failure is geometric: offset arithmetic assumes a flat Euclidean space; on the curved, anisotropic Qwen3 manifold the relation vector does not translate between domains. This degradation is worse than on isotropic embeddings because anisotropy amplifies the directional bias. The correct curvature-aware replacement is parallel transport via the connection Laplacian (Singer and Wu 2012); the correct coordinate-free replacement is Gromov–Wasserstein (Mémoli 2011). Neither is additive offset in disguise.

### VSA/HRR binding as the cross-domain bridge

Holographic Reduced Representations (Plate 1995) and related Vector Symbolic Architectures (Kleyko et al. 2022) provide an invertible role-filler algebra — bind `(relation ⊛ source)`, unbind `T^{-1} ⊛ bound` — at O(d log d). **Rejected as a cross-domain bridge.** VSA binding assumes near-orthogonal random hypervectors; Qwen3 embeddings are anisotropic and correlated, so direct binding produces unreliable superpositions (SNR ∝ √(d/M) <WARNING! missing provenance>: formula and the "M ≪ ≈100 edges/node" threshold trace to the operator-taxonomy synthesis document §10, attributed to Frady, Kleyko, Sommer 2017; preserved verbatim). More fundamentally, binding presupposes the filler already lives in a shared codebook — it cannot bridge two separately-trained anisotropic embedding domains, which is the cross-domain problem. VSA lands instead on **within-space typed-edge traversal (F3)**: the principled replacement for refuted offset arithmetic within a single space, using a learned or random encoding layer (QAVSA-style, ACL Workshop RepL4NLP 2024). This is retained as a worth-exploring operator for typed-edge composition but not as the cross-domain mechanism.

### Persisting a dense cosine kNN layer as the similarity tier

Storing all pairwise cosine similarities, or a dense kNN matrix, as the persistent similarity index. **Rejected.** HNSW already provides approximate kNN at query time from the active embedding space; duplicating it as a dense persisted layer wastes storage and becomes stale on every ingest. The committed design persists the **structural tier** (curvature, similarity-edge graph for Laplacian/spectral ops, GW couplings) while leaving proximity queries to the in-memory HNSW. The similarity-edge graph persists kNN edges at graph scale (O(N·k) rows) to enable SpMV-based spectral computation, not to replace HNSW for retrieval.

## Consequences

- **Foundational substrate epic (E0) gates all others.** The whitened/isotropic space, DerivedStructureRegistry, and MaterializationScheduler are prerequisites for every spectral, curvature, and transport operator. No downstream epic (in-area spectral operators, cross-domain transfer, Koopman prediction) should begin implementation before E0 delivers the whitened space and the registry plumbing.

- **Cross-domain (E3) is PoC-gated.** The GW coupling cache is not funded until the GW-on-small-domains ablation (N ≈ 50–100 per domain, POT or pure-Rust Sinkhorn, coupling T inspected for structurally sensible cross-domain matches rather than nearest-cosine matches) succeeds. Similarly, the concept-erased space is not materialized until a linear domain-classifier probe establishes that domain identity is linearly separable in Qwen3 final embeddings.

- **Qwen3 stays the ingest embedding.** The whitened projection is added as a derived space. This is not a reconstruction event and does not trigger the #742 fence-and-reopen path.

- **The reconstruction seam (#624/#742) gains an RMT effective-rank guard.** Before committing an HNSW reindex on reopen-at-D′, the Marchenko–Pastur effective rank (Gavish and Donoho 2014) is computed from the new embedding space. If the effective rank collapses, the new space is degenerate and the commit is refused. This is the cheapest high-value addition to the existing reconstruction path.

- **Curvature-protected forgetting is a derived policy.** The existing prune pass gains a guard: a memory whose only outgoing edges are negative-curvature bridge edges (by FRC on the hot path; ORC when the batch cache is fresh) is not expired, because forgetting it severs cross-domain reachability. This composes with the Ebbinghaus + LID decay signals rather than replacing them.

- **Content-fingerprint invalidation is a maintenance obligation.** Every future operator that caches derived structure must register a row in the DerivedStructureRegistry and include its relevant parameters in the content fingerprint. A cache that uses epoch-only staleness detection and silently produces wrong results on parameter drift (e.g., retuned whitening matrix or sim-graph k) is a correctness bug, not a performance trade-off.

- **Operator arithmetic that was previously rejected must not be resurrected.** Relation-as-offset for abstract-relation cross-domain transfer is permanently closed. Any future proposal that relies on cosine vector arithmetic for structural analogy between domains must demonstrate that it is not in the same failure class before being accepted.
