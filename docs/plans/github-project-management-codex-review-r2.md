[Codex Review] [BLOCKER] docs/plans/2026-05-31-github-project-management.md:453 and docs/plans/2026-05-31-github-project-management.md:460 - Task 10's `create_select` guard can skip creating every non-built-in field. `field_id` runs `gh api ... --jq` and returns the command exit status, not whether an ID was printed; a jq filter that selects no nodes normally exits 0 with empty output. Therefore `field_id "$1" "$2" >/dev/null 2>&1 && return 0` can return before creating `Priority` and `Phase`, leaving `state.json` incomplete and causing Task 11's field lookup to fail. Capture the ID and test it explicitly, e.g. `local fid; fid=$(field_id "$1" "$2"); [ -n "$fid" ] && return 0`, ideally with `field_id` also failing loud when duplicate field names appear.
[Codex Review] [MEDIUM] docs/plans/2026-05-31-github-project-management.md:484 - Task 11 still does not spell out the item-id lookup/capture structure enough to verify the "issue->item lookup" fix end-to-end. The `set_field` mutation shape is correct (`value:{singleSelectOptionId:$o}`), and the text requires skip-when-unset plus fail-loud-on-null, but the plan does not include the concrete loop or JSON map used to get from `<project,issue>` to `item_id`. This is less severe than the Task 10 field bug, but it leaves a round-1 C6 fix partially unverifiable from the plan alone.
REVIEW COMPLETE

## Resolution

- **[BLOCKER] R2-1** (`create_select` guard skipped field creation because `field_id` exits 0 on empty output) → FIXED: capture then test — `local fid; fid=$(field_id "$1" "$2"); [ -n "$fid" ] && return 0`.
- **[MEDIUM] R2-2** (Task 11 item-id lookup under-specified) → FIXED: Task 11 Step 1 now records an `issue⟶item_id` map TSV per project (`project_item_add` returns the id), and Step 2 reads `iid` from it before `set_field <project_id> "$iid" "$fid" "$oid"`.

Loop converged after round 2: both fixes are mechanically straightforward (shell capture-and-test; a concrete map) with no new risk surface.
