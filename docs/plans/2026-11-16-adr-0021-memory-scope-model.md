# docs(adr): add ADR-0021, the memory scope model amending ADR-0007

Plan for dutiona/memory-engine#1015 (umbrella dutiona/infra#37). A bare `#N` is a dutiona/memory-engine issue. The
PR ships the ADR, the "Amended by" lines in ADR-0007 and ADR-0005, and the index line; implementation is later
tickets. The outline below is the plan's substance: each bullet is a rule the ADR must state, not ADR prose.

## Outcome

`docs/design/adr/0021-memory-scope-model.md` records the full scope model: five axes, a Home tree kept from ADR-0007,
Audiences that only narrow, Locators over opaque project IDs with logged alias operations, three Portable-path tokens,
write-time placement with a Placement receipt, a deterministic recall procedure that filters before ranking,
fail-closed states, the meet rule for derived memories, a row-by-row proof that ADR-0020's minimal scope contract is a
strict subset (no first-release row rewritten), re-filing from the ADR-0001 log without re-capture, the contract
knowledge-base shares, and numbered conformance cases. ADR-0007 and ADR-0005 carry "Amended by" lines, the ADR index
lists 0021, the docs build gains no warning, and knowledge-base#513 has a comment linking the ADR. Merging is
Michaël's approval.

## Constraints

Issue acceptance (verbatim):
- "It covers the axes (user, project, machine, agent, session), the tree plus tags, visibility rules, who chooses a memory's level at write time, a fail-closed default, and project aliases."
- "It states how ADR-0020's minimal contract evolves into it, re-filing history from the ADR-0001 event log without re-capture."
- "ADR-0007 gains an "Amended by ADR-0021" line, and KB #513 gets a comment linking it."

Settled in the 2026-11-16 brainstorm with Michaël (`~/.local/state/brainstorm/2026-11-16-me-scope-model/brief.md`,
"Decisions so far", verbatim; never reopened):
- Home + audience model; tags descriptive only — Michaël: findings 1–3 "resonate ... echo to real problems we've had".
- Opaque project IDs with machine-qualified locators and recorded resolution evidence — same.
- Portable paths: tokenize inside project/home, machine-restrict the rest; agents re-interpret when replacement fails — "a + c ... we'll need to rely on each agent wit".
- Write level: file as asked + receipt + hint; a tool changes only the level (project ⇄ user) by the returned ID — Michaël, option b.
- Agent identity = lane; lane + role + audience when unsure — Michaël.
- The project registry is a separate crate inside ME, built to be extracted; KB asks ME. A standalone identity service later (memory-engine#1034, low priority) — Michaël, option a.
- Non-git aliases: the agent proposes a best-effort match, a human confirms; no automated matcher — Michaël.
- Names Home / Audience / Locator / Portable path / Placement receipt — Michaël.
- Defaulted in the ADR, for review in the PR: a summary's audience is the intersection of its sources' audiences; re-filing replays locator resolution over the ADR-0001 log, keeps the old scope as evidence, and diffs visibility before switching; fail-closed states (no project → unscoped bucket; unknown alias → error; global only on explicit request); alias add, merge and correct are distinct, logged operations; KB shares the names, the audience rules and conformance cases, not ME's row IDs.

Open questions this plan settles (brief, verbatim): "How ADR-0020's minimal contract (decision 18) maps onto
Home/Audience, so the first release is a strict subset"; "Session-level audiences: kept as a possible audience
dimension, unused by default"; "Exact token set for portable paths beyond `${PROJECT_ROOT}` and `${HOME}` (e.g. the
dev root)".

ADR-0020's scope rules as planned (its lane's plan, §I and §J, branch `docs/1014-adr-0020`, not yet on `main`):
project Home `user:<user>/project:<label>`, the label the key with every byte outside `A-Z a-z 0-9 - . _ ~`
percent-encoded, and no key when the segment exceeds 256 bytes; user level `user:<user>`; bucket `unscoped:<machine>`,
top level; reads are the session's project and the user level exactly, a keyless session reads its bucket and the
user level, `recall`'s `project` swaps the project and never reaches a bucket, nothing reads root;
`remember(content, level?)` with `project` or `user` answers with the ID, the Home and who sees it, plus a hint naming
`set_level`; `set_level(id, level)` changes only the level and never moves a fact between projects; `forget` and
`set_level` act only on a fact of the session's own project, bucket or user level ("A fact of another project, even
one `recall(project: …)` returned, answers `not found`"); every event keeps its decoded project facts. Amended by owner ruling 1 (below): remote-less keys are machine-qualified, so
equal dev-relative paths on two machines are two projects until a human-confirmed `add`. Key form this plan writes
and relays to the ADR-0020 lane: a path key is `<machine>:<relative path>` (for example `laptop:x`; label
`laptop%3Ax`). `<machine>` is ADR-0020's DNS-label machine name, so a path key's first `:` comes before any `/`, right
after the machine; a normalized remote key either starts with `[` (a bracketed IPv6 host, which is not a
DNS label) or has no `:` before its first `/` (port dropped, `host:path` read as `host/path`), though its path may
hold one (`github.com/example/a:b`). A path key therefore
equals neither another machine's path key nor a remote key, and it splits at its first `:`, so a relative path
holding `:` still parses; the 256-byte segment limit applies to the whole label, as before. ADR-0021 adopts whatever form ADR-0020
ships; item 5 checks the label form explicitly.

Fixed for this PR: public repository, so examples use the user `alice`, machines `laptop` (home `/home/alice`) and
`mac` (home `/Users/alice`), dev roots `/home/alice/perso/dev` and `/Users/alice/dev`, remotes under
`github.com/example/`; no real home path, account name, host name, tailnet name, address or token. The memory
services' infra decision is "infra D12"; this ADR cites none. ADR-0020 is cited by number in plain text, linked only
once its file is on `main`. One plan issue, published by the lifecycle. The PR says `Closes #1015` and
`Refs dutiona/infra#37`. Branch commits carry no co-author line.

## Owner rulings at plan approval (2026-11-16)

Michaël, relayed by the orchestrator: "1. yes option B. 2. I agree too. I approve the plan."

**1. Option B: remote-less keys are machine-qualified; no automatic join.** Two machines whose project roots sit at
the same path under their dev roots get two projects, and a remote-less path whose bytes equal a remote key gets its
own project; they become one only through a human-confirmed `add` (the agent may propose it, best effort). Remote
keys stay machine-independent, so remote projects still share automatically. ADR-0020's rule is amended the same way
in its own lane before it ships (the ADR-0020 worker is told to drop the auto-join), so ADR-0021 describes ADR-0020's
rule as amended, and no auto-join exists to migrate. Context kept for the record: the epic spec's decision 17 reason
(verbatim: "Reason: the same project on the laptop and the desktop must share memory, and normalizing on the server
gives every harness one implementation.") is now met for remote-less projects by one confirmed `add` per extra
machine; decision 19 (owner, Q12c) scopes aliases "for one project known under two keys, such as a non-git folder
nested differently on two machines"; the brainstorm decisions "non-git aliases: the agent proposes, a human confirms"
and "machine-qualified locators" hold literally.

**2. ADR-0005 gains an "Amended by ADR-0021" line (item 4)**, accepted. §10 changes how the service places cluster
and global summaries, which ADR-0005's pass 3 frames as "across the entire knowledge base".

## ADR-0021 outline

Normative rules in tables and numbered lists, prose only for the why. No DDL. Tokens and identifiers in backticks;
no `|` inside a table cell. Every decision carries its provenance: (owner, date), (defaulted, for review) or (settled
in this ADR, for review).

**Header.** `# ADR-0021: Memory Scope Model`; `**Status:** Accepted` with the sentence "Merging this record is
Michaël's approval", ADR-0020's convention; `**Date:** 2026-11-16`; `**Amends:** ADR-0007, ADR-0005` (linked);
`**Related:** ADR-0001, ADR-0004, ADR-0009, ADR-0015, ADR-0016, ADR-0017, ADR-0018 (linked), ADR-0020 (plain),
#1015, #1034, knowledge-base#513`.

**Context.** One owner, several machines whose home and dev roots differ, several harnesses and models, work by lane.
Seen: one project under two paths; paths that break on the other machine; facts filed at the wrong level. ADR-0007
gives each entity one `scope_id` on one axis: it cannot say "P, this lane only" or "P, this machine only", has no
machine or agent axis, and ties identity to paths. ADR-0020 ships a minimal contract first. Today's fail-open paths,
each cited by file (re-verified in item 1): `memory_ingest` and `memory_add_fact` with no scope write root
(`None => 1` in `crates/memory-engine-mcp/src/tools/handlers/p0.rs` and `crates/me-ingest/src/lib.rs`); a query with
no scope searches every scope (`resolve_query_scope_ids`, `crates/me-query/src/execute.rs`); resume's pinned tier
lists every scope and its other tiers walk ancestors to root (`crates/memory-engine/src/engine/resume.rs`); the
storage port reads an empty `scope_ids` slice as "all scopes" on seven methods (`crates/me-storage/src/graph.rs`); the
global summary is root-scoped and a cluster summary takes its sources' majority scope
(`docs/advanced/consolidation.md`).

**Scope of the decision.** §3–§12 bind the memory service. Embedded library consumers keep ADR-0007's semantics
through `ScopeQuery` and `DEFAULT 1`.

**§1 Vocabulary.** One sentence each: Home, Audience, Locator, Portable path, Placement receipt; plus Capture
context (immutable provenance of an event or call), Recall context (who asks, from where), Filing decision (an
appended change of Home or Audience), visibility class (a Home with a set of Audiences). "Global" is not used:
user-level means Home `user`; owner-wide recall is its own operation.

**§2 Axes** (table: axis, identity, Home role, Audience role, provenance). user: the owner, `user:<user>`, a server
setting. project: an opaque project ID found through Locators; a Home level, never an Audience dimension. machine:
the name bound to the request's token, never a hostname or path; keys the bucket; an Audience dimension. agent: the
lane, one logical agent across harnesses and models, optionally narrowed by role; harness, version, model and
subagent type are per-turn provenance, and a subagent acts in its parent's lane. session: ADR-0020's session ID with
its segments; provenance; an Audience dimension, unused by default. Closing rule: where a memory came from is not
where it applies.

**§3 Home: the tree** (amends ADR-0007).
- Root (id 1) → `user:<user>` → `project:<label>`; `unscoped:<machine>` at top level, unreachable from any
  user-rooted query. A memory has exactly one Home. The bucket is the only node keyed by a machine and is a leaf;
  sessions, machines and agents are never parents of a Home.
- Root is never a Home and never read by recall, the context block, a derived job or a by-ID operation. Rows left at
  root by `DEFAULT 1` are legacy and unfiled, not user-level. Two writers use root on purpose, and their `scope_id`
  is inert: co-session edges (`CO_SESSION_SCOPE_ID`, `crates/memory-engine/src/engine/graph.rs`), visible only
  through their ends; outcome signals (`crates/memory-engine/src/engine/outcome.rs`), never recalled. Both are
  excluded from the legacy review list and the root-write counter.
- Project ID = the project node's label, opaque, never parsed and never resolved as a Locator. First-release labels
  (ADR-0020's encoded keys) are adopted as IDs as they are; a project created after the registry gets a minted random
  ID within the 256-byte segment limit. Stored path strings (`session_checkpoints.scope_path`) are renderings, never
  visibility inputs.
- "Tree plus tags": the tree is Home; Audience selectors are the only tags that touch visibility, and only narrow;
  descriptive tags (`rust`, `build`), pins and importance never grant or restrict.

**§4 Audience.**
- Dimensions lane, role, machine, session. OR within a dimension, AND across dimensions, OR across a memory's
  Audiences. An Audience has no anchor of its own: it narrows within the Home and never reaches past it.
- Logically every memory has at least one Audience; the default is one unrestricted Audience. Storage may represent
  the default by the absence of rows (so first-release rows need none); a memory marked narrowed whose rows are
  missing is invisible.
- A restricted dimension matches only a Recall context that carries a value in its set; a missing value never matches.
- Adding an Audience widens, so only a logged maintenance operation adds one; tagging, dedup and merge never do.
- Machine comes from the token; lane, role and session are client-declared: a retrieval boundary between cooperating
  agents, not secrecy from the owner or from a database reader.
- Session (question b) and role are defined and matchable; no default write sets them and no model tool offers them
  until usage shows a need. Re-filing uses session to keep legacy `session:` rows as narrow as they were.

**§5 Recall**, as ordered steps:
1. Build the Recall context: owner, machine, lane and role when declared, session when known, and the context Home
   (the session's project, else its machine's bucket). Candidate generation searches the query as written and its
   tokenized form (§9, the reader's roots; no `${PROJECT_ROOT}` under the `project` swap), as a union inside the
   visible set, so tokenized memories and literal-path memories (first-release ones included) are both found; the
   tokenized form is embedded only when tokenizing changed the query, within ADR-0020's recall embed time limit.
2. Candidate Homes = {context Home, `user`}. `project` replaces the context Home for that call with the project
   whose Locator key equals the value byte for byte (ADR-0020's key form, as `recall` reports it: a remote key, or a
   machine-qualified path key such as `laptop:x`, so a key names at most one project); it never reaches a bucket; no
   match is a typed error. A project
   not minted yet (§8: only writes mint) contributes nothing, so the read is `user` only.
3. Every channel generates candidates inside the visible set: FTS, vector with its widening, graph expansion over the
   visible subgraph only (no hop through a hidden node), every bootstrap tier, the archive fallback.
4. Fusion, reranking, limits, counts, explanations and the context block see only those candidates.
5. Expand Portable paths (§9). 6. Log what was served (ADR-0020's recall log).
- Filtering after top-k is forbidden: it starves results and leaks hidden memories to a consumer `Reranker`. This
  binds release 1 for Homes already (ADR-0020's "exactly"); stage 2 extends it to Audiences.
- An edge is visible only when both ends are; its own `scope_id` grants nothing. Recall never returns raw events.
- **By-ID authority** (`forget`, `set_level`): a memory qualifies only if its Home is in the session's own candidate
  set {context Home, `user`}, never the `project` swap, and an Audience matches; otherwise `not found`, the same
  answer as for a missing ID (ADR-0020 §J).
- The visibility predicate is defined once as a reference relation; each storage backend compiles it natively and
  the backend conformance battery checks every backend against the reference on the cases.
- Owner-wide recall (every project and `user`, buckets excluded) is not in ADR-0020's lean map; if added it is a
  separate explicit operation, never a magic `project` value.

**§6 Fail-closed states** (table: state, result, case).

| State | Result |
| --- | --- |
| no Locator for the session | writes go to `unscoped:<machine>`; reads are that bucket and `user` |
| empty scope or selector | no match (`resolve_query_empty_string_is_no_match_not_everything`: ME; KB by #513) |
| unknown key in `project` | typed error, never empty results |
| Recall context lacks a restricted dimension | that Audience does not match |
| narrowing to the caller's own lane or role with none declared | typed error, nothing stored |
| narrowed memory with missing Audience rows | invisible |
| a memory at root | invisible everywhere; listed for review by re-filing (inert root writers excepted) |
| `user` Home | only on explicit intent (`remember`'s level, `set_level`) |
| owner-wide recall, a bucket read from a project session | only through an explicit operation |
| empty Home set reaching storage | no results; "all scopes" is its own explicit selector |
| empty derived meet | no derived memory is produced |
| knowledge-base cannot reach ME's registry | the write fails retryably; never root, never `user` |

**§7 Placement at write time.**
- The agent states intent: a level (`project` default, `user` when explicit, ADR-0020's `remember` vocabulary) plus
  optional narrowing to its own lane or to the writing machine. The server resolves identities and files as asked; it
  never moves a write. A narrowing it cannot apply is a typed error (dropping it would widen). Server-derived memories
  (ADR-0020's final messages and compaction summaries) take the default placement.
- Write-time dedup and reinforcement match only inside one visibility class, never across Audiences.
- Placement receipt = ADR-0020's `remember` answer, extended: call ID, memory ID, Home in words (first Locator key,
  `user`, or `unscoped on <machine>`), Audience in words, the transforms applied (tokens; any machine restriction with
  the path that caused it), and an optional hint.
- Hints are deterministic rules (ADR-0004: no LLM in core), advisory, outside conformance: content that reads
  user-wide filed under a project or the reverse (ADR-0020's heuristic); a user-level episodic memory; a path-caused
  machine restriction; a memory at `user` holding `${PROJECT_ROOT}`; a remote-less project whose relative path (machine part
  aside) another machine's project also holds: an advisory observation of exact equality, never a binding and not a
  matcher; proposing the `add` is the agent's best effort, and nothing joins until a human confirms.
- `set_level(id, level)`, ADR-0020's, unchanged in contract and authority (§5): toggles the Home between the
  session's project (or its bucket) and `user`; keeps the ID, the Audiences and the content (owner: "edit only the
  level"); appends a Filing decision; answers with a receipt.

**§8 Locators and the project registry.**
- A Locator is an ADR-0020 key with its kind and observation: `remote` (the normalized remote, machine-independent;
  upstream-first deliberately groups a fork with its upstream) or `dev-relative` (the project root relative to the
  dev root, recorded with the machine that saw it). ADR-0020's key order is kept exactly: a local-path or `file:`
  remote is ignored as an input, so the next rule applies (origin, then the dev-relative path); only a final key
  ADR-0020 rejects is "no Locator" (a `project:<label>` segment over 256 bytes; a root equal to or outside the dev
  root, or with a `.` or `..` segment, and no usable remote).
- Resolution is a pure function of the event's raw facts, its machine and a registry state, so a replay against a
  recorded state is deterministic. Every binding records its evidence: raw remotes without userinfo, the roots, the
  rule applied, the resolver version, the actor.
- First sight (only write paths mint: the event endpoint and `remember`; a unique binding per Locator serializes two
  concurrent first sights): a `remote` Locator whose key equals a bound `remote` key binds to that project (remote keys are
  machine-independent); a `dev-relative` Locator binds only to its own (machine, path) binding, never to another
  machine's equal path nor to a remote key with the same bytes (owner ruling 1, option B; ADR-0020's rule as
  amended); otherwise the registry mints a project. Joining two remote-less projects is always a human-confirmed
  `add`, which the agent may propose (the receipt hints when another machine holds the same dev-relative path). A mid-session remote change re-resolves only later events.
- Operations, distinct and logged with actor, reason and evidence, run as maintenance CLI commands with the unit
  stopped (ADR-0020's maintenance ruling: "Pruning, erasure and reversing a forget are maintenance CLI operations, run
  with the unit stopped"): `add` (a Locator joins a project), `merge` (two projects become one; the absorbed ID redirects for
  resolution and is never reissued), `correct` (rebind or retire a Locator from now on, and/or move chosen memories).
  `merge`, and `add` of a Locator that already minted a project, run as one §12 re-filing scoped to the absorbed
  project: preview (bound to the log position it was computed at), approval, then one transaction that applies the
  redirect and the memories' Filing decisions together, refused when the absorbed project gained or changed a memory
  after the preview's position (a new preview is then needed). Before that transaction nothing changes (resolution and memories stay as they were); after it, both
  projects' sessions resolve to the survivor and read its memories. No interval leaves a project unreachable. The agent proposes and prints the command; Michaël running it is the confirmation. No
  automated matcher, and no automatic join of remote-less projects.
- Registry crate `me-project-registry` inside ME: it depends on no memory crate and owns a store port that ME's
  backends (SQLite and Postgres) implement inside ME's one database (one writer, one backup); no memory table
  references it except through Home labels; its operations are recorded in the ADR-0001 log under a payload it owns,
  so an extracted service (#1034) can replay them. Its API (resolve, look up, add, merge, correct) is the one #1034
  would serve. KB asks ME.

**§9 Portable paths** (question c). Exactly three tokens, longest prefix first, at path-segment boundaries. Each
token means "the same relative path under this root on the reader's machine", nothing more:
- `${PROJECT_ROOT}`: exactly one prefix per session, the top level of the session's own checkout (a linked
  worktree's own), and only when the client sends that fact; without it, no `${PROJECT_ROOT}` is written and project
  paths fall to `${DEV_ROOT}`. At write time a path outside the session's checkout top level is never mapped onto it,
  even one in another checkout of the same repository: it falls to `${DEV_ROOT}` (or `${HOME}`, or "other"). A
  linked worktree nested under the session's checkout is indistinguishable from a subdirectory and tokenizes as
  `${PROJECT_ROOT}/.worktrees/<name>/…`, a known imprecision the agent re-interprets. At recall it
  expands to the reader's own checkout top level (by design: the reader's checkout of this project), and stays
  literal and flagged when the reader sent no checkout fact. Only in project-Home memories.
- `${DEV_ROOT}`: the machine's dev root, already on ADR-0020's wire. Why: it is exactly where the machines differ
  (`/home/alice/perso/dev` against `/Users/alice/dev`), and `${HOME}` alone breaks every sibling-repository path.
- `${HOME}`: the client's home.
- Rejected: XDG tokens (under `${HOME}`), temp paths (machine-local, so restriction is right), per-project root
  tokens (every project's root on every machine), user-defined tokens (no need shown).
- Any other absolute path stays literal and adds a writing-machine restriction to every Audience; the receipt names
  it. The recognizer's grammar is the implementing ticket's, bound by the cases (a URL or an API route is not a path).
- Tokenizing runs at write, after redaction, on memories only; the event log keeps the original. Escaping is
  invertible: in incoming content, a run of n ≥ 1 `$` immediately before `{PROJECT_ROOT}`, `{DEV_ROOT}` or `{HOME}`
  is stored as n + 1 `$`; a server-inserted token has exactly one, and the server never inserts a token directly
  after a `$` (such a path stays literal, "other"). At recall a single `$` expands, and a run of n + 1 is returned as
  n, so `${HOME}` and `$${HOME}` written by an agent come back as written.
- Recall expands with the reader's values; `${PROJECT_ROOT}` only when the memory's Home is the session's own context
  Home (never through the `project` swap); a token without a value stays literal and is flagged, and the agent
  re-interprets it.
- New wire facts the implementing ticket adds to ADR-0020's envelope: the client's home, its checkout top level, and
  the lane (and role). Until a fact arrives, a path under it counts as "other", which fails closed.

**§10 Derived memories.** The meet is the intersection of reader sets. Reader sets: `user` is read by every context;
project P by contexts whose context Home is P and by `project: P` calls; bucket M by keyless sessions on M. Home meet:
`user` ∧ X = X for any Home X; X ∧ X = X; any two different projects or buckets, or a project and a bucket, have no
meet (no context reads both). Audience meet: pairwise conjunction of the sources' Audiences (restricted sets
intersect, unrestricted imposes nothing), empties dropped. No Home meet or no Audience left: the derived memory is not
produced. Applies to summaries (`summaries.source_fact_ids`), wisdom lineage (`lineage.source_fact_ids`), activity
promotion and consolidation merges. Dedup and conflict resolution stay inside one visibility class; across classes
both memories stand. Cache keys carry the resolved Recall context. ADR-0016 structures: the whitening map stays global
per embedding space (ADR-0016's "Global `W` by default", resting on ADR-0017; a statistic carries no content);
`similarity_knn` edges follow the edge rule; the structures ADR-0016 registers per scope and space (diffusion kernel,
eigenbasis, curvature cache) are keyed by visibility class in the service, so a precomputed structure never spans a
hidden member; graph expansion uses the visible subgraph (§5). The root-scoped global summary and the majority-vote
cluster scope (ADR-0005's pass 3 and pass 2) are superseded for the service; the derived jobs stay off (ADR-0020)
until they follow this section.

**§11 Evolution from ADR-0020's minimal contract** (question a).
- "Strict subset" means: (1) every first-release row keeps its `scope_id`, Audiences (none) and content; (2) every
  read of first-release rows from any context finds the same visible set as under ADR-0020 (ranking may differ), with a registry built only
  by first sight (no `add`, `merge` or `correct`); (3) reaching the full model needs additive schema only (new
  tables, new columns with defaults, no `UPDATE`, no relabel). Responses may gain fields (receipt, hints, flags); new
  writes follow the full model (tokens, restrictions); behaviour ADR-0020 leaves undefined (an unknown `project`
  value) is defined here as an error.
- Mapping table MC-1..MC-9 (columns: ADR-0020 rule, full-model form, data change): project write → Home
  `project:<label>`, unrestricted, its key the first Locator, its label adopted as ID, none; explicit user level →
  Home `user`, none; no key (including every rejected key) → Home `unscoped:<machine>`, the context Home of a keyless
  session, none; reads project + `user` exactly → §5 with only unrestricted Audiences, none; `project` swaps and never
  reaches a bucket → §5 step 2 with byte-exact key match, none; key rules (ADR-0020 as amended by owner ruling 1:
  remote keys machine-independent, remote-less keys machine-qualified) → `remote` and `dev-relative` Locators,
  registry rows only (stage 1 adopts every key observed per node in the log); `remember` answer and `set_level` → §7's receipt and level change, none; write authority
  of `forget` and `set_level` → §5's by-ID authority, none; decoded project facts per event and call → Capture
  context and re-filing input, none.
- Stages, each additive and its own later ticket: (1) registry, adopting each first-release node with its observed
  keys; (2) Audiences, narrowing intents, filter-first extended to Audiences; (3) Portable paths with the new wire
  facts; (4) alias operations and the re-filing tool; (5) knowledge-base adoption (#513).

**§12 Re-filing from the event log.**
- Changes filing only (Home and Audiences), never content, never re-extracts (ADR-0001: facts are consumer-derived).
- Capture context per memory kind: server-derived memories from their lifecycle event; `remember` and `set_level`
  from their call record, which must keep the request's decoded project facts, machine and lane and be written to the
  ADR-0001 log (a requirement on ADR-0020's call record); facts by `source_event_id` otherwise; derived memories by
  `source_fact_ids`; edges by their ends. Read through the schema-evolution upcaster; no upcaster invents a field.
- Procedure: fix a registry state (the fold of its logged operations up to a recorded log position); a memory's
  filing is then a fold, in log order, of its initial placement (re-resolved from its Capture context against that
  state, so a later `correct` of a Locator applies to history, which is its purpose) and its deliberate later decisions
  only: each `set_level`, whose `project` target is re-resolved from its own call record's Capture context (the
  calling session's project, as ADR-0020 defines it); each explicit memory move of a `correct`; and every item of an
  earlier re-filing that exists only by human confirmation (an approved machine restriction, a confirmed legacy-label
  mapping, the hand filing of a row with no Capture context), replayed as recorded, a recorded target project
  resolving through any later `merge` redirect to its survivor. The other items of earlier
  re-filings, which only materialize resolution, are re-derived, never replayed (a `merge` redirect is already in the registry state), so a later
  `correct` is never overridden by an older re-filing and replay never undoes a deliberate `set_level` or move. Build a candidate filing; diff it symbolically per memory (old and new
  Home and Audiences on every dimension, lane, role, machine and session included), rendered also as newly visible
  and newly hidden memories for each context seen in the log; the owner approves; apply as appended Filing decisions
  (memory, previous and new filing, operation, reason, actor, resolver version, registry log position) in one
  transaction. Applying twice changes nothing. The log is never rewritten.
- Limits: replay cannot recover context never recorded. Rows with no Capture context, root rows and legacy nodes are
  never re-filed automatically: they are listed for review; a mapping proposed from a legacy label applies only once a
  human confirms it. First-release memories keep literal paths; the diff may propose a machine restriction for one,
  applied only on approval.
- Proof of §11: replaying first-release history under the full model's defaults gives an empty diff (the re-filing
  tool's first acceptance test).

**§13 Shared with knowledge-base** (MUST wording, as ADR-0015 uses for cross-layer rules). Shared: the names, the
Home kinds and the root rule, §4 and §5's matching rules (an exact Home set, never subtree expansion of `user`),
§6's states, §10's meet (a chunk is never more visible than its document), and the cases marked shared. KB sends ME
the raw project facts, stores the opaque ID it gets back (never a key, path or ME row ID) and, after a `merge`, moves
its documents through its own reviewed filing step. KB's existing corpus is unfiled (root) until a KB-owned filing
step files it. Not shared: ME's schema, the registry internals, ME's re-filing tool, the receipt. One copy: KB cites
this ADR and the case IDs rather than mirroring it, because ME owns the registry KB asks. A change to this section
links a KB issue.

**§14 What this amends.** ADR-0007, for the memory service: `scope_id` is the Home and Audiences decide visibility;
recall uses §5's Home set, not `Ancestors`/`Inherited`; root is never read, so `DEFAULT 1` rows are unfiled, not
shared ("all entities default to root scope" no longer applies); project nodes come from the registry. Unchanged: one
database, the tree and its cache, `ScopeQuery` for library consumers, integer scope IDs. ADR-0005, for the service: pass 3's
integration "across the entire knowledge base" and pass 2's cluster summaries produce summaries only within a §10 meet
(the placement rules in `docs/advanced/consolidation.md` change with the implementing stage).

**Conformance cases** (table: ID, setup, expected, shared ME+KB or ME only; plain IDs `C1`…).
- Homes and fail-closed: C1 default write in P: read in P from every machine, lane and harness; not in Q; not keyless
  (shared). C2 `user` write: read in P, Q and keyless (shared). C3 keyless write on `laptop`: Home `unscoped:laptop`;
  read by keyless laptop sessions; never by a project session, through `project`, or on `mac` (shared). C4 empty
  selector: no match (shared). C5 unknown key in `project`: error; a bare `x` when only `laptop:x` exists: error (shared).
  C6 `project: Q` from P: Q and `user`, nothing of P (shared). C7 a legacy root row is never read by recall, the
  context block or a by-ID operation; a co-session edge between two visible memories is visible (shared). C8 a
  descriptive tag never widens (shared). C9 a pinned memory of Q never appears in P's context block (ME).
- Audiences: C10 Home P, lane L: read by L on any machine; not by L2; not with no lane (shared). C11 Home `user`,
  machine `laptop`: read on the laptop in any project; on `mac` in no project. C12 Audiences {lane L} or {machine
  `laptop`}: read by L anywhere and by anyone on the laptop; one Audience {L, `laptop`}: read by L on the laptop only
  (shared). C13 narrowed memory with missing rows: invisible (shared). C14 a lane narrowing with no lane declared:
  error, nothing stored (ME). C15 an identical write in lane L1 does not reinforce the same content in lane L2 (ME).
- Ranking and authority: C16 with the 50 nearest memories hidden and k = 5, recall returns 5 visible results and no
  hidden one reaches a count or the reranker (shared). C17 a memory of Q returned by `recall(project: Q)` from P gets
  `not found` from `forget` and from `set_level`; so does a hidden memory (shared). C18 an edge to a hidden end is
  hidden; in A–H–B with H hidden, B is not reached through H (shared).
- Derived: C19 meets: {P} ∧ {P, lane L} = {P, lane L}; {P, lane a} ∧ {P, lane b}: none; {P} ∧ {Q}: none;
  {P} ∧ {`user`}: P; {`unscoped:laptop`} ∧ {`user`}: `unscoped:laptop`; {`unscoped:laptop`} ∧ {P}: none (shared).
- Locators: C20 one remote in ssh and https forms on `laptop` and `mac`: one project; upstream beats origin (shared).
  C21 the same dev-relative path on two machines: two projects, keys `laptop:x` and `mac:x`, the receipt hints that an
  alias can be proposed; from `mac`, `recall(project: "laptop:x")` reads laptop's project like any other key; after a
  confirmed `add`, one; nested differently (`x`, `old/x`): two, until `add`; a remote-less path whose bytes
  equal a remote key: its own project; the remote keys `github.com/example/a:b` and `[2001:db8::1]/example/x` and
  the path key `laptop:a:b` are distinct and each names one project (shared). C22 a root at or outside the dev root with no remote, or a
  key over 256 bytes: bucket; a local-path or `file:` remote with a root inside the dev root: the dev-relative
  project, as in ADR-0020 (shared). C23 `merge` P2→P1: a write to P2 after the preview refuses the apply until a new
  preview is approved; before approval, P2's sessions resolve to P2 and read P2's
  memories; after the one approved transaction, they resolve to P1 and read P2's memories there; no read in between
  misses them (shared). C24 after `correct` retires a Locator from
  P1 and binds it to P2, a session sending it files into P2; another machine's equal dev-relative path still binds only to its own project.
  C25 knowledge-base with ME unreachable: the write fails retryably and nothing lands at root or `user` (shared).
- Portable paths: C26 written on `laptop` (home `/home/alice`, dev root `/home/alice/perso/dev`, checkout
  `/home/alice/perso/dev/x`): `…/perso/dev/x/src/a.rs` → `${PROJECT_ROOT}/src/a.rs`; `…/perso/dev/kb/README.md` →
  `${DEV_ROOT}/kb/README.md`; `/home/alice/.config/t.toml` → `${HOME}/.config/t.toml`; read on `mac` (home
  `/Users/alice`, dev root `/Users/alice/dev`, checkout `/Users/alice/dev/x`) as `/Users/alice/dev/x/src/a.rs`,
  `/Users/alice/dev/kb/README.md`, `/Users/alice/.config/t.toml`. C27 a session in the linked worktree
  `/home/alice/perso/dev/x/.worktrees/w` that sends its checkout top level: `…/x/.worktrees/w/src/a.rs` →
  `${PROJECT_ROOT}/src/a.rs`, but the main tree's `…/perso/dev/x/src/a.rs` → `${DEV_ROOT}/x/src/a.rs`; without the
  checkout fact, the worktree path → `${DEV_ROOT}/x/.worktrees/w/src/a.rs` and no `${PROJECT_ROOT}` is written; a
  reader without the fact gets `${PROJECT_ROOT}` literal and flagged; `$/home/alice/.config/t.toml` stays literal. C28 on `mac` (dev root `/Users/alice/dev`),
  `/Users/alice/devtools/a` is `${HOME}/devtools/a`. C29 `/opt/tool/bin/t` stays literal, restricts to the writing
  machine, the receipt names it; `https://github.com/a/b` and `GET /v1/events` restrict nothing. C30 a literal `${HOME}`
  and a literal `$${HOME}` the agent wrote are stored as `$${HOME}` and `$$${HOME}` and recalled as written. C31 `${PROJECT_ROOT}` stays literal through `project: P` from Q; a `user`
  write never stores it. C32 a query holding `/home/alice/perso/dev/x/src/a.rs` finds the memory stored as
  `${PROJECT_ROOT}/src/a.rs` and a first-release memory holding that path literally, by full-text search (ME).
- Placement and evolution: C33 `set_level(id, user)` keeps ID, Audiences and content, the receipt hints on a
  `${PROJECT_ROOT}` token; back to `project` returns it to the session's project (ME). C34 for memories filed under
  ADR-0020's rules as amended, a registry built by first sight only, and any context including machine-qualified path
  keys as `project` values from another machine and a path whose bytes equal a remote key, both rule sets read the
  same set (property test, ME). C35 a `remember` fact re-files automatically
  from its call record; a memory with no Capture context is listed, never re-filed; the preview lists every changed
  memory on every dimension; a memory moved to `user` by `set_level` stays at `user` after a replay; P → `user` (from P) →
  `project` (from Q) replays into Q; a re-filing followed by a `correct` replays to the corrected project; an approved machine
  restriction survives a later registry change and replay; applying twice changes nothing (ME).

**Alternatives considered** (one line each): several tree memberships per memory (union leaks, intersection hides);
a flat five-axis tuple with wildcards and specificity ranking ("unrestricted" turns into "every project"; a narrow
stale fact shadows a fresh one); include/exclude tags (visibility stops being monotonic); a five-dimensional R-tree
(an index technique, not a model; a backend may still use one); versioned filing lenses (kept only as previewable
re-filing); machine or session as tree levels (the one-axis problem again); refusing non-portable paths or episodic
`user` writes (replaced by restriction and hints); the registry in KB, or a standalone service now (owner: crate in
ME, #1034 later).

**Consequences.** Positive: one project across machines and harnesses; lane- and machine-specific memories without
new tree levels; a misfiling fixed in one call; history re-files with its old filing kept; release 1 needs no
migration; KB aligns on a contract, not a schema. Negative: two concepts where ADR-0007 had one, filtered on every
read (vector widening costs more); stored content differs from what the agent wrote and an expansion can fail;
server-derived memories (final messages) that mention system paths become machine-local; the same remote-less project
on two machines needs one confirmed `add` per extra machine before it shares memory; KB cannot file under a new project while ME is down (#1034); rows from
before ADR-0020 need a human. Mitigations: the cases as tests in both repositories; the reviewed diff before every
switch; receipts and hints; status counters for root writes after cutover (inert writers excluded), path
restrictions, missing Audience rows, remote-less projects with a proposable alias, and legacy rows awaiting review.

**Not decided here.** Physical schema and migrations; tool names and arguments for narrowing intents and alias
operations; the header names of the new wire facts; role and session intents; publication (adding an Audience) as an
agent tool; capability-based applicability ("needs CUDA" stays descriptive); more than one user; the standalone
identity service (#1034). Automated non-git matching is excluded, not deferred.

## Surfaced for Michaël (into the PR body; nothing fixed or filed without his call)

1. **Readings to review**, each the only one that keeps release 1 a subset or fail-closed:
   project ID = the node label, first-release labels adopted; `${PROJECT_ROOT}` = the session's own checkout only,
   never remapped across checkouts; a narrowing that cannot apply is an error; `set_level` keeps content; `Status: Accepted` (ADR-0020's
   convention).
2. **Notes for the ADR-0020 lane** (relayed by the orchestrator; never sent directly): its key rule drops the auto-join of equal remote-less keys (owner ruling 1, already relayed), recommended form
   `<machine>:<relative path>` for path keys (Constraints); the fail-open code paths in
   Context must not be reused as they are by the context block or `recall`; the call record must keep the request's
   decoded project facts and machine and reach the ADR-0001 log, or remembered facts cannot be re-filed; the new wire
   facts (home, checkout top level, lane) are cheaper reserved now; an unknown `project` value should be an error in
   both ADRs.
3. **Follow-up tickets**: one per §11 stage, filed after the merge if he wants them.

## Implementation sequence

Private scratch `S` = this issue's fleet artifact directory (outside the repository). `ADR` =
`docs/design/adr/0021-memory-scope-model.md`. D1, P1 and H1 are defined under Verification; each returns nonzero on
failure.

- [ ] **1. Rebase, preflight, verified facts, the plan commit.**
  - Files: this plan and its review artifacts under `docs/plans/` (the lens drafts are git-ignored, `.gitignore:59`).
  - Red/green: `git rebase origin/main` (main moved to `a5d756b`, #1037); then `git merge-base --is-ancestor
    origin/main HEAD` exits 0; the worktree is locked and clean. ADR-0020's state from `git ls-tree origin/main
    docs/design/adr/` and its PR. D1's baseline `B` re-measured at the new base. `S/deny.ere` exists and is non-empty
    (account names, host names, the tailnet name; never committed). Each Context citation re-read at its file. P1
    exits 0 on every file this commit adds.
  - Commit: `docs(plans): plan ADR-0021, the memory scope model`.

- [ ] **2. Write ADR-0021 and list it in the index.**
  - Files: `docs/design/adr/0021-memory-scope-model.md` (new), `docs/design/adr/index.md`.
  - Red: H1 prints one `MISSING` line per heading and anchor; `grep -cx '0021-memory-scope-model'
    docs/design/adr/index.md` → 0.
  - Change: the ADR per the outline; the toctree line after `0019-wisdom-substrate` (after `0020-…` if that is on
    `main`), in the same commit so no commit leaves an orphan page.
  - Green: H1 exits 0; the index grep → 1; `grep -cE '^\| C[0-9]+ \|' "$ADR"` → 35; `grep -cE '^\| MC-[0-9] \|'
    "$ADR"` → 9; `grep -nE 'TBD|TODO' "$ADR"` prints nothing; while ADR-0020 is not on `main`,
    `grep -n '0020-harness' "$ADR"` prints nothing; P1 and D1 exit 0.
  - Commit: `docs(adr): add ADR-0021, the memory scope model`.

- [ ] **3. Mark ADR-0007 as amended.**
  - Files: `docs/design/adr/0007-hierarchical-scoping.md`.
  - Red: `grep -c '^\*\*Amended by:\*\* \[ADR-0021\](0021-memory-scope-model.md)' docs/design/adr/0007-hierarchical-scoping.md` → 0.
  - Change: below `**Date:** 2026-03-10`, `**Amended by:** [ADR-0021](0021-memory-scope-model.md) (for the memory
    service each memory has one Home in this tree, Audiences decide its visibility, and root is never read)`. The body
    stays: it still describes shipped behaviour, and ADR-0021 §14 lists what changes.
  - Green: the grep → 1; P1 and D1 exit 0.
  - Commit: `docs(adr): mark ADR-0007 as amended by ADR-0021`.

- [ ] **4. Mark ADR-0005 as amended.**
  - Files: `docs/design/adr/0005-consolidation-3pass.md`.
  - Red: the same grep against ADR-0005 → 0.
  - Change: below its date line, `**Amended by:** [ADR-0021](0021-memory-scope-model.md) (in the memory service,
    passes 2 and 3 summarize only within one visibility meet of their sources, never across the entire store)`.
  - Green: the grep → 1; P1 and D1 exit 0.
  - Commit: `docs(adr): mark ADR-0005 as amended by ADR-0021`.

- [ ] **5. Reconcile with ADR-0020.** After item 4, and again before item 8 whenever ADR-0020 or `main` moved.
  - Red: an MC row, or a name in §3 (label encoding, the machine-qualified path-key form, bucket placement) or §5/§7 (`project`, `remember`'s answer,
    `set_level`, `forget`), that ADR-0020's text (on `main`, else its pushed branch read with `git show`, never in its
    worktree) states otherwise.
  - Change: wording follows ADR-0020; a semantic divergence that breaks the subset goes to Michaël before any edit.
    If ADR-0020 merged: rebase on `origin/main`, keep `0020-…` before `0021-…`, link ADR-0020 in the header, re-measure
    `B`, re-run item 2's green checks, `git push --force-with-lease`.
  - Green: every MC row names a rule ADR-0020 states.
  - Commit, only if the text changed: `docs(adr): align ADR-0021 with ADR-0020 as written`.

- [ ] **6. Open the PR.**
  - Body in `S/pr-body.md`: what the ADR decides; the settlements of questions (a), (b), (c); the defaulted items;
    "Surfaced for Michaël"; the classification, execution-path, planning-tier and review lines; plan issue link;
    `Collateral findings`; test plan; `Closes #1015`, `Refs dutiona/infra#37`, `Refs dutiona/knowledge-base#513`.
  - Red/green: P1 exits 0 on the body and on `git log --format=%B origin/main..HEAD`; the only closing keyword is
    `Closes #1015` (a negated one still closes); `gh pr view --json body` contains both references.

- [ ] **7. `/super-review` at the Light gate**, with Codex and agy as the voices wherever the gate lets the worker
  choose. Each accepted finding gets its own `docs(adr): …` commit; after each, item 2's green checks and P1 on the
  commit messages.

- [ ] **8. `/finish-pr`** to READY (it archives this plan to its one plan issue and removes the `docs/plans/` files),
  with Verification at the pushed head and a fresh unresolved-thread re-poll of 0.

- [ ] **9. Comment on knowledge-base#513** (after READY; another repository; no closing keyword).
  - Body in `S/kb-513.md`, under 15 lines: the PR (ADR under review, merge pending) and the ADR's future `main` path;
    what #513 aligns on: §3's Home kinds and the root rule; an exact Home set instead of #513's subtree expansion; no
    `scope_id DEFAULT 1` as a filing default, and a KB-owned filing step for the existing corpus, which #513's
    migration would otherwise leave at root; the empty-string rule as case C4; opaque project IDs from ME's registry;
    documents and chunks inheriting their source's Audience; the shared case IDs; the retryable error when ME is
    unreachable (C25).
  - Red/green: P1 exits 0 on the body; the comment read back with `gh api` equals the file; its URL goes into the PR
    body. If review later changes §13, the comment is edited, not duplicated. Hand the merge to Michaël; never merge.

## Documentation

This PR is the documentation: the new ADR, its toctree line, and the back-links in ADR-0007 and ADR-0005.
Deliberately unchanged: the glossary's Scope, ScopeQuery and resume entries, `docs/advanced/consolidation.md`, the
usage guides and `docs/reference/crate-layout.md`, because they describe shipped code this PR does not change; each
implementing stage updates what its behaviour changes. The stale ADR count in `AGENTS.md`, `CLAUDE.md` and
`GEMINI.md` is fixed by ADR-0020's PR, so it stays untouched here. No samples: no public API changes.

## Testing

N/A for unit, integration and end-to-end tests: Markdown only, no behaviour changes. The 35 conformance cases are the
test specification the implementing stages write, in ME and, for shared cases, in knowledge-base by #513, each named
by its case ID; C34 is a property test over generated first-release filings. This PR's own checks, red before and
green after their items: H1 (acceptance 1 and 2 traced by heading and anchor), P1 (public repository), D1 (an orphan
page or a dead relative link is a MyST warning), the "Amended by" greps and the KB read-back (acceptance 3).

## Verification

```sh
ADR=docs/design/adr/0021-memory-scope-model.md
FILES="$ADR docs/design/adr/0007-hierarchical-scoping.md docs/design/adr/0005-consolidation-3pass.md docs/design/adr/index.md"
# D1: docs build without -W (main already warns). Fails if the build fails, the count moves off B, or a warning names a touched file.
docs_build() { uv run --no-project --with sphinx --with myst-parser --with sphinx-copybutton \
  --with sphinxcontrib-mermaid --with sphinx-rtd-theme sphinx-build -q -E -b html \
  -D exclude_patterns=_build,Thumbs.db,.DS_Store,ROADMAP.md,plans -w "$S/$1.warn" docs "$S/html-$1" || return 2
  n=$(grep -c WARNING "$S/$1.warn"); echo "warnings=$n"
  ! grep -nE 'adr/(0021-memory-scope-model|0007-hierarchical-scoping|0005-consolidation-3pass|index)' "$S/$1.warn" &&
  { [ -z "${B:-}" ] || [ "$n" -eq "$B" ]; }; }
# P1: public-repository hygiene; exit 0 = clean, 1 = a hit, 2 = could not scan.
privacy() { [ -s "$S/deny.ere" ] || return 2; hit=0
  grep -nEi -f "$S/deny.ere" "$@"; r=$?; [ $r -le 1 ] || return 2; [ $r -eq 1 ] || hit=1
  out=$(grep -noE '(/home|/Users)/[A-Za-z0-9._-]+' "$@"); r=$?; [ $r -le 1 ] || return 2
  printf '%s\n' "$out" | grep -vE ':(/home|/Users)/alice$' | grep . && hit=1
  grep -nE '\bme_[A-Za-z0-9]{32}\b|tskey-[A-Za-z0-9-]{10,}|[a-z0-9-]+\.ts\.net' "$@"; r=$?   # shapes, every file
  [ $r -le 1 ] || return 2; [ $r -eq 1 ] || hit=1
  for f in "$@"; do [ -r "$f" ] || return 2   # address range: strip the documented literal, then match the rest
    sed 's#100[.]64[.]0[.]0/10##g' "$f" | grep -nE '\b100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}\b' |
      sed "s#^#$f:#" | grep . && hit=1; done
  if [ -z "${P1_RULES_TEXT:-}" ]; then   # bare words the plan and its reviews discuss; P1_RULES_TEXT=1 skips only these
    grep -nwE 'tailnet|D11' "$@"; r=$?; [ $r -le 1 ] || return 2; [ $r -eq 1 ] || hit=1
  fi; return $hit; }
# H1: one MISSING line per absent heading or acceptance anchor; nonzero if any.
miss=0; for h in '**Status:** Accepted' '## Context' '## Decision' '### 1. Vocabulary' '### 2. Axes' '### 3. Home' \
  '### 4. Audience' '### 5. Recall' '### 6. Fail-closed states' '### 7. Placement at write time' \
  '### 8. Locators and the project registry' '### 9. Portable paths' '### 10. Derived memories' \
  "### 11. Evolution from ADR-0020's minimal contract" '### 12. Re-filing from the event log' \
  '### 13. Shared with knowledge-base' '### 14. What this amends' '## Conformance cases' '## Alternatives considered' \
  '## Consequences' '## Not decided here' 'descriptive tags' '`add`' '`merge`' '`correct`' 're-capture' 'session' \
  'unscoped:<machine>' '${DEV_ROOT}' 'Placement receipt' 'strict subset'; do
  grep -qF -- "$h" "$ADR" || { echo "MISSING: $h"; miss=1; }; done; [ "$miss" -eq 0 ]
```

P1 is proven able to fail once, in item 1, on probe files kept in `S/scratch/`: one line holding the documented range
literal next to an address inside that range, and one non-`alice` home path, each make P1 return 1; a line holding
only the range literal returns 0. P1 takes file names. Before each push it runs on every committed file of the branch
(`git diff --name-only origin/main...HEAD`; the `docs/plans/` files with `P1_RULES_TEXT=1`) and on the commit
messages saved first with `git log --format=%B origin/main..HEAD > "$S/commit-messages.txt"`. Each other published
artifact is scanned right before its own publication: `S/pr-body.md` before the PR is opened or edited (item 6),
`S/kb-513.md` before the comment is posted (item 9). Any nonzero result blocks that push or post.

At the final pushed head, after `/finish-pr` archives the plan, each unpiped, output to `S/gate-<n>.log`, exit code
recorded: all eleven commands of `AGENTS.md`'s Verify block, with its `RUSTDOCFLAGS` export and
`cargo +nightly fuzz build` included (nightly and cargo-fuzz are installed); expected exit 0 each, since no Rust changes. A command that cannot run is reported as not
run, never as passed. Then D1, H1 and P1 (exit 0 each), `git diff --name-only origin/main...HEAD` (only the four
files), and the unresolved-thread re-poll (0). CI is not dispatched (docs-only; AGENTS.md makes the local matrix
primary); the PR body says so.
