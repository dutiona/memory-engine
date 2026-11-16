# ADR-0020: Harness-Neutral Lifecycle API, Its Contracts and the Lean Tool Map

**Status:** Accepted. Merging this record is the owner's approval of its contracts and of the tool map (#1014).
**Date:** 2026-11-16
**Parent:** epic #953, umbrella dutiona/infra#37
**Amends:** ME-D6 (#953), ADR-0001
**Related:** ADR-0003 (bi-temporal model), ADR-0007 (scope tree), ADR-0009 (session bootstrap), ADR-0015 (embedding identity), ADR-0021 (scope model, #1015)

## Context

memory-engine becomes an always-on service that every machine of one owner talks to. A Claude Code plugin captures each session's lifecycle events and reads a context block at session start, and the model reaches memory through a small MCP surface. Today the engine speaks MCP over stdio only, has no authentication and no lifecycle endpoint, and exposes 26 tools.

Many tickets build against the same contracts in parallel: the plugin (#1019, #1028, #1030), the event and context routes (#1025, #1027), Bearer authentication (#1018, dutiona/knowledge-base#571), the project key (#1022), the lean surface (#1020, #1023), redaction (#1024), the degraded embedder (#1021), backfill (#1026) and the recall log (#1029). This record fixes those contracts once. A merged ADR wins over any restatement in a ticket.

The rules below that are marked "owner's decision" were taken in the umbrella dutiona/infra#37 and are recorded here as rules of this ADR. Everything else is decided here.

### The ME-D6 amendment

The ME epic (#953) locked this decision, ME-D6:

> this is research-artifact dogfooding, not productization — resist generalizing the harness wiring for hypothetical other consumers.

This ADR amends it. The lifecycle API is harness-neutral: one neutral core, with one server-side adapter per harness. Michaël's reason, given when the umbrella was defined: the "need has evolved since then". Other harnesses (Codex, agy, OpenCode, Cline, dsh) are expected to plug in later, and harness-specific logic kept on the server means they need no client-side logic.

What survives of ME-D6 is its warning against abstractions with one user. Only Claude Code is implemented, and the adapter boundary is a `match`, not a trait (§1).

### Sources

Facts about Claude Code come from its hooks reference, <https://code.claude.com/docs/en/hooks>, read on 2026-11-16 with Claude Code 2.1.325 installed. A few facts were observed on that version in a headless session with a probe plugin, because the documentation does not settle them; each is marked "observed" where it is used (§3, §12, Alternatives considered).

## Decision

### 1. Layers

Four layers, each with one job:

| Layer | Where | Job |
|---|---|---|
| Client | the harness's plugin, on each machine | Queue each event locally, deliver it, read the context at session start. It never interprets an event |
| Wire | HTTP | A neutral envelope (headers) around the harness's own bytes (body) |
| Adapter | server crate, one per harness | Turn one harness body into one neutral record; render the context in the harness's own output format |
| Core | engine crate | Apply neutral records: store, order, segment, project, recall |

- **Native in, native out.** The request body is the harness's hook input, byte-for-byte. The context response is the harness's own hook output, rendered by its adapter. Only the headers are neutral. A client therefore never parses JSON and never escapes a string into JSON.
- **The core knows no tool by name and no harness field by name.** A tool's name is data the core stores, never a condition in its code. That is the layering `crates/memory-engine-mcp/src/activity_policy.rs` was written for, and a review can grep for it.
- **One adapter per harness, chosen by a `match` on `ME-Harness`.** There is no adapter trait until a second adapter exists.
- **§2 to §10 are harness-neutral.** A second harness adds a sibling of §11 and §12 and edits nothing above them. If it would need to, the boundary is wrong and this ADR is amended first. Where §3 and §10 show a Claude Code value, they say so and point to §11 or §12.

### 2. Neutral core

A neutral record has:

- **A kind**, one of six: `session_start`, `prompt`, `tool_use`, `stop`, `compact`, `session_end`.
- **An actor**: `main`, or `subagent` with its ID and type. Segment boundaries and recallable memories belong to the main agent.
- **An outcome**, where the kind has one: `success`, `failure` or `interrupted` for a `tool_use`; `completed` or `api_error` for a `stop`.
- **The kind's own content**: the session's start source; the prompt; the tool name with excerpts of its input and result; the final message; the compaction summary; the reason a session ended.
- **Provenance**: the machine (from the token), the harness, the client version, the session ID, the agent, the branch, the working directory the harness reported, the project root and the project key. Provenance is what lets ADR-0021 re-file history later without capturing it again.

### 3. Wire contract

#### Routes

| Route | Use |
|---|---|
| `POST /lifecycle/events` | Record one event |
| `POST /lifecycle/context` | Read the context for the lifecycle event in the body. Today it serves a session start only |

- The read is a POST because it carries the hook input as its body.
- **Requesting the context is a read, separate from recording the event** (owner's decision). The context route never records the event; the event arrives through the client's queue. The only thing the read writes is its recall-log entry (§9), keyed by the machine and the event ID; the first read wins, and its entry is never replaced. A client reads once per session-start event and never retries. A second read for the same event is answered with the status line alone and no block, so that no session can receive a block the log does not hold.
- Both routes sit behind the same Bearer layer as the MCP route (#1018). No route is unauthenticated.
- There is no version in the path and no version header. A later incompatible envelope adds an `ME-Envelope-Version` header, and its absence means this ADR's envelope. A version in the path would be wrong: queued events outlive a plugin upgrade, so a version has to travel with the event.
- The backfill upload is a third authenticated route. Its path, limits and statuses belong to #1026; this ADR only requires the same Bearer layer and the same redaction.

#### Envelope

| Header | Required | Value |
|---|---|---|
| `ME-Event-Id` | yes | `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`. Opaque to the server, unique per machine for ever |
| `ME-Event-Time` | yes | `YYYY-MM-DDTHH:MM:SSZ`, UTC, second resolution |
| `ME-Event-Seq` | yes | 1 to 6 decimal digits, leading zeros allowed: the tie-break within one second (§5) |
| `ME-Harness` | yes | `[a-z0-9-]{1,32}`. `claude-code` today. It selects the adapter |
| `ME-Client-Version` | yes | The plugin's version, `[0-9A-Za-z.+-]{1,64}` |
| `ME-Project-Root` | no | The main working tree's root for a git checkout, so that linked worktrees share it; otherwise the session's project directory |
| `ME-Project-Dev-Root` | no | The machine's dev root, from the plugin's configuration |
| `ME-Project-Upstream-Url` | no | The `upstream` remote's URL as `git remote get-url` prints it, with the user part removed |
| `ME-Project-Origin-Url` | no | The `origin` remote's URL, read the same way |
| `ME-Project-Branch` | no | The checked-out branch of the project directory; absent when detached. Provenance only |
| `ME-Outbox-Queued` | no, context read only | 1 to 6 decimal digits: the events waiting in the client's queue, the read's own event excluded |
| `ME-Outbox-Parked` | no, context read only | 1 to 6 decimal digits: the events in the client's `outbox/rejected/` |
| `ME-Event-Outcome` | response, on every 204 | `stored`, `duplicate` or `quarantined` |

- **The time format** is what `date -u +%Y-%m-%dT%H:%M:%SZ` prints on both GNU and BSD. BSD `date` has no `%N`, so a client timestamp has second resolution and `ME-Event-Seq` breaks ties.
- **The version is recorded as what it is.** No hook input field and no documented environment variable carries Claude Code's own version, and the client never parses JSON. The client therefore sends its own version, under a header named for it. The server stores `client_version` as sent, and leaves `harness_version` empty until an adapter can read one from a body.
- **No header names the machine.** Identity comes from the token (§4).
- **A fact that does not exist has no header**: no such remote, not a git tree, a detached head.
- **`ME-Event-Outcome`** makes "stored once" and "quarantined" observable at the HTTP seam the tests drive. A client ignores it.
- **The two outbox counts are optional.** An absent or malformed count is read as zero; it never fails the read.
- **`git remote get-url`** is the one way to read a remote: it applies the user's URL rewrites, so every channel sends the same URL for one checkout.
- **Limits.** Each header value is at most 4,096 bytes. The server reads the body as raw bytes and ignores `Content-Type`.

#### Encoding of the project facts

The five `ME-Project-*` values are percent-encoded on both channels, the hooks (through `curl`) and MCP (through Claude Code's `fetch`), so a non-ASCII path gives one key on both.

- A client must encode every byte outside printable ASCII (0x21 to 0x7E), and each of `%`, `"` and `\`, as `%HH`. It may encode any other byte.
- The server decodes every `%HH`, in either hex case. Decoding is canonical, so two clients that encode differently still give one key. Encoded values need no JSON escaping, and a line break cannot inject a header.
- Encoding needs only the shell: one character at a time by parameter expansion under `LC_ALL=C`, then the `printf` builtin. This was measured on dash, bash and busybox sh.
- **A bad fact is dropped, not the event.** Facts are optional. A fact is ignored, and flagged in the event's provenance, when it holds a raw byte outside 0x21 to 0x7E, holds a `%` that two hex digits do not follow, does not decode to valid UTF-8, decodes to a control character, or exceeds its limit; the event is stored and filed without it. Rejecting the event instead would park every event of a project whose path is legal but odd, and a replay would fail the same way.
- **A project directory whose path holds a line break yields no project fact, on either channel.** The one exception is the forged delimiter of the Claude Code MCP helper (§12).
- **Userinfo never leaves the machine.** Before it queues an event, the client removes everything up to and including the last `@` in front of the host, in both forms a remote takes: `scheme://user:secret@host/path` (a bare token before the `@` included) and `user@host:path`. The server strips again before it stores or logs anything. Stored facts never hold userinfo.
- **The dev root is plain ASCII.** It is sent as configured, and it must be an absolute path of printable ASCII without space, `"`, `%` or `\`, so that its encoded form is itself. The server ignores any other value, on both channels alike. The reason is Claude Code's MCP channel (§12): a plugin's helper cannot read the plugin's configuration, so there the dev root travels in a static header, which nothing can encode. A static header was observed to send a non-ASCII character as one Latin-1 byte, not as UTF-8.

#### The MCP channel

A harness can also give the model its tools over MCP. Those requests carry the same project facts, under the same header names and with the same encoding, so that one project has one key on both channels. How the Claude Code plugin does it is in §12.

#### Statuses

| Status | The server sends it when | The client |
|---|---|---|
| 204 | The event is committed: stored, a duplicate (same key, same body), or quarantined | removes the event |
| 200 | Context route only: the hook-output JSON of §10 | prints the body |
| 400 | A required header is missing or off its grammar | parks the event |
| 401 | No token, a malformed one, or one whose hash is not in the file | holds everything and reports it |
| 403 | Never sent by the server; an intermediary may send it | holds everything and reports it |
| 408 | The body stalls: no byte for 60 s | retries |
| 409 | The key exists with another body | parks the event |
| 413 | The body exceeds 16 MiB (16,777,216 bytes) | parks the event |
| 422 | The server cannot apply this event: an unknown `ME-Harness`, an event its adapter cannot map (for Claude Code: an unknown `hook_event_name`, no `session_id`), any failure while applying it, an unexpected error included; or a context read whose body is not a session start | parks the event |
| 429 | Never sent today | retries |
| 5xx | A fault that does not depend on the event: storage unavailable or busy, shutting down | retries |
| anything else | Never sent by these routes: a wrong URL, an older server, an intermediary. That covers 404, 405, any other 4xx, any 3xx and any other 2xx | holds everything and reports it |

- **The table is total.** 204 removes; 400, 409, 413 and 422 park; 408, 429, 5xx and a connection error retry; everything else holds. To park is to move the event to `outbox/rejected/`, where it is kept, never deleted.
- **Why a wrong endpoint holds.** A 404 means the request never reached the lifecycle API, so nothing was judged about the event. Parking on it would empty the queue into `outbox/rejected/` one event at a time and need a manual replay; holding delivers by itself once the cause is fixed. A 422 is the API's judgement on one event, so that event parks and the queue goes on.
- **Why only a 204 removes.** A 200 from something that is not this server must not delete an event.
- **16 MiB** is well above the largest hook input expected (a tool call that writes a large file) and bounds what one request holds in memory.
- **No event-specific fault ever gets a 5xx.** A retry stops the queue (§12), so an event that fails every time must not hold every later one. An error the server cannot classify while it applies one event is answered 422, never left to become a 500. A 5xx always means "the same request will work later".
- **409** turns an event-ID collision into a parked event. Without it, a reused ID would be acknowledged as a duplicate and deleted.
- **Order of the checks, on the event route.** The server checks the envelope (400), then the size (413), then that it knows the harness (422), then the key (a duplicate, or 409), then parses the body (quarantine), then maps and applies it (422). A second upload of a quarantined body is therefore a `duplicate`, and a reused ID with a body that does not parse is a 409, never a quarantine. The context route runs the envelope, size and harness checks, then answers 200 or 422; it never answers `duplicate` and never quarantines.
- **To hold** is to stop the flush and leave every event queued. The user learns of it at the next session start, from the status line (§10); a capture hook itself prints nothing.
- **On the context route no answer moves or removes a queued event.** The "client" column above is the event route's. On the context route the client prints the body of a 200 whose media type is `application/json`, with or without parameters, and otherwise prints its fallback line (§10): `token rejected` for a 401 or a 403, `unreachable` when there is no answer, `error` and the status for anything else. A session-start hook's plain output goes into the model's context, so nothing else is ever printed.
- **The client never follows a redirect.**

#### Quarantine

A well-formed envelope from a known harness whose body its adapter cannot parse (for Claude Code: not UTF-8, not JSON, or not a JSON object) is quarantined and answered 204, so it never loops in a queue. A quarantine record is metadata only: the machine, the event ID, the client time as sent, the time of receipt, the harness, the byte size, the SHA-256 of the body, and the parse error as a class with its line and column. It never holds the raw body, and never the parser's message, which can quote the body: parsing failed before redaction or stubbing could run.

Valid JSON that the adapter cannot map is not quarantined. It gets a 422 and parks, so the event stays whole on the client and can be replayed after a server upgrade. For the same reason the server is deployed before the plugin.

### 4. Machine tokens

- **One token per machine**, and the machine's identity is taken from the token. The same token serves memory-engine and knowledge-base.
- **Shape.** `me_` followed by 32 random base62 characters (`[0-9A-Za-z]`, from a CSPRNG, about 190 bits). Redaction matches exactly that shape: those 35 characters with no `[0-9A-Za-z_]` on either side. The bare prefix would also match code such as `me_types`.
- **Client file.** `~/.config/memory-engine/secrets/token`, mode 0600, holding the token and at most one trailing line feed. The request header is exactly `Authorization: Bearer` followed by one space and the token.
- **Check.** A presented value off the token shape gets a 401 without being hashed. Otherwise its hash is compared in constant time.

#### The server-side token-hash file

Both servers implement this format; infra D12 only references it.

- One `<machine> <sha256-hex>` line per machine, each ended by a line feed, the last one included.
- `<machine>` is a DNS label: `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`.
- `<sha256-hex>` is 64 lowercase hex digits: the SHA-256 of the token's 35 ASCII bytes, with no line feed.
- Exactly one space between the two. No comment, no blank line, no carriage return, no other whitespace.
- **Fail closed.** A missing or unreadable file, a malformed line, a repeated machine or a repeated hash makes the service refuse to start and name the line number. An empty file is valid and admits no one.
- The file is read at startup only; a change needs a restart.

**Test vector.** The token made of `me_` followed by 32 `0` characters hashes to:

```text
f7ca4ebc8eb419b37dea09411716d9aa5506b9e5572f1a5e8b3170e815ee13aa
```

Every implementation must reject a line with uppercase hex, an uppercase machine name, two spaces, a carriage return before the line feed, a last line without a line feed, and a second line for the same machine.

### 5. Delivery and session segments

#### Delivery (owner's decision)

> Delivery is at-least-once, and the server applies each event idempotently. The event's ID and its effects commit in one transaction, and the server acknowledges only after the commit.

- **Idempotency key.** `(machine, event ID)`. It commits with the SHA-256 of the body, in the transaction that commits the event's effects. The server answers 204 only after that commit.
- **Restore.** Restoring a backup can lose up to 6 h of events the server had already acknowledged, because their copies in the client queues are gone.

#### Order

- The server orders a session's events by the order key `(ME-Event-Time, ME-Event-Seq as a number, ME-Event-Id)`.
- It never compares a client clock with its own, and never rejects a time for skew.
- **Seq** is unique among one machine's events that share an `ME-Event-Time`, and increases in the order the events reserved it. Events produced one after another keep their order, which is every case where order matters. Events produced at the same time (parallel tool calls, subagents) get an arbitrary order among themselves. Seqs need not be contiguous.
- A precedence of kinds would not work as a tie-break. A non-interactive run emits its last stop and its session end in the same second, and an exit followed by a resume in the same second has no correct kind order.

#### Delivery order is best effort

Correctness never depends on the order of arrival. A duplicate upload is answered `duplicate`, the server orders by key whatever the arrival order, and the rules below define an event that arrives after its segment ended. The client rules of §12 keep order in the normal case; a rare failure of one costs a late event, never a lost one.

#### Session segments

A session's events are grouped into segments, because a session can stop and start again under the same ID (a resume keeps the ID), and because a killed harness sends no session end.

All comparisons below use the order key, so two events in one second compare by seq. `L` is the session's latest segment, the last one appended. The idle threshold is 30 minutes.

1. **Lateness is decided first.** An event is late when its key is before `L`'s start, or at or before `L`'s end once `L` has ended. A late event is stored and has its own effects: a late final message still becomes a memory. It changes no segment and re-runs no derived work. Segments are only appended: an event older than `L` never creates one, and a late session start with `source: resume` opens nothing.
2. **An event that is not late**:
   - with no segment yet, opens the first one;
   - if `L` has ended, opens a new one;
   - if `L` is open, the event comes after `L`'s last event, and it is a main-agent session start whose source is `startup`, `resume`, `clear` or `fork`: ends `L` at its last event and opens a new one;
   - if `L` is open and the event's client time is more than 30 minutes after `L`'s last event: ends `L` there and opens a new one;
   - otherwise joins `L`. That includes an event that arrives out of order inside `L`'s range.
3. **A session end ends the segment it is in**, whatever its reason, whether it joined that segment or opened it. The end is the segment's last key.
4. **Idle sweep.** When the server has received no event of a session for 30 minutes, it ends `L` at its last event. This is the only rule that reads the server's clock, and it compares it with the server's own times of receipt.
5. **Derived work runs once per ended segment.** In this epic that is one checkpoint (the segment's bounds, how it ended, counts per kind, and the fact made from its last main-agent final message) and the uptake rows of §9. A segment with no main-agent prompt and no main-agent stop gets neither. Consolidation is off (§8).

One segment's life. A later event never reopens it; it opens the next segment.

```{mermaid}
stateDiagram-v2
    [*] --> Open: an event opens the segment
    Open --> Open: an event joins
    Open --> Ended: session end
    Open --> Ended: idle sweep
    Open --> Ended: an event opens the next segment
    Ended --> Ended: a late event is stored
    Ended --> [*]
```

- A session start with source `compact`, and every subagent event, are not boundaries by their kind. They follow rules 1 and 2 like any other event.
- Lateness is rare by construction: delivery follows key order, except for a replayed parked event or a failure of the client's flush lock.
- The checkpoint of an ended segment describes it as it stood when it ended. A late event does not rewrite it. A late completed stop still gets its own uptake row.
- **Accepted.** A machine that goes offline for more than 30 minutes in the middle of a session has that session split at the gap, and so does a long silence such as a 40-minute build. A split costs one more checkpoint and loses no captured content. With 30 minutes, a killed session is finalized within the half hour.

### 6. Capture contract

#### The promise (owner's decision)

> Kept in full, after redaction: prompts, the agent's final messages, compaction summaries. Truncated to 512 characters, after redaction: tool inputs and tool results. The promise: known secret formats are redacted, and events that touch sensitive files become stubs. Arbitrary secrets such as unprefixed passwords are not guaranteed to be caught.

Some activity is not captured at all, and the promise says nothing about it: a tool call rejected or cancelled before it runs, a tool call whose capture hook the user cancels, and a turn the user interrupts (§11).

#### Rules

- **Order of steps.** Read the call ID (§8), redact, then truncate. Truncating first could cut a secret below its pattern.
- **The cap is 512 bytes.** Tool input, tool result and tool error are each cut to 512 bytes of UTF-8, back to a character boundary. That is at most 512 characters, and it is what `truncate()` in `crates/memory-engine/src/engine/activity.rs` already does.
- **A sensitive-path stub** keeps the tool, the path, the outcome and the time, plus the event's provenance. The path is the matched path only; for a shell command, the matched token and never the command. Nothing of the input, the result or the error is kept.
- **What becomes recallable.** A main-agent `stop` with outcome `completed` and a non-empty final message, and a main-agent `compact`, each become one Episodic fact under the event's project (or the machine's bucket, §7). Full-text search finds it at once; vector search finds it once it is embedded (#1021). A stop that a hook then continues still counts: it was a final message. The fact is valid from the event's client time, or from its receipt if that is earlier, so that a fast client clock cannot schedule a memory in the future (ADR-0003).
- **What stays in the event log only.** Prompts, tool events, subagent final messages and stops with outcome `api_error`. Facts the model remembers itself are stored as before.
- **The embedding drain is rate-limited and yields to interactive queries.** The rate is set from the embedder's measured throughput (#1021).

#### Relation to ADR-0001

ADR-0001 says: "Facts are consumer-derived via explicit `add_fact()` calls, not auto-projected from events." The two projections above are a narrow exception. They are rule-based, they are the only ones, and delivery forces them: the event and its effects commit in one transaction. ADR-0001 carries an "Amended by" line that points here.

### 7. Project key and minimal scope contract

ADR-0021 (#1015) defines the full scope model and replaces this section. This is the minimum the first release needs.

#### Project key (owner's decision, refined)

The server derives the key from the facts the client sends.

- **The key is the normalized `upstream` remote, else the normalized `origin`, else the project root's path relative to the machine's dev root, else nothing.** Preferring `upstream` keeps a fork and its upstream on one key.
- **Normalizing** drops the scheme, the userinfo, the port, a trailing `/` and `.git`, lowercases the host, and reads `host:path` as `host/path`. The ssh and https forms of a remote give the same key, for example `github.com/example/project`.
- A local-path or `file:` remote is ignored: it would give a per-machine key.
- **Path key.** The root must sit strictly below the dev root, at a `/` boundary, with no `.` or `..` segment. A root equal to the dev root gives no key. Paths are POSIX only.

#### Scopes

ADR-0007 splits a scope path on `/`. A key used as is would therefore nest: project `notes` would read project `notes/2026`. A project is one scope node.

| Level | Scope path |
|---|---|
| Project | `user:<user>/project:<label>` |
| User | `user:<user>` |
| No project | `unscoped:<machine>`, a top-level node |

- `<label>` is the key with every byte outside `A-Z a-z 0-9 - . _ ~` percent-encoded in uppercase hex. The key above becomes `project:github.com%2Fexample%2Fproject`.
- `<user>` is a server setting.
- When the segment `project:<label>` exceeds 256 bytes, the scope tree's limit, there is no key.
- No query rooted at the user can reach an `unscoped` bucket.

#### Writes and reads (owner's decision)

- **"The session's project" is always the project of the request at hand.** The key is derived from the facts that event or that MCP request carries. It is never looked up from what an earlier event of the session said, so nothing depends on the session start having arrived.
- **Writes** go to the session's project. They go to user level only when the agent asks for it explicitly (§8).
- **No key derived:** the memory goes to that machine's `unscoped` bucket, which is never injected into a project.
- **Reads.** `recall` and the context block read the session's project and the user level, exactly. A session with no key reads its machine's bucket and the user level. Another project is reachable only through `recall`'s `project` argument. It swaps the project: the read is then the named project and the user level, not the session's own project, and it never reaches a bucket. Nothing reads the root scope.

Every event keeps its decoded facts, userinfo removed, so ADR-0021 can re-file history without capturing it again.

### 8. Lean tool map

#### The fate of today's 26 tools

In the order of `crates/memory-engine-mcp/src/tools/definitions.rs`. Every one leaves the MCP surface.

| # | Tool | Fate | Where it goes |
|---|---|---|---|
| 1 | `memory_ingest` | event endpoint | `POST /lifecycle/events`. Raw event appends leave the network |
| 2 | `memory_add_fact` | model tool | `remember`. The model sets no vector, scope path, pin or validity |
| 3 | `memory_query` | model tool | `recall` |
| 4 | `memory_resume_context` | event endpoint | `POST /lifecycle/context` builds the block |
| 5 | `memory_list_due` | removal | No lean tool schedules a fact |
| 6 | `memory_next_due_time` | removal | Nothing polls a timer over HTTP |
| 7 | `memory_explain_fact` | maintenance CLI | `explain`, which exists |
| 8 | `memory_get_fact` | maintenance CLI | `inspect`, which exists |
| 9 | `memory_statistics` | maintenance CLI | `stats`, which exists. Live health is `status` (#1021) |
| 10 | `memory_flush_insights` | in-server job, off | The follow-up decides whether it returns |
| 11 | `memory_consolidate` | in-server job, off | The follow-up. The `consolidate` command stays |
| 12 | `memory_forget` | maintenance CLI | `prune` (#1020). The word "forget" passes to the retraction |
| 13 | `memory_dump_state` | maintenance CLI | `export`, which exists. No tool writes a path over the network |
| 14 | `memory_pin_fact` | removal | No successor in this epic; pinning returns with the admin dashboard |
| 15 | `memory_unpin_fact` | removal | Same as 14 |
| 16 | `memory_replay_events` | maintenance CLI | `dump events`, which exists |
| 17 | `memory_fact_history` | maintenance CLI | `inspect` prints the four timestamps |
| 18 | `memory_bootstrap_session` | upload route | The authenticated backfill upload (#1026) |
| 19 | `memory_record_outcome` | maintenance CLI | `record-outcome`, which exists |
| 20 | `memory_outcome_counts` | maintenance CLI | `outcome-counts`, which exists |
| 21 | `memory_record_activity` | event endpoint | `tool_use` events |
| 22 | `memory_checkpoint_session` | in-server job, on | One checkpoint per ended segment (§5) |
| 23 | `memory_load_context` | event endpoint | `POST /lifecycle/context` |
| 24 | `memory_dream_cycle` | in-server job, off | The follow-up |
| 25 | `memory_apply_cycle_report` | in-server job, off | The follow-up applies the report in the server |
| 26 | `memory_get_recent_insights` | removal | Its source (row 10) is off; `recall` covers retrieval |

Tally: 2 model tools, 4 to the event endpoint, 1 to the upload route, 9 to the maintenance CLI, 5 in-server jobs (1 on), 5 removals. "Event endpoint" means the two `/lifecycle/` routes. "Upload route" is a sixth fate: backfill goes through the running server, so neither the stopped-unit CLI nor the event endpoint can be its home. Where a tool is removed, the engine method stays.

#### The model's tools

Four tools: `recall`, `remember`, `forget` and `set_level`. The last two are new, not among the 26. Each takes a JSON object and refuses unknown arguments.

```text
recall(query: string, project?: string)
remember(content: string, level?: "project" | "user")
forget(id: integer, reason: string)
set_level(id: integer, level: "project" | "user")
```

- **`recall`** returns up to 10 results. Each carries the fact ID, its kind (remembered, final message or compaction summary), its scope (a project key, `user` or `unscoped`), its date and its machine. An answer built from full-text search alone says so. `project` names another project by its key and is read in place of the session's own (§7). There is no limit argument and no mode: a fixed size keeps the recall log comparable.
- **`remember`** stores a Semantic fact, at `project` level by default. It answers with the fact ID, where the memory went and who sees it, for example: `stored 412 in project github.com/example/project, visible to all agents`. When a server-side heuristic finds the placement doubtful (content that reads user-wide filed under a project, or the reverse), it adds one hint line that names `set_level`. It never refuses and never moves the memory itself. The heuristic's rules are not part of this contract.
- **`forget`** retracts a fact (owner's decision): "this fact won't come up again". It ends the fact's validity in the bi-temporal model. The fact leaves recall. A tombstone stops it from coming back through promotion or re-import. The fact row and the raw events stay, and an admin can reverse it. It answers `forgotten`, `already forgotten` or `not found`.
- **`set_level`** changes only a memory's level. `project` means the calling session's project, or its machine's bucket. It never moves a fact between two projects in one call. It answers with the same receipt as `remember`, and reports a fact already at that level as unchanged. The move is an event that records the scope the fact left, so it is auditable, and the opposite call from the same project undoes it. User level is shared: a fact raised to it from one project can be brought down into another by a session of that other project.
- **Write authority.** `forget` and `set_level` act only on a fact in the session's own project (or bucket) or at user level. A fact of another project, even one `recall` returned through its `project` argument, answers `not found`, which does not reveal that it exists.
- **Pruning and deletion are never the agent's.**

#### The call ID

Every model-tool response starts with a call ID, so that the link to its session survives the 512-byte cut of captured tool results.

- A response is a tool result whose first text content starts with the line `call ` followed by 16 lowercase hex digits.
- That includes failures. The four tools return a failure as a tool result, never as a JSON-RPC error, which has no first line.
- The server records each call: its ID, the machine, the tool, the time, and the IDs of the facts returned or changed.
- The adapter links a captured tool event to a call when the first line of its result or error text is a call line, and the server holds that call for the same machine. A forged line matches nothing.
- 16 hex digits stay under every redaction rule (the hex rule needs 32, the entropy rule needs 20 characters), and a random ID cannot repeat after a restore.

#### Maintenance

- Pruning, erasure and reversing a forget are maintenance CLI operations, run with the unit stopped: `prune` (#1020), `erase` and `unforget` (#1023).
- "Refuses while the server holds the store" is one mechanism for both sides. The server holds an exclusive advisory lock on a lock file next to the database for its whole life. Every `memory-engine-cli` command that opens the engine writable takes the same lock or refuses.
- A read-only command (`explain`, `inspect`, `stats`, `query`, `dump`, `outcome-counts`, `status`) takes no lock and may run against the live store: the read-only open never migrates.

#### Derived jobs

The derived jobs (`memory_consolidate`, `memory_dream_cycle`, `memory_flush_insights`, `memory_apply_cycle_report`) are off in this epic: no tool, route or timer runs them. They need a summary-LLM endpoint that this epic does not provision. Until then, automatic capture becomes recallable only through final messages and compaction summaries, and nothing is distilled or deduplicated across sessions.

A follow-up turns them on in the server, never as tools: after a segment ends, for work local to that segment; or on an in-server schedule, for work that spans segments.

### 9. Recall log

Per-prompt recall stays off. Whether to switch it on is decided later, on data that cannot be collected after the fact. The server therefore logs these records from the first session. Each is redacted like every other write.

- **Injection**, one per context read. The session start's machine and event ID, the session ID, the start source, the project key, the server time and the duration. The retrieval settings as run: the mode, the cause of a degraded read, the ADR-0015 embedding space, the caps and the fusion parameters. The candidates, each with its fact ID, its rank, its score per retrieval signal and its fused score. The injected items, each with its fact ID, position and size. The block's size and the status line.
- **Model recall**, one per `recall` call. The call record of §8, the redacted query, the `project` argument, the scope read, the retrieval settings, the results with their ranks, per-signal scores and fused scores, and whether the answer was degraded. The session ID is filled in when the call is linked.
- **Uptake**, written when a segment ends. One row per main-agent completed stop of the segment, naming the session's injection whose session start has the greatest order key below the stop's, among the injections the server holds then. A late completed stop gets its row when it is applied. A row is never rewritten, and it names no injection when none is held.

An uptake row is a link, with no score. Final messages and injected items are kept in full, so the follow-up can score uptake with the method it validates.

An injection records what the server served, not what the session saw. Normally the injection exists before any later event of its session, because the context read writes it inside the session-start hook. The exception is a read the client gave up on: the session never received that block, a row may still name it, and the server's records cannot always tell, since a slow link looks like a normal read.

### 10. SessionStart context and budgets

#### Budgets

| Budget | Value | Past it |
|---|---|---|
| Recall embed time limit | 500 ms, for `recall` and for the context build | Full-text search, flagged as degraded |
| SessionStart server budget | 1,000 ms from request to response | The status line alone, `degraded (context timed out)` |
| Client timeout on the context read | 2.5 s in all, 1 s to connect. For the Claude Code client: `curl --connect-timeout 1 --max-time 2.5` | The client's fallback line |

500 ms is about four times the embedder's measured 95th percentile under load. 1,000 ms leaves the full-text fallback room. 2.5 s adds connection setup over a slow link, and a machine that cannot reach the server gives up after one second.

#### Output

The context route returns the harness's finished hook output, rendered by its adapter, which the client prints as it is. For Claude Code it is this JSON object:

```json
{
  "systemMessage": "memory: OK",
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "memory: OK\n..."
  }
}
```

- `systemMessage` carries the status line. It is the only field Claude Code shows the user.
- `additionalContext` carries the context block, which only the model sees. Its first line repeats the status line, so the model also knows when recall is degraded.
- The block is at most 9,000 bytes of UTF-8. That is at most 9,000 characters however they are counted, under Claude Code's cap of 10,000.
- Every start source gets a block.
- The context read carries the same body and the same header file as the queued session-start event (so the session ID, the start source, the event ID and the project facts), plus the two outbox counts.

#### Status line

One grammar, for the lines the server writes and the lines the client writes:

```text
line  = "memory: " state [ "; outbox " N " queued, " N " parked" ]
state = "OK" / "degraded (" cause [ ", " N " waiting, oldest " age ] ")"
      / "unreachable" / "token rejected" / "no token file" / "error " status
cause = "embedder down" / "embedder slow" / "embedder key rejected" / "context timed out"
age   = N ( "m" / "h" / "d" )
```

- The outbox part appears when either count is not zero. The server folds in the counts the client sent.
- The waiting part appears when the embedding queue is not empty. `OK` never takes it.
- Degraded means that this read fell back to full-text search, or that the embedding queue is paused. It never means merely that items wait.
- `age` uses the largest whole unit.
- **The last four states are made by the client**, when it has no answer to print (§3). The Claude Code client prints a JSON object with `systemMessage` only. It picks one of four fixed templates and substitutes integers only (the status, the counts), so it never has to escape a string.

### 11. Claude Code adapter

#### Hook mapping

The actor is `subagent` when the hook is `SubagentStart` or `SubagentStop`, or when the hook input carries `agent_id`; otherwise `main`. Claude Code's internal agents fire `SubagentStop` too, and this rule keeps their messages out of recall.

| Hook | Kind | Outcome and content |
|---|---|---|
| `SessionStart` | `session_start` | The start source |
| `UserPromptSubmit` | `prompt` | The prompt, in full |
| `SubagentStart` | `prompt` | A subagent turn starts; no text |
| `PostToolUse` | `tool_use` | `success`; excerpts of the input and the result |
| `PostToolUseFailure` | `tool_use` | `failure`, or `interrupted` when its `is_interrupt` field is true; an excerpt of its `error` |
| `Stop` | `stop` | `completed`; the final message, in full; recallable when the actor is main |
| `StopFailure` | `stop` | `api_error`; never recallable, because its message is the error text |
| `SubagentStop` | `stop` | `completed`; the final message, in the event log only |
| `PostCompact` | `compact` | The trigger; the summary, in full; recallable when the actor is main |
| `SessionEnd` | `session_end` | The reason |

`PostToolUseFailure` is captured so that a failed tool call keeps its outcome.

The outcome records which hook fired. When the user interrupts a running tool, the reference describes two cases: the tool finishes and `PostToolUse` runs with ten seconds to complete, or the hooks are cancelled at once. `PostToolUseFailure` does not fire for a cancelled tool. A call whose capture hook was cancelled is not captured.

#### Hooks that are not captured

- **Never registered, because registering one changes Claude Code's behaviour:** `PreCompact` (its output steers the summary), `PreModelSwitch` (a timeout blocks the switch), `WorktreeCreate` and `WorktreeRemove` (they replace git's behaviour), `MessageDisplay` (its output replaces the displayed text).
- **Redundant:** `PreToolUse` and `PostToolBatch` (the two tool hooks carry the call and its outcome), `PermissionRequest` and `PermissionDenied` (a call that runs is captured by the tool hooks), `UserPromptExpansion` (the prompt as typed is captured).
- **No memory content:** `Setup`, `InstructionsLoaded`, `Notification`, `TaskCreated`, `TaskCompleted`, `TeammateIdle`, `ConfigChange`, `CwdChanged`, `DirectoryAdded`, `FileChanged`, `PostModelSwitch`, `Elicitation`, `ElicitationResult`.

#### Other duties of the adapter

- **The activity filter.** The adapter takes it over, with its ignore list and its 300 s duplicate window. It classifies a `tool_use` as ignored, duplicate or recorded, for the activity stream only. Every event is stored whatever its class. Nothing is promoted to a fact: promotion would make tool events recallable, which the capture contract rules out.
- **Path extraction for stubs.** The adapter finds the paths a tool call touches; the core decides whether a path is sensitive (#1024).
- **The call link** of §8 and **the context response** of §10.

### 12. Claude Code client

Only what both sides rely on. The rest belongs to #1019.

- **Utilities.** POSIX `sh`, `mkdir`, `mv`, `rm`, `cat`, `date`, `git` and `curl`, plus the shell's own builtins. The client never parses JSON.
- **Outbox.** `~/.local/state/memory-engine/outbox/`, with directories at mode 0700 and files at 0600: queued bodies are not redacted, and `outbox/rejected/` is kept. An event is the hook input as its body and a header file, published by rename. It leaves the outbox only after a 204.
- **The token** is read from its file at send time. It is never in a queued or parked file, so a rotated token needs no requeue. It never reaches a command line either: `curl` reads the `Authorization` header from its standard input. It is never in the plugin's configuration.
- **Seq and event ID.** With the body already on disk, the client takes the time, then reserves `mkdir <outbox>/seq/<epoch>.<n>` from n = 0 upward: the first success is the seq. The loop is bounded. The event ID is `<epoch>-<seq as 6 digits>-<pid>`, so file names sort in order-key order under a plain glob. One `date` call gives the epoch and the header time, so the two cannot straddle a second.
- **A capture hook never blocks the session.** Every captured hook is synchronous and only writes to the outbox, then starts a detached flush. An asynchronous hook would be killed when a non-interactive run ends, before its write lands. A capture hook prints nothing and, on any failure, exits 0. The session-start hook is the one exception: after it has queued its event it also makes the context read below and prints its result. Every capture hook's registration carries a 5 s timeout; SessionEnd keeps Claude Code's 1.5 s budget, which the timeout of a plugin-provided hook does not raise.
- **One flusher.** A flush runs under the outbox's `mkdir` lock, and a second flush that finds it held exits. The lock records the flusher's PID and a time stamp the flusher renews before each request. It is broken only when that process is gone, when the stamp is older than one hour, or when neither can be read. With these utilities that test is best effort, which §5 makes safe.
- **Flush order.** Oldest first by order key, one request at a time, with the reactions of §3. A park moves the event to `outbox/rejected/` and the flush goes on. A retry or a hold ends the flush at once; the next flush, started by the next hook or by the watcher, begins again from that event.
- **No lost wakeup.** A hook publishes its event before it tries the lock. A flush that ends because the queue is empty releases the lock, then looks once more, and starts again if an event is there. A flush that ended on a retry or a hold does not look again.
- **Transfer limits.** A flush bounds stalls, not total time (`--connect-timeout`, `--speed-limit`, `--speed-time`), so a 16 MiB event on a slow link still gets through; the speed limit is set so that it does within the lock's hour. The detached flush redirects its input and output; otherwise Claude Code waits up to 10 s for the hook.
- **The watcher** is a per-machine timer that runs the flush with no session open (dutiona/my-dotfiles#1911), so an offline machine's events arrive later without another session.
- **The context read.** The session-start hook counts the queue, enqueues its own event, starts the flush, then posts the same body and header file plus the counts, once and with no retry, and prints as §3 and §10 say.

#### The MCP channel

The plugin's `headersHelper` prints `Authorization`, `ME-Harness`, `ME-Client-Version` and the four encoded facts it derives from the project directory (the root, the two remotes, the branch), under the same names as on the event route. The dev root is the fifth fact and travels in a static header (§3). Both channels derive the project facts from `CLAUDE_PROJECT_DIR`, never from the helper's working directory. The hook input's `cwd` is kept only as provenance. `CLAUDE_PROJECT_DIR` does not move when the model runs `cd` or enters a worktree, so both channels keep one project for the whole session.

The project directory reaches the helper as data, on its standard input. The helper command has the form `exec <helper> <<'DELIMITER'`, and the body of that here-document is the `${CLAUDE_PROJECT_DIR}` substitution. Observed on Claude Code 2.1.325:

- `CLAUDE_PROJECT_DIR` is not in a plugin helper's environment;
- the substitution is textual, so a path that holds a `'` breaks a quoted argument, and the helper never runs;
- the here-document delivers a path with a space, a non-ASCII character and a `'` intact.

The helper accepts exactly one line that names an existing directory; otherwise it sends no project fact.

**One accepted residual.** A directory whose name holds a line break followed by the delimiter line ends the here-document early. It cannot run code: with `exec`, the shell is replaced before it reads another line (checked on dash, bash, busybox sh and zsh). It can cut the path short, so that session's MCP calls are filed under the shortened path, which can be another project's. Putting such a directory beside a project needs write access to the dev root, which already allows far more. The owner accepted this residual. The alternative that was not taken is under "Alternatives considered".

## Consequences

### Positive

- Every ticket listed in Context builds against one record, in parallel.
- A second harness adds an adapter and a client; the core, the wire and the tools do not change.
- No event is lost to a session exit, an offline machine or a server restart: the client keeps it until a 204.
- The model's surface shrinks from 26 tools to 4, and none of them prunes or deletes.
- A misconfigured client heals by itself once fixed: a wrong URL or a rejected token holds the queue; it does not empty it.

### Negative

- A session start can wait up to 2.5 s when the server is slow or unreachable.
- A session is split by a silence or an outage longer than 30 minutes.
- A server fault that hits one machine's oldest event holds that machine's queue. It shows as a queued count that stays above zero at session start.
- A quarantined body cannot be recovered from the server. The transcript on the machine still has it.
- Restoring a backup loses up to 6 h of acknowledged events. Fact IDs issued since the backup are issued again to other facts, so an ID a model still holds from that window can name another fact.
- Nothing is distilled or deduplicated across sessions while the derived jobs are off.
- Untrusted text can reach a final message, become a memory and come back in every later block. That is why the context block frames its items as recalled claims with their provenance, not as instructions (#1027).
- The stores start empty. Nothing filed at the root scope, or under a scope path that does not follow §7, is read.
- The MCP channel carries the accepted residual of §12.

### Revisit when

- a second harness joins;
- Claude Code's hook input starts to carry its version, or its helper starts to receive the project directory in its environment;
- ADR-0021 merges and replaces §7.

## Alternatives considered

- **Claude Code's `http` hook type**, which posts the hook input by itself. It is not supported on SessionStart and has no offline queue: an unreachable server would lose events.
- **Asynchronous capture hooks.** They are killed when a non-interactive run ends, before their write lands.
- **Mapping kinds or parsing JSON on the client.** Every harness would then carry its own logic, and a shell client cannot parse JSON.
- **A version in the route path.** Queued events outlive a plugin upgrade.
- **A tie-break by kind, or by arrival.** Neither orders an exit and a resume in the same second.
- **Nested scope labels**, the project key used as a scope path. Projects whose keys share a prefix would read each other.
- **Storing an unparseable body raw.** Redaction never ran on it.
- **202 Accepted.** It invites acknowledging before the commit.
- **The dev root read from a file the hooks write.** It would lift the ASCII restriction, but it races the first connection after an install.
- **The project directory as a quoted argument of the helper.** It breaks on a path that holds a quote.
- **A session-ID header on the MCP channel**, with a server check of each call's project against its session. It would close the residual of §12 on today's Claude Code, provided the server also refuses a call for which it holds no session-start root. The session ID was observed in a plugin helper's environment on 2.1.325; that it equals the hook input's session ID was not checked. It rests on a variable that is not documented for a helper, and it costs a header and a server check.

## References

- Claude Code hooks reference: <https://code.claude.com/docs/en/hooks>, read 2026-11-16.
- Claude Code MCP documentation, for `headersHelper`: <https://code.claude.com/docs/en/mcp>, read 2026-11-16.
- Ticket #1014; epic #953; umbrella dutiona/infra#37.
- ADR-0001, ADR-0003, ADR-0007, ADR-0009, ADR-0015.
