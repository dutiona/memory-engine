# Codex plan review, all rounds (headless, thread recorded in the super-plan state)

## Round 1

[Codex Review]

Michaël, the plan needs revision before implementation. Confidence: high. Its altitude is appropriate for a new client with a binding wire contract; the exact request, encoding and lock decisions are justified. The ADR’s narrower status reactions also correctly override the issue’s restatement.

Altitude report SHA-256: 2f2df9c27a42c3c0d788389e42688fd37b966a91105fac49ee8670ab2095cdda

- [HIGH] **D8, item 6 — stale-lock recovery can remove a new live owner’s lock.** Two contenders can both inspect the same stale owner. One removes it and acquires a new lock; the other then executes its already-decided `rm -rf O/lock`, deleting that new lock. The first flusher can subsequently renew or release another flusher’s lock after a separate ownership check. This is broader than the stated residual of one duplicate: multiple active flushers can interfere with delivery and parking, and a flusher can remove another’s lock. Server idempotency does not protect the local lock or rejected directories. Specify a recovery and release protocol that handles ownership changes, and test these interleavings.

- [HIGH] **D5 steps 6–10 and D8, items 5–7 — first-use endpoint publication can lose the wakeup.** A hook publishes its event before recording the endpoint. An existing flusher can see the event, find no endpoint, classify that as a hold, and prepare to exit. The hook then records the endpoint and starts its flush while the old lock is still held; that flush exits, and the old flusher does not perform the second look because it stopped on a hold. The event remains queued until a later hook or watcher. Order endpoint publication and lock handling so this transient initialization state cannot strand the wakeup, and add a controlled interleaving test.

- [HIGH] **Item 3 — the early real-run gate cannot pass at its planned stage.** Item 2 does not implement detached flushing; item 5 introduces it. Yet item 3 starts the stub at 503 and requires it to have answered only 503, while claiming to prove capture against the stub. There will be no request to prove that assertion. Split capture-only assertions from delivery assertions, or move the stub-response check after item 5.

- [HIGH] **Item 3 and Q1 — the proposed `env -i` command puts the Claude credential in argv.** `env -i ... CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" claude ...` passes the expanded assignment as an argument to `env`, contrary to the plan’s explicit claim that the credential never enters argv. Construct the isolated environment without passing the secret as an external command argument, and retain the scratch-HOME isolation.

- [HIGH] **Item 1 and G3 — the restricted-PATH harness cannot run as described.** Each case sets `PATH=$T/rbin` to the helper’s restricted utility set, but the cases themselves invoke tools outside that set, including `python3`, `cmp`, `find`, `pgrep` and `sleep`. The stated oracle is explicitly absent from the helper’s PATH. Restrict PATH only for helper invocations, or resolve the harness utilities independently, while preserving the test that detects unauthorized helper commands. Verify the same arrangement over SSH on macOS.

- [MEDIUM] **D4, D5 step 5 and D8 pruning — permanent event-ID uniqueness is not established.** Reservations are deleted after an hour, while IDs combine epoch, sequence and PID. A clock rollback followed by PID reuse can recreate a previously acknowledged ID; the server may then treat a different body as a collision and park it, or treat an identical one as a duplicate. ADR §3 requires IDs unique per machine forever, and §5 requires sequence uniqueness for a given event time. Address the rollback/reuse case explicitly rather than assuming elapsed wall-clock time proves an epoch cannot recur.

- [MEDIUM] **D8 and item 6 — the token-without-LF case lacks a reliable success rule.** POSIX `read` assigns the final unterminated line but returns nonzero at EOF. The plan says such a token is accepted, yet does not specify that its nonzero status must be accepted when a value was read, and the tests do not cover it. Add a test for both permitted file forms.

- [MEDIUM] **D5 step 6 and item 2 — directory publication does not handle an existing destination safely.** On both target platforms, `mv source-directory existing-directory` can place the source *inside* the destination and succeed. If an ID is reused, the hook can report success while its new body is nested under an existing queued event and never delivered as its own event. Treat an existing destination as a collision and test it; do not rely on `mv` failing.

- [LOW] **Verification G3 and the repository-hygiene assertion — the proposed checks encode incidental host state.** G3 requires a specific `bash 3.2.57` output, although the acceptance requires macOS bash 3.2 behavior, and `ls ...` is required to report the real state root absent even if it legitimately existed before testing. Check the relevant shell behavior and establish before/after evidence that tests did not change the real state instead.

The plan contains Documentation, Testing and Verification sections. This review was read-only; no tests, builds or gates were run.


## Round 2

[Codex Review]

Michaël, revision 2 improves the plan, but the new lock protocol and verification sequence still need changes. Confidence: high.

Altitude report SHA-256: 8dd03d8e910877c2e36d10fc1c07d64b88428f904300edf4f1fc421fa410d778

The additional detail is justified by the round-1 findings; the plan remains at implementation-plan altitude.

- [HIGH] **D8, item 6 — renewal can overwrite a replacement owner.** The revised protocol checks `O/lock/owner`, then writes `O/lock/owner.$$` and renames it onto `owner`. Between the check and write, a stale-lock breaker can replace `O/lock` with its own live lock. The original flusher then writes into that replacement directory and overwrites its owner record, so it can continue sending while the replacement flusher is active. This contradicts the stated invariant and is not covered by `lock_steal`, which only asserts that the displaced owner *would* stop at its next check. Test and resolve the check-to-renew interleaving with two real flushers.

- [HIGH] **D8, item 6 — the clock-rollback residual remains a contract violation, and its stated outcome is incomplete.** The revision explicitly accepts reuse of an acknowledged ID despite ADR §3 requiring uniqueness forever. Moreover, §5 says the server returns `duplicate` for the same ID **and same body**, so a distinct event with an identical body can be acknowledged and removed without being stored. A 409 and parking is only the different-body case. This needs a design that meets the contract or an approved ADR amendment.

- [HIGH] **Item 1, G3 — the new mode check cannot work in the planned macOS run.** `version` requires `git ls-files -s`, but G3 transfers only `.claude-plugin` and `plugins/memory-engine` into a fresh directory, without a Git repository or index. The same check also cannot find the new helper in the index before item 1’s first commit unless it is staged before the green gate. Make the mode check compatible with both the pre-commit and transferred-tree runs while still verifying the committed mode.

- [MEDIUM] **D5 step 1, items 2 and 7 — partial-tree recovery does not cover the directories used later.** The repair runs only if `mkdir O/tmp/$$` fails. If `O/tmp` remains but `S/bin` or `O/seq` was removed, that mkdir succeeds; stable-copy publication or sequence reservation then fails and capture silently drops the event. The new `tree_heals` case deletes `O/tmp`, so it cannot expose this. Repair or retry each missing prerequisite, and test those independent deletions.

- [MEDIUM] **D7, item 4 — the newly accepted linked-worktree residual conflicts with the binding project-root contract.** For a separate Git directory or bare repository with linked worktrees, the plan sends each worktree’s top level, whereas ADR §3 requires the main working-tree root so linked worktrees share it. The plan cannot accept this exception on its own; resolve the root or obtain an ADR amendment.

Round-1 dispositions:

- **Lock race:** not resolved; the revised renewal interleaving above is new evidence.
- **First-use endpoint lost wakeup:** resolved by publishing the endpoint before the event.
- **Early real-run stub assertion:** resolved by moving it to item 7.
- **Claude credential in argv:** resolved by constructing the environment with `os.execve`.
- **Restricted-PATH harness:** resolved for harness commands; the separate G3 Git-index issue above is new.
- **Permanent event-ID uniqueness:** not resolved; the revision expressly accepts the conflict and omits the same-body duplicate outcome.
- **Token without final LF:** resolved by the specified read rule and `token_forms`.
- **Existing event-directory collision:** resolved for an existing queued destination by checking it before publication.
- **Host-specific verification:** resolved for the shell version and replaced with before/after state checks.

This was a read-only delta review; no project tests or gates were run.


## Round 3

[Codex Review]

Michaël, revision 3 remains at a reasonable altitude, but the new high-water design has correctness gaps. Confidence: high.

Altitude report SHA-256: 990193f4d846ad3b1e4316dceb2f37abf5f300a8fa614e23ee0c80232eaa130c

- [HIGH] **D5 steps 6–7 and D8, item 2 — high-water selection races with reservation and pruning.** Capture A can read high-water `h` and pause before reserving. Capture B then reserves a much newer epoch, and a flusher prunes reservations below that new high-water minus 3,600. A resumes and reserves the older epoch after pruning. A later clock rollback and PID reuse can recreate an ID from that older epoch. The sequential `clock_back` case does not exercise this interleaving. The high-water decision, reservation and pruning need a protocol that prevents a capture from reserving below a newly established high-water mark.

- [HIGH] **D5 steps 6–7, item 2 — a backward clock step can silently drop all subsequent captures.** While the clock remains below `h`, every event uses the same epoch, but the sequence loop permits only 1,000 reservations. After 1,000 events, subsequent hooks abandon their events until the clock catches up; a large correction could make that prolonged. Add a design and test for more than 1,000 captures during rollback without silently losing events.

- [HIGH] **D7, item 4 — porcelain worktree paths require decoding.** `git worktree list --porcelain` can quote and C-escape paths containing characters such as quotes, backslashes or non-ASCII bytes. Taking the text after `worktree ` as the literal path sends a quoted or escaped value as `ME-Project-Root` for precisely the special-character paths the contract requires to work. Cover those paths in the separate-Git-directory and bare-worktree cases and use a representation that recovers the original path.

- [MEDIUM] **D5 heal rule, item 2 — retrying a failed body write cannot guarantee byte-identical input.** If `O/tmp/$$/body` is opened and `cat` consumes part of stdin before a write failure, recreating directories and rerunning `cat` reads only the remaining bytes. The resulting queued body can be truncated while appearing successfully captured. The retry rule must distinguish failures before input consumption from partial writes; test an injected mid-write failure.

Round-2 dispositions:

- **Lock renewal overwrites a replacement owner:** resolved by the per-PID stamp and immutable owner record.
- **Clock rollback and permanent ID uniqueness:** not resolved; the new concurrent pruning interleaving and sequence exhaustion are described above.
- **Mode check without a Git index in G3:** resolved by moving it to final verification.
- **Independent missing-directory recovery:** addressed for failures before an operation consumes input; the new body-retry problem is described above.
- **Linked-worktree root:** addressed for ordinary paths; quoted porcelain paths remain a new gap described above.

This was a read-only delta review; no project tests or gates were run.


## Round 4

[Codex Review]

Michaël, revision 4 still conflicts with the binding ADR on project roots and sequence assignment. Confidence: high. The added detail remains appropriate for the plan’s altitude.

Altitude report SHA-256: 91d17b9dbfce73436c5649fd12cb4ddf850e1885b8d1eb4c319aa80937f769c8

- [HIGH] **D7, item 4 — the common Git directory is not the required project root.** For a submodule, the common directory is typically the parent repository’s `.git/modules/...` directory; for `--separate-git-dir`, it is the separately located Git directory. ADR §3 requires the **main working-tree root**, not merely a value shared by linked worktrees. The revised `facts_separate_gitdir` case asserts the Git directory and therefore enshrines the mismatch. Resolve the working-tree root or obtain an ADR amendment.

- [HIGH] **D5 step 7 — the new counter changes the ADR’s sequence and timestamp contract.** ADR §12 specifies reserving `mkdir <outbox>/seq/<epoch>.<n>` from `n = 0` upward and taking epoch and header time from one `date` call. The revision instead starts above a persisted high-water pair, can advance the epoch beyond the clock, and formats that synthetic epoch with another call. This needs an approved contract change; the plan cannot override the ADR.

- [MEDIUM] **D5 step 7 and D8 — the post-reservation check does not make pruning atomic with publication.** A hook can pass its re-read and then pause. Other hooks can advance the high-water mark by more than an hour, and a flusher can prune its still-live reservation before it publishes. After that hook exits, a later reuse of the old epoch, sequence and PID can recreate the ID. The new `frozen_reserve` case advances the high-water mark *before* the re-read, so it does not cover this later interleaving.

Round-3 dispositions:

- **High-water selection versus pruning:** not fully resolved; the post-check pause above is a further interleaving.
- **Sequence exhaustion during clock rollback:** addressed by advancing the synthetic epoch, subject to the ADR conflict above.
- **Quoted porcelain worktree paths:** resolved by removing that parsing path; the replacement root value has the separate contract defect above.
- **Retry after a partial body write:** resolved by abandoning the event on write failure.

This was a read-only delta review; no project tests or gates were run.


## Round 5

[Codex Review]

Michaël, the two ADR tensions are now presented for your decision, but Q7 and Q8 need corrections before they provide a reliable basis for that decision. Confidence: high.

Altitude report SHA-256: c9f63be5c71978f5da18694e7c38825d228635d5fc709131b4cca438a6e5cca2

The revision remains at appropriate plan altitude: it replaces disputed design rules with explicit decision points rather than adding implementation detail.

- [HIGH] **Q7 option (c) overstates its guarantees.** It says the revision-4 counter ensures uniqueness and that nothing is ever dropped. Round 4 identified a pause after the reservation re-read that can still permit pruning of a live reservation; the counter also retained a bounded retry loop that can abandon a capture. Present option (c) as requiring a corrected design, not as an established solution.

- [MEDIUM] **Q7 option (a) understates the identical-body collision consequence.** “Which is the same data” obscures that these are two distinct lifecycle occurrences. The second event and its effects can be omitted even when its bytes match the first. State that consequence alongside the existing 409/park outcome.

- [MEDIUM] **Q8 does not fully describe the common-directory alternative.** To make all worktrees share a root, that option must also use the common directory for the main tree, changing the current D7 case 2. Further, the common directory can sit outside the configured dev root; ADR §7 then produces no path key, rather than a shared key. Include both consequences so the options can be compared.

The review was limited to the delta and was read-only; no project tests or gates were run.


## Round 6

[Codex Review]

Michaël, the round-6 delta resolves all three round-5 findings. The plan remains at appropriate altitude; Q7 and Q8 remain decisions for your approval.

Altitude report SHA-256: f78c6a36ce009d63588f2f0d58f90ec42df261656bfd21a1304a6315c0dab78e

- **Q7 option (c):** resolved; it now calls for a corrected design and another review.
- **Q7 identical-body collision:** resolved; it states that the second occurrence and its effects are not stored.
- **Q8 common-directory alternative:** resolved; it describes the change to the main tree and the possible loss of a path key.

LGTM


## Round 7

[Codex Review]

Michaël, revision 7 introduces two issues that need correction. Confidence: high. The delta remains at appropriate plan altitude.

Altitude report SHA-256: c59ea5fed69e8f4654f93ae38ea88aea0a523ada822fd15020bdb35a05358612

- [HIGH] **D5 step 2 — a failed endpoint update can deliver to the previous endpoint.** If `O/endpoint` contains an old valid URL and writing the newly configured URL fails, the best-effort rule still queues the event and starts a flush. That flush reads the old URL and can send the new event and bearer token there. Preserve capture on failure, but prevent delivery using an endpoint known to be stale; add a case with an existing endpoint and a failed update.

- [MEDIUM] **D7 cases 1–2, item 4 — reordering does not handle a separate Git directory named `.git` in a linked worktree.** In the main tree, Git dir equals common dir, so case 1 sends the main tree. In its linked worktree they differ, and case 2 strips `/.git` from the separately located Git directory, sending its parent. That parent need not be either working tree, contrary to the Q8 exception and the claim that case 2 identifies the main tree. Add this layout to `facts_layouts` and define its root consistently.

This was a read-only delta review; no project tests or gates were run.


## Round 8

[Codex Review]

Michaël, the endpoint change does not yet close the round-7 finding. Confidence: high. The delta remains at appropriate plan altitude.

Altitude report SHA-256: 736380950394fcd62b4996fb1ded5c58fbadb63f421c5f29c2023691bad5db15

- [HIGH] **D5 step 2 and D8 — removing the endpoint does not invalidate a flusher’s cached URL.** A flusher can acquire the lock and read the old endpoint; the hook then fails to record the new URL, removes `O/endpoint`, and publishes its event. If the flusher enumerates the queue afterward, it sends that event and token to the old URL already held in memory. The new `endpoint_update_fail` case checks a fresh flush, so it misses this interleaving. Also account for `rm -f` failing: the plan currently claims removal succeeds unconditionally. Add a controlled active-flusher case and a protocol that prevents the stale send.

Round-7 dispositions:

- **Failed endpoint update:** not resolved; the active-flusher interleaving above is new evidence.
- **Separate Git directory named `.git`:** addressed as an explicit Q8 decision, with the layout and its consequence stated.

This was a read-only delta review; no project tests or gates were run.


## Round 9

[Codex Review]

Michaël, the per-request re-read addresses the round-8 cached-URL interleaving when the endpoint remains removed or changed. A concurrent writer can still restore the stale value. Confidence: high.

Altitude report SHA-256: 7eb0903638a270322032a78680b3d6238c7b25bf862c6e786d92e64daa632405

- [HIGH] **D5 step 2 and D8 — another hook can republish the old URL after it is withdrawn.** A hook with the new configuration can fail its endpoint update and remove `O/endpoint`; a concurrently running session with the old configuration can then successfully write the old URL back. The new event is queued, and a flusher starting afterward reads that old URL consistently before every request. It can therefore send multiple events and the token to the old endpoint. This is distinct from the stated one-request check-to-act window. Define how conflicting concurrent configurations are handled and test that interleaving.

The review was read-only and limited to the delta; no project tests or gates were run.


## Round 10

[Codex Review]

Michaël, the delta accurately states the per-machine endpoint residual and places both endpoint tests in item 5, where flushing is implemented. The plan remains at appropriate altitude.

Altitude report SHA-256: e95939ca4f741e00b243ed380723f19efc80f6a73774676248b71efbbbd12bd9

LGTM


## Round 11

[Codex Review]

Michaël, the delta replaces the two paths with portable text and leaves the operational meaning intact.

Altitude report SHA-256: 8d448789593d8542fb5d407011beb187ab6752053c091968bf22d785a645e166

LGTM


REVIEW COMPLETE
