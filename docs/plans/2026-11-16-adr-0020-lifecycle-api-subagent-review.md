# Clean-slate review: ADR-0020 plan

A general-purpose subagent with no context from the planning session reviewed the plan in seven rounds: round 1
in full, then the delta of each revision. Its reports are reproduced as returned; absolute paths on the
worker's machine are replaced by repository-relative ones. The last round reviewed the final plan bytes.

## Round 1

Altitude report SHA-256: a4e9077c25a385fcf73be402fde3b055f439aacfe84aa324fa4b064e23ce0c2c

**Verdict: not LGTM yet.** No blocker, but three HIGH contract defects, each a one- or two-line change to the plan. Confidence is high where I checked against code or the hooks reference, and medium where I reason about a client that does not exist yet. I ran no build, test or gate.

### Altitude

I acknowledge the override: one artifact, with the values in the plan. The deliverable is a decision record, so the values are the design. A design split would be a third copy, and product extraction would be writing the ADR before approval. The digest binding holds: the report's plan hash and both input hashes match the files.

Two corrections to the report:

- **"The ticket body restates every decision this plan relies on" is not accurate.** The ticket names decisions 13, 17, 18 and 22 without their text, and the plan also leans on 20, 21, 25 and 27 (see M3).
- **The measurements omit the drafts directory.** `docs/plans/2026-11-16-adr-0020-lifecycle-api-drafts/` holds 160,114 untracked bytes next to the plan (see M11).

### HIGH

- **[HIGH] H1, §B and §I: remote URLs keep their credentials.** The two URL headers carry `git remote get-url` "as configured", and "every event keeps its decoded raw facts". Userinfo is dropped only when the key is derived, so a remote of the form `https://user:secret@host/…` is sent on every request and stored on every event. Fix: the client strips userinfo before sending (parameter expansion is enough), the server strips again, and the ADR states that stored facts never hold userinfo.
- **[HIGH] H2, §I: the label limit is off by eight.** "A label over 256 bytes gives no key", but `MAX_SEGMENT_LEN = 256` applies to the whole segment `project:<label>`, which `ensure_path` validates. Labels of 249 to 256 bytes pass the plan's rule and fail on every event and every `remember` of that project. Fix: put the limit on the segment (label at most 248 bytes). Also fix the label's hex case to uppercase, or `%2F` and `%2f` become two nodes.
- **[HIGH] H3, §C and §D: a retry has no bound.** A retry-class answer stops the flush and restarts from the same event. "No payload fault ever gets a 5xx" is asserted but nothing enforces it. Any event-specific server fault (H2 mapped to 500, a panic in the adapter) loops that event for ever and holds every later event from that machine. Fix: the ADR states that a failure applying one validated event is committed as a quarantine-class record or answered 422, and 5xx is reserved for faults independent of the event. Add the oldest queued age to the status suffix so a stuck head is visible.

### MEDIUM

- **[MEDIUM] M1, §J: the call ID cannot be the first line of errors today.** The server returns argument and not-found errors as JSON-RPC errors (`ErrorData::invalid_params`, `resource_not_found`), which have no first line that reaches a hook verbatim. The ADR must say the four tools return every failure as a tool result starting with the call line. The hooks reference also gives no shape for an MCP tool's `tool_response`, so define the extraction as the first text content and have #1025 pin it with an observed fixture.
- **[MEDIUM] M2, §B: the MCP channel's source for the project directory is unstated.** The hooks reference documents `CLAUDE_PROJECT_DIR` for hooks, stdio MCP servers and LSP servers only. The public MCP docs (fetched today, not a named input) list three helper variables, none of them the project directory, and run a plugin helper in the plugin root. They do substitute `${CLAUDE_PROJECT_DIR}` into a plugin's `headersHelper` command, which runs through a shell. A path with a quote, `$` or backtick then breaks or injects. Name the mechanism and its quoting rule, or cite an observation on 2.1.325 that the variable is exported. The helper also runs once per connection, so after `/cd` the two channels can disagree.
- **[MEDIUM] M3, Constraints and §N: quoted decisions are missing and contradicted.**
  - The text of decisions 13, 17, 18 and 22 is in neither the plan nor the ticket, so approving the plan cannot cover them.
  - The plan amends two of them (decision 3 against 17's `user:michael/project:<key>`, decision 2 against the status classes), so the ADR would quote a rule and then contradict it.
  - "Spec decision 21", 25 and 27 would be dangling references to a private document in a public record.
  - Fix: add the public-safe text to the plan, mark each amended quote, and replace bare numbers by content.
- **[MEDIUM] M4, §D and §M: the flush has no concurrency rule.** Every capture hook starts a flush, and "one request at a time" names no mechanism. Without a lock, each later hook re-uploads a slow 16 MiB event in parallel. With an `mkdir` lock, a flusher killed at teardown leaves it stale and nothing flushes. Also, "oldest first" must come from glob order (no `sort`), and `<epoch>-<seq>-<pid>` sorts `10` before `2`: require a zero-padded seq in the queue file name.
- **[MEDIUM] M5, §C: 400 on "a fact fails decoding" parks the event.** Facts are optional. A project path that is not valid UTF-8 parks every event of that project, and a replay fails the same way. §B ignores a bad dev root, so do the same here: drop and flag the fact, store the event.
- **[MEDIUM] M6, §C: 3xx is undefined and any 2xx deletes.** A redirect falls into the client's default class. A 200 from something that is not ME removes an event. On the context route a non-JSON 200 body is printed, and SessionStart adds plain stdout to the model's context. Fix: never follow redirects, 3xx holds, remove only on 204, print only a JSON-typed 200.
- **[MEDIUM] M7, §F: four segment rules are ambiguous.**
  - "Subagent events never open a segment by themselves; the 'after the end' rule still applies" contradicts itself.
  - Precedence is unset between "late" and "a `resume` start opens a segment" when the start is timestamped before the latest end.
  - Server-side idleness splits an offline laptop's session at the connectivity gap. "Longer than a long tool call" is false for a 40-minute build or an open permission prompt.
  - Say whether "latest injection before it" is session-wide.
- **[MEDIUM] M8, §J: "refusing to run while the server holds the store" has no mechanism.** The code has WAL and `busy_timeout` only. Server and CLI sides sit in different tickets, so the ADR must name the lock.
- **[MEDIUM] M9, §M: the outbox contract has three gaps.**
  - Say `Authorization` is added at send time. Otherwise each queued and parked file holds the token, and a rotation strands the queue on 401.
  - Name the outbox path: §A's "queued events outlive a plugin upgrade" depends on it.
  - Require mode 0700: bodies are unredacted, and `outbox/rejected/` is kept for ever.
- **[MEDIUM] M10, §M: a capture hook can hang the session.** "From n = 0 upward: the first success" never ends if `mkdir` fails for another reason. Registrations carry no `timeout`, so the default is 600 s. State a bounded loop, a short per-hook timeout, and exit 0 with empty stdout on any failure.
- **[MEDIUM] M11, V2: the leak scan can false-pass and misses files.**
  - `-- $FILES` is unquoted. zsh does not word-split, so grep gets one nonexistent name, prints nothing on stdout and exits 2. Use an explicit list and require exit status 1.
  - It skips the altitude report, the drafts and commit messages. `draft-risk-first.md` lines 182 and 449 hold a token-shaped literal that matches the plan's own regex. Scan `git diff origin/main...HEAD` and `git log --format=%B`, and state that the drafts are never committed.
- **[MEDIUM] M12, Outcome and §A: backfill has no route.** Row 18 sends to "the authenticated backfill upload", but §A defines no path, limit or statuses. Define it, or assign it to #1026 behind the same Bearer layer and redaction.
- **[MEDIUM] M13, §N: Consequences has no planned content.** It needs at least:
  - the stored prompt-injection path (untrusted text, then a final message, then every later SessionStart);
  - that a quarantined body is unrecoverable;
  - that facts at today's scope paths become unreachable by `recall`.

### LOW

- **[LOW] §G, interrupted calls.** `PostToolUse` is always `success`, but the reference says a cancelled running tool carries the interruption in its result (Bash output has `interrupted`).
- **[LOW] §G, internal agents.** `SubagentStop` also fires for prompt suggestions and `/btw` with an empty `agent_type`. Say how they are recorded.
- **[LOW] §G, actor.** Derive the actor from `agent_id` on every hook, not from a fixed "main".
- **[LOW] §G, two wrong reasons.** `PermissionRequest` precedes the decision, so the call may run. `UserPromptExpansion` does not carry the same content as `UserPromptSubmit`.
- **[LOW] §H, stops.** Every Stop becomes a fact, including stops continued by another Stop hook and empty final messages. Say so.
- **[LOW] §L, status line.** Give the grammar as a production: the `<age>` format, whether `OK` takes the waiting suffix, and whether the client fallback also sets `additionalContext`.
- **[LOW] §J, write authority.** Say whether facts reached through `recall(project:)` can be forgotten or re-levelled.
- **[LOW] §J, `remember` hint.** The heuristic is unspecified and unowned. Bound it, or allow it to be empty in this epic.
- **[LOW] §K, uptake rows.** One row per stop and item is a cross product. One row per stop naming the injection carries the same.
- **[LOW] §E, hash file and header.** Specify a missing hash file and the `Authorization` grammar. Add must-reject vectors (uppercase hex, two spaces, CRLF, no final LF).
- **[LOW] §B, utility set.** Percent-encoding needs the `printf` builtin under `LC_ALL=C`. Say builtins are in the set.
- **[LOW] Decision 2, consistency.** Unknown `ME-Harness` is a "server too old" case like 404, yet it parks.
- **[LOW] Decision 8, alternative.** The helper can read a file, so a dev root stored next to the token would be encoded like the other facts.
- **[LOW] V1.** The absolute count of 31 breaks on rebase. Diff head against merge-base instead.
- **[LOW] V5.** No anchor for `ME-Harness`, `recall(`, `remember(`, `1,000 ms`, `--max-time 2.5` or the call ID.
- **[LOW] V7.** It is not a command, and it reads the live page instead of the pinned copy.
- **[LOW] Merge gate.** It skips the fuzz build, but `AGENTS.md` says "Do not skip any" and the build is a no-op here. Run it, or ask Michaël to waive it.
- **[LOW] §N, placeholders.** `<placeholder>` outside a code span is inline HTML in MyST and vanishes silently.
- **[LOW] Fact IDs after a restore.** Integer IDs issued in the 6 h window are reissued, so a stale `forget(412)` hits another fact.
- **[LOW] Tool map row 10.** It says both "off" and "replaced". Pick one.

### Over-engineering, structure, feasibility

- **Over-engineering:** little. No adapter trait, no version header and a fixed recall size are the right calls.
- **Structure:** Documentation, Testing (N/A with reason) and Verification are present, and every ticket criterion has a section.
- **Feasibility:** the two items are achievable in order. V3, V4 and V6 are correct as written.

### What checks out

- The 26 tool names and the tally match the code, and every CLI command the map calls "exists" does.
- All 33 hook events are placed.
- `truncate()` cuts bytes, the redaction thresholds are 32 and 20, and the ADR-0001 quote is exact.
- `exact` scope mode exists.
- 9,000 bytes sits under the 10,000-character cap.
- A plugin cannot raise the SessionEnd budget, and a `headersHelper` cannot read `user_config`.

Not verified: the private spec decisions, the 31-warning baseline, and the 124 ms figure.

Evidence files (repository-relative; the reviewer gave absolute paths):
- crates/me-types/src/types/scope.rs
- crates/me-backend-sqlite/src/store/scopes.rs
- crates/memory-engine-mcp/src/error.rs
- crates/me-backend-sqlite/src/store/schema/pragmas.rs
- docs/plans/2026-11-16-adr-0020-lifecycle-api-drafts/draft-risk-first.md
- a fresh copy of the public Claude Code MCP documentation page

## Round 2

Altitude report SHA-256: d4030770dbc0bf81b38dc116a9fb07c68647fdb5d74480a20c585afb94aac460

**Verdict: close, not LGTM yet.** There is no blocker and no HIGH. Every round-1 finding of mine is resolved in substance, but two of the resolutions carry a defect of their own, both MEDIUM and each one sentence to fix. The rewritten §F, §C and §D hold together apart from the items below. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is now 51,982 bytes against an ADR estimate of 55 to 70 KB. The growth is contract precision (segment rules, status classes, the lock, the ledger), which is what this tier exists to review, and both fallbacks still fail for the reasons the report gives. The margin is narrow, so any further growth should be values, not rationale.

Both corrections check out:

- **Decisions quoted.** Constraints now carry the text, and no private decision number remains in sections A to N.
- **Drafts.** They are measured, and `.gitignore` line 59 (`docs/plans/*-drafts/`) ignores them. That also settles my round-1 worry that they could be committed: I had tested the parent directory, not the drafts.
- **Digest binding.** The report's plan hash matches the plan file.

### Round-1 findings

- **Resolved as stated:** H1, H2, H3, M1, M3, M5, M6, M7, M8, M9, M10, M11, M12, M13, and every LOW except the cancelled-tool one (N5).
- **Resolved, with a new defect:** M2 (see N1) and M4 (see N2).

### "Not changed" reasons

- **SessionEnd budget (Codex):** sound. The reference says "Timeouts set on plugin-provided hooks don't raise the budget."
- **Percent-encoding in POSIX sh (Antigravity):** sound. I reproduced the technique inline on dash, bash, `bash --posix` and busybox sh, with non-ASCII, space, `%`, `"` and `\`, and all four gave the same correct output.
- **Unknown `ME-Harness` parks (mine):** sound, given the server is deployed before the plugin.
- **Dev root from a file (mine):** sound for the variant decision 8 names, a file the hooks write. A file Michaël writes himself, like the token file, has no race; that stays his call under decision 8.
- **Oldest queued age (mine):** accepted. The stated reason is weak on a machine running several sessions, where a non-zero count at session start is normal. The 422 rule removes the main cause of a stuck head, so the count is enough.

### New findings in the delta

- **[MEDIUM] N1, §B MCP channel: the stated guard cannot work.** The substitution route is "single-quoted, refusing a path that contains `'`". Claude Code substitutes the path into the command string and then runs it through a shell. A path with `'` therefore breaks or injects before the helper starts, so nothing in the helper can refuse it. The evidence is the public MCP docs, which are not a named input: they say the helper runs in a shell, and that `user_config` substitution was withdrawn from `headersHelper` for this reason. Fix: observe now, not in #1028, whether 2.1.325 exports `CLAUDE_PROJECT_DIR` to a plugin helper. If it does, state the environment as the only route. If not, state that a project path containing `'` is unsupported on the MCP channel and cannot be guarded.
- **[MEDIUM] N2, §D "One flusher": a lost wakeup.** "A second flush that finds the lock exits", and nothing makes the holder look again. In every `claude -p` run, Stop and SessionEnd fire milliseconds apart, as §D itself notes. The SessionEnd flush finds the Stop flush's lock and exits, and a holder that listed the queue once never sends SessionEnd. That event then waits for the next hook on the machine, the idle sweep closes the segment as idle, and the late SessionEnd opens a one-event segment. Fix: the holder rescans until a pass sends nothing, and checks once more after releasing the lock. Related: "a live flusher's lock never is broken" can only be approximated by a PID check with these utilities, so say it is best effort.
- **[LOW] N3, §C: the remaining 4xx have no row.** They park only through the ticket quote in Constraints. Decision 2's own reason ("cannot come from the lifecycle API") applies equally to 407, 421 or 431 from an intermediary, which would empty the queue one event at a time. Cleaner: park only on 400, 409, 413 and 422, and hold every other non-retry status.
- **[LOW] N4, §F rule 2: a session end that opens a segment.** The "then ends `L`" clause sits under "otherwise joins `L`". A session end that opens a segment (after an idle end, or more than 30 minutes after the last event) is not said to end it. Make it a rule of its own. Also say "latest" means last appended: a boundary start below `L`'s last key gives overlapping ranges.
- **[LOW] N5, §G: an unsupported claim.** "A running tool the user cancels still fires `PostToolUse`" is not in the reference. It says only that `PostToolUseFailure` does not fire and that `Esc` cancels the hooks. State it as unknown and add the case to §H's "not captured".
- **[LOW] N6, §J: "a maintenance command" is undefined.** The tool map labels nine rows "maintenance CLI", including read-only `stats`, `inspect` and `export`. Name the commands that take the lock; I would include every writer (`consolidate`, `migrate`, `record-outcome`), not only the three.
- **[LOW] N7, §B: userinfo wording.** "Removes `user:password@`" reads as one form. The common token-only form `https://<token>@host` must go too: say everything up to the last `@` of the authority.
- **[LOW] N8, §M: token and argv.** Constraints now quote "No token appears in argv", but §M does not say how. The obvious `-H "Authorization: …"` breaks the rule; say the header reaches curl on stdin.
- **[LOW] N9, V2: an empty pattern file passes.** It gives status 1. Add `test -s "$PRIVATE_PATTERNS"`.
- **[LOW] N10, §N Consequences: fact IDs.** "A fact ID is good only in the session that received it" is wrong both ways. IDs from before the backup stay good, and an ID from the lost window is wrong even in its own session.

### Verification commands

V1, V2 and V7 are correct as written. I ran only V7's left-side extraction on the pinned reference: it yields 33 names from a single table.

Evidence files (the reviewer gave absolute paths): the round-2 plan diff and the hooks reference copy in the review directory; `.gitignore`; a fresh copy of the public Claude Code MCP documentation page.

## Round 3

Altitude report SHA-256: 1879fb1a2de2fce865e2b8c32f799ca67c6492001ae9d5d9f62def157159bcc2

**Verdict: not LGTM yet, by two sentences.** There is no blocker and no HIGH. Every round-2 finding of mine is resolved, and the one "not changed" reason is sound. The revision introduced two MEDIUM defects, both in sentences added this round, and both fixes shorten the plan. With them corrected I would say LGTM. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is now 54,517 bytes against an ADR estimate of 55 to 70 KB, so the margin is almost gone. The growth this round is still contract precision, and both fallbacks still fail for the reasons the report gives. The report's plan hash matches the plan file. This should be the last round that adds text; the fixes below remove or narrow sentences.

### Round-2 findings

- **Resolved as stated:** N2 (lost wakeup), N3 (the status table is total), N4 (rule 3 is its own rule, `L` is the last segment appended), N5, N7, N8, N9, N10.
- **Resolved:** N1. §B no longer claims a guard that cannot exist.
- **Resolved, with a new defect:** N6 (see R1 below).

### "Not changed" reason

- **Observing the helper's environment now (N1, second half):** sound. The observation needs a session with a plugin loaded, which this documentation ticket does not have. The ADR is correct under either outcome, and the limit is listed under #1028.

### New findings in the delta

- **[MEDIUM] R1, §J maintenance lock: the reason contradicts the repository.** The plan now says every CLI command that opens the engine takes the lock, "the read-only ones included, because opening the engine can migrate the store". The read-only open does not migrate. `open_engine` in `crates/memory-engine-cli/src/db.rs` builds with `.read_only(true)`, documented as "never acquires a write lock and never runs migrations"; only `open_engine_writable` migrates. As written, `stats`, `inspect`, `explain`, `query`, `dump`, `outcome-counts` and JSON `export` refuse whenever the server runs. Those commands are the tool map's replacements for rows 7, 8, 9, 13, 16, 17 and 20, so inspecting a fact would mean stopping the service. Fix: commands that open the engine writable take the lock; read-only opens take none.
- **[MEDIUM] R2, §D "One flusher" against "Flush order": a retry becomes a busy loop.** The flusher "looks once more and starts again if an event is there". Flush order says a retry-class or hold-class answer "stops the flush". After such a stop the failed event is still there, so a literal client starts again at once and never ends. An off-network laptop spins on connection attempts for the whole outage, and a rejected token sends 401 requests in a loop, which is the opposite of "holds everything". This is deterministic, not a race, so the delivery-order rule does not cover it. Fix: a retry-class or hold-class answer ends the flush at once, with no rescan and no second look.
- **[LOW] R3, §D: "The watcher's timer is the backstop" is undefined.** This is the only mention of a watcher in the plan, and the ticket has none. Define it (where it runs, what its timer triggers) or delete the sentence; correctness does not rest on it. If it was meant to bound the reused-PID stall, an age written into the lock with `date` does that with the existing utilities.
- **[LOW] R4, §B MCP channel: "unsupported" understates.** On the substitution route, whatever follows the `'` in a directory name is executed by the shell. Say so, so that #1028 weighs that route as a security decision.

### Checked and consistent

- **§C:** the table and decision 2 agree. 204 removes; 400, 409, 413 and 422 park; 408, 429, 5xx and a connection error retry; everything else holds.
- **§D, delivery order:** "correctness never depends on delivery order" holds for the server rules. A duplicate is answered `duplicate`, order is by key, and a late event is defined. Two flushers racing can cost a late event, never a lost one.
- **§D, wakeup:** the publish-then-lock order with the rescan and second look closes the stranded-event window.
- **§F:** rules 1 to 5 no longer conflict. A session end that opens a segment ends it, an out-of-order boundary start joins `L`, and a late completed stop gets its uptake row without rewriting a checkpoint.

Evidence files (the reviewer gave absolute paths): `crates/memory-engine-cli/src/db.rs`; the round-3 plan diff and the round-2 resolution note in the review directory.

## Round 4

Altitude report SHA-256: 33babd47fa84b0768a0f59f8d27d3e7599b0fbf634ba7354e4ef32bd2513a4b0

**Verdict: LGTM.** All four of my round-3 findings are resolved, nothing was declined, and I found no new contradiction in the delta. Two LOW notes follow; neither blocks. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is now 55,316 bytes, just inside the ADR estimate of 55 to 70 KB, so it is no longer shorter than the low end. I still would not choose a fallback: the 0.8 KB added this round replaces an unsafe route with an observed one. The report's plan hash matches the plan file. The plan should be frozen here; the notes below need at most one clause.

### Round-3 findings

- **R1, §J maintenance lock: resolved.** Only a command that opens the engine writable takes the lock. This matches `crates/memory-engine-cli/src/db.rs`: the commands listed as read-only all go through `open_engine`, which never migrates.
- **R2, §D busy loop: resolved.** A retry-class or hold-class answer ends the flush at once, and the second look follows only a flush that ended on an empty queue. The two sentences no longer conflict.
- **R3, the watcher: resolved.** §M defines it as the per-machine timer of dutiona/my-dotfiles#1911, and §D points to it. Correctness does not rest on it.
- **R4, §B MCP channel: resolved, and better than I asked.** The quoted-argument route is gone. The project directory now reaches the helper as a here-document body under `exec`.

### What I checked myself

- **The here-document claims hold on dash, bash, zsh and busybox sh.** I ran them inline with no files.
  - A path with `'`, `$(…)`, backticks and a non-ASCII character arrives literally.
  - With `exec`, a line injected after a forged delimiter does not run.
  - Without `exec`, it does run, as the plan says.
  - A failed `exec` exits the shell without running the next line.
- **Not checked by me:** the observation that Claude Code 2.1.325 does not export `CLAUDE_PROJECT_DIR` to a plugin helper. I take that from the author; it agrees with the public MCP docs.
- **§K late uptake is defined.** The injection is chosen by order key among those the server holds, a row is never rewritten, and it names none when none is held.

### New notes in the delta

- **[LOW] §D "One flusher": a lock with no PID or stamp is undefined.** A flusher killed between `mkdir` and writing the PID leaves a lock that neither test can judge, so delivery on that machine stops until someone removes it. No event is lost and no server rule breaks. One clause settles it: a lock with no readable PID and stamp is broken. The worst case is then a second flusher, which the delivery-order rule already makes safe.
- **[LOW] §D and §M, for #1019: the one-hour stamp against "no total limit".** A 16 MiB upload slower than about 4.7 KB/s outlives the stamp, so a live lock gets broken. By the delivery-order rule that costs a duplicate upload, not an event. Setting `--speed-limit` at or above that rate removes the case without new plan text.

Evidence files (the reviewer gave absolute paths): `crates/memory-engine-cli/src/db.rs`; the round-4 plan diff and the round-3 resolution note in the review directory.

## Round 5

Altitude report SHA-256: bdb76b6006b37041b0a582992f85001138b3ae9d66d26e6cc7f019100c5f2650

**Verdict: LGTM.** Both of my round-4 notes are resolved, the delta introduces no contradiction that affects a server rule, and decision 10 states its residual correctly. Two LOW notes on decision 10's wording follow; neither blocks. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is now 56,699 bytes, 1.7 KB above the low end of the 55 to 70 KB ADR estimate. The "shorter than the product" self-check therefore holds only against the upper part of the estimate. I still choose no fallback: this round's growth is decision 10, which Michaël has to see in the plan to rule on it. The report's plan hash matches the plan file. Any further edit should be net-zero.

### Round-4 notes

- **Lock with no PID or stamp (§D): resolved.** Such a lock is now broken.
- **One-hour stamp against "no total limit" (§M): resolved.** The speed limit is tied to the lock's hour.

### Decision 10

- **The residual is stated correctly.** With `exec`, no code runs (I confirmed that on four shells in round 4). The here-document can be cut short, and the new guard in §B narrows the effect to a prefix that names an existing directory.
- **The risk assessment is sound, and if anything conservative.** Redirecting to another project needs a directory entry beside that project. Beyond that, a project key is not a security boundary under one machine token: on the hook channel any directory already claims a key through its remote URL.
- **The "undocumented" claim agrees with the docs I have.** `CLAUDE_CODE_SESSION_ID` appears neither in the pinned hooks reference nor in the MCP docs I fetched in round 1.

- **[LOW] Decision 10 against §B: "the hook channel refuses a path with a line break" is looser than §B.** §B only has the server drop the one fact that holds a control character, which is the project root. The remote URL facts from that directory still go out. So for such a path the hook channel can file under the remote's key while the MCP helper sends no fact and files under the bucket. One clause makes decision 10 exact and the channels agree: on both channels, a project directory whose path holds a line break yields no project fact.
- **[LOW] Decision 10, the alternative: "closes it" holds under two conditions the text does not state.**
  - The plan says the variable exists in the helper's environment, not that it equals the hook input's `session_id`.
  - The check is undefined when the server holds no SessionStart for that session, for example when the context read failed and the event is still queued.
  - Nothing changes if Michaël accepts the recommended option. If he picks the alternative, both values must be fixed first.

### Checked and consistent

- **§K uptake:** the new sentence is right. The injection is written inside the synchronous context read, so it either exists before any stop of that session is produced or never exists, and "names no injection when none is held" covers the second case.
- **§B guard:** a path that fails "exactly one line that names an existing directory" sends no fact, so its calls go to the machine's bucket, which §I already defines.

Evidence files (the reviewer gave absolute paths): the round-5 plan diff, the round-4 resolution note and the hooks reference copy in the review directory.

## Round 6

Altitude report SHA-256: 802b2a2b5e5c126cfdd427c50a8940486a3629b8ba6531736819476aa7ccc392

**Verdict: LGTM.** Both of my round-5 notes are resolved, and the three changed sentences break no rule an implementer builds against. One of them makes a claim that §L undercuts; I flag it as LOW below, and it does not block. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is 57,045 bytes, 346 more than last round, and I still choose no fallback. The report's plan hash matches the plan file.

### Round-5 notes

- **Decision 10 against §B: resolved.** §B now says a project directory whose path holds a line break yields no project fact on either channel, and decision 10 says the same for the hook channel.
- **Decision 10, the alternative: resolved.** It now states the fail-closed case (the server refuses a call for which it holds no SessionStart root) and that the variable's equality with the hook input's session ID is still to observe.

### New notes on the changed sentences

- **[LOW] §K against §L: the recorded duration rarely identifies a read the session never received.** §K now says that when a read outlives the client's timeout, "the injection's recorded duration lets an analysis drop it". §L caps the server's own handling at 1,000 ms, while the client gives up at 2.5 s. A read that misses the client's timeout has therefore usually lost its time on the link, not in the server, and its recorded duration looks normal. The filter works only when the server itself overran. The accepted limit stands either way; the sentence should say that the server's records cannot always tell, so #1029 does not rely on the filter.
- **[LOW] §B: the new line-break rule is universal, but decision 10 is its exception.** "No project fact at all, on either channel" does not hold in the forged-delimiter case, where the helper sees one line and files under the shortened path. The next bullet and decision 10 already say so, and no implementer is misled. No plan change is needed; when the ADR is written, word the rule so the residual reads as its exception.

### Checked and consistent

- **Decision 10's alternative with §M:** if the context read fails at start, the server holds no root for that session, so model tools are refused until the queued SessionStart arrives. That is defined behaviour.
- **§K row rules:** "never rewritten" and "names no injection when none is held" are unchanged and still agree with §F rule 1.

Evidence files (the reviewer gave absolute paths): the round-6 plan diff and the round-5 resolution note in the review directory.

## Round 7

Altitude report SHA-256: 15c7398b1fd5e16ea69b684d85f794e9e7917d1377a3abf7748836189c8d1fd1

**Verdict: LGTM.** Both of my round-6 notes are resolved, and the two reworded sentences introduce no contradiction. I have no new finding at any level. I ran no build, test or gate.

### Altitude

I acknowledge the override again: one artifact, with the values in the plan. The plan is 57,191 bytes, 146 more than last round, and I choose no fallback. The report's plan hash matches the plan file.

### Round-6 notes

- **§B line-break rule: resolved.** The rule now names its one exception, the forged delimiter of decision 10, which hides the line break from the MCP helper. §B, the MCP bullet and decision 10 now say the same thing.
- **§K recorded duration: resolved.** The sentence no longer claims a filter. It says a row may name a block the session never received, that the server's records cannot always tell, and that #1029 treats an injection as served, not as seen. That agrees with §L, where the server's 1,000 ms budget sits well under the client's 2.5 s timeout.

Evidence files (the reviewer gave absolute paths): the round-7 plan diff and the round-6 resolution note in the review directory.

## Resolution

Every finding was addressed (severity mode Default). The finding-by-finding outcome of each round, for all
three reviewers, is in `docs/plans/2026-11-16-adr-0020-lifecycle-api-advisor.md`. For this reviewer:

- Round 1 (3 HIGH, 13 MEDIUM, 20 LOW): all changed the plan, except two LOW answered without a change (unknown harness parks; the dev root from a file) and one part of H3 (the oldest queued age in the status line), declined with its reason.
- Round 2 (2 MEDIUM, 8 LOW): all changed the plan, except the request to observe the helper's environment at once, which was then done in round 3.
- Round 3 (2 MEDIUM, 2 LOW): all changed the plan; the MCP route was replaced after a headless probe of Claude Code 2.1.325.
- Round 4 (LGTM, 2 LOW), round 5 (LGTM, 2 LOW), round 6 (LGTM, 2 LOW): all changed the plan.
- Round 7: LGTM on the final bytes, no finding.
- Altitude: the override was acknowledged in every round; the two corrections to the report asked in round 1 were applied.
