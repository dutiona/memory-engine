# Advisor capability unavailable

`advisor()` is not exposed in this worker session: the tool is absent from its tool list, so step 3a of the
plan review could not run. The Full gate fell back to its advisor-less form: Codex (headless), Antigravity and
one clean-slate subagent. Nothing below is attributed to an advisor; each finding names the reviewer that
produced it. The advisor triage after each round and the pre-completion check were replaced by the planning
session's own assessment, which is recorded under Resolution.

Controller metadata, the report the three fallback reviewers assessed in their last round:

Altitude report SHA-256: 15c7398b1fd5e16ea69b684d85f794e9e7917d1377a3abf7748836189c8d1fd1

The reviews themselves are in `docs/plans/2026-11-16-adr-0020-lifecycle-api-external-codex.md`, `docs/plans/2026-11-16-adr-0020-lifecycle-api-external-agy.md` and
`docs/plans/2026-11-16-adr-0020-lifecycle-api-subagent-review.md`.

## Findings of the fallback reviewers, round by round

### Round 1 findings and what changed in the plan

Three reviews (Codex, Antigravity, a clean-slate reviewer). Every finding is listed with its outcome.

#### Changed

- Remote URLs kept their credentials (Codex, clean-slate H1). §B: the client removes userinfo before queueing, the server strips again before storing or logging; §I: stored facts never hold userinfo.
- The label limit ignored the `project:` prefix (Codex, Antigravity, clean-slate H2). §I: the limit applies to the whole segment `project:<label>`; the label's hex is uppercase.
- A retry had no bound (clean-slate H3). §C: an event-specific failure, an unexpected error included, gets 422 and parks; 5xx is reserved for faults that do not depend on the event. §N: a stuck head shows as a queued count above zero at session start.
- Concurrent flushes (Codex, clean-slate M4). §D: one flusher under the outbox's `mkdir` lock, with the stale-lock rule.
- Seq did not establish the promised order (Codex). §D: the seq is the reservation order; sequential hooks keep their order, overlapping ones get an arbitrary one; gaps are allowed. §M: the seq is reserved with the body already on disk, just before publishing.
- Unpadded seq broke glob order (Antigravity, clean-slate M4). §B, §M: 1 to 6 digits; the event ID carries it zero-padded to 6.
- Late against opening (Codex, clean-slate M7, Antigravity). §F is rewritten as four ordered rules: lateness first; segments are only appended; a late `resume` start opens nothing; subagent events and `compact` starts are ordinary events; the splits it accepts are named.
- The client fallback contradicted "integers only" (Antigravity). §L: four fixed templates, integers substituted, `systemMessage` only; the status line is given as a grammar (clean-slate LOW).
- The call ID could not be the first line of an error (clean-slate M1). §J: failures are tool results; the extraction is the first text content; #1025 pins the shape with an observed fixture.
- The MCP channel's source for the project directory was unstated (clean-slate M2). §B: environment when set, else the single-quoted substitution; one project for the whole session.
- Quoted decisions were missing and contradicted (clean-slate M3). Constraints now quote the owner's decisions the plan relies on and mark the two it refines; §N: the ADR states them as rules, without numbers that point into a private document.
- 400 on a bad fact parked the event (clean-slate M5). §B: a bad fact is dropped and flagged, the event is stored.
- 3xx was undefined and any 2xx deleted (clean-slate M6). Decision 2 and §C: only 204 removes; 404, 405, 3xx and any other 2xx hold; no redirect is followed; a body is printed only for a JSON 200.
- The maintenance refusal had no mechanism (clean-slate M8). §J: one advisory lock file next to the database, held by the server, taken by maintenance commands.
- Outbox gaps (clean-slate M9). §M: the path, the modes, the token read at send time.
- A capture hook could hang (clean-slate M10). §M: a bounded loop, a 5 s timeout, exit 0 with empty stdout on any failure.
- The leak scan could false-pass (clean-slate M11). V2 scans the branch diff, its commit messages and the PR body, and requires status 1.
- Backfill had no route (clean-slate M12). §A: a third authenticated route, owned by #1026, behind the same Bearer layer and redaction.
- Consequences had no planned content (clean-slate M13). §N lists it.
- The merge gate skipped the fuzz build (Codex, clean-slate LOW). Verification runs the whole Verify block, the fuzz build included; MSRV stays conditional, as `AGENTS.md` itself says.
- LOW, clean-slate: the actor is derived on every hook and internal agents are kept out of recall (§G); two wrong "not captured" reasons fixed (§G); a cancelled tool's outcome (§G); empty final messages and continued stops (§H); write authority (§J); the hint heuristic's owner (§J); one uptake row per stop (§K); a missing hash file, the `Authorization` grammar and must-reject vectors (§E); builtins in the utility set (§M); V1 against the merge-base; more V5 anchors; V7 as a command on the pinned copy (also Antigravity LOW); the placeholder rule (§N); fact IDs after a restore (§N); tool-map row 10.

#### Not changed, with the reason

- "A plugin cannot raise the SessionEnd budget contradicts the reference" (Codex, MEDIUM). The reference says, under SessionEnd: "Timeouts set on plugin-provided hooks don't raise the budget." The plan's claim stands; §M now words it as the reference does.
- "Percent-encoding exceeds the POSIX sh utility set" (Antigravity, MEDIUM). It does not: parameter expansion under `LC_ALL=C` and the `printf` builtin are enough, measured on dash, bash and busybox sh. §B says how.
- "Unknown `ME-Harness` should hold like 404" (clean-slate, LOW). §C explains the line: a 404 never reached the API, a 422 is the API's judgement on one event; and the server is deployed before the plugin.
- "Read the dev root from a file" (clean-slate, LOW). Decision 8 names it and why it loses: it races the first connection after an install.
- "Add the oldest queued age to the status suffix" (clean-slate, part of H3). Not added: at session start a healthy queue is empty, so a non-zero count already says the head is stuck, and the ticket asks for the two counts only.

#### Altitude report

Two corrections from the clean-slate review are applied: the report no longer claims the ticket restates every decision (the plan's Constraints now quote them), and it measures the drafts directory.

### Round 2 findings and what changed in the plan

Antigravity: LGTM. Codex: three HIGH, one MEDIUM. Clean-slate: two MEDIUM, eight LOW. Every finding is listed.

#### Changed

- An event could be stranded after the last flush (Codex HIGH; clean-slate N2). §D "One flusher": a hook publishes before it tries the lock; the flusher rescans until a pass sends nothing, releases, then looks once more. The watcher's timer is the backstop.
- The stale-lock rule was not implementable (Codex HIGH; clean-slate N2). §D: the lock records the flusher's PID and is broken only when that process is gone; the test is best effort and the plan says so. §D's first bullet now states the property that makes this safe: correctness never depends on delivery order (duplicates are answered `duplicate`, the server orders by key, §F defines a late event).
- A late stop lost its derived work (Codex HIGH). §F rule 1 and §K: a late completed stop still gets its own uptake row when it is applied; the checkpoint of an ended segment is not rewritten, and the plan says so. The claim "loses nothing" now reads "loses no captured content". Rule 1 also says why lateness is rare.
- Userinfo was not removed from every remote form (Codex MEDIUM; clean-slate N7). §B: everything up to and including the last `@` in front of the host, in the `scheme://` form (a bare token included) and in the `user@host:path` form.
- The MCP guard could not work (clean-slate N1). §B: the environment route if Claude Code exports the variable to the helper (#1028 observes it); otherwise the single-quoted substitution, on which a project path containing `'` is unsupported and cannot be guarded, and the ADR says so.
- The remaining 4xx had no row (clean-slate N3). Decision 2 and §C: the table is total. 204 removes; 400, 409, 413 and 422 park; 408, 429, 5xx and a connection error retry; everything else holds.
- A session end that opens a segment (clean-slate N4). §F: rule 3 is now its own rule; `L` is the last segment appended; a boundary start applies only after `L`'s last event, and an out-of-order event inside `L`'s range joins it.
- An unsupported claim about a cancelled tool (clean-slate N5). §G and §H: stated as unknown, and as possibly not captured.
- "A maintenance command" was undefined (clean-slate N6). §J: every `memory-engine-cli` command that opens the engine takes the lock, the read-only ones included; `status` alone takes none.
- Token and argv (clean-slate N8). §M: `curl` reads the `Authorization` header from standard input.
- An empty pattern file passed (clean-slate N9). V2 checks the file first.
- Fact IDs after a restore (clean-slate N10). §N: reworded.

#### Not changed, with the reason

- "Observe now, not in #1028, whether 2.1.325 exports `CLAUDE_PROJECT_DIR` to a plugin helper" (clean-slate N1, the first half of its fix). The observation needs a real Claude Code session with the plugin loaded, which is #1028's scripted run; this ticket changes documentation only. The plan now states both outcomes and what each one means, so the ADR is correct whichever #1028 observes.

### Round 3 findings and what changed in the plan

Antigravity: LGTM. Codex: two HIGH, one MEDIUM. Clean-slate: two MEDIUM, two LOW. Every finding is listed.

#### Changed

- The MCP fallback could run a project path as shell code (Codex HIGH; clean-slate R4). It is gone. I ran the observation both reviews asked about: a headless Claude Code 2.1.325 session with a probe plugin, on a project path holding a space, a non-ASCII character and a `'`. Results: `CLAUDE_PROJECT_DIR` is not in a plugin helper's environment; the substitution is textual, so an unquoted or a single-quoted argument breaks the command and the helper never runs; a here-document with a quoted delimiter delivers the path intact on standard input. §B now fixes that route, `exec <helper> <<'DELIMITER'`, with the path as data. A name crafted to hold the delimiter line can only cut the path short: with `exec` the shell is replaced before it reads another line (checked on dash, bash, busybox sh and zsh; without `exec` the next line runs).
- The post-unlock second look conflicted with retry and hold (Codex HIGH; clean-slate R2). §D: a retry-class or hold-class answer ends the flush at once, with no second look; the second look follows only a flush that ended because the queue was empty. The next flush is started by the next hook or by the watcher's timer.
- The watcher was undefined (clean-slate R3; Codex, backstop ownership). §M defines it: the per-machine timer of dutiona/my-dotfiles#1911 that runs the flush with no session open. §D: the lock also carries a stamp the flusher renews before each request, and is broken when its process is gone or the stamp is older than one hour.
- Late uptake had no defined association (Codex MEDIUM). §K: the row names the session's injection whose SessionStart has the greatest order key below the stop's, among the injections the server holds when the row is written; it names none when none is held; a row is never rewritten.
- The maintenance lock's reason contradicted the repository (clean-slate R1). §J: only a command that opens the engine writable takes the lock; read-only commands take none and may run against the live store, because `open_engine` never migrates.

#### Not changed

Nothing was declined this round.

### Round 4 findings and what changed in the plan

Antigravity: LGTM. Clean-slate: LGTM, two LOW. Codex: one HIGH, one MEDIUM. Every finding is listed.

#### Changed

- A delimiter line in the project path can select the wrong project (Codex HIGH). The finding is right about the mechanism: any framing inside the here-document can be forged by a name that holds it, because the substitution is textual; only an out-of-band check can close it. §B now says so plainly (no code runs; the path can be cut short) and adds the cheap guard: the helper accepts exactly one line that names an existing directory, else it sends no fact. The remaining case is put to the owner as decision 10, with both options: accept it (recommended: creating such a directory beside a project needs write access to the dev root, which already allows far more, and the hook channel refuses a path with a line break), or close it with an `ME-Session-Id` header and a server-side check, which rests on a variable Claude Code does not document for a helper.
- Uptake seemed to depend on arrival order (Codex MEDIUM). §K: it does not, and the plan now says why. An injection is not delivered through the outbox: the context read itself writes it, synchronously, inside the SessionStart hook, so it exists before any later event of that session is produced.
- A lock with no PID or stamp (clean-slate LOW). §D: such a lock is broken.
- The one-hour stamp against "no total limit" (clean-slate LOW). §M: the speed limit is set so that a 16 MiB event finishes within the lock's hour.

#### Not changed

Nothing was declined this round. Decision 10 is an accepted residual, stated as one, for the owner to accept or replace at approval.

### Round 5 findings and what changed in the plan

Antigravity: LGTM. Clean-slate: LGTM, two LOW. Codex: two MEDIUM, no HIGH. Every finding is listed.

#### Changed

- A synchronous context request does not guarantee the injection exists first (Codex MEDIUM). §K: the claim is narrowed. Normally the injection exists first; the exception is a read that outlives the client's timeout, whose block the session never received. Whether an uptake row names it depends on timing, and the plan accepts that: the injection's recorded duration lets an analysis drop it.
- Decision 10's alternative lacked its fail-closed case (Codex MEDIUM; clean-slate LOW). It now says the server refuses a model-tool call for which it holds no SessionStart root, and that the variable's equality with the hook input's session ID is still to observe.
- "The hook channel refuses a path with a line break" was looser than §B (clean-slate LOW). §B: a project directory whose path holds a line break yields no project fact at all, on either channel; decision 10 says the same.

#### Not changed

Nothing was declined this round.

### Round 6 findings and what changed in the plan

Antigravity: LGTM. Clean-slate: LGTM, two LOW. Codex: one HIGH, one MEDIUM. The two reviews name the same two sentences.

#### Changed

- §B's line-break rule contradicted decision 10 (Codex HIGH; clean-slate LOW). §B now names the exception: no project fact on either channel, except the forged delimiter of decision 10, which hides the line break from the MCP helper.
- The recorded duration cannot establish that a block reached the session (Codex MEDIUM; clean-slate LOW). §K no longer claims it: a row may name a block the session never received, the server's records cannot always tell, and #1029 treats an injection as served, not as seen.

#### Not changed

Nothing was declined this round.

### Round 7

Codex: LGTM. Antigravity: LGTM. Clean-slate: LGTM. No finding; the plan did not change after this round.

## Resolution

- Seven rounds; the last one reviewed the final plan bytes and all three reviewers answered LGTM.
- Findings by severity, counted once at the highest severity when two reviewers raised the same one: 14 HIGH, 23 MEDIUM, 30 LOW, no BLOCKER (by round: 41, 12, 5, 4, 3, 2, 0). All were addressed (severity mode Default): by a change to the plan, or by a written reason where the plan did not change. The reasons are in the round sections above.
- Declined with a reason, in all: the SessionEnd budget claim (the hooks reference supports the plan); percent-encoding in POSIX sh (measured on three shells, later reproduced by a reviewer); unknown harness holding like a 404; the dev root from a file; the oldest queued age in the status line.
- One conflict between reviewers was arbitrated without an advisor: from round 2 to round 6 Antigravity answered LGTM while Codex kept finding defects in the sentences each revision added. Each Codex finding was checked on its merits and fixed or answered; none was dismissed because another reviewer had passed the plan.
- One finding ended as an accepted residual, put to the owner as decision 10 of the plan with its alternative.
- One fact was established by experiment during the review: what a plugin `headersHelper` receives on Claude Code 2.1.325. It replaced the MCP route the plan started with.
- Altitude: the override "one artifact, with the values in the plan" was acknowledged by all three reviewers in every round.
- Pre-completion check, in place of the advisor: no finding was dismissed without a written reason, the last round changed nothing, and the evidence gate of step 4.5 passes on the final bytes.
