Reviewer: Codex gpt-6.1-terra (medium) via codex-headless-round.sh (headless, resumed thread, 3 rounds)

~~~~
Michaël, **LGTM** for the round-3 delta. Confidence is high.

The altitude report’s proceed disposition remains justified. Item 4 now passes both filters to libtest, and Decision 8 explicitly records the archive/promote residual and its consequence.

Altitude report SHA-256: 6ff4dbd2815adc72233d1d08b9b39bcfeb127e01f3b04beee69eddbe99caef37

No project tests or gates were run in this read-only review.

REVIEW COMPLETE~~~~

## Resolution

Round 1 (plan 7db7d050…):
- [HIGH] Item 10 archive race (identity read apart from the candidate scan): fixed in revision 2. `FactGraph::select_archive_candidates` returns `ArchiveCandidates { facts, edges, embedding_identity }` read in one read transaction (Decision 8, item 10); the documented residual was removed.
- [HIGH] Item 10 `None == None` scored: fixed in revision 2. Vectors are scored only when both identities are present and equal; `search_scores_vectors_only_within_one_identity` covers no active identity.
- [HIGH] Item 8 does not complete for a rejected fact that holds a vector: kept, arbitrated. #1081's defect and acceptance concern a deferred item that failed (400/413/422) and that the new provider also rejects; completing past a fact with a serving vector would drop that vector, a policy change no issue asks for. Decision 7 now states the reason and offers Michaël the reversible alternative at approval; `reconstruct_still_aborts_when_the_target_rejects_a_fact_with_a_vector` pins the default. Codex did not re-raise it in rounds 2 and 3.
- [HIGH] Item 4 dimension-only case: fixed in revision 2. `validate_embed_dim(&RecordedEmbedding, dim)` uses `dim()`; `an_all_waiting_store_refuses_another_dimension` and the dimension-only restore case were added.
- [HIGH] Item 13 CLI key file unvalidated without an embedder: fixed in revision 2. Key settings resolve at the top of `build_optional` (Decision 11); `an_fts_only_query_refuses_a_bad_key_setting` added.
- [MEDIUM] Item 8 window-progress trap: fixed in revision 2. The cursor advances on the fetched window before filtering, only an empty fetch ends a pass; `backfill_passes_a_window_made_only_of_skipped_facts` and `reconstruct_resumes_past_facts_it_skipped` added.

Round 2 (plan cc3ef502…):
- [HIGH] A promote between the archive's candidate read and `commit_archive_atomic`: arbitrated as an accepted residual, recorded in Decision 8 (revision 3). The pak holds the vectors it captured under the identity it records, so nothing is scored across spaces; the facts become text-only one reconstruction early (the outcome of archiving just before it); their deletion is the archive's intent and the retained `fact_vectors` rows cascade. Codex accepted it in round 3.
- [MEDIUM] Item 4 red command with two positional filters: fixed in revision 3 (filters after `--`).

Round 3 (plan c2643491…): LGTM.

Degradations: none. The Codex model is `gpt-6.1-terra` per Michaël's 2026-11-21 instruction ("if you need codex, gpt 6.1 terra only"), not the debate-defaults roster's `gpt-6.2-sol`.
