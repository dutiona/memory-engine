# docs(adr): add ADR-0020, the harness-neutral lifecycle API, its contracts and the lean tool map

Plan for dutiona/memory-engine#1014 (umbrella dutiona/infra#37, epic #953). Route: Documentation, Lightweight
path, Super planning, Full review. A bare `#N` below is a dutiona/memory-engine issue.

This plan fixes the values the ticket leaves open, so they can be challenged before the ADR prose exists.
Approving the plan approves those values as the ADR's starting point; merging the ADR is Michaël's approval
of the contracts and of the tool map.

## Outcome

`docs/design/adr/0020-harness-neutral-lifecycle-api.md` exists and `docs/design/adr/index.md` lists it. It
fixes every value that the plugin (#1019, #1028, #1030), the event and context routes (#1025, #1027), Bearer
authentication (#1018, dutiona/knowledge-base#571, infra D12), the scope logic (#1022), the lean surface
(#1020, #1023), redaction (#1024), the degraded embedder (#1021) and the recall log (#1029) build against in
parallel. It names no host, tailnet address, deployment port or token, quotes no hook payload, and adds no
warning to the docs build.

## Constraints

Verbatim from the ticket unless marked. "infra D11" in the ticket reads infra D12 (orchestrator correction,
2026-11-16: D11 was taken on 2026-11-13).

- "`docs/design/adr/0020-harness-neutral-lifecycle-api.md` exists and `index.md` lists it. It holds no tailnet name, address or token (public repository)."
- "the neutral core kinds (session start, prompt, tool use, stop, compact, session end), the mapping from Claude Code hooks taken from the installed hooks reference (including `PostToolUseFailure` and `SubagentStart` where present, so failed tool calls keep their outcome), and one server-side adapter per harness, which takes over the activity filter. The ME-D6 amendment is recorded with Michaël's reason (round 1, Q1)."
- ME-D6 (#953): "this is research-artifact dogfooding, not productization — resist generalizing the harness wiring for hypothetical other consumers." Amendment (define round 1, Q1): keep the harness-agnostic API; "need has evolved since then".
- "the body is the hook input byte-for-byte"
- "the idempotency key `(machine, event ID)`, with 2xx sent only after commit"
- "client reactions by status: 2xx removes the event; 401 or 403 holds everything and reports it; 408, 429, 5xx and connection errors retry; any other 4xx parks the event in `outbox/rejected/` (kept, never deleted)" (refined by decision 2 below)
- "every token is `me_` followed by 32 random base62 characters, a shape redaction matches exactly"; "the client token file is `~/.config/memory-engine/secrets/token`"; "the server-side token-hash file format (one `<machine> <sha256-hex>` line per machine) is specified here".
- "A session ends at SessionEnd, or after a fixed idle threshold when none arrives, because a killed Claude Code sends none. An event is "late" only when its client timestamp is at or before that end; it is applied but never reopens the segment. A SessionStart with `source: resume`, or any event timestamped after the end, opens a new segment of the same session ID".
- "a well-formed envelope whose hook JSON cannot be parsed is stored as metadata only (machine, event ID, timestamp, byte size, SHA-256 of the body, parse error), never the raw body".
- "final messages (Stop) and compaction summaries are stored as episodic memories under the session's project, with vectors and FTS; prompts and tool events stay in the event log only; remembered facts as before. The embedding drain is rate-limited and yields to interactive queries."
- "Path and URL header values are percent-encoded on both channels"; "Both channels derive the project facts from `CLAUDE_PROJECT_DIR`, never from the helper's working directory; the hook JSON's `cwd` is kept only as provenance."
- "Pruning, erasure and reversing a forget are maintenance CLI operations, run with the unit stopped."
- "Every model-tool response starts with a call ID, so the session link survives the 512-character truncation of captured tool results."
- "The derived jobs (`memory_consolidate`, `memory_dream_cycle`, `memory_flush_insights`, `memory_apply_cycle_report`) are off in this epic". "The ADR records how a follow-up turns them on: in the server after a segment ends, or on an in-server schedule."
- "the context route returns the finished hook-output JSON, with the status line as `systemMessage` (the only field Claude Code shows the user) and the context block as `additionalContext` (seen by the model only). The context read carries the same body and header file as the queued SessionStart event […], plus the local queued and parked counts as headers".

The umbrella's decisions the ticket names by number, verbatim from its specification. The ADR states them
as rules of its own, without numbers that point into a private document.

- Forget (13): "`forget(id, reason)` retracts a fact: "this fact won't come up again". It ends the fact's validity in the bi-temporal model. It leaves recall. A tombstone stops it from coming back through promotion or re-import. The raw events stay, and an admin can reverse it." "Pruning and deletion are never the agent's."
- Delivery (15): "Delivery is at-least-once, and the server applies each event idempotently. The event's ID and its effects commit in one transaction, and the server acknowledges only after the commit." "Events carry the client's timestamp, and the server orders a session's events by it." "A late event never reopens an ended session." "Requesting the context block is a read, separate from recording the event". "Restoring a backup can lose up to 6 h of events the server had already acknowledged, because their outbox copies are gone."
- Project key (17): "The key is the normalized `upstream` remote, else the normalized `origin`. Normalizing strips the scheme, user and `.git`, lowercases the host, and makes ssh and https forms equal." "Without a remote, the key is the project root's path relative to the machine's dev root." "Scope path: `user:michael/project:<key>`" (this last sentence is refined by decision 3 below).
- Minimal scope (18): "Writes go to the session's project. They go to user level only when the agent asks for it explicitly". "No project key derived: memory goes to a per-machine `unscoped` bucket, which is never injected into a project." "Recall covers the session's project plus explicit user-level memories. Another project is reachable only through recall's `project` argument."
- Client (20): "Utilities: POSIX `sh`, `mkdir`, `mv`, `rm`, `cat`, `date`, `git` and `curl`. It never parses JSON." "The flush lock is a `mkdir` lock". "No token appears in argv, in queued files or in the plugin's `userConfig`."
- Capture (22): "Kept in full, after redaction: prompts, the agent's final messages, compaction summaries." "Truncated to 512 characters, after redaction: tool inputs and tool results. That is the cap ME's `record_activity` already applies." "The promise: known secret formats are redacted, and events that touch sensitive files become stubs. Arbitrary secrets such as unprefixed passwords are not guaranteed to be caught."

Relayed by the orchestrator (Michaël, #1015 brainstorm, 2026-11-16): `remember` answers with the stored
memory's ID and its effective level and audience, adds a hint when the placement looks wrong, and "never
refuses or silently moves the memory"; one more model tool changes only a memory's level (project ⇄ user) by
ID; the full scope model stays in ADR-0021.

Public repository: no tailnet name, address, deployment port, token or raw hook payload in the ADR, this
plan, a commit or the PR. The PR says `Closes #1014` and `Refs dutiona/infra#37`, never a bare `#37`.

## Decisions for Michaël

Each departs from a ticket's wording or adds to it. The plan proceeds on the recommended value; approving the
plan accepts them, and any one can be struck at approval.

1. **A fourth model tool, `set_level(id, level)`**, and the `remember` receipt (the relayed decision; §J). The ticket and #1020 name three tools. No ticket owns `set_level` or the hint yet; recommended owner: #1022.
2. **Only 204 removes an event, and only 400, 409, 413 and 422 park one** (§C). Any other answer that is not a retry (404, 405, another 4xx, any 3xx, any other 2xx) cannot come from the lifecycle API: it means a wrong URL, an older server or something in between. Parking on it would empty the queue into `outbox/rejected/` one event at a time and need a manual replay; holding delivers by itself once the cause is fixed. The ticket says "2xx removes" and "any other 4xx parks".
3. **A project is one scope node** (§I). The literal `user:michael/project:<key>` would nest, because ADR-0007 splits paths on `/` (`crates/me-backend-sqlite/src/store/scopes.rs`): project `notes` would read project `notes/2026`. The key becomes one percent-encoded label.
4. **Caps in bytes** (§H, §L): 512 bytes for tool input and result, which is what `truncate()` in `crates/memory-engine/src/engine/activity.rs` does today (its comment says characters); 9,000 bytes for the context block, which is at most 9,000 characters however they are counted.
5. **Tool events are never promoted to facts** (§G). The activity filter moves into the adapter with its ignore list and dedup window; its promote list would contradict "tool events stay in the event log only".
6. **Pin, unpin and the due-list tools are removed** with no successor in this epic (tool map rows 5, 6, 14, 15); the engine methods stay. Pinning returns with the admin dashboard follow-up.
7. **A sixth fate, "upload route"**, for `memory_bootstrap_session` (row 18): backfill goes through the running server, which none of the ticket's five fates describes.
8. **The dev root is plain ASCII** (§B): a plugin `headersHelper` cannot read `userConfig`, so on the MCP channel the dev root travels in a static header that nothing can encode. Reading it from a file the hooks write would lift the restriction but race the first connection after an install.
9. **Two commits outside the two named files**, both documentation: an "Amended by ADR-0020" line in ADR-0001 (item 1), and the stale ADR count in `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` (item 2).
10. **One accepted residual on the MCP channel** (§B). A session started in a directory whose name holds a line break followed by the helper's delimiter line makes the helper see only the path before the line break. No code runs, but that session's MCP calls are filed under the shortened path, which can be another project's. Putting such a directory beside a project needs write access to the dev root, which already allows far more, and on the hook channel such a path yields no project fact. Recommended: accept it and state it. The alternative closes it on today's Claude Code only: the helper also sends an `ME-Session-Id` header (`CLAUDE_CODE_SESSION_ID` is in a helper's environment on 2.1.325, undocumented; that it equals the hook input's session ID is still to observe), and the server refuses a model-tool call whose project root differs from the one that session's SessionStart sent, or for which it holds none. It costs a header and a server check in #1025 and #1020, and rests on an undocumented variable.

## Proposed contract values

Each value carries its reason. Facts about Claude Code come from the hooks reference
(`https://code.claude.com/docs/en/hooks`, read 2026-11-16; installed Claude Code 2.1.325).

### A. Routes

- `POST /lifecycle/events` records one event. `POST /lifecycle/context` reads the context for the lifecycle event in its body; today it serves SessionStart only. Why POST for a read: it carries the hook input as its body.
- No version in the path, and no version header today. A later incompatible envelope adds `ME-Envelope-Version`; its absence means this ADR's envelope. Why not the path: queued events outlive a plugin upgrade, so a version must travel with the event.
- Both routes sit behind the same Bearer layer as `/mcp` (#1018). No route is unauthenticated.
- The context read never records the event; the event arrives through the outbox. Its only write is the recall-log injection (§K), keyed by `(machine, event ID)`; the first read wins.
- The backfill upload is a third authenticated route. Its path, limits and statuses are #1026's; this ADR only requires the same Bearer layer and the same redaction.

### B. Envelope

| Header | Required | Value |
|---|---|---|
| `ME-Event-Id` | yes | `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, opaque to the server, unique per machine for ever |
| `ME-Event-Time` | yes | `YYYY-MM-DDTHH:MM:SSZ`, UTC, what `date -u +%Y-%m-%dT%H:%M:%SZ` prints on GNU and BSD |
| `ME-Event-Seq` | yes | 1 to 6 decimal digits, leading zeros allowed: the tie-break within one second (§D) |
| `ME-Harness` | yes | `[a-z0-9-]{1,32}`; `claude-code` today; selects the adapter |
| `ME-Client-Version` | yes | the plugin's version, `[0-9A-Za-z.+-]{1,64}` |
| `ME-Project-Root` | no | the main working tree's root for a git checkout, so linked worktrees share it; else `CLAUDE_PROJECT_DIR` |
| `ME-Project-Dev-Root` | no | the machine's dev root, from `userConfig` |
| `ME-Project-Upstream-Url`, `ME-Project-Origin-Url` | no | `git remote get-url`, with the user part removed |
| `ME-Project-Branch` | no | the checked-out branch at `CLAUDE_PROJECT_DIR`; absent when detached; provenance only |
| `ME-Outbox-Queued`, `ME-Outbox-Parked` | context read only | decimal counts, the read's own event excluded |
| `ME-Event-Outcome` (response) | on every 204 | `stored`, `duplicate` or `quarantined` |

- **Version, "recorded as such".** No hook input field and no documented environment variable carries Claude Code's version, and the client never parses JSON. The client therefore sends its own version under a header named for it. The server stores `client_version` as sent and leaves `harness_version` empty until an adapter can read one from a body.
- **Absent facts.** A fact that does not exist (no such remote, not a git tree) has no header. No header names the machine: identity comes from the token.
- **Encoding.** The five project facts are percent-encoded on both channels. A client must encode every byte outside printable ASCII (0x21 to 0x7E) and each of `%`, `"` and `\`, as `%HH`; it may encode any other byte. The server decodes every `%HH` (either hex case). Why: decoding is canonical, so `curl -H @file` and the `headersHelper` JSON give one key even if they encode differently; encoded values need no JSON escaping; a newline cannot inject a header. It needs only the shell: one character at a time by parameter expansion under `LC_ALL=C`, `printf '%d' "'c"` masked to a byte, `printf '%%%02X'`. Measured on dash, bash and busybox sh; #1019's tests repeat it on bash 3.2.
- **A bad fact is dropped, not the event.** Facts are optional. A project directory whose path holds a line break yields no project fact, on either channel; the one exception is the forged delimiter of decision 10, which hides the line break from the MCP helper. A fact that does not decode to valid UTF-8, holds a control character or exceeds 4,096 bytes is ignored and flagged in the event's provenance; the event is stored and filed without it. Why not 400: a legal but odd path would park every event of its project, and a replay would fail the same way.
- **Userinfo never leaves the machine.** Before it queues an event, the client removes everything up to and including the last `@` in front of the host, in both forms a remote takes: `scheme://user:secret@host/path` (a bare token before the `@` included) and `user@host:path`. Parameter expansion is enough. The server strips again before it stores or logs anything. Stored facts never hold userinfo.
- **Dev root (decision 8).** Sent as configured. It must be an absolute path of printable ASCII without space, `"`, `%` or `\`, so its encoded form is itself. The server ignores any other value, on both channels alike.
- **MCP channel.** The `headersHelper` prints `Authorization`, `ME-Harness`, `ME-Client-Version` and the encoded facts under the same names. The project directory reaches it as data, on standard input: the helper command is `exec <helper> <<'DELIMITER'`, and the here-document's body is the `${CLAUDE_PROJECT_DIR}` substitution. Observed on Claude Code 2.1.325 (2026-11-16, a headless session with a probe plugin): `CLAUDE_PROJECT_DIR` is not in a plugin helper's environment; the substitution is textual, so a path with a `'` breaks a quoted argument and the helper never runs; the here-document delivers a path with a space, a non-ASCII character and a `'` intact. The helper accepts exactly one line that names an existing directory; otherwise it sends no fact. A directory name crafted to hold the delimiter line cannot run code: with `exec`, the shell is replaced before it reads another line (checked on dash, bash, busybox sh and zsh). It can cut the path short, which decision 10 accepts. `CLAUDE_PROJECT_DIR` does not move when Claude runs `cd` or enters a worktree, so both channels keep one project for the whole session.
- **`ME-Event-Outcome`** makes "stored once" and "quarantined" observable at the HTTP seam the tests drive (#1025). The client ignores it.
- **Limits.** Each header value is at most 4,096 bytes. The server reads the body as raw bytes and ignores `Content-Type`.

### C. Statuses and the body limit

| Status | The server sends it when | Client |
|---|---|---|
| 204 | committed: stored, duplicate (same key, same body SHA-256) or quarantined | removes the event |
| 200 | context route only: the hook-output JSON (§L) | prints the body |
| 400 | a required header is missing or off its grammar | parks |
| 401 | no token, a malformed one, or one whose hash is not in the file | holds everything, reports |
| 403 | never sent by ME; an intermediary may send it | holds everything, reports |
| any status not in this table (404, 405, another 4xx, any 3xx, another 2xx) | never sent by the lifecycle routes: a wrong URL, an older server, an intermediary | holds everything, reports (decision 2) |
| 408 | the body stalls: no byte for 60 s | retries |
| 409 | the key exists with another body SHA-256 | parks |
| 413 | the body exceeds 16 MiB (16,777,216 bytes) | parks |
| 422 | the server cannot apply this event: unknown `ME-Harness`, unknown `hook_event_name`, no `session_id`, a failure while applying it; a context read for a kind other than session start | parks |
| 429 | never sent today | retries |
| 5xx | a fault that does not depend on the event: storage unavailable or busy, shutting down | retries |

- **16 MiB** clears #1025's 10 MB test with margin and bounds what one request holds in memory. A parked oversize event is kept.
- **No event-specific fault ever gets a 5xx.** An unparseable body is quarantined; anything else the server cannot apply for a reason tied to this event, an unexpected error included, gets 422 and parks. Why: a retry-class answer stops the queue (§D), so an event that fails every time must not hold every later one. A 5xx always means "the same request will work later".
- **Quarantine or 422.** A body that is not UTF-8, not JSON, or not a JSON object is quarantined and answered 204, so it never loops in an outbox. Valid JSON the adapter cannot map gets 422 and parks: the event stays whole on the client and can be replayed after a server upgrade. That is why the server is deployed before the plugin.
- **The table is total.** 204 removes; 400, 409, 413 and 422 park; 408, 429, 5xx and a connection error retry; everything else holds. A 404 means the request never reached the lifecycle API, so nothing was judged about the event and the queue holds. A 422 is the API's judgement on one event, so that event parks and the queue goes on.
- **409** turns an event-ID collision into a parked event. Without it, a reused ID would be acknowledged as a duplicate and deleted.
- **The client never follows a redirect.** On the context route it prints a body only when the status is 200 and the type is `application/json`, because a SessionStart hook's plain stdout goes into the model's context.

### D. Identity, order and delivery

- **Idempotency.** `(machine, ME-Event-Id)` and the body's SHA-256 commit in the transaction that commits the event's effects; 204 only after it.
- **Order.** The server orders a session's events by the order key `(ME-Event-Time, ME-Event-Seq as a number, ME-Event-Id)`. It never compares a client clock with its own, and never rejects a time for skew.
- **Seq.** Unique among one machine's events that share an `ME-Event-Time`, and increasing in the order the events reserved it. Hooks that run one after another keep their order, which is every case where order matters: Claude Code waits for a synchronous hook before the next event. Hooks that overlap (parallel tool calls, subagents) get an arbitrary order among themselves. Seqs need not be contiguous: a hook killed after reserving leaves a gap. Why not a precedence of kinds: every `claude -p` run emits Stop and SessionEnd in one second, and an exit followed by a `--resume` in one second has no correct kind order.
- **Delivery order is best effort; correctness never depends on it.** A duplicate upload is answered `duplicate`, the server orders by key whatever the arrival order, and §F defines an event that arrives after its segment ended. The rules below keep order in the normal case and save bandwidth; a rare failure of one costs a late event, never a lost one.
- **One flusher.** A flush runs under the outbox's `mkdir` lock; a second flush that finds it held exits. The lock records the flusher's PID and a time stamp the flusher renews before each request, and it is broken only when that process is gone (`kill -0`), the stamp is older than one hour, or neither can be read. With these utilities the test is best effort, which the rule above makes safe.
- **Flush order.** Oldest first by order key, one request at a time. A park-class answer moves the event to `outbox/rejected/` and the flush goes on. A retry-class or hold-class answer ends the flush at once; the next flush, started by the next hook or by the watcher's timer (§M), begins again from that event.
- **No lost wakeup.** A hook publishes its event before it tries the lock. A flush that ends because the queue is empty releases the lock, then looks once more and starts again if an event is there. A flush that ended on a retry or a hold does not look again.

### E. Machine tokens

- **Token.** `me_` and 32 characters of `[0-9A-Za-z]` from a CSPRNG (about 190 bits). One per machine; the same token serves ME and KB. Redaction matches those 35 characters when no `[0-9A-Za-z_]` touches either side, so `me_types` stays.
- **Client file.** `~/.config/memory-engine/secrets/token`, mode 0600: the token and at most one trailing LF. The request header is exactly `Authorization: Bearer <token>`.
- **Hash file.** Lines of `<machine> <sha256-hex>` and LF, the last line included. `<machine>` is `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`, a DNS label. `<sha256-hex>` is 64 lowercase hex digits: the SHA-256 of the token's 35 ASCII bytes, no newline. Exactly one space. No comment, blank line, CR or other whitespace.
- **Fail closed.** A missing or unreadable file, a malformed line, a repeated machine or a repeated hash makes the service refuse to start and name the line number. An empty file is valid and admits no one. The file is read at startup only; a change needs a restart. The deployed path belongs to infra D12, which only references this format.
- **Check.** A presented value off the token shape gets 401 without hashing; otherwise its hash is compared in constant time.
- **Vectors, without a token-shaped literal.** The ADR states the SHA-256 of `me_` followed by 32 `0` characters, computed at writing time, and lists the lines every implementation must reject: uppercase hex, an uppercase machine, two spaces, CRLF, a last line without LF, a repeated machine. Three implementations (Rust, Python, the infra mint script) then agree. No example token appears anywhere.

### F. Session segments

All comparisons use the order key of §D, so two events in one second compare by seq. `L` is the session's
latest segment, the last one appended.

- **Idle threshold: 30 minutes.** A killed session is finalized within the half hour, and the epic's idle-hour check crosses it. A split costs one more checkpoint and loses no captured content.
- **1. Lateness is decided first.** An event is late when its key is before `L`'s start, or at or before `L`'s end once `L` has ended. A late event is stored and has its own effects (a late final message still becomes a memory). It changes no segment and re-runs no derived work: the checkpoint of an ended segment describes it as it stood when it ended, and a late completed stop still gets its own uptake row (§K). Segments are only appended: an event older than `L` never creates one, even between two earlier segments, and a late `resume` start opens nothing. Lateness is rare by construction: delivery follows key order, except for a replayed parked event or a failure of the flush lock.
- **2. An event that is not late:**
  - with no segment yet, opens the first one;
  - if `L` has ended, opens a new one;
  - if `L` is open and the event comes after `L`'s last event, and it is a main-agent session start whose `source` is `startup`, `resume`, `clear` or `fork`, ends `L` at its last event and opens a new one;
  - if `L` is open and the event's client time is more than 30 minutes after `L`'s last event, ends `L` there and opens a new one;
  - otherwise joins `L`. That includes an event that arrives out of order inside `L`'s range, whatever its kind.
- **3. A session end ends the segment it is in**, whatever its `reason`, whether it joined that segment or opened it. The end is the segment's last key: the session end's own, unless a later event was applied first.
- **4. Idle sweep.** When the server has received no event of a session for 30 minutes, it ends `L` at its last event. This is the only rule that reads the server clock, and it compares it with the server's own receipt times.
- **5. Derived work** runs once per ended segment: one checkpoint per segment (its bounds, how it ended, per-kind counts, the fact of its last main-agent final message) and the uptake rows (§K). A segment with no main-agent prompt and no main-agent stop gets neither, so a lone session end after an idle end costs nothing. Consolidation stays off.
- **Not boundaries by their kind:** a session start with `source: compact`, and every subagent event. They follow rules 1 and 2 like any other event.
- **Accepted.** A laptop that goes offline for more than 30 minutes in mid-session has its session split at the gap (rule 4), and so does a 40-minute build with no event (rule 2).

### G. Neutral core and the Claude Code adapter

- **Core record.** One of six kinds (`session_start`, `prompt`, `tool_use`, `stop`, `compact`, `session_end`), an actor (`main`, or `subagent` with its ID and type), an outcome where the kind has one, and provenance: machine (from the token), harness, client version, session ID, agent, branch, the hook input's `cwd`, project root and key. The core holds no tool name and no harness field name.
- **Adapter.** One per harness, in the server crate, chosen by a `match` on `ME-Harness`. No adapter trait until a second adapter exists: ME-D6's warning still holds for an abstraction with one implementation. The adapter owns the mapping, the activity filter, the path extraction for stubs, and the rendering of the context response.
- **Actor.** `subagent` when the hook is `SubagentStart` or `SubagentStop`, or when the hook input carries `agent_id`; else `main`. Claude Code's internal agents (prompt suggestions, side questions) fire `SubagentStop` too; this rule keeps their messages out of recall.

| Claude Code hook | Kind | Outcome and content |
|---|---|---|
| `SessionStart` | `session_start` | `source` |
| `UserPromptSubmit` | `prompt` | the prompt, in full |
| `SubagentStart` | `prompt` | a subagent turn starts; no text |
| `PostToolUse` | `tool_use` | `success`; input and response excerpts |
| `PostToolUseFailure` | `tool_use` | `failure`, or `interrupted` when `is_interrupt` is true; `error` excerpt |
| `Stop` | `stop` | `completed`; the final message, in full; recallable when the actor is main |
| `StopFailure` | `stop` | `api_error`; never recallable (its message is the error string) |
| `SubagentStop` | `stop` | `completed`; final message in the event log only |
| `PostCompact` | `compact` | `trigger`; the summary, in full; recallable when the actor is main |
| `SessionEnd` | `session_end` | `reason` |

- The outcome records which hook fired. The reference does not say whether a running tool the user cancels fires `PostToolUse`; it says only that `PostToolUseFailure` does not fire and that the result carries the interruption. If no hook fires, the call is not captured (§H).
- **Never registered, because registering changes Claude Code's behaviour:** `PreCompact` (its stdout steers the summary), `PreModelSwitch` (a timeout blocks the switch), `WorktreeCreate` and `WorktreeRemove` (they replace git's behaviour), `MessageDisplay` (its output replaces the displayed text).
- **Not captured:** `PreToolUse` and `PostToolBatch` (the two tool hooks carry the call and its outcome); `PermissionRequest` and `PermissionDenied` (a call that runs is captured by the tool hooks); `UserPromptExpansion` (the prompt as typed is captured); and the events with no memory content (`Setup`, `InstructionsLoaded`, `Notification`, `TaskCreated`, `TaskCompleted`, `TeammateIdle`, `ConfigChange`, `CwdChanged`, `DirectoryAdded`, `FileChanged`, `PostModelSwitch`, `Elicitation`, `ElicitationResult`).
- **Activity filter (decision 5).** It classifies a `tool_use` as ignored, duplicate (300 s window) or recorded, for the activity stream only. Every event is stored whatever its class, and nothing is promoted to a fact. Found while planning: today's patterns (`prettier`, `git_commit`) match no Claude Code `tool_name`; #1025 sets the adapter's own.

### H. Capture, stubs, quarantine

- **Promise.** The owner's wording verbatim, then what is not captured at all: a tool call rejected or cancelled before it runs fires neither tool hook, a running tool the user cancels may fire none, and an interrupted turn fires no Stop.
- **Order.** Read the call ID (§J), redact, then truncate. Truncating first could cut a secret below its pattern.
- **Cap (decision 4).** Tool input, response and error are each cut to 512 bytes of UTF-8, back to a character boundary. Prompts, final messages and compaction summaries are kept in full.
- **Stub.** `tool`, `path`, `outcome`, `time`, plus the event's provenance. `path` is the matched path only; for a shell command, the matched token, never the command. Nothing of the input, the response or the error is kept.
- **Quarantine record.** Machine, event ID, client time as sent, received-at, harness, byte size, the body's SHA-256, and the parse error as a class with line and column. Never the parser's message, which can quote the body.
- **Recallable.** A main-agent `stop` with `completed` and a non-empty final message, and a main-agent `compact`, each become one Episodic fact under the event's project (or the machine's bucket), found by FTS at once and by vector once embedded (#1021). A stop that another Stop hook then continues still counts: it was a final message. The fact's validity starts at the event's client time, or at receipt if that is earlier, so a fast client clock cannot schedule a memory in the future. Everything else stays in the event log.
- **Drain.** The ticket's sentence verbatim: "The embedding drain is rate-limited and yields to interactive queries." The rate is #1021's, from the embedder's measured throughput.
- **ADR-0001.** These two projections are a narrow exception to "Facts are consumer-derived via explicit `add_fact()` calls, not auto-projected from events", forced by the single transaction of delivery. ADR-0020 says so, and ADR-0001 gains an "Amended by" line (decision 9).

### I. Project key and the minimal scope contract

The owner's rules of Constraints, then:

- **Key.** The normalized upstream URL, else the normalized origin URL, else the project root relative to the dev root, else none. Normalizing drops the scheme, userinfo, port, a trailing `/` and `.git`, lowercases the host, and reads `host:path` as `host/path`. A local-path or `file:` remote is ignored: it would give a per-machine key.
- **Path key.** The root must sit strictly below the dev root at a `/` boundary, with no `.` or `..` segment. A root equal to the dev root gives no key.
- **Scopes (decision 3).** Project: `user:<user>/project:<label>`, the label being the key with every byte outside `A-Z a-z 0-9 - . _ ~` percent-encoded in uppercase hex (`github.com%2Fexample%2Fproject`). User level: `user:<user>`. Bucket: `unscoped:<machine>`, a top-level node that no query rooted at the user can reach. `<user>` is a server setting. When the segment `project:<label>` exceeds 256 bytes, the scope tree's limit, there is no key.
- **Writes.** To the session's project; to user level only on `remember(level: "user")` or `set_level`; to the machine's bucket when there is no key.
- **Reads.** `recall` and the context block read the session's project and the user level, exactly. A session with no key reads its machine's bucket and the user level. `recall`'s `project` argument swaps the project and never reaches a bucket. Nothing reads the root scope.
- **ADR-0021** (#1015) replaces this section. Every event keeps its decoded facts, userinfo removed, so history re-files without re-capture.

### J. Model tools and the call ID

Final surface: `recall`, `remember`, `forget`, `set_level`. Each takes a JSON object and refuses unknown arguments.

- `recall(query: string, project?: string)` returns up to 10 results. Each carries the fact ID, its kind (remembered, final message, compaction summary), its scope (a project key, `user` or `unscoped`), its date and its machine. A full-text-only answer says so. No `limit` and no mode: a fixed size keeps the recall log comparable for the switch-on rule.
- `remember(content: string, level?: "project" | "user")`, default `project`. It stores a Semantic fact and answers with the fact ID, where it went and who sees it, for example `stored 412 in project github.com/example/project, visible to all agents`. When a server-side heuristic finds the placement doubtful (content that reads user-wide filed under a project, or the reverse), it adds one hint line naming `set_level`. It never refuses and never moves the memory itself. The heuristic's rules are not part of the contract; they belong to the ticket that owns `set_level`.
- `forget(id: integer, reason: string)`: the retraction of Constraints. It answers `forgotten`, `already forgotten` or `not found`.
- `set_level(id: integer, level: "project" | "user")` changes only the level. `project` means the calling session's project, or its machine's bucket. It never moves a fact between two projects (maintenance does). It answers with the same receipt as `remember`; a fact already at that level is reported unchanged. The move is an event, so it is auditable and undone by calling it again.
- **Write authority.** `forget` and `set_level` act only on a fact in the session's own project (or bucket) or at user level. A fact of another project, even one `recall(project: …)` returned, answers `not found`, which does not reveal that it exists.
- **Call ID.** Every model-tool response is a tool result whose first text content starts with the line `call ` and 16 lowercase hex digits. That includes failures: the four tools return them as tool results, never as JSON-RPC errors, which have no first line. The server records each call (ID, machine, tool, time, the fact IDs returned or changed). The adapter links a captured tool event to a call when the first line of its response or error text is a call line and ME holds that call for the same machine; a forged line matches nothing. #1025 pins the shape of an MCP tool's `tool_response` with an observed fixture, since the hooks reference does not give it. Why 16 hex: under every redaction rule in `crates/memory-engine/src/bootstrap/redact.rs` (hex needs 32, entropy needs 20 characters), and random, so a restore cannot make it repeat.
- **Maintenance CLI.** New commands: `prune` (#1020), `unforget` and `erase` (#1023). "Refuses while the server holds the store" is one mechanism for both sides: the server holds an exclusive advisory lock on a lock file next to the database for its whole life, and every `memory-engine-cli` command that opens the engine writable takes the same lock or refuses. A read-only command (`explain`, `inspect`, `stats`, `query`, `dump`, `outcome-counts`, `status`) takes no lock and may run against the live store: `open_engine` in `crates/memory-engine-cli/src/db.rs` never migrates.
- **Derived jobs.** Off: no tool, route or timer runs them. A follow-up turns them on in the server, after a segment ends (segment-local work) or on an in-server schedule (cross-segment work). Both need a summary-LLM endpoint this epic does not provision.

### K. Recall log

- **Injection**, one per context read: the SessionStart `(machine, event ID)`, session ID, `source`, project key, server time and duration, the retrieval settings as run (mode, degraded cause, the ADR-0015 embedding space, caps, fusion parameters), the candidates (fact ID, rank, per-signal and fused scores), the injected items (fact ID, position, bytes), the block size and the status line.
- **Model recall**: the call record of §J plus the redacted query, the `project` argument, the scope read, the retrieval settings, the results (fact ID, rank, scores) and the degraded flag. The session ID is filled when the call is linked.
- **Uptake**, written when a segment ends: one row per main-agent completed stop of the segment, naming the session's injection whose SessionStart has the greatest order key below the stop's, among the injections the server holds then. A late completed stop gets its row when it is applied. A row is never rewritten, and it names no injection when none is held. Normally the injection exists first: the context read writes it inside the SessionStart hook, before any later event of that session is produced. The exception is a read the client gave up on: the session never received that block, a row may still name it, and the server's records cannot always tell (a slow link looks like a normal read). #1029 therefore treats an injection as served, not as seen. A link, with no score: final messages and injected items are kept in full, so the follow-up scores uptake with the method it validates.

### L. Budgets and the SessionStart output

| Budget | Value | Past it |
|---|---|---|
| Recall embed time limit | 500 ms, for `recall` and for the context build | full-text search, flagged degraded |
| SessionStart server budget | 1,000 ms from request to response | the status line alone, `degraded (context timed out)` |
| Hook client timeout | `curl --connect-timeout 1 --max-time 2.5` on the context read | the client's fallback line |

- 500 ms is about four times the 124 ms p95 measured under load. 1,000 ms leaves the fallback room. 2.5 s adds TLS setup over a slow link, and an off-network laptop gives up after one second.
- **Output.** One JSON object: `systemMessage` (the status line) and `hookSpecificOutput` with `hookEventName: "SessionStart"` and `additionalContext` (the block). The block is at most 9,000 bytes of UTF-8 and its first line repeats the status line, because the model never sees `systemMessage`. Every `source` gets a block.
- **Status line**, one grammar for the server and the client:

  ```text
  line  = "memory: " state [ "; outbox " N " queued, " N " parked" ]
  state = "OK" / "degraded (" cause [ ", " N " waiting, oldest " age ] ")"
        / "unreachable" / "token rejected" / "no token file" / "error " status
  cause = "embedder down" / "embedder slow" / "embedder key rejected" / "context timed out"
  age   = N ( "m" / "h" / "d" )
  ```

  The outbox part appears when either count is non-zero. The waiting part appears when the embedding queue is not empty; `OK` never takes it. Degraded means this read fell back to full-text search or the queue is paused, never merely that items wait. `age` uses the largest whole unit.
- **Client fallback.** The last four states are client-made. The client prints `{"systemMessage":"<line>"}` and nothing else: no `additionalContext`. It picks one of four fixed templates and substitutes integers only (the status, the counts), so it never escapes a string.

### M. Claude Code client

Only what both sides rely on; the rest is #1019's.

- **Outbox.** `~/.local/state/memory-engine/outbox/`, directories 0700 and files 0600: queued bodies are unredacted, and `outbox/rejected/` is kept. The token is read from its file at send time and is never in a queued or parked file, so a rotated token needs no requeue. It never reaches a command line either: `curl` reads the `Authorization` header from standard input (`-H @-`).
- **Seq and event ID.** With the body already on disk, the helper takes the time, then reserves `mkdir <outbox>/seq/<epoch>.<n>` from n = 0 upward: the first success is the seq. Lock-free, and `mkdir` is in the utility set. The loop is bounded; past the bound the hook gives up. The event ID is `<epoch>-<seq as 6 digits>-<pid>`, so file names sort in order key order under a plain glob; the PID covers ledger pruning and clock steps, and 409 catches what is left. One `date` call yields the epoch and the header time.
- **A capture hook never blocks the session.** On any failure it exits 0 and prints nothing; its registration carries a 5 s `timeout`. SessionEnd keeps Claude Code's 1.5 s budget, which the timeout of a plugin-provided hook does not raise, so every capture hook only enqueues.
- **Watcher.** A per-machine timer that runs the flush with no session open (dutiona/my-dotfiles#1911), so an offline machine's events arrive later without another session.
- **Flush.** The lock and order of §D and the reactions of §C, with stall-based limits (`--connect-timeout`, `--speed-limit`, `--speed-time`) and no total limit, so a 16 MiB event on a slow link still gets through; the speed limit is set so that it does within the lock's hour. The detached flush redirects its stdio; otherwise Claude Code waits up to 10 s for the hook.
- **Context read.** Count, enqueue the SessionStart event, start the flush, then POST the same body and header file plus the counts. Print as §C and §L say.
- **Utility set.** The eight utilities of Constraints and the shell's own builtins (`printf`, `[`, `kill`, parameter expansion).

### N. Form of the ADR

- House format of ADR-0015 and ADR-0019. `**Status:** Accepted`, with the line that merging the record is Michaël's approval; `**Date:** 2026-11-16`; `**Amends:** ME-D6 (#953), ADR-0001`; `**Related:** ADR-0003, ADR-0007, ADR-0009, ADR-0015, ADR-0021 (#1015)`.
- Sections: Context (the problem, ME-D6 quoted with its amendment and reason, the hooks reference and its date); Decision §1 layers, §2 neutral core, §3 wire contract (A to C), §4 machine tokens (E), §5 delivery and session segments (D, F, with one `{mermaid}` state diagram), §6 capture (H), §7 project key and minimal scope (I), §8 lean tool map (the table below, J), §9 recall log (K), §10 SessionStart context and budgets (L), §11 Claude Code adapter (G), §12 Claude Code client (M); Consequences; Alternatives considered; References.
- §2 to §10 are harness-neutral: a second harness adds a sibling of §11 and §12 and edits nothing above them.
- Consequences, at least: what gets easier (parallel tickets, a second harness); the latency a session start can pay (2.5 s at worst); a session split by an outage or a long silence; a stuck head of queue shows as a queued count that stays above zero at session start; the 6 h restore window: fact IDs issued since the backup are issued again to other facts, so an ID a model still holds from that window can name another fact; a quarantined body is not recoverable from ME (the transcript on the machine still has it); nothing is distilled while consolidation is off; untrusted text can reach a final message, become a memory and return in every later block, which is why #1027 frames block items as recalled claims with their provenance; the stores start empty, and nothing filed at the root scope or an old-style path is read; the server is deployed before the plugin.
- Alternatives recorded as rejected: Claude Code's `http` hook (no SessionStart support, no offline queue); async capture hooks (killed at `-p` teardown); a version in the path; a tie-break by kind or by arrival; nested scope labels; storing an unparseable body raw; 202 Accepted; a dev root read from a file; the project directory as a quoted argument of the helper.
- Writing rules: downstream tickets by issue number, never by plan ID; the owner's decisions as rules, credited to the umbrella dutiona/infra#37, without decision numbers; "infra D12"; synthetic examples only (`github.com/example/project`, `~/dev/project`); field names, never a hook-input JSON example; a placeholder in angle brackets only inside a code span (MyST drops it elsewhere); no `|` inside a table cell; the diagram as a `{mermaid}` directive.

## Tool map

Today's 26 tools, in the order of `crates/memory-engine-mcp/src/tools/definitions.rs`. Every row leaves MCP in
#1020. "Event endpoint" means the two `/lifecycle/` routes.

| # | Tool | Fate | Where it goes |
|---|---|---|---|
| 1 | `memory_ingest` | event endpoint | `POST /lifecycle/events`; raw appends leave the network (#1025) |
| 2 | `memory_add_fact` | model tool | `remember`; no vector, scope path, pin or validity from the model |
| 3 | `memory_query` | model tool | `recall` |
| 4 | `memory_resume_context` | event endpoint | `POST /lifecycle/context` builds the block (#1027) |
| 5 | `memory_list_due` | removal | no lean tool schedules a fact (decision 6) |
| 6 | `memory_next_due_time` | removal | nothing polls a timer over HTTP |
| 7 | `memory_explain_fact` | maintenance CLI | `explain` (exists) |
| 8 | `memory_get_fact` | maintenance CLI | `inspect` (exists) |
| 9 | `memory_statistics` | maintenance CLI | `stats` (exists); live health is `status` (#1021) |
| 10 | `memory_flush_insights` | in-server job, off | the follow-up decides whether it returns |
| 11 | `memory_consolidate` | in-server job, off | the follow-up; the `consolidate` command stays |
| 12 | `memory_forget` | maintenance CLI | `prune` (#1020); the word `forget` passes to the retraction |
| 13 | `memory_dump_state` | maintenance CLI | `export` (exists); no path write on the network |
| 14 | `memory_pin_fact` | removal | decision 6 |
| 15 | `memory_unpin_fact` | removal | decision 6 |
| 16 | `memory_replay_events` | maintenance CLI | `dump events` (exists) |
| 17 | `memory_fact_history` | maintenance CLI | `inspect` prints the four timestamps |
| 18 | `memory_bootstrap_session` | upload route | the authenticated backfill upload (#1026, decision 7) |
| 19 | `memory_record_outcome` | maintenance CLI | `record-outcome` (exists) |
| 20 | `memory_outcome_counts` | maintenance CLI | `outcome-counts` (exists) |
| 21 | `memory_record_activity` | event endpoint | `tool_use` events (#1025) |
| 22 | `memory_checkpoint_session` | in-server job, on | one checkpoint per ended segment (§F) |
| 23 | `memory_load_context` | event endpoint | `POST /lifecycle/context` |
| 24 | `memory_dream_cycle` | in-server job, off | the follow-up |
| 25 | `memory_apply_cycle_report` | in-server job, off | the follow-up applies the report in the server |
| 26 | `memory_get_recent_insights` | removal | its source (row 10) is off; `recall` covers retrieval |

Tally: 2 model tools, 4 event endpoint, 1 upload route, 9 maintenance CLI, 5 in-server jobs (1 on), 5 removals.
New, and not among the 26: `forget` (#1023) and `set_level`.

## Where the ADR will differ from a ticket's restatement

The merged ADR wins; these are listed in the PR body so each ticket's own plan re-reads its section.

- #1019: `ME-Event-Seq` and the `mkdir` ledger; flush order under one lock with a PID and a renewed stamp, and the second look; stall-based limits; removal on 204 only, parking on four statuses only, hold on anything unexpected; userinfo removed before queueing; the token on curl's standard input.
- #1020 and the infra deploy smoke: the final `tools/list` has four tools; failures are tool results; the maintenance lock, taken by every command that opens the engine writable.
- #1022: one encoded label per project; `unscoped:<machine>`; the main working tree as project root; a bad fact is dropped, not refused with the event.
- #1025: 409 on a reused ID; 422 for an event the server cannot apply; `ME-Event-Outcome`; one checkpoint per segment.
- #1027: 9,000 bytes; the status grammar; the status line repeated at the top of the block.
- #1028: the dev root restriction; four tools in the skill; the project directory on the helper's standard input (the environment route it hoped for does not exist), and decision 10's residual.
- #1029: one uptake row per stop, a link; a late stop still gets one.
- #1030: `--connect-timeout 1 --max-time 2.5`; `memory: error <status>`; a body printed only for a JSON 200.

## Implementation sequence

- [ ] **1. Add ADR-0020, list it, and mark ADR-0001 amended**
  - Files: `docs/design/adr/0020-harness-neutral-lifecycle-api.md` (new), `docs/design/adr/index.md`, `docs/design/adr/0001-event-sourcing.md`.
  - Red, before writing: V3 prints the 26 tool names as missing; V5 prints every anchor as missing.
  - Change: the ADR in the form of §N, carrying sections A to M and the tool map as approved; the token test vector computed (V6); the toctree line `0020-harness-neutral-lifecycle-api` after `0019-wisdom-substrate`; in ADR-0001's header, the line `**Amended by:** ADR-0020 (two rule-based projections from lifecycle events)`.
  - Green: V1 to V7 give their expected results.
  - Commit: `docs(adr): add ADR-0020, the harness-neutral lifecycle API, its contracts and the lean tool map`. The body says why: one record so that seventeen tickets build in parallel, and whose merge approves the tool map.
- [ ] **2. Boy-scout: stop counting the ADRs**
  - Files: `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`.
  - Red: `grep -n '9 Architecture Decision Records\|ADRs (9 decisions)' AGENTS.md CLAUDE.md GEMINI.md` prints three lines (there are 19 ADRs, 20 after item 1).
  - Change: each line names the directory without a count.
  - Green: the grep prints nothing; each file's table or list still renders (same column count).
  - Commit: `docs: stop counting the ADRs in the agent instruction files`.

## Documentation

The ADR is the documentation. `README.md`, `AGENTS.md`, `GEMINI.md`, `docs/reference/mcp-server.md` and the
`server.rs` instructions describe the tool surface, which #1020 changes; #1020 owns them. The PR body carries
the tool-map table, the acceptance trace of V5, the list of differences above and the collateral findings.

Collateral, recorded in the ledger and the PR body, not fixed here:

- The docs build is already red under `-W` on `main`: 31 warnings measured at the branch point (two dead toctree entries in `docs/design/index.md`, nine documents in no toctree, seven bare `mermaid` fences, twelve dead cross-references, a missing `_static` directory). It spans many files, so it needs its own PR; recommended: one issue.
- `truncate()`'s comment and the `memory_record_activity` schema say characters; the code cuts bytes. For #1024, which moves truncation.
- The activity filter's patterns match no Claude Code tool name. For #1025.
- No ticket owns `set_level` or the `remember` hint.

## Testing

N/A for code tests: the change adds no behaviour. The checks that can fail are V1 to V7 below, each tied to a
failure this change can have: a new Sphinx warning, a leak, a wrong tool map, a file outside the route, a
criterion with no section, a wrong test vector, a hook left unplaced. Each contract is tested where it is
built; the ADR's token vectors, status table, status grammar and output object are the shared vectors.

## Verification

Run from the repository root at the final clean HEAD. `ADR=docs/design/adr/0020-harness-neutral-lifecycle-api.md`;
`$S` is a fresh scratch directory outside the repository.

- **V1, docs build adds no warning.** `-W` is not usable: the base already has warnings (see Documentation).

  ```sh
  build() { uv run sphinx-build --keep-going -q -b html \
    -D exclude_patterns=_build,Thumbs.db,.DS_Store,ROADMAP.md,plans -w "$2" "$1" "$S/html-$(basename "$2")"; }
  base=$(git merge-base origin/main HEAD); mkdir -p "$S/base"; git archive "$base" docs | tar -x -C "$S/base"
  build "$S/base/docs" "$S/base.warn"; build docs "$S/head.warn"
  wc -l < "$S/base.warn"; wc -l < "$S/head.warn"
  grep -nE '0020-harness-neutral-lifecycle-api|adr/index|0001-event-sourcing' "$S/head.warn"
  ```

  Expected: both builds exit 0, the two counts are equal (31 at the branch point), the grep prints nothing. `-D` restates `docs/conf.py`'s list and adds `plans`, because the plan files are in no toctree until `/finish-pr` archives them.
- **V2, leak scan** over everything the branch publishes: its diff, its commit messages and the PR body saved to `$S/pr-body.md`. The deployment's names, addresses and ports are in a private pattern file outside the repository, one extended regular expression per line, never committed or quoted. The lens drafts under `docs/plans/*-drafts/` are ignored by git and never committed.

  ```sh
  { git diff origin/main...HEAD; git log --format=%B origin/main..HEAD; cat "$S/pr-body.md"; } > "$S/published.txt"
  test -s "$PRIVATE_PATTERNS" || echo "pattern file missing or empty"
  grep -nEf "$PRIVATE_PATTERNS" "$S/published.txt"; echo "private: $?"
  grep -nE 'ts\.net|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]+\.[0-9]+|fd7[a]:|me_[0-9A-Za-z]{32}|/home/[a-z]|"(session_id|transcript_path|tool_input|tool_response)"[[:space:]]*:' "$S/published.txt"; echo "generic: $?"
  ```

  Expected: no match, no "pattern file" line, and both status lines print `1` (a `2` means the scan did not run).
- **V3, the tool map has exactly the 26 names.**

  ```sh
  diff <(sed -nE 's/^[[:space:]]+"(memory_[a-z_]+)",$/\1/p' crates/memory-engine-mcp/src/tools/definitions.rs) \
       <(sed -nE 's/^\| [0-9]+ \| `(memory_[a-z_]+)` \|.*/\1/p' "$ADR")
  ```

  Expected: no output, and the left side has 26 lines.
- **V4, route.** `git diff --name-only origin/main...HEAD -- . ':!docs/plans'` lists the three files of item 1 and the three of item 2, nothing else.
- **V5, acceptance trace.** Every anchor is in the ADR: `for a in …; do grep -qF -- "$a" "$ADR" || echo "missing: $a"; done` prints nothing, for the anchors `need has evolved since then`, `PostToolUseFailure`, `SubagentStart`, `POST /lifecycle/events`, `POST /lifecycle/context`, `byte-for-byte`, `ME-Event-Id`, `ME-Event-Time`, `ME-Event-Seq`, `ME-Harness`, `ME-Client-Version`, `ME-Project-Upstream-Url`, `ME-Project-Origin-Url`, `ME-Project-Root`, `ME-Project-Dev-Root`, `ME-Project-Branch`, `ME-Outbox-Queued`, `ME-Outbox-Parked`, `outbox/rejected/`, `16 MiB`, `~/.config/memory-engine/secrets/token`, `<machine> <sha256-hex>`, `infra D12`, `6 h`, `30 minutes`, `source: resume`, `not guaranteed`, `SHA-256 of the body`, `Episodic`, `yields to interactive queries`, `userinfo`, `CLAUDE_PROJECT_DIR`, `%2F`, `recall(query: string`, `remember(content: string`, `forget(id: integer, reason: string)`, `set_level(id: integer`, `16 lowercase hex`, `unit stopped`, `in-server schedule`, `uptake`, `systemMessage`, `additionalContext`, `9,000 bytes`, `500 ms`, `1,000 ms`, `--max-time 2.5`. `grep -n 'D11' "$ADR"` prints nothing. The PR body maps each ticket criterion to its section.
- **V6, token vector.** `printf 'me_%032d' 0 | sha256sum` prints the digest the ADR states.
- **V7, every hook is placed.** `$HOOKS_REF` is the saved copy of the hooks reference the ADR cites (fetched 2026-11-16).

  ```sh
  comm -23 <(sed -n '/^| Event | When it fires |/,/^$/p' "$HOOKS_REF" | sed -nE 's/^\| `([A-Za-z]+)` \|.*/\1/p' | sort) \
           <(awk '/^### 11\. /{p=1;next} p&&/^##/{exit} p' "$ADR" | grep -oE '`[A-Z][A-Za-z]+`' | tr -d '`' | sort -u)
  ```

  Expected: no output; the left side has 33 names.
- **Merge gate** (fleet brief): every command of `AGENTS.md`'s Verify block, the fuzz build included, unpiped, each exiting 0. A command that cannot run is reported INCOMPLETE, not skipped. The MSRV job is not run: `AGENTS.md` asks for it only when let-chains or edition-sensitive code change.
- **After `/finish-pr` archives the plan:** `git ls-files docs/plans` prints nothing.
- **If #1015 merges first:** rebase; `docs/design/adr/index.md` lists 0019, 0020 and 0021 in order; §7 agrees with ADR-0021; V1, V3 and V5 pass again.
