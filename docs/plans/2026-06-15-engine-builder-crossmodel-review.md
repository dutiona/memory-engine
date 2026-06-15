# Cross-model plan review (Round 1, one-shot) — Engine Builder Plan

Reviewers: **Codex** (`codex exec`, read-only), **Gemini** (`gemini -p`, yolo), **Antigravity/agy**
(`Gemini 3.1 Pro (High)`). One-shot mode chosen over persistent tmux for quota economy (Codex near
rate limit) — the plan is self-contained with code citations. Note: `advisor()` is unavailable in this
harness; this cross-model panel + the clean-slate subagent review substitute for it.

All three: **design sound, LGTM modulo the sequencing fix.** No design objections — typestate split-state
builder, golden-equivalence-harness strategy, and D8/D9 deferral all affirmed by all three.

## Findings (deduped)

- **[BLOCKER] (Codex + agy, independent — 2 votes)** D4/Task 3 sealing breaks green-at-every-commit.
  Sealing `EngineConfig` fields (removing `pub`) in Task 3 makes the not-yet-migrated `config.read_only =`
  / `config.backup_dir =` field-poke sites (core tests `tests.rs:817,3015,3225`, `benches/search_bench.rs:377`,
  `cli/src/db.rs:43,57`) fail to compile at Task 3's own `cargo test --workspace` gate. **Fix (both gave
  it):** Task 3 = additive only (`#[non_exhaustive]` + `with_*`, fields stay `pub`); migrate field-pokes
  in Task 4 (core) + Task 5 (cli); remove `pub` (seal) in Task 6 after all sites migrated.
- **[HIGH] (Codex)** Equivalence harness (Task 1/5) too weak for the R1 claim. Snapshot tuple omits
  `backup_dir` and `read_pool_size`, and `search_config_is_some` proves presence not value/precedence.
  Since D6 routes File-`build()` through `open_from_config(&into_config(), reranker)`, the harness must
  prove `read_pool_size`, `backup_dir`, and the effective `SearchConfig` survive that conversion.
- **[MEDIUM] (agy)** R3 references "Task 8" but tasks stop at Task 7. Add a formal Task 8 = feature-gated
  verification (`cargo build --features ann` / `--features async` spot builds), not just §6 commands.
- **[MEDIUM] (Gemini)** Add an `ann`-disabled + `SearchConfig`-provided test/build case to verify the
  builder doesn't force-link HNSW symbols when `ann` is off.
- **[LOW] (Codex)** §6 lists `cargo doc --no-deps --all-features` but the top-of-file end-state gate also
  requires the default-feature doc path clean — add explicit default-feature `cargo doc --no-deps`.

## Resolution

- [BLOCKER] sealing sequencing → **Addressed.** D4 restructured into staged sealing; Task 3 is now
  additive (`#[non_exhaustive]` + `with_*`, fields stay `pub`); Task 4/5 migrate core/cli field-pokes;
  Task 6 removes `pub` (seals) after all sites are migrated + before/with constructor deletion.
- [HIGH] equivalence tuple → **Addressed.** Task 1 snapshot tuple extended to
  `(embed_dim, is_file_backed, is_read_only, read_pool_size, backup_dir_is_some, search_config effective
value, upcaster_len, reranker_name)`; Task 5 re-point demands byte-identity on the extended tuple.
- [MEDIUM] formal Task 8 → **Addressed.** Added Task 8 (feature-gated verification matrix) to the task
  list; R3 reference is now consistent.
- [MEDIUM] ann-disabled + search_config → **Addressed.** Added to §5 Testing and the Task 8 matrix.
- [LOW] default-feature `cargo doc` → **Addressed.** §6 now lists `cargo doc --no-deps` (default) +
  `--all-features`.
