# ADR-0021: Memory Scope Model

**Status:** Accepted. Merging this record is the owner's approval of the scope model (#1015).
**Date:** 2026-11-16
**Parent:** umbrella dutiona/infra#37
**Amends:** [ADR-0007](0007-hierarchical-scoping.md) (scope tree), [ADR-0005](0005-consolidation-3pass.md) (consolidation)
**Related:** [ADR-0001](0001-event-sourcing.md) (event log), [ADR-0004](0004-trait-based-extensibility.md) (no LLM in core), [ADR-0009](0009-session-bootstrap.md) (session bootstrap), [ADR-0015](0015-cross-layer-embedding-identity-policy.md) (cross-layer policy), [ADR-0016](0016-geometric-associative-memory.md) (derived structures), [ADR-0017](0017-cross-layer-faithful-metric-and-derived-structure-parity-policy.md) (shared metric), [ADR-0018](0018-wave2-crate-decomposition-memoryctx.md) (crate DAG), [ADR-0020](0020-harness-neutral-lifecycle-api.md) (lifecycle API and its minimal scope contract, #1014), #1034 (standalone identity service), dutiona/knowledge-base#513

Every decision below carries its provenance: *(owner)* for a decision the owner made in the 2026-11-16 design
session or at plan approval (recorded verbatim on
[dutiona/infra#37](https://github.com/dutiona/infra/issues/37#issuecomment-6928664516)), *(defaulted, for review)* for a default the owner asked to see written down and reviewed in the PR, and
*(settled here)* for a detail this record fixes.

## Context

One owner works on the same projects from several machines whose home and dev roots differ (`/home/alice/dev/x` on
one, `/home/alice/perso/dev/x` on another), through several harnesses and models, with work organized by lane. Three
failures have been seen in practice: one project known under two paths; memories whose paths break on the other
machine; facts filed at the wrong level.

ADR-0007 gives every entity one `scope_id` in one tree. That tree cannot say "project P, but only for this lane" or
"project P, valid on this machine only". It has no machine axis and no agent axis, and its paths tie a project's
identity to where it sits on disk. ADR-0020 ships a minimal scope contract first (its §7: a project key, a
per-machine `unscoped` bucket, recall of the session's project and the user level). This record defines the full
model that contract grows into.

The insight that removes ADR-0007's one-axis limit without throwing the tree away: **where a memory came from is not
where it applies**. Capture context (machine, harness, model, lane, session) is provenance. Home and Audience are the
filing. A fact captured by one harness on the laptop belongs to the project, for every agent on every machine, unless
an Audience says otherwise.

Today's code has fail-open paths that this model forbids in the memory service:

- `memory_ingest` and `memory_add_fact` with no scope write at root (`None => 1` in
  `crates/memory-engine-mcp/src/tools/handlers/p0.rs` and `crates/me-ingest/src/lib.rs`).
- A query with no scope searches every scope (`resolve_query_scope_ids` in `crates/me-query/src/execute.rs`).
- Session resume's pinned tier lists pinned facts of every scope, and its other tiers walk ancestors up to root
  (`crates/memory-engine/src/engine/resume.rs`).
- The storage port reads an empty `scope_ids` slice as "all scopes" on seven methods (`crates/me-storage/src/graph.rs`).
- The global consolidation summary is always root-scoped, and a cluster summary takes its sources' majority scope
  (`docs/advanced/consolidation.md`).

## Decision

**Scope of the decision.** Sections 3 to 12 bind the memory service, the server that agents reach through ADR-0020.
Embedded library consumers keep ADR-0007's semantics: `ScopeQuery`, lazy paths and `DEFAULT 1`.

### 1. Vocabulary

The five names are the owner's *(owner)*. Both repositories use them.

- **Home**: the one node of the scope tree a memory is filed at: the user, a project, or a machine's unscoped bucket.
- **Audience**: who may recall a memory within its Home, as restrictions on lane, role, machine and session.
- **Locator**: how a session finds its project: a key derived from a remote or from a path under the dev root. Each
  Locator is an alias of one opaque project ID.
- **Portable path**: a path stored relative to a root token (`${PROJECT_ROOT}`, `${DEV_ROOT}`, `${HOME}`) and expanded
  for the reader at recall.
- **Placement receipt**: what a write returns: the memory's ID, its Home, its Audience and, when the level looks wrong,
  a hint.

Supporting terms:

- **Capture context**: the immutable provenance of one event or model-tool call (machine, harness and version, model
  when reported, lane, session and segment, the decoded project facts, the resolved key and the rules version).
- **Recall context**: who asks and from where (owner, machine, lane and role when declared, session when known, and
  the context Home).
- **Filing decision**: an appended change of a memory's Home or Audiences, with its reason and actor.
- **Visibility class**: a Home together with a set of Audiences.

This record does not say "global". User-level means Home `user`. Owner-wide recall is its own operation (§5).

### 2. Axes

| Axis | Identity | Role in the Home | Role in an Audience | Provenance |
| --- | --- | --- | --- | --- |
| user | the owner, `user:<user>`, a server setting | the top Home level | none | recorded |
| project | an opaque project ID, found through Locators | a Home level | never a dimension | the resolved key, recorded |
| machine | the name bound to the request's token (ADR-0020 §4), never a hostname or a path | keys the unscoped bucket | a dimension | recorded |
| agent | the lane, one logical agent across harnesses and models, optionally narrowed by a role | none | the lane and the role are dimensions | harness, version, model and subagent type, per turn |
| session | ADR-0020's session ID with its segments | none | a dimension, unused by default | recorded |

- **The agent is the lane** *(owner)*: "one logical agent. harness and model are metadata the each agent turn. I can
  swap model in between, I can resume an agent work with another harness." Lane plus role plus Audience is the way to
  narrow when unsure *(owner)*. A subagent acts in its parent's lane.
- Where a memory came from is not where it applies: "captured by one harness" never means "private to that harness",
  and "observed on the laptop" never means "valid only on the laptop".

### 3. Home: the tree

This section amends ADR-0007 for the memory service.

- The tree is root (id 1) → `user:<user>` → `project:<label>`, plus `unscoped:<machine>` at the top level, which no
  query rooted at the user can reach (ADR-0020 §7). A memory has exactly one Home *(owner)*. The bucket is the only node
  keyed by a machine, and it is a leaf. Sessions, machines and agents are never parents of a Home: they are
  restrictions (§4).
- **Root is never a Home and is never read** *(settled here)*: not by recall, the context block, a derived job or a
  by-ID operation. Rows left at root by `DEFAULT 1` are legacy and unfiled, never user-level. Two writers use root on
  purpose and their `scope_id` is inert: co-session edges (`CO_SESSION_SCOPE_ID` in
  `crates/memory-engine/src/engine/graph.rs`), visible only through their ends (§5); outcome signals
  (`crates/memory-engine/src/engine/outcome.rs`), never recalled. Both are left out of the legacy review list (§12)
  and of the root-write counter.
- **A project's ID is its node's label** *(settled here)*: opaque, never parsed, never resolved as a Locator.
  First-release labels (ADR-0020's percent-encoded keys) are adopted as IDs as they are. A project created after the
  registry exists gets a minted random ID within the 256-byte segment limit. A stored path string
  (`session_checkpoints.scope_path`) is a rendering, never an input to visibility.
- **"A tree plus tags"** *(owner: tags descriptive only)* means three things: the tree holds Homes; Audience
  selectors are the only tags that touch visibility, and they only narrow; descriptive tags (`rust`, `build`), pins
  and importance never grant or restrict visibility.

### 4. Audience

- The dimensions are lane, role, machine and session. Values within one dimension combine with OR; dimensions within
  one Audience combine with AND; a memory's Audiences combine with OR *(owner)*.
- An Audience has no anchor of its own. It narrows within its memory's Home and never reaches past it: reaching
  another project means the `user` Home.
- **Logically every memory has at least one Audience; the default is one unrestricted Audience** *(settled here)*.
  Storage may represent the default by the absence of rows, so first-release rows need none. A memory marked as
  narrowed whose Audience rows are missing is invisible.
- A restricted dimension matches only a Recall context that carries a value in its set. A missing value never matches.
- Adding an Audience widens visibility, so only a logged maintenance operation adds one. Tagging, deduplication and
  merging never do.
- The machine comes from the token. Lane, role and session are declared by the client, so they are a retrieval
  boundary between cooperating agents, not secrecy from the owner or from anyone who can read the database.
- **Session and role** *(defaulted, for review)*: both are defined and matchable. No default write sets them and no
  model tool offers them until usage shows a need. Re-filing uses the session dimension to keep legacy `session:` rows
  as narrow as they were (§12).

### 5. Recall

Recall is a deterministic procedure *(settled here)*:

1. **Build the Recall context**: owner, machine, lane and role when declared, session when known, and the context
   Home, which is the session's project, else its machine's bucket. Candidate generation searches the query as written
   and its tokenized form (§9, with the reader's roots, and no `${PROJECT_ROOT}` under the `project` swap), as a union
   inside the visible set. Tokenized memories and memories holding a literal path, first-release ones included, are
   both found. The tokenized form is embedded only when tokenizing changed the query, within ADR-0020's recall embed
   time limit.
2. **Candidate Homes** = {context Home, `user`}. Recall's `project` argument replaces the context Home for that call
   with the project whose Locator key equals the value byte for byte. The key form is ADR-0020's, as `recall` reports
   it: a remote key, or a machine-qualified path key such as `laptop:x`, so a key names at most one project. The swap
   never reaches a bucket. While another machine's path key resolves to a different project,
   that project is read only when named this way, never automatically *(owner)*; once a confirmed `add` joins the
   two keys they name one project (§8). A value that is not the key of a known project (ADR-0020 §8: a project is known once an
   event or a memory was filed under it) is a typed error, never a `user`-only read. Without the argument, a
   session whose own project is not minted yet (§8: only writes mint) reads `user` only, because that project holds
   nothing yet.
3. **Every channel generates candidates inside the visible set**: full-text search, vector search with its widening,
   graph expansion over the visible subgraph only (no hop through a hidden node), every bootstrap tier, and the
   archive fallback.
4. Fusion, reranking, limits, counts, explanations and the context block see only those candidates.
5. Expand Portable paths (§9).
6. Log what was served (ADR-0020 §9).

Rules that hold at every step:

- **Filtering after top-k is forbidden.** It starves results and leaks hidden memories to a consumer `Reranker`.
  This binds the first release for Homes already (ADR-0020 §7 reads "exactly"); stage 2 (§11) extends it to
  Audiences.
- An edge is visible only when both its ends are. Its own `scope_id` grants nothing. Recall never returns raw events.
- **By-ID authority** (`forget`, `set_level`): a memory qualifies only when its Home is in the session's own
  candidate set {context Home, `user`}, never the `project` swap, and one of its Audiences matches. Otherwise the
  answer is `not found`, the same answer as for a missing ID (ADR-0020 §8).
- The visibility predicate is defined once, as a reference relation. Each storage backend compiles it natively, and
  the backend conformance battery checks every backend against the reference on the conformance cases.
- **Owner-wide recall** (every project and `user`, buckets excluded) is not in ADR-0020's lean tool map. If it is ever
  added, it is a separate explicit operation, never a magic value of `project`.

### 6. Fail-closed states

An empty scope resolves to nothing, never to everything *(owner)*. The states, with the case that tests each:

| State | Result | Case |
| --- | --- | --- |
| no Locator for the session | writes go to `unscoped:<machine>`; reads are that bucket and `user` | C3 |
| empty scope or selector | no match (`resolve_query_empty_string_is_no_match_not_everything`: in ME; in knowledge-base by #513) | C4 |
| unknown key in `project` | a typed error, never empty results | C5 |
| the Recall context lacks a restricted dimension | that Audience does not match | C10 |
| narrowing to the caller's own lane or role with none declared | a typed error, nothing stored | C14 |
| narrowed memory with missing Audience rows | invisible | C13 |
| a memory at root | invisible everywhere; listed for review by re-filing (the inert root writers excepted) | C7 |
| `user` Home | only on explicit intent: `remember`'s level, or `set_level` | C2 |
| owner-wide recall; a bucket read from a project session | only through an explicit operation | C3 |
| an empty Home set reaching storage | no results; "all scopes" is its own explicit selector | C4 |
| an empty derived meet | no derived memory is produced | C19 |
| knowledge-base cannot reach ME's registry | the write fails retryably; never root, never `user` | C25 |

The first three rows and the `user` row are the owner's defaults *(defaulted, for review)*; the rest are settled
here.

### 7. Placement at write time

- **Who chooses.** The agent states its intent: a level (`project` by default, `user` when explicit, in ADR-0020's
  `remember` vocabulary) plus an optional narrowing to its own lane or to the writing machine. The server resolves the
  identities and files the memory as asked. It never refuses for the level and never moves a write *(owner)*. A
  narrowing it cannot apply is a typed error, because dropping it would widen *(settled here)*. Server-derived
  memories (ADR-0020's final messages and compaction summaries) take the default placement.
- Write-time deduplication and reinforcement match only inside one visibility class, never across Audiences.
- **The Placement receipt** is ADR-0020's `remember` answer, extended *(owner)*: the call ID, the memory ID, the Home
  in words (the project's first Locator key, `user`, or `unscoped on <machine>`), the Audience in words, the transforms
  applied (tokens, and any machine restriction with the path that caused it), and an optional hint.
- **Hints** are deterministic rules (ADR-0004: no LLM in core), advisory, and outside conformance. One fires for:
  content that reads user-wide filed under a project, or the reverse (ADR-0020's heuristic); a user-level episodic
  memory; a machine restriction caused by a path; a memory at `user` holding `${PROJECT_ROOT}`; a remote-less project
  whose relative path, machine part aside, another machine's project also holds. That last one is an observation of
  exact equality, never a binding and not a matcher: proposing the `add` is the agent's best effort, and nothing joins
  until a human confirms.
- **`set_level(id, level)`** is ADR-0020's, unchanged in contract and in authority (§5) *(owner)*: it moves the Home
  between the session's project (or its bucket) and `user`. It keeps the ID, the Audiences and the content (the owner:
  edit "only the level"), appends a Filing decision and answers with a receipt. A memory raised to `user` from one
  project can be brought down into another by a session of that other project: the two-call move ADR-0020 §8
  allows, for rehoming memories after a project is reworked, renamed or rehomed *(owner)*.

### 8. Locators and the project registry

- **A Locator is an ADR-0020 key, with its kind and its observation** *(owner: opaque project IDs with
  machine-qualified locators and recorded resolution evidence)*:
  - `remote`: the normalized remote (ADR-0020 §7), machine-independent. Preferring `upstream` deliberately keeps a
    fork with its upstream.
  - `dev-relative`: the path key `<machine>:<relative path>`, the project root relative to the dev root of the machine
    that saw it, for example `laptop:x`.
- **ADR-0020's key order is kept exactly.** A local-path or `file:` remote is ignored as an input, so the next rule
  applies (origin, then the path key). Only a final key ADR-0020 rejects is "no Locator": a `project:<label>` segment
  over 256 bytes, or no usable remote together with a root equal to or outside the dev root, or a root with a `.` or
  `..` segment.
- **A path key never equals a remote key, and never another machine's path key.** A path key starts with a machine
  name (a DNS label, so its first character is a letter or a digit) followed by its first `:`, with no `/` before
  it. A remote key either starts with `[` (a bracketed IPv6 host) or has no `:` before its first `/`, because
  normalizing drops the port and reads `host:path` as `host/path`; its repository path may still hold a `:`
  (`github.com/example/a:b`). A path key is split at its first `:`, so a relative path that holds a `:` still parses.
- **Resolution is a pure function** of the event's raw facts, its machine and a registry state, so a replay against a
  recorded state is deterministic. Every binding records its evidence: the raw remotes with userinfo removed, the
  roots, the rule applied, the rules version and the actor.
- **First sight** *(owner: no automatic join of remote-less projects)*. Only write paths mint a project: the event
  endpoint, `remember`, and a `set_level` that files a memory in the session's project *(owner, on
  [dutiona/infra#37](https://github.com/dutiona/infra/issues/37#issuecomment-6959376409): an agent can rehome
  memories into a renamed or rehomed project that holds nothing yet)*. A unique binding per Locator serializes two
  first sights that arrive together.
  - A `remote` Locator whose key equals a bound `remote` key binds to that project.
  - A `dev-relative` Locator binds only to its own (machine, path) binding: never to another machine's equal path,
    never to a remote key.
  - Otherwise the registry mints a project.
  - Joining two remote-less projects is always a human-confirmed `add`, which the agent may propose. The owner's
    words: "the agent will do best effort to match and there will alsways be Human in the loop to confirm".
  - A remote that changes during a session re-resolves only the later events; earlier ones stay as they were filed.
- **Operations** *(defaulted, for review: distinct, logged operations)*. Each is logged with its actor, reason and
  evidence, and each runs as a maintenance CLI command with the unit stopped, under ADR-0020's maintenance ruling
  ("Pruning, erasure and reversing a forget are maintenance CLI operations, run with the unit stopped").
  - `add`: a Locator joins a project.
  - `merge`: two projects become one. The absorbed ID redirects to the survivor for resolution and is never reissued.
  - `correct`: rebind or retire a Locator from now on, and/or move chosen memories.
  - `merge`, and an `add` of a Locator that already minted a project, run as one re-filing (§12) scoped to the
    absorbed project: a preview bound to the log position it was computed at, the owner's approval, then one
    transaction that applies the redirect and the memories' Filing decisions together. The transaction is refused when
    the absorbed project gained or changed a memory after the preview's position; a new preview is then needed. Before
    the transaction nothing changes; after it, both projects' sessions resolve to the survivor and read its memories.
    No interval leaves a project unreachable.
  - The agent proposes and prints the command; the owner running it is the confirmation. There is no automated
    matcher.
- **The registry is a crate inside ME, built to be extracted** *(owner)*: "For now, let's have it attached to ME, as
  a separate crate, easily extractable". The crate, `me-project-registry`, depends on no memory crate. It owns a store
  port that ME's backends (SQLite and Postgres) implement inside ME's one database, so there is one writer and one
  backup. No memory table references it except through Home labels. Its operations are recorded in the ADR-0001 log
  under a payload it owns, so an extracted service can replay them. Its API (resolve, look up, `add`, `merge`,
  `correct`) is the one the standalone identity service (#1034) would serve. Knowledge-base asks ME.

### 9. Portable paths

The owner's ruling *(owner)*: tokenize paths inside the project and the home, restrict the rest to the machine, and
"rely on each agent wit to ensure paths are correctly reinterpretated if replace fails". The token set is settled
here: exactly three tokens, matched longest prefix first, at path-segment boundaries. Each token means "the same
relative path under this root on the reader's machine", and nothing more.

| Token | Root | Written in |
| --- | --- | --- |
| `${PROJECT_ROOT}` | the top level of the session's own checkout (a linked worktree's own), only when the client sends that fact | project-Home memories only |
| `${DEV_ROOT}` | the machine's dev root (already on ADR-0020's wire) | any memory |
| `${HOME}` | the client's home directory | any memory |

- **`${PROJECT_ROOT}` has exactly one prefix per session.** Without the checkout fact, no `${PROJECT_ROOT}` is
  written and project paths fall to `${DEV_ROOT}`. At write time a path outside the session's checkout top level is
  never mapped onto it, even one in another checkout of the same repository: it falls to `${DEV_ROOT}`, `${HOME}` or
  "other". A linked worktree nested under the session's checkout cannot be told from a subdirectory, so it tokenizes
  as `${PROJECT_ROOT}/.worktrees/<name>/…`, a known imprecision the agent re-interprets. At recall the token expands to
  the reader's own checkout top level, by design: the reader's checkout of this project. It stays literal and is
  flagged when the reader sent no checkout fact.
- **Why `${DEV_ROOT}`.** The dev root is exactly where the machines differ (`/home/alice/perso/dev` against
  `/Users/alice/dev`), and `${HOME}` alone breaks every path into a sibling repository.
- **Rejected tokens**: XDG directories (under `${HOME}` already); temporary paths (machine-local, so a restriction is
  right); a per-project root token (the server would need every project's root on every machine); user-defined tokens
  (no need shown).
- **Any other absolute path stays literal** and adds a writing-machine restriction to every Audience of the memory;
  the receipt names the path. The recognizer's grammar belongs to the implementing ticket, bound by the cases: a URL
  or an API route is not a path (C29).
- **Tokenizing runs at write time**, after redaction, on memories only. The event log keeps the original text.
- **Escaping is invertible.** In incoming content, a run of n ≥ 1 `$` immediately before `{PROJECT_ROOT}`,
  `{DEV_ROOT}` or `{HOME}` is stored as n + 1 `$`. A server-inserted token has exactly one, and the server never
  inserts a token directly after a `$` (such a path stays literal, "other"). At recall a single `$` expands and a run
  of n + 1 is returned as n, so `${HOME}` and `$${HOME}` written by an agent come back as written.
- **Recall expands with the reader's values.** `${PROJECT_ROOT}` expands only when the memory's Home is the session's
  own context Home, never through the `project` swap. A token without a value stays literal and is flagged, and the
  agent re-interprets it.
- **New wire facts**, which the implementing ticket adds to ADR-0020's envelope: the client's home, its checkout top
  level, and the lane (and role). Until a fact arrives, a path under that root counts as "other", which fails closed.

### 10. Derived memories

A summary's Audience is the intersection of its sources' Audiences *(defaulted, for review)*. This record defines
the intersection from what each Home's readers are *(settled here)*:

- **Readers.** `user` is read by every context. Project P is read by the contexts whose context Home is P and by
  `project: P` calls. Bucket M is read by keyless sessions on machine M.
- **Home meet.** `user` ∧ X = X for any Home X, and X ∧ X = X. Two different projects, two different buckets, or a
  project and a bucket have no meet: no context reads both.
- **Audience meet.** The pairwise conjunction of the sources' Audiences: restricted sets intersect, an unrestricted
  dimension imposes nothing, and a conjunction left with an empty dimension is dropped.
- **No Home meet, or no Audience left: the derived memory is not produced.** This covers summaries
  (`summaries.source_fact_ids`), wisdom lineage (`lineage.source_fact_ids`), activity promotion and consolidation
  merges.
- Deduplication and conflict resolution stay inside one visibility class. Across classes both memories stand, and
  neither shadows the other.
- Cache keys carry the resolved Recall context.
- **ADR-0016 structures.** The whitening map stays global per embedding space (ADR-0016: "Global `W` by default",
  resting on ADR-0017); a statistic carries no content. `similarity_knn` edges follow the edge rule (§5). The
  structures ADR-0016 registers per scope and space (diffusion kernel, eigenbasis, curvature cache) are keyed by
  visibility class in the service, so a precomputed structure never spans a hidden member.
- **Consolidation.** The root-scoped global summary (ADR-0005's pass 3) and the majority-vote cluster scope (its pass
  2) are superseded for the service. The derived jobs stay off (ADR-0020 §8) until they follow this section.

### 11. Evolution from ADR-0020's minimal contract

ADR-0020's minimal contract is a **strict subset** of this model *(settled here)*. "Strict subset" means three
things:

1. Every first-release row keeps its `scope_id`, its Audiences (none, so unrestricted) and its content.
2. Every read of first-release rows from any context finds the same visible set as under ADR-0020 (ranking may
   differ), with a registry built only by first sight, before any `add`, `merge` or `correct`.
3. Reaching the full model needs additive schema only: new tables and new columns with defaults; no `UPDATE`, no
   relabel.

Responses may gain fields (the receipt, hints, flags), and new writes follow the full model (tokens, restrictions).

| Row | ADR-0020 rule | Full-model form | Data change |
| --- | --- | --- | --- |
| MC-1 | writes go to the session's project | Home `project:<label>`, unrestricted; its key is the first Locator and its label is adopted as the ID | none |
| MC-2 | user level only on explicit request | Home `user`, unrestricted | none |
| MC-3 | no key: the machine's `unscoped` bucket, never injected into a project | Home `unscoped:<machine>`, the context Home of a keyless session | none |
| MC-4 | reads are the session's project and the user level, exactly | §5 with only unrestricted Audiences | none |
| MC-5 | recall's `project` swaps the project and never reaches a bucket; an unknown key is an error | §5 step 2, byte-exact key match | none |
| MC-6 | the key rules: remote keys machine-independent, path keys `<machine>:<path>`, nothing joins two keys | `remote` and `dev-relative` Locators (§8) | registry rows only; stage 1 adopts every key observed per node in the log |
| MC-7 | `remember`'s answer and `set_level` | §7's Placement receipt and level change | none |
| MC-8 | `forget` and `set_level` act only in the session's own project, bucket or user level | §5's by-ID authority | none |
| MC-9 | every event and call record keeps its decoded project facts, the resolved key and the rules version | the Capture context, the input of re-filing (§12) | none |

The path-key form and the "nothing joins two keys" rule are the owner's ruling for this model *(owner)*, which
ADR-0020 §7 states for the first release, so there is no automatic join to migrate.

The model is reached in additive stages, each its own later ticket:

1. The registry, adopting each first-release node with its observed keys.
2. Audiences and the narrowing intents, with the filter-first rule extended to Audiences.
3. Portable paths, with the new wire facts.
4. The alias operations and the re-filing tool.
5. Knowledge-base's adoption (#513).

### 12. Re-filing from the event log

History is re-filed from the ADR-0001 log without re-capture *(defaulted, for review)*.

- **Re-filing changes the filing only** (Home and Audiences), never content, and never re-extracts facts (ADR-0001:
  facts are consumer-derived).
- **The Capture context of each memory kind**: a server-derived memory from its lifecycle event; a `remember` or a
  `set_level` from its call record, which keeps the request's decoded project facts and machine (ADR-0020 §8) and,
  once it exists, its lane. The call record MUST be part of the ADR-0001 log (an appended event, or a record the log
  replays with the same guarantees), so replaying the log alone re-files remembered facts and level changes; a call
  table kept beside the log would not satisfy this; any other fact through `source_event_id`; a derived memory through its
  `source_fact_ids`; an edge through its ends. Events are read through the schema-evolution upcaster
  ([schema evolution policy](../schema-evolution-policy.md)); no upcaster invents a field.
- **Procedure.**
  1. Fix a registry state: the fold of the registry's logged operations up to a recorded log position.
  2. Compute each memory's filing as a fold, in log order, of its initial placement (re-resolved from its Capture
     context against that state, so a later `correct` of a Locator applies to history, which is its purpose) and of
     its deliberate later decisions only:
     - each `set_level`, whose `project` target is re-resolved from its own call record's Capture context (the calling
       session's project, as ADR-0020 defines it);
     - each explicit memory move of a `correct`;
     - every item of an earlier re-filing that exists only by human confirmation (an approved machine restriction, a
       confirmed legacy-label mapping, the hand filing of a row with no Capture context), replayed as recorded, a
       recorded target project resolving through any later `merge` redirect to its survivor.

     The other items of earlier re-filings, which only materialize resolution, are re-derived and never replayed (a
     `merge` redirect is already in the registry state). So an older re-filing never overrides a later `correct`, and
     replay never undoes a deliberate `set_level` or move.
  3. Diff the candidate filing symbolically, per memory: old and new Home and Audiences on every dimension (lane,
     role, machine and session included), also rendered as the newly visible and the newly hidden memories for each
     context seen in the log.
  4. The owner approves the diff.
  5. Apply it as appended Filing decisions (memory, previous and new filing, operation, reason, actor, rules version,
     registry log position) in one transaction. Applying the same re-filing twice changes nothing. The log is never
     rewritten, and the old scope stays as evidence.
- **Limits.** Replay cannot recover context that was never recorded. Rows with no Capture context, root rows and
  legacy nodes are never re-filed automatically: they are listed for review, and a mapping proposed from a legacy
  label applies only once a human confirms it. First-release memories keep their literal paths; the diff may propose
  a machine restriction for one, applied only on approval.
- **The proof of §11**: replaying first-release history under the full model's defaults gives an empty diff. That is
  the re-filing tool's first acceptance test.

### 13. Shared with knowledge-base

Knowledge-base shares the names, the Audience rules and the conformance cases, not ME's row IDs *(defaulted, for
review)*. The shared rules use the MUST wording ADR-0015 uses for cross-layer rules.

- Knowledge-base MUST use the five names, the three Home kinds and the root rule (§3).
- It MUST apply §4's and §5's matching rules: an exact Home set, never a subtree expansion of `user`.
- It MUST implement §6's states and §10's meet: a chunk is never more visible than its document.
- It MUST pass the cases marked shared, citing their IDs.
- It sends ME the raw project facts and stores the opaque project ID it gets back, never a key, a path or an ME row
  ID. After a `merge` it moves its documents through its own reviewed filing step.
- Its existing corpus is unfiled (root) until a knowledge-base-owned filing step files it.
- Not shared: ME's schema, the registry's internals, ME's re-filing tool, the Placement receipt.
- **One copy.** Knowledge-base cites this record and the case IDs rather than mirroring it, as ADR-0015 is mirrored,
  because ME owns the registry knowledge-base asks. A change to this section links a knowledge-base issue.

### 14. What this amends

- **ADR-0007**, for the memory service: `scope_id` is the Home and Audiences decide visibility; recall uses §5's Home
  set, not `Ancestors` or `Inherited`; root is never read, so `DEFAULT 1` rows are unfiled, not shared (ADR-0007's
  mitigation "all entities default to root scope" no longer applies); project nodes come from the registry. Unchanged:
  one database, the tree and its cache, `ScopeQuery` for library consumers, integer scope IDs.
- **ADR-0005**, for the memory service: pass 3's integration "across the entire knowledge base" and pass 2's cluster
  summaries produce summaries only within a §10 meet. The placement rules in `docs/advanced/consolidation.md` change
  with the implementing stage.

## Conformance cases

The cases are the tests the implementing stages write, each named by its ID. "Shared" cases run in both repositories;
"ME" cases in ME only. Examples use the user `alice`, the machines `laptop` (home `/home/alice`, dev root
`/home/alice/perso/dev`) and `mac` (home `/Users/alice`, dev root `/Users/alice/dev`), projects P and Q, and lanes L and
L2.

| ID | Setup | Expected | Runs in |
| --- | --- | --- | --- |
| C1 | a default write in P | read in P from every machine, lane and harness; not in Q; not by a keyless session | shared |
| C2 | a `user` write | read in P, in Q and by keyless sessions | shared |
| C3 | a keyless write on `laptop` | Home `unscoped:laptop`; read by keyless laptop sessions; never by a project session, through `project`, or on `mac` | shared |
| C4 | an empty selector | no match | shared |
| C5 | an unknown key in `project`; a bare `x` when only `laptop:x` exists; the well-formed key of a project not yet minted; no `project` argument from a session whose own project is not yet minted | a typed error for the first three; a `user`-only read for the last | shared |
| C6 | `project: Q` from a session in P | Q's memories and `user`; nothing of P | shared |
| C7 | a legacy root row; a co-session edge between two visible memories | the row is never read by recall, the context block or a by-ID operation; the edge is visible | shared |
| C8 | a descriptive tag on a memory | never widens its visibility | shared |
| C9 | a pinned memory of Q | never appears in P's context block | ME |
| C10 | Home P, Audience {lane L} | read by L on any machine; not by L2; not by a context with no lane | shared |
| C11 | Home `user`, Audience {machine `laptop`} | read on `laptop` in any project; on `mac` in no project | ME |
| C12 | Audiences {lane L} or {machine `laptop`}; then one Audience {lane L, machine `laptop`} | the first: read by L anywhere and by anyone on `laptop`; the second: read by L on `laptop` only | shared |
| C13 | a narrowed memory whose Audience rows are missing | invisible | shared |
| C14 | a lane narrowing from a session with no lane | a typed error; nothing stored | ME |
| C15 | an identical write in lane L while the same content exists in lane L2 | no reinforcement across the two | ME |
| C16 | the 50 nearest memories hidden, k = 5 | recall returns 5 visible results; no hidden memory reaches a count or the reranker | shared |
| C17 | a memory of Q returned by `recall(project: Q)` from P; a hidden memory | `forget` and `set_level` answer `not found` for both | shared |
| C18 | an edge to a hidden end; A to H to B with H hidden | the edge is hidden; B is not reached through H | shared |
| C19 | derived memories | {P} ∧ {P, lane L} = {P, lane L}; {P, lane L} ∧ {P, lane L2}: none; {P} ∧ {Q}: none; {P} ∧ {`user`}: P; {`unscoped:laptop`} ∧ {`user`}: `unscoped:laptop`; {`unscoped:laptop`} ∧ {P}: none | shared |
| C20 | one remote in ssh and https forms, on `laptop` and on `mac` | one project; `upstream` wins over `origin` | shared |
| C21 | the same relative path `x` on both machines; `x` and `old/x`; a remote-less path equal to a remote key; the keys `github.com/example/a:b`, `[2001:db8::1]/example/x` and `laptop:a:b` | two projects, `laptop:x` and `mac:x`, and the receipt hints that an alias can be proposed; from `mac`, `recall(project: "laptop:x")` reads laptop's project; one project after a confirmed `add`; `x` and `old/x` stay two until `add`; the remote-less path gets its own project; the three keys are distinct and each names one project | shared |
| C22 | a root at or outside the dev root with no remote; a key over 256 bytes; a local-path or `file:` remote with a root inside the dev root | the bucket; the bucket; the path-keyed project, as in ADR-0020 | shared |
| C23 | `merge` of P2 into P1 | a write to P2 after the preview refuses the apply until a new preview is approved; before approval, P2's sessions resolve to P2 and read its memories; after the one approved transaction they resolve to P1 and read P2's memories there; no read in between misses them | shared |
| C24 | `correct` retires a Locator from P1 and binds it to P2 | a session sending it files into P2; another machine's equal relative path still binds only to its own project | ME |
| C25 | knowledge-base writes while ME is unreachable | the write fails retryably; nothing lands at root or `user` | shared |
| C26 | written on `laptop` with checkout `/home/alice/perso/dev/x`: `/home/alice/perso/dev/x/src/a.rs`, `/home/alice/perso/dev/kb/README.md`, `/home/alice/.config/t.toml`; read on `mac` with checkout `/Users/alice/dev/x` | stored as `${PROJECT_ROOT}/src/a.rs`, `${DEV_ROOT}/kb/README.md`, `${HOME}/.config/t.toml`; read as `/Users/alice/dev/x/src/a.rs`, `/Users/alice/dev/kb/README.md`, `/Users/alice/.config/t.toml` | ME |
| C27 | a session in the linked worktree `/home/alice/perso/dev/x/.worktrees/w`, with and without the checkout fact; a reader without it; the content `$/home/alice/.config/t.toml` | with the fact: `…/x/.worktrees/w/src/a.rs` is `${PROJECT_ROOT}/src/a.rs` and the main tree's `…/x/src/a.rs` is `${DEV_ROOT}/x/src/a.rs`; without it: the worktree path is `${DEV_ROOT}/x/.worktrees/w/src/a.rs` and no `${PROJECT_ROOT}` is written; the reader gets `${PROJECT_ROOT}` literal and flagged; the `$`-prefixed path stays literal | ME |
| C28 | `/Users/alice/devtools/a` on `mac` | `${HOME}/devtools/a`, not a `${DEV_ROOT}` path | ME |
| C29 | `/opt/tool/bin/t`; `https://github.com/a/b`; `GET /v1/events` | the first stays literal, restricts the memory to the writing machine, and the receipt names it; the other two restrict nothing | ME |
| C30 | an agent writes a literal `${HOME}` and a literal `$${HOME}` | stored as `$${HOME}` and `$$${HOME}`; recalled as written | ME |
| C31 | a project memory holding `${PROJECT_ROOT}` read through `project: P` from Q; a `user` write holding a project path | the token stays literal; the `user` memory never stores `${PROJECT_ROOT}` | ME |
| C32 | a query holding `/home/alice/perso/dev/x/src/a.rs` | full-text search finds the memory stored as `${PROJECT_ROOT}/src/a.rs` and a first-release memory holding the path literally | ME |
| C33 | `set_level(id, user)` on a project memory, then `set_level(id, project)` | ID, Audiences and content kept; the receipt hints on a `${PROJECT_ROOT}` token; the second call returns the memory to the session's project | ME |
| C34 | memories filed under ADR-0020's rules, a registry built by first sight only, any context, including path keys used as `project` values from another machine and a path equal to a remote key | both rule sets read the same set (a property test) | ME |
| C35 | re-filing | a `remember` fact re-files from its call record; a memory with no Capture context is listed, never re-filed; the preview lists every changed memory on every dimension; a memory raised to `user` by `set_level` stays there after a replay; P to `user` (from P) to `project` (from Q) replays into Q; a re-filing followed by a `correct` replays to the corrected project; an approved machine restriction survives a later registry change and a replay; applying twice changes nothing | ME |

## Alternatives considered

- **Several tree memberships per memory.** A union leaks a project's machine-specific fact into other projects on that
  machine; an intersection hides a project-wide fact that has no machine membership.
- **A flat five-axis tuple with wildcards and specificity ranking.** "Unrestricted project" turns into "every
  project", and a narrow stale fact shadows a fresh general one.
- **Include and exclude visibility tags.** Visibility stops being monotonic.
- **A five-dimensional spatial index of the tuple.** An index technique, not a model; a backend may still use one to
  evaluate the predicate.
- **Versioned filing lenses over history.** Kept only as the previewable re-filing of §12.
- **Machine or session as tree levels.** The one-axis problem again.
- **Refusing non-portable paths, or episodic writes at user level.** Replaced by the machine restriction and by
  hints, because the server never refuses a placement.
- **Joining equal remote-less keys automatically.** Rejected by the owner: two unrelated directories of the same name
  would share memory, and a missed join is the safe failure.
- **The registry in knowledge-base, or a standalone service now.** The owner chose a crate inside ME, with the
  standalone service later (#1034, low priority).

## Consequences

### Positive

- One project spans machines and harnesses, whatever their home and dev roots.
- Lane-specific and machine-specific memories need no new tree levels.
- A misfiled memory is fixed in one call, from the ID its receipt returned.
- History re-files from the event log, with its old filing kept as evidence.
- The first release needs no migration.
- Knowledge-base aligns on a contract, not on ME's schema.

### Negative

- Two concepts where ADR-0007 had one, both filtered on every read; vector widening costs more.
- Stored content differs from what the agent wrote, and an expansion can fail.
- Server-derived memories (final messages) that mention system paths become machine-local.
- The same remote-less project on two machines needs one confirmed `add` per extra machine before it shares memory.
- Knowledge-base cannot file under a new project while ME is down (#1034).
- Rows from before ADR-0020 need a human to file them.

### Mitigations

- The conformance cases run as tests in both repositories.
- A reviewed diff precedes every re-filing.
- Receipts and hints tell the agent where a memory went and when it looks misplaced.
- Status counters: root writes after cutover (the inert writers excluded), path restrictions, narrowed memories with
  missing Audience rows, remote-less projects with a proposable alias, and legacy rows awaiting review.

## Not decided here

- The physical schema and its migrations.
- Tool names and arguments for the narrowing intents and the alias operations.
- The header names of the new wire facts.
- Role and session write intents.
- Publication (adding an Audience) as an agent tool.
- Capability-based applicability: "needs CUDA" stays a descriptive tag.
- More than one user.
- The standalone identity service (#1034).

Automated non-git matching is excluded, not deferred.

## References

- [ADR-0001](0001-event-sourcing.md), [ADR-0004](0004-trait-based-extensibility.md),
  [ADR-0005](0005-consolidation-3pass.md), [ADR-0007](0007-hierarchical-scoping.md),
  [ADR-0009](0009-session-bootstrap.md), [ADR-0015](0015-cross-layer-embedding-identity-policy.md),
  [ADR-0016](0016-geometric-associative-memory.md),
  [ADR-0017](0017-cross-layer-faithful-metric-and-derived-structure-parity-policy.md),
  [ADR-0018](0018-wave2-crate-decomposition-memoryctx.md), [ADR-0020](0020-harness-neutral-lifecycle-api.md) (#1014).
- The owner's rulings, verbatim: [dutiona/infra#37](https://github.com/dutiona/infra/issues/37#issuecomment-6928664516).
- The [schema evolution policy](../schema-evolution-policy.md) and the consolidation guide
  (`docs/advanced/consolidation.md`).
- Issues: #1015 (this record), #1034 (standalone identity service), dutiona/knowledge-base#513 (knowledge-base
  scoping), dutiona/infra#37 (umbrella).
