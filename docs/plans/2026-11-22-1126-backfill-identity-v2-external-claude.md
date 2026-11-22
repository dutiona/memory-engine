Reviewer: Claude Opus (fresh general-purpose subagent, no conversation context) via the Agent tool; stand-in for Codex

~~~~
Altitude report SHA-256: 0e185627ee21cb44d7c71b3a806f1ea45d3d6b8f95209881d1e22c4a1844a8f4

**LGTM.** Every resolution in the r3 delta holds against the code, and the delta adds no new load-bearing problem.

**Altitude, assessed first.** Every figure in the report is right, and so is its "proceed".
- `plan.md` hashes to `4cff7c90…31e0`, matching the plan digest in the report's header.
- The plan is 583 lines, 59147 bytes and 19 tasks. Its one fenced block runs from line 560 to 578, with 17 body lines.
- It differs from the predecessor by 116 lines.
- The revision 3 section describes the delta accurately. It adds no new structure and duplicates nothing, and no override is needed.

**Resolutions checked:**
- **Golden and struct literals: holds.**
  - `restore.rs` has exactly eleven `EngineSnapshot` struct literals. Two more matches, at lines 1533 and 2943, are return types and need no change.
  - The `types.rs` literal and the `snapshot_populated_engine_dump` golden are now listed.
  - The `inspect::` filter also matches `engine::inspect::…`, so the separate `engine::inspect` Green command is redundant but harmless.
- **Call-record fixture: holds.**
  - The fixture now matches `remember.rs`: the record comes first, and the fact points to it through `source_event_id`.
  - For a fact target, `select` leaves `sel.events` empty. So `{fact: 3, source_event: <record>}` really does land in `linked`.
  - The test's real red signal is "`get_fact(3)` is `None`, so `linked` names no live fact", because today the new fact would take id 3.
- **Negative-mark check dropped: accepted.**
  - `max` already makes a low or negative mark harmless.
  - The upper-bound case is the same as a restored row id of `i64::MAX`, which restore already trusts today.
  - No stale reference to `validate_rejects_a_negative_sequence` remains, and `validate_snapshot` has rightly come off item 15's file list.
- **Breaking-change list, rustdoc and the acceptance mapping in the PR body: all done**, and they read consistently.

REVIEW COMPLETE
~~~~

## Resolution

Codex took the successor's round 1 (plan 3fd256b5…, resumed thread from the predecessor's review). Its findings, all resolved in revision 2:
- [HIGH] The call-record acceptance could pass vacuously: fixed. Named call records were added, and the mapping of "export, erase, restore, import" to the test's order is stated.
- [HIGH] An older binary ignores `sequences`: stated, not changed. An older reader cannot be made to refuse a snapshot: its only gate is `schema_version`, and bumping that needs a migration and makes older binaries refuse the store itself. Decision 14 and the docs now give the guarantee as "an export taken by this version or later, imported by this version or later", and both directions are tested.
- [MEDIUM] Not all six marks were proven: fixed with `restore_takes_each_exported_mark_or_the_highest_row`.
- [LOW] The empty golden was wrong: fixed (it gains the `scopes` mark).

Codex then hit its usage limit (`turn.failed`: usage limit until 2026-11-25 13:06), so round 2 never ran. The stand-in for its slot was agy's `claude-opus-5-5`, which answered 429 ("Individual quota reached … Resets in 72h"). The slot then went to a fresh Opus subagent.

Stand-in round 1 (plan d00f9b8f…). Its findings, all resolved in revision 3:
- [MEDIUM] Missing second golden and struct literals: fixed (`snapshot_populated_engine_dump`, the eleven literals in `inspect/restore.rs`, `inspect/types.rs`, and the `engine::inspect` Green command).
- [MEDIUM] `remember` does not write `fact_ids`, and `linked` comes from `source_event_id`: fixed. The fixture is written that way.
- [LOW] `EngineSnapshot::sequences` was missing from the breaking-change list: fixed.
- [LOW] The negative-mark check guarded a harmless value: the check was dropped, with the rationale in Decision 14.
- [LOW] Rustdoc not listed: fixed.
- [LOW] Redundant assertion, and the order mapping in the PR: fixed.

Stand-in round 2 (plan 4cff7c90…, final): LGTM.

Degradations: codex = usage limit reached during successor round 2 (until 2026-11-25 13:06); its round 1 was collected and every finding resolved. agy-Opus stand-in = 429 quota, so a fresh Opus subagent took its place.
