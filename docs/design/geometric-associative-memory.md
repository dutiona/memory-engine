# Geometric Associative Memory — Design

> **Status:** Design doc for the geometric associative memory epic family (E0–E5). Operators are the ground; features emerge from operator compositions. Source notes: [note 34](../../autonomous-agent-project/raw/landscape/34-geometric-memory-operator-taxonomy.md) (operator taxonomy), [note 35](../../autonomous-agent-project/raw/landscape/35-memory-data-structure-landscape.md) (data-structure inventory), [note 37](../../autonomous-agent-project/raw/landscape/37-geometric-associative-memory-bibliography.md) (verified bibliography).

---

## 1. Overview and the bottom-up principle

The memory store is a triple-coupled mathematical object. At any time `t`, the active store is a set of `N(t)` memories. Each memory carries an embedding `v_i ∈ R^d` (d ≈ 1024, Qwen3), typed relational edges (`cites`, `extends`, `supersedes`) and similarity edges, bi-temporal timestamps (`t_created`/`t_expired` system time; `t_valid`/`t_invalid` real-world time), and scalar decay fields (importance, access count, Ebbinghaus activation).

The three coupled structures are geometric (point cloud in R^d with cosine metric), graph (weighted typed directed edges), and temporal (bi-temporal index; geometry and graph evolve in time). These are coupled, not independent: edge weights derive from geometry; the graph evolves in time; decay reshapes both. A correct operator typically reads from two or more of the three.

**The bottom-up principle.** Capabilities are not designed top-down and then implemented; they emerge from operator compositions. The operators are the ground-truth primitives. Features (F1 recall, F3 abstraction, F2 cross-domain transfer, prediction, forgetting, pattern-detection) are names for what specific operator stacks produce when composed. This distinction matters because it dictates build order: lay operators in dependency order, then observe which features emerge.

The decisive gap in the current engine: everything in-engine is **pointwise-metric (cosine / FTS5) + low-pass diffusion (RWR)**. There is no de-biased metric, no graph Laplacian, no curvature, no transport, and no distributional or structural operator. Six operator families fill the gap (see Section 2). None of them are buildable correctly on raw anisotropic Qwen3 vectors — the whitened/isotropic space (Section 3) is the gate for the entire epic.

---

## 2. Feature families and the operator stack each stands on

Capabilities emerge from compositions, not single operators. The table below states each feature family, its operator stack, and a calibrated buildable-confidence on real Qwen3 embeddings. Full operator details, epistemic tiers, and per-operator complexity are in note 34 §2–§6; this section summarizes the composition logic and gates.

### F1 — Associative recall (robust)

**Operator stack:** anisotropy-correction + CSLS (faithful metric) → diffusion-distance ranker (Coifman et al. 2005/2006; Belkin and Niyogi 2003) → PPR/RWR reframed as spectral low-pass heat-diffusion filter (Chung 2007; Gasteiger et al. 2019; Kondor and Lafferty 2002) → Modern Hopfield attractor completion for partial cues (Hopfield 1982; Ramsauer et al. 2021).

**Buildable-confidence: HIGH** for the first three operators (all `use-now`, verified, composable). Hopfield is downgraded to `worth-exploring` pending a capacity/separation check: Ramsauer-style retrieval requires exponential separation of stored patterns, and at 10k+ dense anisotropic Qwen3 memories it risks collapsing to averaged attractors rather than clean completion. Gate Hopfield on the anisotropy fix landing first.

The current RWR spreading activation is already an instance of the spectral low-pass filter (PPR = heat-diffusion at temperature `t`). The verified spectral identity gives this a principled tuning knob and a diagnosis for why it cannot bridge domains: the spectral gap λ₂ is the quantitative cross-domain-gap meter.

### F3 — In-area abstraction

**Operator stack:** SVD/PCA theme axes → dictionary-learning atoms (Olshausen and Field 1996/1997; Mairal et al. 2010) as induced schemata → spectral clustering (Ng, Jordan, Weiss 2001; von Luxburg 2007) for consolidation units → Wasserstein barycenter as distributional centroid (Peyré and Cuturi 2019; Cuturi 2013).

**Buildable-confidence: MEDIUM-HIGH.** SVD and spectral clustering are immediate. Dictionary-learning-as-schema and barycenter-as-summary each need one PoC. Dictionary atoms can be materialized as `fact_type='schema'` graph nodes (no schema change to existing tables), which also delivers the F2 shared-atom bridge mechanism at no extra cost — one structure, two features.

Sparse autoencoders (Elhage et al. 2022; Bricken et al. 2023) are a higher-interpretability path to concept atoms but require white-box model access not available in the engine; they are `could-work` only if monosemantic atoms can be extracted from final embeddings without intermediate activations.

### F2 — Cross-domain transfer

**Operator stack:** Ollivier-Ricci curvature (ORC; Ollivier 2009; Ni et al. 2019; Topping et al. 2022) to find bridge edges → concept-erasure via INLP/LEACE (Ravfogel et al. 2020; Belrose et al. 2023) to strip domain identity → Gromov-Wasserstein (GW; Mémoli 2011; Alvarez-Melis and Jaakkola 2018; Scetbon, Peyré, Cuturi 2022) for coordinate-free structural matching offline → optionally parallel transport (Singer and Wu 2012; Budninskiy et al. 2019) for curvature-aware relation carry.

**Buildable-confidence: MEDIUM.** This is the hard problem. ORC is `use-now` and is the cheapest concrete handle on cross-domain bridging. Most-negative-ORC edges are cross-community bridges by construction; Forman-Ricci curvature (FRC; Forman 2003; Sia et al. 2019) is cheaper and incremental, suitable for the ingest hot-path bridge proxy (ORC, which requires Sinkhorn per-edge, belongs in batch). The GW PoC (GW on two small domains, N≈50–100 each) is the single highest-stakes experiment in the entire epic: if the coupling matrix T aligns structurally analogous pairs rather than nearest-cosine neighbors, GW becomes the dream-cycle cross-domain operator. Parallel transport (connection Laplacian / VDM) is the curvature-aware replacement for the refuted offset-arithmetic operator, but costs O(N·k·d²) at d=1024 — intractable globally; it must be restricted to LID-estimated d_local ≪ 1024.

**Key consequence of A4 (critic): curvature-protected forgetting.** Never expire a fact whose only edges are negative-curvature bridges — forgetting it severs the cross-domain reachability F2 depends on. This composes ORC/FRC × Ebbinghaus × LID-density into a topology-protected forget policy. Euler-χ (Edelsbrunner and Morozov 2017) should also be promoted from passive monitor to active consolidation/forget trigger: a Δχ above threshold wakes the consolidation pass.

**GW feasibility cap.** GW (Frank-Wolfe + Sinkhorn) is NP-hard. Scetbon et al. (2022) low-rank couplings reduce the coupling T to rank r but do not reduce the ground cost matrix, which remains O(N_c²) per cluster pair. Clusters must be hard-capped via recursive split before any cost matrix is formed; the budget is O(max_cluster²), not O(mean²). GW is dream-cycle/offline only; it is never on any real-time or blocking path.

**What is refuted and will not be rebuilt:** relation-as-offset / analogy arithmetic for abstract relations. Verified failure at ~9–11% on lexicographic BATS, degraded further on anisotropic Qwen3. The geometric reason: cosine-offset is a flat zero-curvature tool on a curved anisotropic manifold. Superseded by parallel transport (curvature-aware) and GW (coordinate-free). Do not rebuild it.

### Prediction

**Operator stack:** trilateration / MDS (Tenenbaum et al. 2000; Biswas et al. 2006) for locating a missing or hypothetical memory from anchor distances vs. Koopman / DMD (Koopman 1931; Brunton et al. 2022; Schmid 2010; Lusch et al. 2018) + Kalman (Kalman 1960) for true trajectory forecasting. These are distinct: trilateration is reconstruction (location from known distances), not forecasting.

**Buildable-confidence: MEDIUM-LOW.** The math is verified for both paths. The blocker is trajectory sparsity: Koopman/Kalman/DMD assume trajectory density that the store may not accumulate per topic. Until measured on real session histories, prediction stays `worth-exploring`/`could-work`. Trilateration is the safer prediction primitive because it only requires existing anchor distances, not a dense temporal trajectory. The Koopman mode buffer should not be built until the access-trajectory density PoC clears.

### Forgetting

**Operator stack:** Ebbinghaus decay (in-engine) modulated by LID density (Amsaleg et al. 2015; Houle 2017) for flat-region pruning and novelty detection → Euler-χ/ECC as active consolidation trigger (Edelsbrunner and Morozov 2017) → Hawkes self-exciting point processes (Hawkes 1971; Ozaki 1979; Du et al. 2016; Mei and Eisner 2017) as the bursty-access modulator replacing the current ln-normalized access-count frequency signal.

**Buildable-confidence: HIGH** for LID + Euler-χ modulation/audit (both `use-now`). Hawkes is `worth-exploring`: the Ozaki recursion is O(1) amortized per access event, composes multiplicatively with the Ebbinghaus decay as `score = exp(−λ·t_age) · λ*(t)/λ_ref`, and is strictly more accurate than FFT for the irregularly-sampled bursty access patterns the engine actually sees. Information Bottleneck as an implementable forgetting objective is `LOW` confidence — it requires Y = future queries and variational IB, both of which violate the zero-LLM constraint.

**Important asymmetry:** high-LID nodes (sparse/novel, at cluster boundaries) are exactly the nodes that may also sit on negative-curvature bridge edges. The curvature-protected forgetting rule (A4) overrides LID-based pruning for these nodes.

### Pattern-detection

**Operator stack:** GFT high-frequency energy (Shuman et al. 2013; Sandryhaila and Moura 2014) for anomaly/contradiction detection → graph wavelets (Hammond et al. 2011) for multi-scale importance → FFT on per-memory access time-series (Cooley and Tukey 1965) for recurrence detection → persistent homology H0/H1/H2 (Edelsbrunner et al. 2002; Carlsson 2009; Cohen-Steiner et al. 2007; Bauer 2021) and Mapper (Singh et al. 2007) for structural shape and drift.

**Buildable-confidence: MEDIUM.** All operators verified; all `worth-exploring` or `could-work`. FFT on access series is the cheapest first win: flat-spectrum memories (no recurrence) correlate with low future access and are strong forget candidates. Persistent homology and Mapper are batch-only (Ripser practical to N ≤ 2000); loops in H1 identify contradictory cycles. GFT/spectral filtering requires the Laplacian eigenbasis, which is an offline-expensive structure.

**RMT as the parameter-free backbone.** Marchenko-Pastur (Marchenko and Pastur 1967), Tracy-Widom (Tracy and Widom 1994; Johnstone 2001), BBP spike detection (Baik et al. 2005), and Gavish-Donoho optimal SVD truncation (Gavish and Donoho 2014) supply the parameter-free signal/noise boundary that every spectral operator in the above stacks silently lacked. The MP edge λ₊ = σ²(1+√γ)² with γ = d/N gives rank k* = #{λ > λ₊} as the parameter-free k for SVD truncation, spectral clustering, GFT cutoff, and manifold dimension. These thresholds are O(1) given γ and are `use-now`. They should be computed as the RMT k* cache (a ~60-byte scalar struct) at ingest time and consulted by every spectral structure.

**VSA typed-edge traversal.** Vector-Symbolic Architectures (HRR binding: Plate 1995; Frady et al. 2017; Kleyko et al. 2022) provide an invertible role-filler algebra for typed edges — bind/unbind at O(d log d). This is the principled replacement for refuted offset arithmetic within a shared space, applicable to F3 (within-domain typed-graph traversal). It does not bridge two anisotropic domains (VSA presupposes a shared codebook; Qwen3 is anisotropic and correlated, requiring a learned/random encoding layer first) — the ranking for cross-domain remains GW ≳ parallel-transport > VSA. VSA is `worth-exploring` for typed-edge F3 only, with the codebook covering relation-types only (not per-fact).

---

## 3. Representation-space plan

All vector coordinate systems over facts. The single universal prerequisite is the whitened/isotropic space. The `embedding_spaces` table (schema v13+, from #622) is the extension point for every derived space; `fact_vectors` stages non-active vectors for backfill (#623) and rollback (#742).

### Raw Qwen3 active space

**Dim:** d ≈ 1024. **Freshness:** real-time (per fact). **Status: have** (`facts.embedding`). The current operational space. Supports cosine, FTS5, current RWR. Every metric and spectral operator built on raw cosine is biased due to anisotropy (Ethayarajh 2019): random-pair cosine sits far above zero, so every edge weight derived from raw cosine is inflated, and any Laplacian, ORC, GW, or diffusion operator built on those weights inherits the bias silently.

### Whitened / isotropic space — universal prerequisite

**Dim:** d (same). **When built:** dream-cycle (full rebuild on model swap). **Compute:** blocking (randomized SVD O(N·d·k); ABT O(N·p·d)). **Status: extend** (`embedding_spaces` row + companion `projection_matrices` table for W). **Unblocks:** everything metric/spectral/curvature/transport.

This is not optional polish. Building any spectral, curvature, or transport operator before this space exists silently corrupts all of them via biased Laplacian and biased edge weights. The ABT method (mean-subtraction + removal of top-p principal components) is method-standard; its primary citation (Mu, Bhat, Viswanath 2018, ICLR) carries a cross-lens conflict and should be treated as "citation unconfirmed — validate empirically in PoC #1." The anisotropy exists (Ethayarajh 2019, verified); plain whitening is the safe fallback while the ABT PoC runs. The projection matrix W is stored in `projection_matrices`; the epoch is bumped inside the promote transaction via `SpaceEpochCounter` so any reader that sees promoted `facts.embedding` also sees the new epoch.

**Scope policy: one global `W`.** `W`/`μ` are fit once per embedding space, scope-orthogonal — not per scope. Although derived structures are registered per `scope × space`, the whitening map is global: this keeps a single faithful metric across all scopes and preserves cross-layer (knowledge-base) parity under ADR-0017, since `W`/`μ` lie outside ADR-0015's identity tuple and a per-scope fit would diverge invisibly to the identity check. Per-scope whitening fragments the metric (cross-scope similarity undefined, ME↔KB parity broken) and is a deferred opt-in only. See ADR-0016 §(a), ADR-0017 §5–§6, and #853.

**Important caveat (C2 from critic):** the `SpaceEpochCounter` guards embedding-space swaps but not edge-weight changes. When sim-graph k or W is retuned without a full promote, the Laplacian and ORC eigenbasis go silently stale. Per the two-gate freshness rule (ADR-0016 §(b); cross-layer ADR-0017 §3), the `DerivedStructureRegistry` row for every spectral/curvature structure carries a **metric fingerprint** = hash{epoch, W-version, k} (catching a retuned metric) **and** a `dirty_since` marker flipped on every corpus write (catching an incremental ingest); a structure is fresh iff the fingerprint matches **and** it has not been dirtied since materialization. `scope-row-count-bucket` is deliberately _not_ in the fingerprint — an ingest within the same bucket would still read fresh — so corpus-content staleness rides the dirty marker, while material growth triggers a `W`-refit that bumps `W-version` through the normal path. Epoch-alone (or fingerprint-alone) invalidation is the single most likely source of a "looks fresh, is corrupt" spectral result.

### Diffusion-map coordinates

**Dim:** d′ ≈ 64–256 (RMT k\*). **When built:** dream-cycle (full rebuild on topology shift). **Compute:** offline (Lanczos O(k²N); Nyström O(L·N) incremental). **Status: new** (registered space at d′, reuses eigenbasis).

Provides diffusion distance (multi-hop noise-robust similarity), Laplacian eigenmaps (Belkin and Niyogi 2003), and the low-dimensional state space for Koopman/Kalman (d′ ≪ 1024). The eigenbasis is the substrate shared by GFT, spectral clustering, diffusion maps, and graph wavelets; it is computed once per dream-cycle and cached in `eigenbasis_cache` (U_k, Λ_k, node_ids). The dimension d′ is set by the RMT k\* cache, not by a heuristic elbow. Query-time diffusion-distance lookup uses Nyström landmark coordinates stored in `diffusion_kernel_cache` at O(L·k) size.

### Concept-erased relational skeleton

**Dim:** d (rank-deficit). **When built:** dream-cycle (rebuild on domain-distribution shift). **Compute:** blocking (LEACE closed-form O(d²·k)). **Status: new** (projection matrix P in `projection_matrices`; apply on-the-fly to avoid staleness).

Strips the domain-identity subspace from embeddings to expose the relational skeleton — the tractable linear approximation to Gentner's (1983) structure-mapping. A linear domain classifier run first (PoC #2: is domain identity linearly separable in Qwen3 final embeddings?) gates whether this space is viable. High classifier accuracy ⇒ LEACE/INLP is viable; low accuracy ⇒ that space degrades to noise and SAE-style atoms require white-box model access the engine does not have. Apply the projection P on-the-fly at query time rather than materializing a full projected copy, to avoid a second staleness surface.

### VSA hypervector codebook

**Dim:** d (random/learned). **When built:** ingest (incremental, relation-types only). **Compute:** real-time (O(d log d) bind/unbind via FFT). **Status: new** (global matrix blob; not a per-fact `embedding_spaces` row — this is a global binding dictionary, not a per-fact coordinate system).

Stores one hypervector per relation type (`cites`, `extends`, `supersedes`). HRR bind/unbind (Plate 1995; Frady et al. 2017; Kleyko et al. 2022) enables typed-edge traversal within a shared space at O(d log d). Capacity is lossy: SNR ∝ √(d/M), reliable superposition at M ≪ ~100 edges per node, unbinding is approximate and requires a clean-up codebook. This does not bridge two anisotropic domains — it replaces refuted offset arithmetic for within-space typed relation traversal only.

### Hyperbolic Poincaré space

**Dim:** d′ ≈ 5–20. **When built:** dream-cycle. **Compute:** offline-expensive (Riemannian SGD). **Status: new**. Encodes scope/taxonomy hierarchy parsimoniously (abstract concepts near origin, specific near boundary). The primary citation for hyperbolic neural networks (Ganea et al. 2018) is on the §V unverified list — treat as `worth-exploring` pending the PoC on linear separability; the underlying hyperbolic geometry is standard. The #742 reopen-at-D′ fence is not triggered for this space because it uses a separate query path at a different dimension.

### GW coupling cache

**Dim:** per domain-pair coupling (low rank r). **When built:** dream-cycle. **Status: new**. This is a coupling cache, not a per-fact embedding space. The coupling matrix T at low rank r via Scetbon et al. (2022) stores the structural correspondence between two memory regions. Invalidated on cluster shift (TTL-based). Size is O(r·(|A|+|B|)) per domain pair (~40KB–5MB depending on r and cluster size). Clusters must be hard-capped before any cost matrix is formed; see Section 2, F2.

---

## 4. Freshness and invalidation model

The append-only store (soft-delete via `t_expired`) means every derived spectral, curvature, diffusion, and transport artifact is invalidated on every ingest. This is the central engineering constraint of the entire epic. Three maintenance strategies handle the full inventory.

### Real-time-incremental

Structures maintained inline in the post-commit HNSW notify hook (extended to `IngestDirtyNotifier`). These are O(deg) or O(1) and safe on the hot path:

- **LID per-node** (`facts.lid`): O(k) reusing the HNSW kNN already computed for sim-edges. Gating: only for vector queries where kNN is in hand; FTS/graph queries cache LID lazily — never compute a fresh ANN probe per result on the FTS path.
- **FRC per-edge** (`edges.frc`): O(deg) incremental, degree+triangle, no optimal transport. The real-time bridge proxy; ORC belongs in batch.
- **Euler-χ increment**: O(1) from degree deltas; full V−E+T is blocking.
- **Hawkes intensity state** (μ, last_t, R via Ozaki recursion): O(1) amortized per access event. Must be fire-and-forget async off the read path — write-on-read serializes reads behind SQLite's single writer. Batch access events into one deferred write per query via the #96 call-site.
- **RMT k\* cache** (N, d, γ, λ₊, τ*, k*): O(1) given γ; σ² from sampled projection. Updated on ingest; keys on (N, d, scope-row-count-bucket) to avoid continuous drift on every single ingest.
- **DerivedStructureRegistry dirty-flip + SpaceEpochCounter check**: single atomic load+compare.
- **PPR/RWR push** (query-ephemeral): O(1/α·|E_touched|) push per query; do NOT cache the result.

Two-gate freshness: every structure in the registry carries a metric fingerprint `hash{epoch, W-version, k}` **and** a `dirty_since` marker flipped on every corpus write; a structure is fresh iff the fingerprint matches **and** it has not been dirtied since materialization (ADR-0016 §(b); ADR-0017 §3). `scope-row-count-bucket` is intentionally excluded from the fingerprint — within-bucket ingests would read fresh — so content staleness rides the dirty marker, not the fingerprint.

### Dirty-flag / amortize in dream-cycle

Structures that tolerate drift between ingest and the next dream-cycle. A dirty-flag is flipped on ingest; the `MaterializationScheduler` during the dream-cycle claims dirty rows (`status='rebuilding'`), runs the heavy computation via `spawn_blocking`, then flips to `fresh`:

- Whitened/isotropic space + anisotropy cache (W, top-p PCs, CSLS denominators)
- kNN similarity-edge graph + sparse CSR adjacency
- ORC per-edge cache
- Concept-erased projection (LEACE P)
- RIE-cleaned similarity / low-rank Gram
- Dictionary atoms + schema nodes (Mairal online incremental is the ingest path; full refit after major forget waves)
- Spectral cluster labels + centroids
- Laplacian eigenbasis + diffusion-map coordinates + diffusion kernel cache
- GW coupling cache (TTL-invalidated on cluster shift)
- Persistent homology diagrams, Mapper graph

Two startup consistency checks are mandatory: re-dirty anything with `last_computed_at < MAX(facts.t_created)` (crash between commit and notify), and `UPDATE status='dirty' WHERE status='rebuilding'` (crash mid-build).

### Full-rebuild on reconstruction

Triggered by model-swap events (embedding dimension or model identity changes, as handled by #623 same-dimension reconstruction and #742 different-dimension reopen-at-D′). Everything whose validity derives from the embedding coordinate frame must be discarded and rebuilt:

- Whitened space + W matrix: meaningless at new model
- kNN similarity-edge graph: wholesale delete + rebuild (cosine changes globally)
- All spectral/curvature/transport caches: epoch mismatch triggers delete
- Dictionary atoms: meaningless at new dimension
- Koopman mode buffer

The Gavish-Donoho effective-rank check (Gavish and Donoho 2014) should run before committing the HNSW reindex at the new dimension on a reopen-at-D′ event: if effective rank collapses, the new space is degenerate — refuse the commit. This directly hardens the #742 fence path and is the cheapest high-value guard in the whole epic.

---

## 5. Compute-tier partition

Three tiers determine where each structure lives and how the engine schedules its maintenance.

### Real-time (sub-ms to ms; hot-path safe)

- HNSW notify, graph-adjacency notify (have)
- LID per-node (O(k); vector queries only — never a fresh ANN probe on FTS path)
- Hawkes intensity (Ozaki O(1)/access; async fire-and-forget, batched write)
- FRC per-edge (O(deg); degree+triangle, no optimal transport)
- Euler-χ increment (O(1) from degree deltas)
- RMT k\* / Marchenko-Pastur / BBP / Gavish-Donoho thresholds (O(1) given γ)
- VSA bind/unbind (O(d log d) FFT)
- DerivedStructureRegistry dirty-flip + SpaceEpochCounter check
- PPR/RWR push (query-ephemeral; do not cache)

### Blocking (seconds; async/off-path via `spawn_blocking`)

- Whitened/isotropic space + anisotropy cache (randomized SVD, ~O(N·d·k))
- kNN similarity-edge graph build (HNSW top-k + INSERTs); CSR adjacency build
- ORC per-edge (Sinkhorn, batch; FRC is the real-time proxy)
- LEACE/INLP concept-erasure projection (closed form O(d²·k))
- RIE cosine-cleaning / low-rank Gram (Bun et al. 2017; Khawar and Zhang 2019)
- Dictionary-learning batch init (Mairal et al. 2010); spectral cluster k-means after eigenbasis cached
- Schema-node materialization (shared atoms → `fact_type='schema'` facts + `instantiates` edges)

### Genuinely-long / offline (minutes+; dream-cycle/offline only)

- Laplacian partial eigendecomposition (Lanczos O(k²N)); diffusion-map coordinates
- GW couplings (Scetbon 2022 low-rank + N ≤ 500/side hard cap; pairwise cost matrices in cold .pak)
- Persistent homology (Ripser; Bauer 2021; practical to N ≤ 2000); Mapper (Singh et al. 2007)
- Tangent frames + connection Laplacian / VDM (Singer and Wu 2012) — the single heaviest structure: O(N·k·d²); must be restricted to LID-estimated d_local ≪ 1024 and processed per-landmark-patch from cold storage, never held globally in RAM
- Hyperbolic Poincaré training (Riemannian SGD) — `worth-exploring`, pending PoC
- Koopman / DMD mode buffer + topic-centroid trajectories (Brunton et al. 2022; Schmid 2010) — blocked on access-trajectory density PoC
- GDC preprocessing (Gasteiger et al. 2019) — O(N²) naive; build last only if spectral PoC pays off

**Dependency order** (critical for correctness): whitened space → landmark/Nyström set (FPS selection needs the whitened metric) → {kNN sim-graph at scale, Nyström diffusion, GW anchors}. The kNN sim-graph must strictly consume whitened vectors, never raw cosine. The landmark set is a hidden prerequisite for diffusion, GW, and MDS and must be built before them.

---

## 6. Epic map E0–E5

Each epic corresponds to a layer of the operator stack. Epics are ordered by dependency: E0 must complete before E1 is valid; E2 before E3; and so on.

| Epic   | Name                                  | Scope                                                           | Key structures                                                                                                                                                                                                                                                                                                                  | Key operators                                                                                                                                                                                                                                                                                                                                                                                                                    | Gates                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------ | ------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **E0** | Faithful metric ground layer          | The universal prerequisite                                      | Whitened/isotropic space + W matrix; anisotropy cache (μ, top-p PCs, CSLS denominators); RMT k\* cache; `DerivedStructureRegistry`; `SpaceEpochCounter`; two-gate freshness (metric fingerprint + `dirty_since` marker)                                                                                                         | Mean-centering + whitening (ABT method, citation unconfirmed — validate PoC #1); CSLS hubness correction (Conneau et al. 2018); RMT Marchenko-Pastur + Gavish-Donoho rank threshold (Marchenko and Pastur 1967; Gavish and Donoho 2014)                                                                                                                                                                                          | Anisotropy audit PoC: measure mean random-pair cosine on live store; apply whitening; re-measure recall@k. Decision: if random-pair cosine drops toward 0 and recall rises, lock as ground layer.                                                                                                                                                                                                                                                               |
| **E1** | Graph substrate and real-time signals | Ingest-hot-path scalar fields and the proximity graph           | kNN similarity-edge graph (composite covering index required: `(source_fact_id, relation, t_expired)` covering `(target_fact_id, weight)`); sparse CSR adjacency; LID per-node (`facts.lid`); FRC per-edge (`edges.frc`); Hawkes intensity state; Euler-χ monitor → trigger; access-event ring; `IngestDirtyNotifier` extension | LID (Amsaleg et al. 2015; Houle 2017); FRC (Forman 2003); Hawkes-Ozaki (Hawkes 1971; Ozaki 1979); diffusion kernels (Coifman et al. 2005)                                                                                                                                                                                                                                                                                        | kNN sim-graph connectivity PoC: build at k=16–32, compute λ₂/connected-component count. Confirms the typed-edge graph is too sparse for a usable Laplacian and that the derived sim-graph fixes it. Sim-graph must consume whitened space.                                                                                                                                                                                                                      |
| **E2** | Spectral and curvature layer          | Batch offline structures for recall and bridge detection        | Laplacian eigenbasis + diffusion-map coordinates + diffusion kernel cache; ORC per-edge cache; spectral cluster labels + centroids; `MaterializationScheduler` with two-gate (fingerprint + `dirty_since`) invalidation                                                                                                         | GFT (Shuman et al. 2013; Sandryhaila and Moura 2014); spectral clustering (Ng et al. 2001; von Luxburg 2007); Chebyshev filtering (Defferrard et al. 2016; Hammond et al. 2011); ORC (Ollivier 2009; Ni et al. 2019; Topping et al. 2022); diffusion distance (Coifman et al. 2005/2006)                                                                                                                                         | Diffusion-distance vs cosine recall PoC (sweep t); ORC bridge map PoC (list 20 most-negative edges, manually verify cross-domain). Decision: diffusion-distance beats cosine on multi-hop recall at some t ⇒ replace/augment ranker; most-negative edges are real bridges ⇒ ship ORC + curvature-protected forgetting.                                                                                                                                          |
| **E3** | Abstraction and typed traversal       | F3 in-area abstraction and within-space compositional traversal | Dictionary atoms D + sparse codes X; schema nodes (`fact_type='schema'`, pinned; `instantiates` edges); inverted atom→{fact_id} index (required for O(1) bridge query — the `fact→atoms` sparse-code table alone makes F2 bridge query O(N)); VSA codebook (relation-types only)                                                | Dictionary learning (Olshausen and Field 1996/1997; Mairal et al. 2010); VSA HRR bind/unbind (Plate 1995; Frady et al. 2017; Kleyko et al. 2022); Wasserstein barycenter as distributional cluster centroid (Peyré and Cuturi 2019)                                                                                                                                                                                              | Dictionary atoms PoC: learn overcomplete dictionary (K ≈ 2–4×d) on live store; inspect whether co-activated atoms span domains. Decision: if shared atoms reliably bridge domains, materialize atoms as schema-node graph residents.                                                                                                                                                                                                                            |
| **E4** | Cross-domain transfer (F2)            | The missing chunk — coordinate-free structural matching         | Concept-erased projection (LEACE P); RIE-cleaned similarity / low-rank Gram; GW domain-pair coupling cache (low-rank, cluster-capped); pairwise intra-cluster cost matrices (cold .pak)                                                                                                                                         | INLP/LEACE (Ravfogel et al. 2020; Belrose et al. 2023); GW (Mémoli 2011; Alvarez-Melis and Jaakkola 2018; Scetbon et al. 2022); RIE cosine-cleaning (Bun et al. 2017; Khawar and Zhang 2019); FGW (Vayer et al. 2019)                                                                                                                                                                                                            | GW PoC on two small domains (N ≈ 50–100 each): compute internal cosine-distance matrices, run entropic GW (POT / pure-Rust Sinkhorn), inspect coupling T. Decision: if T aligns structurally analogous pairs (not just nearest-cosine), GW becomes the dream-cycle cross-domain operator. This is the single most informative PoC in the epic.                                                                                                                  |
| **E5** | Prediction and heavy geometry         | Trajectory forecasting and curvature-aware relation carry       | Koopman/DMD mode buffer + topic-centroid trajectories; tangent frames + connection-Laplacian / VDM (per-landmark-patch, cold storage); persistent homology diagrams (H0/H1/H2); Mapper graph                                                                                                                                    | Koopman/DMD (Koopman 1931; Brunton et al. 2022; Schmid 2010; Lusch et al. 2018); Kalman (Kalman 1960); parallel transport / VDM (Singer and Wu 2012; Budninskiy et al. 2019); trilateration/MDS (Tenenbaum et al. 2000; Biswas et al. 2006); PH/Mapper (Edelsbrunner et al. 2002; Carlsson 2009; Bauer 2021; Singh et al. 2007); hyperbolic training (Riemannian SGD — primary citation §V unverified, treat as worth-exploring) | Access-trajectory density PoC: measure per-topic access/ingest cadence on real session histories. Decision: if cadence is too sparse, shelve Koopman/Kalman; trilateration (reconstruction) is the safe prediction primitive. Parallel-transport sanity PoC: on LID-estimated d_local ≪ 1024, transport one known relation across one hop vs. flat offset. Decision: if PT beats offset, justify the heavier build; otherwise route F2 entirely through ORC+GW. |

### PoC gates summary

Four experiments gate the most expensive structural investments. They can be run in parallel on real Qwen3 embeddings; recommended order by value-per-effort:

1. **Anisotropy audit (E0 gate, hours):** mean random-pair cosine on live store + whitening sweep. Gates the entire epic. Run first.
2. **GW on 50 nodes per domain (E4 gate, hours):** entropic GW via POT on two well-separated topic clusters. Gates whether the headline missing-chunk capability is feasible at all.
3. **kNN sim-graph connectivity (E1/E2 gate, minutes):** build at k=16–32, measure λ₂. Gates the Laplacian substrate for all of E2.
4. **Access-trajectory density (E5 gate, measurement):** per-topic cadence on real session histories. Gates the Koopman/Kalman build decision for E5.

Domain-identity linear separability (is domain separable in Qwen3 final embeddings?) is a standing open question that affects E4 (LEACE viability) and is cheaply probed by training a linear domain classifier — run this alongside PoC #1.

---

**Key source files:**

- `/home/mroynard/dev/memory-engine/src/store/schema/mod.rs` — schema v14, all SQL tables; `config` K/V; edge indexes at lines 643–644
- `/home/mroynard/dev/memory-engine/src/search/ann.rs` — HNSW: immutable `embed_dim`, tombstones, `rebuild_from_db` under write lock, reconstruction seam
- `/home/mroynard/dev/memory-engine/src/store/embedding_spaces.rs` — multi-space registry, the extension point for all derived spaces
- `/home/mroynard/dev/memory-engine/src/store/fact_vectors.rs` — non-active vector staging, cursorless anti-join, backfill substrate
- `/home/mroynard/dev/memory-engine/src/storage/graph.rs` — in-memory adjacency / degree cache
- `/home/mroynard/dev/autonomous-agent-project/raw/landscape/34-geometric-memory-operator-taxonomy.md` — full operator table (61+ operators), epistemic tiers, composition diagram, PoC specifications
- `/home/mroynard/dev/autonomous-agent-project/raw/landscape/35-memory-data-structure-landscape.md` — full data-structure inventory with H/E/N classification, freshness matrix, gap analysis, critic amendments
