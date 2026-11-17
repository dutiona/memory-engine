# External review: Codex (headless, config default gpt-6.1-terra, read-only sandbox)

Review-tree paths are written as `<review-tree>` (public repository).

## Round 1 (plan SHA-256 576e640aa93bd683194e81631a40ba7c5e54c462098daf1120dde8e113ef6e69), verbatim

Michaël, the plan needs revision before implementation. Confidence is high in the first two findings; the temporal finding follows directly from the current query behavior.

The altitude report’s **proceed** disposition is justified: the normative key rules specify persistent behavior, and the test lists are not copies of product files. The report, plan, and provenance-input digests match. The plan contains Documentation, Testing, and Verification sections. Its proposed D2 amendment to ADR-0021 is explicitly conditional on your approval.

Altitude report SHA-256: 378409c50b3e9819fc052e4ca2144e76b8a2ad50fe0e23368bb759b3b7c7d81f

- **[HIGH] A committed move can be reported and recorded as a failure.** Item 3 calls `cache_scope_chain` *after* the transaction commits ([plan, lines 245–251](<review-tree>/.review/plan.md:245)). That cache walk performs fallible storage reads ([scope code](<review-tree>/crates/me-index/src/scope/mod.rs:54)). If one fails, item 8 treats the error as a failed move and records a refusal, although the fact and its move record already committed ([plan, lines 421–425](<review-tree>/.review/plan.md:421)). The caller can retry a move that happened, and the new project may remain absent from the cache. Define and test post-commit failure handling so a committed operation cannot be presented as an uncommitted one and the cache is reconciled.

- **[HIGH] A failed `remember` can mint a project that the contract says is unknown.** The accepted residual and item 6 allow scope creation before the fact insert succeeds ([plan, lines 87–89](<review-tree>/.review/plan.md:87), [lines 354–359](<review-tree>/.review/plan.md:354)). Current ingestion confirms that scope creation is a separate transaction ([ingest code](<review-tree>/crates/me-ingest/src/lib.rs:205)). An insertion failure then leaves a node that item 5 treats as a known project, although no event or memory was filed there. This contradicts ADR-0021 §5 and C5. Make project creation and the successful filing atomic, or determine known-project status from actual filing evidence; add a failed-write acceptance test.

- **[MEDIUM] D4 mistakes a future invalidation for an already invalid memory.** Items 3 and 8 reject every non-null `t_invalid` ([plan, lines 245–247](<review-tree>/.review/plan.md:245), [lines 421–423](<review-tree>/.review/plan.md:421)). Current reads consider a fact valid while `t_invalid` is later than the query time ([facts code](<review-tree>/crates/me-backend-sqlite/src/store/facts.rs:455)). Consequently, `recall` can return a currently valid fact that `set_level` answers `not found`. Align the transactional check with the intended temporal semantics and test a future invalidation.

This was a read-only review; no tests, builds, or gates were run.

REVIEW COMPLETE
## Round 2 (delta to plan SHA-256 223edc02864136079c6f672bc4b40b6be7919d1feed2df95b39bd7ebefc34f90), verbatim

Michaël, the round-two delta needs revision. Confidence is high.

The altitude report’s **proceed** disposition remains justified: the revision adds detail to the same coordinated implementation rather than embedding a second product artifact. Its digest matches the reviewed plan.

Altitude report SHA-256: 6aa214ddd654d3198bf6eddb4f33a30e7d4854efcddc276cc19ea2c0eb7a719f

- **[HIGH] The unknown-project failure remains possible.** The revision fixes a fact-insert failure after a successful call record, but explicitly retains the failure between creating the project node and inserting that record ([plan, lines 89–91](<review-tree>/.review/plan.md:89)). Item 4 performs those operations separately ([lines 304–307](<review-tree>/.review/plan.md:304)); a failed record insert therefore leaves a project with no filed event or memory that `recall(project: K)` treats as known. This still violates ADR-0021 §5 and C5. Create the node and record atomically, or base known-project status on committed filing evidence; test a record-insert failure.

- **[MEDIUM] Item 4’s green gate depends on behavior assigned to item 6.** Item 4’s revised test expects a keyed `remember` record under the project ([lines 284–288](<review-tree>/.review/plan.md:284)), while its implementation says every caller still uses the bucket except the valid `remember` changed in item 6 ([lines 304–307](<review-tree>/.review/plan.md:304)). The required item 4 green test cannot pass in that sequence. Move the expectation to item 6 or implement that record placement in item 4.

The revised move path addresses the post-commit fallible cache read, and the future-`t_invalid` change addresses the temporal finding. This was a read-only delta review; no tests or gates were run.

REVIEW COMPLETE
## Round 3: not run (degraded)

`codex-headless-round.sh --resume` answered `CODEX_EXEC_FAILED`: the account hit its usage limit (resets
2026-11-18 12:20). Per the fleet rule, Vibe (Mistral Large 4) took over as the external voice for revisions 3 to 6; see
`2026-11-17-1022-project-key-external-vibe.md`. OpenCode was not usable: `opencode-review` reviews a commit of the
dotfiles repository only, behind an owner-risk activation.

## Resolution

- Round 1 [HIGH] post-commit cache failure → the port returns `ScopedWrite.chain`, read inside the transaction; the
  cache insert after the commit is infallible (item 3).
- Round 1 [HIGH] a failed `remember` minting an unknown project → D1 and round 2 below.
- Round 1 [MEDIUM] future `t_invalid` → liveness at `now` in the port's check and in `set_level` (D4, items 3, 8).
- Round 2 [HIGH] node and record not atomic → `EventLog::insert_event_at` and `MemoryEngine::ingest_at` create the
  record's path in the record's transaction; every call record goes through a scoped write; test
  `a_remember_whose_record_fails_mints_nothing`.
- Round 2 [MEDIUM] item 4's test depending on item 6 → item 4 asserts the bucket; item 6 asserts the target.
