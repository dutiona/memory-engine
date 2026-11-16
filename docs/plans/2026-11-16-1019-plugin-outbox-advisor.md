# Advisor capability unavailable

Altitude report SHA-256: 2f2df9c27a42c3c0d788389e42688fd37b966a91105fac49ee8670ab2095cdda

- Reason: `advisor()` is not exposed in this fleet worker's tool set (the environment sets the experimental flag, but no advisor tool is callable here).
- Gate: Full. Advisor-less form per `debate-defaults`: Codex + agy + clean-slate subagent, all three ran in round 1.
- Findings the fallback produced, attributed to the reviewer that produced them (none is attributed to an advisor):
  - Codex (headless, round 1): 5 HIGH, 3 MEDIUM, 1 LOW — stale-lock recovery removing a new owner's lock; first-use endpoint ordering losing a wakeup; item 3 asserting stub responses before any flush exists; the Claude credential in `env`'s argv; the restricted PATH breaking the harness's own tools; permanent event-ID uniqueness under clock rollback; the token-without-LF `read` rule; `mv` nesting onto an existing directory; G3 and the state-root check encoding incidental host state. Full text: `~/.local/state/super-plan/2026-11-16-1019-plugin-outbox/codex-shared.md`.
  - agy (gemini-3.8-flash-high, round 1): LGTM. Full text: the round directory's `review.md` under `~/.local/state/super-plan/2026-11-16-1019-plugin-outbox/`.
  - Clean-slate subagent: 5 MEDIUM, 13 LOW — see `2026-11-16-1019-plugin-outbox-subagent-review.md`.

## Resolution

Applied to plan revision 2 (SHA-256 `ac2e10b72a66f56365d2f5ba11b0e24ac071b67b3ea6783e45da393cbe50c8f8`). Codex's findings:

- [HIGH] stale-lock recovery can remove a new live owner's lock → D8: break by one atomic rename into a private name, then delete; the invariant (a flusher sends only right after reading its own record, and stops at the first check that reads anything else) and the bounded residual (one duplicate, an early stop, nothing lost) are stated; new `lock_steal` and `release_race` cases drive the two worst interleavings with `mv`/`rm` wrappers.
- [HIGH] first-use endpoint ordering → D5 records the endpoint (step 2) before the event is published (step 8).
- [HIGH] item 3 stub assertion before any flush → removed from item 3 (capture only); `real: the stub answered only 503 during the run` moved to item 7.
- [HIGH] credential in `env`'s argv → item 3 uses `os.execve` with an environment built from nothing; `real_no_token_argv` case.
- [HIGH] restricted PATH breaks the harness → only the helper's process tree runs on `rbin`; assertions use the normal PATH.
- [MEDIUM] permanent ID uniqueness → accepted residual stated (D8 prune, R12): needs a backward step over an hour plus pid and seq reuse; the server's 409 parks it, kept, not lost; keeping reservations for ever would grow `seq/` per event.
- [MEDIUM] token without LF → D8 states `IFS= read -r tok || [ -n "$tok" ]`; `token_forms` case covers with LF, without, and empty.
- [MEDIUM] `mv` onto an existing directory → D5 step 7 treats an existing `O/<id>` as a collision and moves to the next seq; `id_collision` case.
- [LOW] G3 and the state check encode host state → G3 checks a bash 3.2 version via `$BASH_VERSION`; the state check compares before/after records.

### Round 2 (Codex: 3 HIGH, 2 MEDIUM; agy: LGTM) → revision 3 (SHA-256 `b64037f714dbeba57626a45b03bbf21cba3b741f103d4607491d5f954e990284`)

- [HIGH] renewal can overwrite a replacement owner → D8: `owner` is written once before publication and never rewritten; renewals go to a per-flusher `<pid>.stamp` published by rename, so a displaced renewal lands under its own name in the replacement and is never read as the replacement's; staleness reads the newer of `owner`'s epoch and the owner's own stamp; `renew_race` case with two real records.
- [HIGH] clock-rollback ID reuse contradicts ADR §3 (and a same-body event could be acknowledged as `duplicate`) → met in the design instead of accepted: D5's monotone epoch (an event never takes an epoch below the high-water mark of `seq/`, with one extra `date` call in that rare path) plus D8's prune below that mark, which never removes the newest second; IDs are unique per machine for ever; `clock_back` case; risk R12 retired.
- [HIGH] the mode check cannot run in the transferred macOS tree → moved from `run.sh` to Verification (`git ls-files -s`), where an index exists.
- [MEDIUM] partial-tree recovery only covers `O/tmp` → D5's heal rule applies at every step that writes into a D4 directory (`O/tmp`, `O/seq`, `O/rejected`, `S/bin`); `tree_heals` deletes each on its own.
- [MEDIUM] the linked-worktree root residual conflicts with §3 → D7 resolves Root from the first line of `git worktree list --porcelain` in every layout other than `…/.git` (main working tree, or the bare repository when there is none); `facts_separate_gitdir` and `facts_bare_worktrees` cases.

### Round 3 (Codex: 3 HIGH, 1 MEDIUM; agy: LGTM) → revision 4 (SHA-256 `b3475a32f626832e9e987d6a554707ffa00bd67cedf9ca71577d704df5e08dac`)

- [HIGH] high-water selection races with reservation and pruning → D5 step 7 re-reads the glob after its reservation and restarts once when its epoch fell more than an hour below the new high-water mark; `frozen_reserve` case.
- [HIGH] a stepped-back clock exhausts one second's seqs and drops events → the seq is now a monotone (epoch, seq) counter taken from the glob's last entry (fixed-width `seq/<epoch>.<seq6>`), starting above the high-water pair rather than at 0; past 999999 it moves to the next epoch, so nothing is ever abandoned for lack of seqs; the usual case costs one `mkdir`; `seq_overflow`, `seq_leading_zeros` (octal trap in dash) and a five-event `clock_back` cases.
- [HIGH] porcelain worktree paths need decoding → `git worktree list` is no longer used: Root is the common dir minus `/.git`, else the common dir itself, which every worktree of one repository shares; probed: in a `--separate-git-dir` layout git itself reports the git dir as the first worktree. `facts_separate_gitdir` uses a working tree named with a space, a quote and é.
- [MEDIUM] retrying a partly consumed body write → the heal rule never applies to the `cat` step; a body-write failure abandons the event; `body_write_fail` case.

### Round 4 (Codex: 2 HIGH, 1 MEDIUM; agy: LGTM) → revision 5 (SHA-256 `7fc9a85042a055f5a4d5570c1969f0a057e8e6a18bbf8b9a72d027fc3285d34a`)

Arbitration (no advisor; orchestrator decision, one line each): the round-2 to round-4 loop on event-ID uniqueness and on the worktree root shows two tensions inside ADR-0020 itself, which no client design can resolve without changing the contract. The worker may not amend a binding ADR on its own, so both go to Michaël at plan approval, with a recommendation, and the plan implements the ADR's literal rules meanwhile.

- [HIGH] the common git dir is not the main working-tree root → D7 rewritten: `…/.git` common dir → its parent; in the main or only working tree → the top level; in a linked worktree of a layout whose main tree git cannot name (probed) → that worktree's top level; escalated as Q8 with a one-sentence ADR clarification recommended.
- [HIGH] the monotone counter changes §12's seq and time rules → reverted to §12's literal rule (seq from 0, one `date` call, reservations pruned after an hour); the uniqueness tension is Q7, options (a) residual plus a one-line ADR amendment (recommended), (b) never prune, (c) the revision-4 counter with a §12 amendment.
- [MEDIUM] the post-reservation re-read does not make pruning atomic → moot with the revert; covered by Q7.

### Round 5 (Codex: 1 HIGH, 2 MEDIUM, all on the wording of Q7/Q8) → revision 6 (SHA-256 `2c708536421d882960be03ef1b89719d75aaa18f30e4b5fd2e8bba3340904d08`)

- [HIGH] Q7 (c) overstated → (c) now says it is not a ready design (the pause-after-re-check window and the bounded retry of revision 4) and would need a corrected, re-reviewed design.
- [MEDIUM] Q7 (a) understated the identical-body case → it now says the second, distinct occurrence and its effects are not stored.
- [MEDIUM] Q8 alternative incomplete → it now says the common dir would apply to the main tree too (D7 case 2 changes) and yields no path key when the git dir is outside the dev root (the project lands in the machine's `unscoped` bucket).

### Round 6 (Codex: LGTM; agy: LGTM; clean-slate subagent re-review: 2 MEDIUM, 5 LOW) → revision 7 (SHA-256 `a2aa2b8b9eca156b1a93258862a83af604c67057e0e8d7233a3d046f6fcc0af8`)

The subagent's re-review findings and their resolutions are in its review file's second `## Resolution` section.

### Round 7 (Codex: 1 HIGH, 1 MEDIUM; agy: LGTM; subagent: LGTM with 2 LOW) → revision 8 (SHA-256 `b73b9a2ceca38acad1438128728dc019168e0360043e013b82d7eae63c044546`)

- [HIGH] a failed endpoint update can deliver to the previous endpoint → D5 step 2 removes `O/endpoint` when the update fails, so every flush holds until a later hook records the new URL; `endpoint_update_fail` case.
- [MEDIUM] a separate git dir named `.git` in a linked worktree → D7 case 2 states git's own view (probed 2026-11-16: `git worktree list` reports the git dir's parent first), and Q8 now lists every layout with what the plan sends; `facts_layouts` covers it.
- Subagent N8/N9 → see its review file's third `## Resolution`.

### Round 8 (Codex: 1 HIGH; agy: LGTM; subagent: LGTM, no new finding) → revision 9 (SHA-256 `726e670757f1e2c2b98a35aa6706bc73c36eba40da166b6f2691b659fb789ce9`)

- [HIGH] removing the endpoint does not invalidate a running flusher's URL, and `rm -f` may fail → D8: the flusher re-reads `O/endpoint` with the `read` builtin before each request and holds when it changed or vanished; the read-to-request window is the same one-request residual as the lock's; D5 step 2 states that when the `rm` fails too, `O` is not writable and step 7's publication fails the same way, so nothing is queued or sent; `endpoint_changed_mid_flush` case.

### Round 9 (Codex: 1 HIGH) → arbitrated, no plan change; review loop closed

- [HIGH] a concurrently running session with the old configuration can write the old URL back after a failed update removed it → **arbitrated as an accepted residual, not a new design defect** (protocol exit 2b, orchestrator decision without advisor). The endpoint is per machine by the issue's own acceptance ("Each hook also records the ME endpoint in a state file under the outbox, so `me-hook flush` works with no arguments"); while two sessions run with different `me_url` values, the recorded endpoint is whichever was written last, with or without any write failure, and every queued event goes there. Both URLs are ones Michaël configured, and the machine token is the same for both, so nothing reaches a party he did not configure; the window ends when the sessions started before the change end. The README's configuration section states it ("restart running sessions after changing `me_url`; until then events go to the URL recorded last"). Changing this needs a per-event endpoint, which would send events queued for an old server to it after a reconfiguration, which is worse. Not escalated as a question: it follows from a confirmed acceptance criterion.
- Revision 10 (SHA-256 `1013f249f1e534125e8142d4c9b29cc44e27ffb427037aa5509d95754942e077`) records that residual in D5 step 2 and the README bullet, and moves the two endpoint cases to item 5 (subagent N10); round 10 asks all three reviewers only to acknowledge the altitude report and check this delta.

Final assessed report (controller metadata; no advisor ran):
Altitude report SHA-256: e95939ca4f741e00b243ed380723f19efc80f6a73774676248b71efbbbd12bd9
- Round 11 (all three: LGTM): revision 11 (SHA-256 `25aca0c4b3f4a49f44c7b1dd70df79a83915f0f98e2a70e07e85cb4537f44fad`) only removes two machine-local paths from Operational steps 1 and 2, which the plan-issue archiver refused.

Final assessed report (controller metadata; no advisor ran):
Altitude report SHA-256: 8d448789593d8542fb5d407011beb187ab6752053c091968bf22d785a645e166
