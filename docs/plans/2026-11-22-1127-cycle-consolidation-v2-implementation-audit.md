# #1127 implementation audit (pre-review)

- Scope: `pre-review`.
- Approved plan: `docs/plans/2026-11-22-1127-cycle-consolidation-v2.md`, sha256 `ae4aacb0746360665d0812f95d16d19fae0ece9694ce9252b99d3d2af633a24e` (plan issue #1137, approved by the owner's marker comment 2026-11-22T01:29:46Z).
- Implementation head audited: `da534e7` (the last code commit; this file is committed on top of it).
- The approved plan bytes are unchanged; this file is the record of what happened.

## Items

| # | Item | Status | Evidence |
|---|---|---|---|
| 0 | Plan and baseline | Implemented | a4ebd35, f9d5951; base suite green before any change |
| 1 | #1099: measure what times out | Dropped (owner) | The owner, 2026-11-22: "split it out." Session 1 ran B x3 and L2-1..8 (L2-9 stopped by the owner); the numbers and the table's reading are handed over on #1099. The remaining trimmed protocol (C x5, P x1) runs in a scheduled idle-host window, outside this PR |
| 2 | #1099: the harness fix | Dropped (owner) | Same decision; the variant waits for item 1's remaining runs. This PR carries no closing keyword for #1099 |
| 3 | #1080: dedup by fact id | Implemented | `fix(consolidate)!` commit. Intent-preserving test variations, below |
| 4 | Boy-scout: consolidation docs | Implemented | b97ac92 |
| 5 | #1062: cycle report declares its identity | Implemented | `fix(cycle)!: a cycle report declares…`; `embedding_fingerprint` is `Option<Box<EmbeddingFingerprint>>` (below) |
| 6 | Boy-scout O2: promotion declares its vector | Implemented | `fix(cycle)!: a promotion declares…` |
| 7 | Boy-scout: CLI identity check before the LLM | Implemented | `fix(cli): check the summary embedder's identity…`; the LLM arm moved into `run_llm` (below) |
| 8 | #1095: set_level not found | Implemented | d1d5cdd |
| 9 | #1089: rename the decay prune | Implemented | `refactor(forgetting)!` commit, after a fresh fetch (origin/main unchanged at 7ba301e; D5's audit list identical, 52 files) |
| 10 | Rebase and Verification | Implemented | rebased on origin/main 7ba301e; Verification table in the PR |
| 11 | PR, review, finish | In progress | this PR; `/super-review` Full, `/finish-pr` |

Counts: 12 planned items: Implemented 9 (0, 3–10), Dropped 2 (1, 2, owner-decided), In progress 1 (11); Modified 0, Added 0. Divergence (Modified + Dropped + Added) = 2 / 12 = 17%, under the 30% threshold, and both dropped items are the owner's decision, quoted above.

## Collateral fixes (Tier 1, not plan items)

- `docs(cli): say that consolidate drains the #209 deferral itself`: cli-inspector said the first invocation after a write defers and a second one runs; the command drains up to 8 deferrals in one call.
- `fix(deps): update url to 2.5.9 for RUSTSEC-2026-0592`: `cargo deny check` failed on the new advisory (origin/main fails it too); a lockfile-only update, Cargo.lock being in this PR's diff already.

## Intent-preserving variations (not Modified)

- **Item 3's tests.** Every behaviour the plan names has a test that a mutation proves load-bearing. Names differ where one test covers a shared mechanism:
  - `an_unforget_between_load_and_apply_discards_the_plan` and `a_reconstruction_between_load_and_apply_discards_the_stale_plan` → `a_rewind_between_load_and_apply_discards_the_plan`: unforget, unpin and reconstruction all go through `rewind_dedup_watermark`, which bumps the generation; plus `a_changed_generation_discards_the_plan` for the apply-side check.
  - `unforget_rewinds_the_dedup_watermark`, `unpinning_rewinds_the_dedup_watermark`, `promote_clears_the_dedup_watermark` → the facade tests `dedup_compares_an_unforgotten_fact`, `dedup_compares_an_unpinned_fact`, `dedup_compares_every_fact_after_a_reconstruction`, each killed by removing its write's rewind.
  - `a_backdated_fact_is_compared` (pipeline) added beside the facade's `dedup_compares_a_backdated_fact`.
- **Item 5: `Option<Box<EmbeddingFingerprint>>`.** The plan wrote `Option<EmbeddingFingerprint>`; unboxed, `CycleOutcome::Ran(CycleReport)` trips `clippy::large_enum_variant` (376 bytes against 16). Boxing the field, as `MemoryError::EmbeddingModelMismatch` already boxes its fingerprints, keeps `CycleOutcome`'s public shape; the wire format is unchanged.
- **Item 7: `run_llm`.** The identity check pushed `commands::consolidate::run` over clippy's 100-line limit, so its LLM arm moved, unchanged, into `run_llm`.

## TDD and mutation evidence

- Red at the base (behavioural tests against the code before each fix): #1095 `forget_and_set_level_answer_not_found_alike` FAILED at f9d5951 (`set_level` answered `error: not found`); #1080 `dedup_compares_{a_backdated,an_unforgotten,an_unpinned,every_fact_after_a_reconstruction}` FAILED at b97ac92 and `dedup_compares_a_reused_id_after_a_restore` passed (the guard, as planned); #1062 `llm_backend_refuses_a_summary_embedder_of_another_identity` (item 5's form) FAILED at the #1080 commit (exit 0, outcome `ran`); #1089 `reexports_are_accessible` did not compile before the rename (E0425 `PrunePolicy`, E0433 `pruning`).
- Mutations, each applied alone and reverted, each KILLED by its named test: generation check, survivor hold, vectorless hold, rewind's generation bump, pin bump, unpin rewind, unforget rewind, promote reset, restore exclusion (#1080); undeclared refusal, cycle identity check, promotion identity check, `LlmDreamCycle`'s declaration, the facade carrying the field, the CLI pre-check (#1062 and items 6–7). 15 of 15 killed.
- Guard proof (#1089): a temporary `MemoryEngine::prune` call in `crates/memory-engine-mcp/src/tools/mod.rs` failed `cargo clippy -p memory-engine-mcp` (rc 101) with `use of a disallowed method memory_engine::engine::MemoryEngine::prune`; reverted.
- Structural sweep (#1089): the plan's three `rg` checks print nothing; the eight ADRs carry both amendment parts; the word list and the line review leave only retraction names and D5's kept classes. `last_consolidated_at` in `crates` appears in `apply_plan`'s deletion, its test, and `inspect/types.rs`'s snapshot round-trip test (an old snapshot's config map: data, kept).
