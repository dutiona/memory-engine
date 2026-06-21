# Stage B (#699) HNSW fidelity — self-review

The delegated adversarial reviewer hit the monthly subagent spend limit and produced no file; this review was conducted directly (subagents unavailable). Verdict: **faithful**.

## Verified
1. **Dispatch predicate** (`storage/sqlite/mod.rs:177` `should_use_hnsw`) replicates `engine/mod.rs:379-387` (`active_count() >= ann_threshold`) AND adds a necessary filter-compatibility gate: HNSW only when `temporal==Active && ids.is_none() && pinned.is_none() && metadata.is_empty()`. `HnswStrategy::check_fact_filters` honors only `t_expired IS NULL + fact_type + scope_ids`; the richer dims must fall through to brute-force — which, post-#684 (full FactFilter→SQL translation, now on main), honors them via `convert::build_filter_sql` + `vector_search_filtered`. So the gate is correct and composes with #684.
2. **HNSW search path** (`storage/sqlite/search_index.rs:59-96`) passes `fact_type`/`scope_ids` to `HnswStrategy::search` (per-candidate `check_fact_filters` + exact `load_embedding` rescore), widens `f32→f64` at the boundary, applies `map_seam_err`. The read guard is held across the search (per-candidate DB reads share the conn). Matches the engine's HNSW handling.
3. **Maintenance**: `notify_insert` (`search/ann.rs`) **tombstones the prior entry for the fact_id** before re-inserting → idempotent, so notifying on reinforce (`insert_or_reinforce_fact`) correctly updates, not duplicates. The backend notifies on its owned writes (insert/reinforce/atomic-insert/batch/expire/delete); cycle-apply returns `to_index` for the caller (Stage A contract). No double/under-notify.
4. **Edges**: `search_config==None` ⇒ never HNSW; `ann_threshold==usize::MAX` ⇒ no index built. Non-ann build: `vector_search` always brute-force (`should_use_hnsw` is `const fn false`).
5. **Engine byte-identical** (`git diff origin/main -- src/engine/` empty); engine keeps its own `hnsw_strategy` until Stage E.

## Gate
109 tests (async,ann,archive) · clippy clean (all-features / async / default) · default build compiles (2 pre-existing #630 dead_code warnings unrelated → collateral #712).
