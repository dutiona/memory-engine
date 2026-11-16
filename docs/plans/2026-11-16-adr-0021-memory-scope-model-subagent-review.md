Altitude report SHA-256: 646ddf1733b961652fb9f87e2bb45de4fcf4960bce2d678c865084aabc88cdcf

# Review: plan for ADR-0021, the memory scope model (#1015)

Reviewer: clean-slate Claude subagent (general-purpose), round 1, plan SHA-256
`fc823301b77118ba23b90a9789b0bb2e3a774aa2be00e9657f9e261cd887b68a`. Saved verbatim below this line.

**Verdict: not LGTM yet.** I found 4 HIGH, 13 MEDIUM and 16 LOW findings, and no BLOCKER. The model holds together, and the plan copies the settled decisions faithfully: the "Settled" list in Constraints matches the brief's "Decisions so far" byte for byte (diffed). The claim that ADR-0020's contract is a strict subset (§11) holds for reads over first-release rows. It fails or has two readings at four points (H1, H2, M1, M2). Each fix is a sentence or a case, not a redesign.

Confidence: high on the comparisons with ADR-0020 (checked against the §I and §J text in the plan snapshot) and on the code citations (re-read at their files). Medium on how ADR-0016 interacts with the model.

## Altitude and duplication (assessed first)

I acknowledge the override, bound to the SHA-256 above, and I think it is justified.
- The report's plan digest (`fc823301…`) matches the plan on disk, so the report is current.
- The dispatch makes the outline the plan, and the outline is short rule bullets, not ADR prose.
- No canonical file, validator or generator is embedded. The one code fence holds three check helpers.
- The duplication between outline and ADR ends when `/finish-pr` archives the plan.
- No bullet needs to move out. The findings below are things the outline lacks, not excess detail.

Two minor points:
- **[LOW]** Items 6–7 restate internals of the review and merge workflow that the skills own: `state.sh bind-review-target`, `route --out`, `PRESELECTED_REVIEW_ROUTE_FILE`, and finish-pr's own "Item 5". "`/super-review` at the Light gate" and "`/finish-pr`" are enough and cannot drift. Also, "Item 5" inside item 7 reads like this plan's item 5.
- The MC table restates ADR-0020's rules in a second permanent ADR. That is acceptable because ADR-0020 §I says ADR-0021 replaces its scope section, provided item 4 keeps the wording in sync, which it does.

## HIGH

**H1. By-ID authority can be read two ways, and one reading widens visibility** (§5 by-ID bullet, §7 `set_level`, MC-7, C14)
- §5 says `forget` and `set_level` "act only on memories the caller can recall".
- After `recall(project: Q)` from a session in P, a memory of Q is recallable. The natural reading then lets `set_level(q_id, user)` publish Q's memory to every project, and lets `set_level(q_id, project)` move it from Q into P.
- ADR-0020 §J says the opposite: "A fact of another project, even one `recall(project: …)` returned, answers `not found`", and `set_level` "never moves a fact between two projects".
- The Constraints section restates ADR-0020 correctly, so this is a drafting slip. It still breaks §11's definition and opens a widening path through a model tool.
- **Fix:**
  - By-ID operations act only on a memory whose Home is in the caller's own candidate set {context Home, `user`}, without the `project` swap, and whose Audiences match.
  - Add an MC row for write authority; none of MC-1..MC-8 covers `forget`.
  - Extend C14: a Q memory returned by `recall(project: Q)` in P gets `not found` from both `forget` and `set_level`.

**H2. A `project` value with a dev-relative key breaks MC-5** (§5 step 1, §8 Locator kinds, MC-5, C5)
- As I read ADR-0020 §I and §J, a project is the label of its key, and a path key is the root relative to the dev root, the same on every machine. So `recall(q, project: "x")` from desktop reaches the project that laptop filed as `x`.
- In §8, a dev-relative Locator is the pair (machine, relative path), and §5 resolves `project` as "a Locator in key form, normalized like a remote".
- Read with the caller's machine, (desktop, `x`) is unbound, so the call hits C5's typed error where release 1 returned results.
- "Normalized like a remote" also lowercases the first segment as a host and rewrites `host:path` to `host/path`. ADR-0020 never does either to a path key, so `Foo/bar` would become `foo/bar`.
- **Fix:**
  - Define resolution per kind. A remote-shaped value is normalized as a remote. Any other value is matched unchanged, as a dev-relative path, against every machine's bindings.
  - If several projects are bound under that path, return a typed "ambiguous" error.
  - Make C28's generator include dev-relative `project` values across machines; it would then catch this.

**H3. `merge` and `add` leave the memories of the moved project undefined** (§8 operations, §5 step 2, §12, C20, §13)
§5's candidate Homes are {context Home, `user`}, and a memory has exactly one Home. After `merge` P2→P1, "the absorbed ID redirects". That has two readings:
- **(a) The redirect applies only to resolution**, which is all C20 tests. Then no session's context Home is P2 any more, and P2's whole history drops out of recall. No step re-files it.
- **(b) The Home set includes absorbed IDs.** Then §5 step 2 is incomplete, and the merge makes each project's memories visible to the other at once. That skips the reviewed visibility diff the settled decision requires ("diffs visibility before switching").

The same question applies to `add` when the joining Locator already minted its own project. It also applies to knowledge-base, which "adopts a merge survivor" by a mechanism the plan never specifies.

**Fix:** pick one reading.
- Recommended: `merge` and `add` run as a §12 re-filing limited to the absorbed project. That means a preview of the newly visible memories, Michaël's approval, then appended Filing decisions. The absorbed ID resolves to the survivor for new resolutions.
- Add a case: after a merge P2→P1 and its approved re-filing, P2's memories are read from P1; before approval, nothing changes.

**H4. Remembered facts have no Capture context to re-file from** (§12 input, MC-8, acceptance 2)
- §12 links facts to their Capture context through `source_event_id`.
- Today `me_ingest::add_fact` writes no event; `source_event_id` is whatever the caller supplies (`crates/me-ingest/src/lib.rs`).
- ADR-0020's call record keeps "ID, machine, tool, time, the fact IDs returned or changed", with no project facts and no lane.
- The MCP request does carry the project facts as headers, but ADR-0020 only promises that "every event keeps its decoded facts".
- So every `remember` fact, the main memory kind, falls into §12's "no Capture context → listed for review". That defeats "re-filing history from the ADR-0001 event log without re-capture" for exactly the memories that matter.
- **Fix:**
  - §12 names where each memory kind's Capture context comes from: server-derived memories from their lifecycle event; `remember` and `set_level` from the call record.
  - Add to the notes for the ADR-0020 lane (Surfaced #2): the call record must keep the request's decoded project facts and machine, and must go into the ADR-0001 log.
  - Extend C29: a `remember` fact re-files automatically.

## MEDIUM

**M1. ADR-0020's no-key rules are missing** (§8 first sight; MC-1, MC-3, MC-6)
- ADR-0020 §I: when the segment `project:<label>` exceeds 256 bytes, there is no key, so the session uses its machine's bucket.
- §8 says "an unknown remote mints a project", with a random ID that has no length limit.
- After stage 1, such a session's context Home moves from the bucket to a new project, and its bucket memories disappear from its reads. That changes a first-release row's visibility from a first-release context, which §11 forbids.
- **Fix:** keep "no Locator" for every key ADR-0020 rejects (over-long, `.` or `..` segments, local or `file:` remotes, a root equal to the dev root) and list them in MC-6. Or declare the change.

**M2. §11's third clause claims too much**
- "Every first-release call keeps its result" is false for writes once stage 3 lands. The same `remember("… /opt/tool/bin/t")` then gets a machine restriction, tokenized content and a new receipt.
- C28 only tests reads.
- **Fix:** "every first-release read over first-release rows keeps its result; new writes follow the full model". Qualify C28 as "with a registry built only by first sight (no add, merge or correct)".

**M3. Filter-first is listed under stage 2, but release 1 needs it for Homes** (§11 Stages)
- The Context section cites fail-open channels: the pinned tier reads every scope, other tiers walk ancestors to root, and an empty slice means "all scopes".
- ADR-0020's "reads … exactly" already needs filter-first in every channel from release 1. Without it, C13 fails for memories hidden by their Home.
- Listing "the filter-first rule in every channel" under stage 2 lets #1022 filter after top-k.
- **Fix:** make filter-first and C13 binding on release 1 for Homes; stage 2 extends them to Audiences.

**M4. Some code writes at root on purpose** (§3 root bullet, §6 root row, C7, the Consequences counters)
- Co-session edges are always written at root (`CO_SESSION_SCOPE_ID = 1` in `crates/memory-engine/src/engine/graph.rs`).
- Outcome signals are too (`crates/memory-engine/src/engine/outcome.rs`: "cross-scope and always written to the root").
- §3 calls every root row "legacy and unfiled" and C7 says a root row is never read. §5 says an edge's own `scope_id` grants nothing and the edge is visible when both ends are.
- For a co-session edge, C7 and §5 contradict each other. Re-filing would list every such edge for review, and the "root writes after cutover" counter would never reach zero.
- **Fix:** name these root writers and say their `scope_id` is inert: edges get visibility from their ends, and outcome events are never recalled. Exclude them from the review list and the counter. Or have the implementing stage stop writing them at root.

**M5. It is unclear whether the root rule binds only the service or the library too** (§3, §6, §14)
- §14 says "For the memory service", but §3 and §6 are unqualified.
- §5's visibility predicate is compiled into every storage backend, and library consumers share those backends.
- Read unqualified, every embedded consumer that relies on ADR-0007's "consumers who do not need scoping can ignore it entirely" loses access to all its data.
- **Fix:** one sentence at the top of the Decision saying §3–§12 bind the memory service, and the library keeps ADR-0007's semantics through `ScopeQuery`.

**M6. Graph traversal and derived structures can carry hidden memories into ranking** (§5 step 3, §10)
- "Every channel generates candidates inside the visible set" does not forbid a hop through a hidden node. A visible A links to a hidden H, which links to a visible B, so B surfaces because of H.
- ADR-0016 (Accepted) adds `similarity_knn` edges built from global top-k neighbours, plus corpus whitening statistics. Global kNN can leave a project's expansion with no visible neighbours at all.
- KB #513 itself says whitening must be computed per scope "or one tenant's anisotropy leaks into another's metric".
- **Fix:**
  - §5: every graph computation at recall time runs on the visible subgraph only.
  - §10: state the position on ADR-0016 and ADR-0017 derived structures, either per visibility class or accepted as a signal that carries no content.
  - Add ADR-0016 to Related.
  - Add a case: in A–H–B with H hidden, B is not reached through H.

**M7. "Visibility class" is undefined, and write-time dedup keys on the Home only** (§10, §7)
- The backfill reinforcement in `crates/me-backend-sqlite/src/store/facts.rs` matches "identical content … in the same `scope_id`".
- Once Audiences exist, it would fold a lane-L1 write into an existing lane-L2 memory. The write is not filed as asked, and the returned ID names a memory the writer cannot recall.
- **Fix:** define a visibility class as (Home, set of Audiences); say write-time dedup and reinforcement stay inside one class; add a case.

**M8. Queries are not tokenized** (§5, §9)
- A memory is stored as `${PROJECT_ROOT}/src/a.rs`. A recall query containing `/home/alice/dev/x/src/a.rs` misses it in full-text search, and the query's embedding drifts from the stored one.
- **Fix:** §5 step 1 tokenizes the query with the reader's values, under the same rules, before generating candidates.

**M9. First-sight minting on read paths** (§8 first sight, against ADR-0020 §A "Its only write is the recall-log injection")
- ADR-0020 §M posts the SessionStart context read after starting the flush, so the read can arrive before the queued event is applied.
- If resolving the project during that read mints it, the read writes. If it does not, the plan never defines what the read returns for a project not yet minted.
- Two concurrent first sights of the same remote also need exactly one winner.
- **Fix:** only write paths mint (the event endpoint and `remember`), serialized by a unique Locator binding. A read of a project not yet minted reads `user` only, which is what ADR-0020 gives today for a scope node that does not exist yet.

**M10. ADR-0005 is amended but not marked** (§10, the ADR header, item 3)
- ADR-0005's pass 3 builds a global summary "across the entire knowledge base". `docs/advanced/consolidation.md` puts it at root and gives cluster summaries their sources' majority scope.
- §10 supersedes both for the service, yet ADR-0005 is not in Related and gets no "Amended by" line, so it will be wrong once this merges.
- **Fix:** add ADR-0005 to Related and give it a one-line "Amended by ADR-0021". That touches a fourth file, so it needs Michaël's OK: it changes the route check "only the three files".

**M11. The KB #513 comment misses the two conflicts #513 will actually hit** (item 8, §13)
- #513 today expands "a query for scope S" to "S ∪ its subtree". Applied to `user:<user>`, that is owner-wide recall, which §5 forbids as a default.
- #513 also plans a "non-breaking" migration that folds every existing row into root. §13's shared root rule makes all those rows invisible, and re-filing is marked "Not shared".
- **Fix:** the comment names both: an exact Home set instead of subtree expansion, and a filing step owned by KB for its existing corpus.

**M12. Preflight never rebases, and the Rust gate will be red** (item 1, Verification)
- `origin/main` is at `a5d756b` (#1037, "clean up clippy 1.98/1.100 lint drift that fails the -D warnings gate"). The branch is at `b59f9ee`.
- So item 1's check "`origin/main` is an ancestor of HEAD" fails today, and the final `cargo clippy … -D warnings` should be expected to fail at `b59f9ee`.
- Item 4 rebases only if ADR-0020 has merged.
- **Fix:** item 1 rebases onto `origin/main` unconditionally and re-measures `B`; item 7 does the same if main has moved again.

**M13. The privacy check P1 does not cover everything that gets published** (items 1 and 6, Verification P1)
- Item 1 commits "this plan and its review artifacts" to a public branch, and finish-pr then archives them to a public issue. Reviewer outputs quote absolute local paths, and the brainstorm brief holds the real machine paths.
- The bodies of item 6's review-fix commits are not scanned either. ADR-0020's V2 scans the diff, the commit messages and the PR body.
- **Fix:**
  - Run P1 on every file item 1 commits and on `git log --format=%B origin/main..HEAD`, before each push.
  - Add ADR-0020's `test -s` guard so an empty or missing `deny.ere` cannot pass silently.
  - Check grep's exit code: 1 means clean, 2 means the scan did not run.

## LOW

- **L1.** Item 2's green check `\]\([^)]*0020-` forbids the link that Constraints require once ADR-0020 is on main ("unlinked until its file is on `main`"). Item 4 should add the link then, and the check should allow it.
- **L2.** Item 2 never gives the command for "the MC count → 8". The index grep has a red value but no green one (it should become 1).
- **L3.** H1 only checks that headings exist. ADR-0020's V5 checks anchors for each acceptance phrase. Add anchors such as `tags`, `alias`, `re-capture`, `session` and `fail-closed`, so acceptance items 1–2 are traced rather than assumed.
- **L4.** A writing rule is missing: no `|` inside a table cell (ADR-0020 §N has it). `level?: "project" | "user"` in an MC cell silently splits the row, and D1 will not warn.
- **L5.** The Context bullet "MCP `remember` with no scope writes root (`p0.rs`)" names a tool that does not exist yet. Today it is `memory_ingest` (`None => 1, // root scope` in `p0.rs`) and `memory_add_fact`, through the same default in `me-ingest`.
- **L6.** §6 says the test `resolve_query_empty_string_is_no_match_not_everything` is in "both repositories". KB does not have it yet; it is a #513 acceptance item. Write "(ME; KB by #513)".
- **L7.** C11 states no expected result. C22 needs its dev root stated (`/home/alice/dev`, desktop) to test the segment boundary. C21 never varies `${HOME}`, although C17 says the homes differ.
- **L8.** Some listed states have no case:
  - KB cannot reach ME (§6's last row), which should be a shared case.
  - A pinned Q memory never appears in P's context block (the Context section cites the pinned tier).
  - A linked worktree outside and inside the main tree (§9 says "both prefixes tokenize").
- **L9.** §8 first sight: no rule covers a third machine whose dev-relative path is already bound to two different projects (after a `correct`). Make it "no automatic binding, plus a hint".
- **L10.** The hint for an unconfirmed same-path alias fires only on writes (§7), but the widening shows up on reads. Also show it on the status line or in the `recall` answer.
- **L11.** "Automated non-git matching is excluded" (Not decided here) sits next to §8's automatic exact-path binding. Qualify it as "beyond ADR-0020's exact-path equality"; Surfaced #1 already raises the tension with Michaël.
- **L12.** §3 says "machines are never tree levels", but `unscoped:<machine>` is a tree node. Say the bucket is the only node keyed by a machine: a leaf Home, never a parent.
- **L13.** §8 does not say who implements the registry crate's persistence: its own storage port implemented by both SQLite and Postgres, or ME's storage on its behalf. Saying so is what makes "depends on no memory crate" and "one writer" both true, and keeps the crate extractable.
- **L14.** Consequences should say that server-derived memories (final messages) mentioning system paths become machine-local under §9.
- **L15.** The Verification wording ("every command … except `cargo +nightly fuzz build` …, from `cargo fmt` through clippy, test and `check -p memory-engine`") can be read as a subset. Say "the ten other commands of the Verify block".
- **L16.** Item 8 posts the KB comment before merge, with the ADR's future `main` URL. If review changes §13, the comment goes stale. Link the PR and say the ADR is under review, or re-check the comment at merge.

## The strict-subset claim, tested

- **Holds:** MC-1 to MC-4 and MC-6 to MC-8, over first-release rows and with a registry built only by first sight. Bucket isolation holds, root is never read, and `set_level` keeps its contract under ADR-0020's authority reading.
- **Fails or has two readings:** H1 (by-ID authority), H2 (dev-relative `project`), M1 (ADR-0020's no-key rules), M2 (write results after stage 3).
- **Separately:** H4 does not break the subset, but it breaks re-filing (acceptance 2) for remembered facts.
- With these fixed, C28 is the right executable proof.

## Structure and feasibility

- Documentation, Testing (N/A with a reason, plus the docs checks) and Verification sections are all present.
- The order of steps works once M12's rebase is added.
- The red-before, green-after checks work for items 2 and 3, apart from L1 and L2.

## Resolution

All findings addressed in the round-2 candidate (plan SHA-256 `b002e504cdcc5de8785fb243060a8a0d71ab7d56e6be6f8a9cb9770c67f41b24`, altitude report re-issued for it). Default severity: every finding actioned.

- [LOW] altitude: items restate skill internals → items 7–8 now name `/super-review` and `/finish-pr` only; "Item 5" wording removed.
- [HIGH] H1 by-ID authority → §5 "By-ID authority": the session's own candidate set, never the `project` swap; MC row for write authority added (MC-9 table now covers it); case C17.
- [HIGH] H2 dev-relative `project` value → §5 step 2: byte-exact match on the Locator key, any kind, any machine, no remote normalization; typed `ambiguous` error; C5, C34 (generator includes dev-relative values across machines).
- [HIGH] H3 merge/add → §8: they move no memory by themselves; memories move only through a §12 re-filing scoped to the absorbed project; the redirect is for resolution; C23. §13: KB moves its documents through its own reviewed filing step.
- [HIGH] H4 remembered facts' Capture context → §12 names the source per memory kind (call records for `remember`/`set_level`), a requirement on ADR-0020's call record; Surfaced #2 note for the ADR-0020 lane; C35.
- [MEDIUM] M1 ADR-0020 no-key rules → §8: every key ADR-0020 rejects stays "no Locator"; MC row; C22.
- [MEDIUM] M2 third clause → §11 now: rows unchanged; reads of first-release rows keep their result with a first-sight-only registry; additive schema; responses may gain fields, new writes follow the full model.
- [MEDIUM] M3 filter-first → §5: binding on release 1 for Homes; stage 2 extends to Audiences.
- [MEDIUM] M4 deliberate root writers → §3 names co-session edges and outcome signals as inert; excluded from review list and counter; C7.
- [MEDIUM] M5 service vs library → "Scope of the decision" sentence before §1.
- [MEDIUM] M6 graph and ADR-0016 → §5 step 3 visible subgraph only; §10 position on ADR-0016/0017 structures; ADR-0016 and ADR-0017 in Related; C18.
- [MEDIUM] M7 visibility class → defined in §1; §7 dedup/reinforcement inside one class; C15.
- [MEDIUM] M8 query tokenization → §5 step 1; C32.
- [MEDIUM] M9 minting on reads → §8 only write paths mint, unique binding serializes; §5 step 2: an unminted project contributes nothing.
- [MEDIUM] M10 ADR-0005 → new item 4 and §14; flagged in "Decision for Michaël at plan approval" since it adds a fourth file.
- [MEDIUM] M11 KB comment → item 9 names the subtree-expansion and root-migration conflicts.
- [MEDIUM] M12 rebase → item 1 rebases onto `origin/main` (verified at `a5d756b`, #1037) and re-measures `B`; item 5 rebases again when main moves.
- [MEDIUM] M13 privacy → P1 checks the deny file, distinguishes "could not scan" (2), covers every committed file, the commit messages, the PR body and the KB comment.
- [LOW] L1 → item 2 forbids the ADR-0020 link only while it is not on `main`; item 5 adds it then.
- [LOW] L2 → item 2 gives the MC and case counts and the index grep's green value.
- [LOW] L3 → H1 checks acceptance anchors.
- [LOW] L4 → "no `|` inside a table cell" writing rule.
- [LOW] L5 → Context cites today's `memory_ingest`/`memory_add_fact` and both `None => 1` sites (re-verified).
- [LOW] L6 → "(ME; KB by #513)".
- [LOW] L7 → C11 states its result; C28 states its dev root; C26 varies the home (`/home/alice` and `/Users/alice`).
- [LOW] L8 → C25 (KB unreachable), C9 (pinned Q never in P's block), C27 (linked worktrees).
- [LOW] L9 → §8: a key bound to several projects binds nowhere automatically and hints; C24.
- [LOW] L10 → §7: the unconfirmed "same key" hint also shows in `recall` answers.
- [LOW] L11 → "Not decided here" now reads "beyond ADR-0020's key equality"; the tension is the Decision for Michaël.
- [LOW] L12 → §3: the bucket is the only machine-keyed node, a leaf.
- [LOW] L13 → §8: the registry owns a store port that both backends implement inside ME's database.
- [LOW] L14 → Consequences: server-derived memories with system paths become machine-local.
- [LOW] L15 → Verification names all eleven commands, the export and the fuzz gate.
- [LOW] L16 → item 9's comment links the PR as under review; edited, not duplicated, if §13 changes.

---

# Round 2 (same subagent, delta review of plan SHA-256 `b002e504cdcc5de8785fb243060a8a0d71ab7d56e6be6f8a9cb9770c67f41b24`)

Altitude report SHA-256: f7aabe4bd08a6e4b34de2eb16a17c2712e6278c1f076a3fed116dd629a5d39e7

Summary of the verdict as returned: no BLOCKER or HIGH remains; three new MEDIUM (N1–N3) and ten LOW. 32 of 33 round-1 findings resolved as claimed, L10 partly. The override re-acknowledged.

- [MEDIUM] N1 tokenizing the query breaks §11 clause (2) for first-release memories holding literal paths; fix: search the literal and the tokenized query as a union inside the visible set; word clause (2) as "the same visible set"; extend C32.
- [MEDIUM] N2 the decision framing: the quoted "owner decision 17" sentence is not in the named inputs; option A also reinterprets "machine-qualified locators"; B's cost is overstated (one confirmation per remote-less project per extra machine, no migration); the ADR-0005 file is a separate decision bundled into the approval line.
- [MEDIUM] N3 `P1_RULES_TEXT=1` skips the credential-shape checks on the riskiest files (external reviewer outputs); run shape-specific patterns on every file, exempt only bare words for plan files.
- [LOW] 1 `ambiguous` ignores the caller's own machine binding, which should win.
- [LOW] 2 a key that binds nowhere has two overlapping rules; recommended: bucket plus hint for reads and writes.
- [LOW] 3 no way to confirm a "same key" binding; express it as `add` with evidence "confirmed".
- [LOW] 4 the merge leaves a dark window between redirect and approved re-filing.
- [LOW] 5 query tokenization under the `project` swap must not produce `${PROJECT_ROOT}`.
- [LOW] 6 the whitening default is stated in ADR-0016 (citing ADR-0017 §5–§6), and ADR-0016's per-scope-and-space structures must be keyed by visibility class or C18 breaks.
- [LOW] 7 the ADR-0005 line amends rules ADR-0005 does not state; word it against pass 3's "across the entire knowledge base".
- [LOW] 8 escaping must be total: a literal `$${HOME}` must round-trip.
- [LOW] 9 fixture drift: C28 introduces a third dev root; the Decision section still says "desktop".
- [LOW] 10 the "same key" hint should also show in the SessionStart context or its status line.

## Resolution (round 2)

All addressed in the round-3 candidate (plan SHA-256 `dbaa23f25a3040aa8e228d5adc9d0858e0991d36adcbeeabe5c2b53e8f9ff612`, altitude report re-issued).

- N1 → §5 step 1: union of the query as written and its tokenized form inside the visible set; §11 clause (2) now "finds the same visible set (ranking may differ)"; C32 extended to a first-release literal-path memory.
- N2 → section renamed "Decisions for Michaël at plan approval": decision 17's reason quoted verbatim from the epic spec (line 178, now a provenance input, `inputs/epic-spec.md`), labelled as the spec's recorded reason; both brainstorm decisions named; A's reading of "machine-qualified" stated; B's cost restated; ADR-0005 is Decision 2.
- N3 → P1 runs the token-shape patterns on every file; `P1_RULES_TEXT=1` skips only the bare words `tailnet` and `D11`.
- L1 → §5 step 2: the caller's machine binding wins; `ambiguous` only without one.
- L2 → §8: a key bound to several projects with no binding for the caller is treated as no Locator (bucket and `user`, reads and writes), with a hint.
- L3 → §8: confirmation is an `add` of the same Locator with evidence "confirmed".
- L4 → already fixed in round 3 from Codex's round-2 finding: redirect and memory moves apply in one approved transaction; C23 reworded.
- L5 → §5 step 1: no `${PROJECT_ROOT}` under the `project` swap.
- L6 → §10 cites ADR-0016's "Global `W` by default"; ADR-0016's per-scope-and-space structures keyed by visibility class.
- L7 → item 4's line and §14 worded against ADR-0005's pass 3 "across the entire knowledge base".
- L8 → C30 covers `$${HOME}`; §9 defines invertible `$`-run escaping.
- L9 → C28 uses `mac` (`/Users/alice/dev`); "desktop" remains only inside the verbatim spec quote.
- L10 → §7 and Decision 1: the hint also shows on the SessionStart status line.

---

# Round 3 (same subagent, delta review of plan SHA-256 `dbaa23f25a3040aa8e228d5adc9d0858e0991d36adcbeeabe5c2b53e8f9ff612`)

Altitude report SHA-256: 2808691b58d1a90a1629368905d497e562f32eb06c1902861c2d3970d449cd85

Summary of the verdict as returned: not LGTM yet; no BLOCKER or HIGH; three new MEDIUM (R1–R3), four LOW (R4–R7); all 13 round-2 findings resolved in the text; override re-acknowledged; Decision 1 framing fair, with decision 19's alias wording worth quoting.

- [MEDIUM] R1 the replay fold freezes old resolutions (an older re-filing overrides a later `correct`) and replays `set_level(project)` to the memory's project instead of the caller's; replay only deliberate decisions (`set_level` re-resolved from its call record, `correct` moves), re-derive re-filings; two cases.
- [MEDIUM] R2 the "same key" hint on the SessionStart status line breaks ADR-0020's closed status-line grammar; put it as its own line in `additionalContext`.
- [MEDIUM] R3 a merge can apply a stale preview; bind the apply to the preview's log position, name the governing ruling, add a case.
- [LOW] R4 escaping is not invertible after an agent-written `$` before a tokenized path; never insert a token right after a `$`.
- [LOW] R5 the main-tree fallback for `${PROJECT_ROOT}` gives writer and reader different bases; no checkout fact, no `${PROJECT_ROOT}`; reword the write-time claim.
- [LOW] R6 the union doubles embedding work; embed the tokenized form only when it differs, within ADR-0020's embed limit.
- [LOW] R7 P1 has no tailnet-address pattern (100.64.0.0/10).

## Resolution (round 3)

All addressed in the round-4 candidate (plan SHA-256 `6135c0c810e39fd720fbe4d45d686525920ab46cbff03f80f4c8223eecd37a33`).

- R1 → §12: replay folds the re-resolved initial placement with deliberate decisions only (`set_level` re-resolved from its call record's Capture context; `correct` moves); re-filings are re-derived; C35 adds both sequences.
- R2 → §7 and Decision 1: the hint is its own line in the SessionStart `additionalContext`, after the status line; ADR-0020's grammar unchanged.
- R3 → §8: alias operations are maintenance CLI commands with the unit stopped (ADR-0020's maintenance ruling, quoted); the preview is bound to its log position and the apply refuses if the absorbed project changed since; C23.
- R4 → §9: the server never inserts a token directly after a `$`; C27 covers `$/home/alice/.config/t.toml`.
- R5 → §9: no checkout fact, no `${PROJECT_ROOT}` (paths fall to `${DEV_ROOT}`); a reader without the fact gets the token literal and flagged; the claim reworded to write time, recall mapping to the reader's checkout stated as by design; C27.
- R6 → §5 step 1: the tokenized form is embedded only when it differs, within ADR-0020's recall embed time limit.
- R7 → P1's shape grep adds the 100.64.0.0/10 address range.
- Decision 1 framing → quotes spec decision 19's alias wording verbatim.

---

# Round 4 (same subagent, delta review of plan SHA-256 `6135c0c810e39fd720fbe4d45d686525920ab46cbff03f80f4c8223eecd37a33`)

Altitude report SHA-256: b415d2fdcee00b728ac18348e24476a5d3bf1be0f404cab64cb7d2c41632d34c

Summary of the verdict as returned: not LGTM yet, but close; R1–R7 resolved; two new MEDIUM, one LOW; override re-acknowledged; Decision 1 framing fair.

- [MEDIUM] N-a replay discards re-filing items Michaël approved (approved machine restrictions, confirmed legacy-label mappings, hand filing of rows with no Capture context); treat human-confirmed items as deliberate decisions replayed as recorded; add a case.
- [MEDIUM] N-b P1's new 100.64.0.0/10 pattern matches the documented range in four committed artifacts, so item 1 cannot pass; ignore the literal range as documented.
- [LOW] N-c the "never mapped onto another checkout" rule needs facts the server lacks for nested worktrees; narrow it to "outside the session's checkout top level" and record the nested case as a known imprecision.

## Resolution (round 4)

All addressed in the round-5 candidate (plan SHA-256 `6334123a9cc4e7874d701834ed6d853629c8698edecb906913626aea57b24db3`).

- N-a → §12: every item of an earlier re-filing that exists only by human confirmation replays as recorded; only resolution-materializing items are re-derived; C35 adds "an approved machine restriction survives a later registry change and replay". (Codex raised the same point at HIGH in its round 4.)
- N-b → P1's address check runs separately and drops lines holding the documented literal `100.64.0.0/10`; dry-run of P1 on every `docs/plans/*.md` file with `P1_RULES_TEXT=1`: rc=0.
- N-c → §9: the write-time rule is "a path outside the session's checkout top level is never mapped onto it"; a nested linked worktree tokenizes as `${PROJECT_ROOT}/.worktrees/<name>/…`, a known imprecision.

---

# Round 5 (same subagent, delta review of plan SHA-256 `6334123a9cc4e7874d701834ed6d853629c8698edecb906913626aea57b24db3`)

Altitude report SHA-256: d03c3008dfa41a8cda4fdee10c142d383536776077867d872420657ee3f2f35a

Verdict as returned: **LGTM.** N-a, N-b and N-c resolved; nothing load-bearing left; override re-acknowledged; P1's greps re-run clean over every `docs/plans/*.md` file. Two new LOW points:

- [LOW] L-1 recorded targets of replayed items (`correct` moves, confirmed legacy mappings, hand filings) must follow a later `merge` redirect to the survivor (fails closed today, not a leak).
- [LOW] L-2 `grep -vF '100.64.0.0/10'` drops whole lines, so a line holding the literal and a real address passes; strip the literal before matching.

## Resolution (round 5)

Addressed in the round-6 candidate (plan SHA-256 `350a4bfcdcceef60c02541d51168b0f583a4019fcc32b44df5ba26c0e520d6b4`).
- L-1 → §12: a recorded target project resolves through any later `merge` redirect to its survivor.
- L-2 → P1 strips the documented literal per line with `sed`, then matches the rest; probes in the artifact dir's `scratch/`: literal + address → rc 1, non-`alice` home → rc 1, literal only → rc 0; P1 over every `docs/plans/*.md` with `P1_RULES_TEXT=1` → rc 0. Verification now requires that probe in item 1. (Codex raised the same point at MEDIUM in its round 5.)

---

# Round 6 (same subagent, delta review of plan SHA-256 `350a4bfcdcceef60c02541d51168b0f583a4019fcc32b44df5ba26c0e520d6b4`)

Altitude report SHA-256: 6015a1eb929eabafea72ebfc26523f03133a532cc39566f8464492ee7fb64ae8

Verdict as returned: **LGTM.** L-1 and L-2 resolved; no BLOCKER, HIGH, MEDIUM or LOW; override re-acknowledged; the new P1 address loop run over every `docs/plans/*.md` file reports no hit, and an inline probe holding the literal and an in-range address still flags. Noted without action: the `sed "s#^#$f:#"` prefix would misbehave on a file name containing `#`; no file P1 scans in this workflow has one.

## Resolution (round 6)

No changes required: LGTM, and the one note is not reachable in this workflow (P1's inputs are fixed names without `#`). Final reviewed plan SHA-256 `350a4bfcdcceef60c02541d51168b0f583a4019fcc32b44df5ba26c0e520d6b4`.

---

# Round 7 (same subagent, delta review of plan SHA-256 `2743211cb7d4bc1101aef20613607610d8a58ee7d84b304b1fe99129c65e1f88`, owner ruling B applied)

Altitude report SHA-256: 75ccddd42d5cde071ff9d8a31fac47bd41e478b47d85b4fb388c5520f52e73bb

Verdict as returned: not LGTM yet; one HIGH, one LOW. Option B applied cleanly, no option-A leftovers; override re-acknowledged.

- [HIGH] H-B1 the machine-qualified key form is never pinned: with label = encoded relative path, laptop `x` and mac `x` share one node and release 1 still joins; the `project` argument's semantics stay open. Fix: put the machine in the key with a separator no normalized remote key contains; align §5 step 2, C5, the §7 hint; item 5 checks the label form.
- [LOW] L-B1 Surfaced #1's "only reading that keeps release 1 a subset" for adopted labels holds only once H-B1 is fixed.

## Resolution (round 7)

Addressed in the round-8 candidate (plan SHA-256 `75ad966bf58e50f6f1afd2a639c074716761e2a69bb57aa16b65df2cd2989ee5`).
- H-B1 → Constraints pin path keys as `<machine>:<relative path>` (label `laptop%3Ax`), non-colliding by construction; §5 step 2 drops the ambiguity clause (a key names at most one project); C5, C21, C34 updated; §7 hint stated as advisory; item 5 checks the label form; relayed in Surfaced #2.
- L-B1 → holds now that labels differ per machine; no text change needed.

---

# Round 8 (same subagent, delta review of plan SHA-256 `75ad966bf58e50f6f1afd2a639c074716761e2a69bb57aa16b65df2cd2989ee5`)

Altitude report SHA-256: 8b027d88192d5cbb363ea8628bdb2faf7306da39cde260c6f0c2ba637d048465

Verdict as returned: **LGTM**; H-B1 resolved; strict subset against ADR-0020 as amended holds; override re-acknowledged. One LOW:
- [LOW] L-C1 "a normalized remote key never contains `:`" overstates it (`https://host/a:b` → `host/a:b`); collision is still impossible for a narrower reason (a path key's first `:` precedes any `/`; a remote key has none before its first `/`); say a path key splits at its first `:`.

## Resolution (round 8)

L-C1 → Constraints state the narrower invariant and the split at the first `:`; C21 adds a colon-in-path case. Round-9 candidate plan SHA-256 `21e8b3a50998918bdae0a70c1b1b8f2f394d57dc21f1d76674693b7bdb62160b`. (Codex raised the same point at MEDIUM.)

---

# Round 9 (same subagent, delta review of plan SHA-256 `21e8b3a50998918bdae0a70c1b1b8f2f394d57dc21f1d76674693b7bdb62160b`)

Altitude report SHA-256: db422fdb638f5215205a0d6ff22442bf6cb877f47341331d64ea3b1f8050f3e0

Verdict as returned: **LGTM**; L-C1 resolved; nothing at any severity remains; override re-acknowledged.

## Resolution (round 9)

No change for this reviewer's findings. Codex's round-9 MEDIUM (a bracketed IPv6 remote host has colons before its first slash) is fixed in the round-10 candidate (plan SHA-256 `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a`).

---

# Round 10 (same subagent, delta review of plan SHA-256 `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a`)

Altitude report SHA-256: 5d24fd5c194894b0cb005b1c9e86bd85cde4ad58f97506c48656d2833f037848

Verdict as returned: **LGTM**; the IPv6 correction is right (the reviewer notes it missed the point in round 9); disjointness now holds by the first character; C21 pins all three key shapes; `2001:db8::/32` is the documentation range and P1 does not match it; no findings at any level.

## Resolution (round 10)

No changes required. Final reviewed plan SHA-256 `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a`.
