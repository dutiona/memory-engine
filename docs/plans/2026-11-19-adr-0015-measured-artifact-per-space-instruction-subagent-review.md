# Clean-slate subagent review (Claude Opus, general-purpose, no prior context), rounds 1-5

## Round 1

**ADR-0015 plan review: §5/§6 lockstep amendment (dutiona/memory-engine#1078, dutiona/knowledge-base#607)**

Altitude report SHA-256: 2506ba2d21eda083c9ed53ca1b8aabb7d05d13bcd34b58b69d68b5c745ae185f

**Verdict: not LGTM. There is one blocker, in block 3's shared §6 paragraph. Everything else holds or needs small fixes.** Confidence is high: I read every cited code path in both snapshots.

**Altitude.** I accept the report's `proceed`. Its plan digest (5b442d86…) matches `.review/plan.md`, and its measurements match the file (291 lines, 9 tasks, 6 fences, 65 fenced lines). The length comes from approved bytes, claim-to-code evidence, the owner's decision record and the gate lists the owner requires. No product file is duplicated.

**What I checked, without running any project gate.** I applied the three blocks to the ADR in memory and wrote no file. Results:
- The diff is 28 lines added and 9 deleted, matching item 4.
- All three blocks are found, and the §5 sentence check passes.
- KB's `^\| Knowledge layer \| `(.+)` \|$` still matches the unchanged table row. `**Knowledge layer.**` cannot match it.
- No new line is over 90 columns except the Status line, and `§` is the only non-ASCII character.
- Neither repository has a pre-commit or Markdown formatter that could reflow the file.
- The leak grep finds nothing in the plan.
- I did not run Sphinx or pytest.

**§5 is word for word.** It matches the section "ADR-0015 §5 sentence to fill" in dutiona/infra#140's PR body, joined across its wrapping. #140 was opened and merged on 2026-11-17 (UTC). "4 parallel slots" matches the plist's `--parallel 4`. Block 1's date convention also holds: #1017's entry says 2026-11-16, and its PR (#1063) merged on 2026-11-17.

---

### [BLOCKER] Block 3's shared paragraph says two things ME's code contradicts, and leaves "recorded with the space" out of the ruling it credits to Michaël

Here is each block 3 sentence against both code bases (paths relative to the review tree):

| Sentence | KB | ME |
|---|---|---|
| "The query instruction is per-space configuration (owner's decision…)" | True: `db.py:642-645`, `migrate.py:65` | **False.** It is per-client: `http.rs:219` sets it at construction; `mcp/src/config.rs:219-222` and `cli/.../embedding_args.rs:228-229` each configure it separately. No backend schema holds it (no `instruction` anywhere in `me-backend-sqlite` or `me-backend-postgres`). The plan's own *Reported to Michaël* item 1 says two clients of one store can differ. |
| "The embedding client prepends **the queried space's** instruction" | True: `search.py:295-297` → `embeddings.py:628-640` | **False:** it prepends the client's own instruction (`http.rs:878-896`). |
| Rest of the shared paragraph, the Memory-layer paragraph, the Knowledge-layer paragraph | True | True: `embed_query` callers are only `mcp/src/tools/recall.rs:109` and `cli/src/commands/query.rs:120`; `fingerprint()` and `EmbeddingFingerprint` (`me-types/.../cognitive.rs:182-198`) have no instruction field; `asymmetric_embedding.rs:211` tests the pinned default with model `"m"` |

The ruling in Constraints reads "per-space configuration **recorded with the space**". Block 3 drops that clause but still marks the sentence "(owner's decision, …)". The ADR would then credit Michaël with a weaker rule than the one he gave. It would also hide, in the permanent record, that ME departs from it; only PR bodies would say so.

The Memory-layer paragraph states a deployment duty for gap 2 (a different model must opt out). It states nothing for gap 1 (clients agreeing, and resetting the instruction after a reconstruction). The evidence table has no row for "per-space" or "queried space's", which is how both claims got past it.

Replacement for block 3's first two paragraphs. All lines are at most 90 columns, ASCII plus `§`. The Knowledge-layer paragraph stays as it is.

```
Asymmetric models embed queries with an instruction and documents without one. The query
instruction is per-space configuration recorded with the space (owner's decision,
2026-11-17, on dutiona/knowledge-base#573's plan dutiona/knowledge-base#606): a
Qwen3-Embedding space carries its store's pinned instruction, verbatim below, unless
configured otherwise, and a space of another model carries none or its own; the Memory
layer departs from this as its paragraph states. The embedding client prepends the
instruction, if any, to each query; callers never build it. Documents never carry an
instruction, so document vectors stay comparable across stores. The instruction is
policy, not identity: the identity tuple (§1) leaves it out, so no mismatch check
compares it, and changing it re-embeds nothing.

**Memory layer.** The store does not record the instruction: it is the embedding client's
configuration (`query_instruction`), so a deployment gives every client of one store (the
MCP server, the CLI) the same instruction and sets it again when a reconstruction (§4)
changes the model. Unset, it is the pinned instruction whatever the model, so a
deployment of another model sets it to `""` (none) or to its own.
```

With this text, *Reported to Michaël* should say "§6 states the ruling whole and ME's departure from it" in place of its current description. It should also add two evidence rows: "per-space, recorded" (KB only; ME is the departure) and "client prepends" (ME `http.rs:878-896`).

### [MEDIUM] The ruling's scope is unclear, and it decides which text is right

In #606, Michaël's own words are "I agree with your concerns and what you'll tell the worker". The ruling written there reads "a bge-m3 space gets none (or its own), Qwen3-Embedding spaces get **KB's** pinned one". That is KB-scoped. Applying it to both layers comes from the issue text in #1078 and #607.

Ask Michaël whether the ruling binds ME:
- **If it does**, use the replacement above.
- **If it does not**, ME does not depart from anything. The shared sentence becomes "In the Knowledge layer, the query instruction is per-space configuration recorded with the space (owner's decision, …)", the "departs" clause goes, and *Reported to Michaël* loses its premise.

### [MEDIUM] Item 8 and the final-HEAD verification cannot run as written

- If Michaël approves a text change during review, item 8 changes the ADR and re-runs items 4-6. But item 4's check compares the ADR against the plan's blocks, so it stays red until the plan's block is changed too. Add: change the block in both worktrees with the same bytes, commit `docs(plans): …`, re-hash, and get Michaël's OK on the new digest. Only then change ME, `cp` to KB, and re-run.
- `/finish-pr` Item 5 deletes `docs/plans/`. After that, the Verification check reading `"$PLAN"` fails because the file is gone. State that this check runs before Item 5, and that only the parity `diff` and `sha256sum` run at the final HEAD.

### [MEDIUM] The stale five-field table is put off with no owner

ME's `docs/design/embedding-identity-and-tei-qwen-migration.md:70-79` is labelled "canonical, both layers" but lists five of the nine fields. Your boy-scout rule says to fix it in its own commit, and the plan instead pushes it to "a separate ME change" with no issue filed. Move it into *Reported to Michaël* with a recommendation: a separate `docs(design)` commit on ME's branch. It doesn't touch the ADR, so parity and the approved-text rule are unaffected. Item 4's numstat expectation and the Documentation section's "no other file changes" would need adjusting. Let him decide.

### [LOW]

1. *Reported to Michaël*: "Until then no ME deployment can hit the gap" claims too much. Gap 1 doesn't depend on the model: a CLI `query --query-instruction ''` against the Qwen store diverges from the MCP server today. Suggested wording: "No current ME deployment hits either gap: every client leaves the instruction unset and serves §5's model."
2. Evidence row "changing it re-embeds nothing", KB cell: "NULL default; PR #612" does not support the claim. KB has no operation that changes a recorded instruction; it is set by `create_space` and carried over by a same-model `re_embed` (`embed_swap.py:506-507`). Cite `tests/test_embed_identity.py:174`, which updates the column and then searches with no re-embed. The ADR text itself is true.
3. Optional precision: "Unset, it is the pinned instruction" is true of ME's HTTP client. A library user's own provider defaults to no instruction (`me-traits/src/lib.rs:94-96`). "the embedding client" is fine if the ADR is about deployments.
4. Item 4: `grep -c '^[-+]|'` prints 0 but exits 1. Note that exit code so it isn't read as a failure.
5. The leak grep misses a bare tailnet MagicDNS label without `.ts.net`. Add `\btail[0-9a-f]{6,}\b`.
6. Re-run item 1's open-PR check (`gh pr list … --json files`, add `--limit`) at item 8, before READY. A concurrent ADR-0015 PR in one repository would otherwise only show up after merge, at item 9.
7. The Documentation re-read list misses KB `docs/usage/embedding-spaces.md:66` and the `routes/embeddings.py:75-77` docstring. Both agree with the amended text.

### What holds

- **Sections:** Documentation, Testing and Verification are all present.
- **Closing keywords:** only the same-repository `Closes #N`, nothing else, negated or not.
- **KB's test:** safe.
- **Sphinx:** the new text adds no new MyST construct and no heading changes.
- **Leaks:** the plan is clean.
- **Order:** mirroring by `cp` and committing with the same message is sound.
- **Gate lists:** they match both AGENTS.md "Verify" blocks.
- **Status line and §5 text (blocks 1-2):** both can be approved as written.

## Round 2

**ADR-0015 lockstep plan, round-2 review of the r2 delta**

Altitude report SHA-256: a84db723747e6f2a7f13ae98fc945cdc4c1495ea5d7b02053b15abccd5e8e207

**Verdict: LGTM.** I have two optional LOW suggestions and nothing that blocks approval. Confidence is high: I checked each new sentence against both snapshots, and I applied the blocks to the base files in memory without writing any file.

**Altitude: `proceed` acknowledged.** The report's own figures are correct:
- Its plan digest, 0f4b532e…, matches `.review/plan.md`.
- The plan is 324 lines and 24,219 bytes, with 7 fences (71 body lines) and 9 tasks.
- `.review/plan-delta-r2.diff` is exactly the change from `last-reviewed-plan.md` to `plan.md`.
- The 33 added lines come from review findings. None of them duplicates or replaces a product file.

**Mechanical checks, all passing:**
- Both ADR copies hash to the base digest `b4b6e60f…`, as item 1 requires.
- Applying the blocks gives 31 lines added and 9 deleted, matching item 4.
- All three blocks are found in the result, and the §5 sentence is still word for word.
- KB's `^\| Knowledge layer \| `(.+)` \|$` still matches the unchanged row.
- No new line exceeds 90 columns, and `§` is the only non-ASCII character.
- Item 6's note produces exactly 4 added lines (3 note lines plus 1 blank). Its widest line is 89 columns. The file has no `superseded by ADR-0015` today, so item 6's red check returns 0 as the plan expects.
- The revised leak grep finds nothing in the plan, the resolution file or the altitude report.

### Block 3, sentence by sentence (ME at e14b188, KB at 2ccc5d3)

| Sentence (current plan lines 82-104) | KB | ME |
|---|---|---|
| Ruling stated whole: "per-space configuration recorded with the space … another model carries none or its own" | True: `migrate.py:65`, `db.py:642-645`, `embeddings.py:38-40` | The departure is now stated, so nothing here is claimed of ME |
| "The Memory layer departs from this rule as its paragraph states." | n/a | True |
| "The embedding client prepends the instruction, if any, to each query; callers never build it." | True: `embeddings.py:628-640`, `search.py:295-297` | True: `memory-engine-embed/src/http.rs:878-896`; the only callers are `mcp/src/tools/recall.rs:109` and `cli/src/commands/query.rs:120` |
| "Documents never carry an instruction, so document vectors under one identity stay comparable across stores." | True: `ingest.py:126-128`, `embed_swap.py:228,272`, `folder_summaries.py:119,123` | True: `embed` and `embed_batch` add no prefix |
| "the identity tuple (§1) leaves it out … changing it re-embeds nothing" | True: `settings.py:67-77`; `test_embed_identity.py:174` updates the column and searches with no re-embed | True: `me-types/.../cognitive.rs:182-198` has no instruction field |
| Memory layer: "The store does not record … each embedding client's configuration (`query_instruction`)" | n/a | True: no `instruction` in `me-backend-sqlite` or `me-backend-postgres`; set separately in `mcp/src/config.rs:219-222` and `cli/.../embedding_args.rs:228-229` |
| Memory layer: "gives every client … the same instruction and sets it again when a reconstruction (§4) changes the model" | n/a | These are deployment duties, not claims about the code; `engine/reconstruct.rs:129-133` uses the embedder the caller passes in |
| Memory layer: "Unset, the HTTP client uses the pinned instruction whatever the model" | n/a | True: `http.rs:219`; tested with model `"m"` at `asymmetric_embedding.rs:211` |
| Knowledge layer (unchanged) | True | n/a |

§6 now states the owner's ruling whole and says nothing that ME's code contradicts. Not taking Codex's "Other models default to no instruction" was correct: stated as the shared default, it would be false of `http.rs:219`. The scope question I raised in round 1 is handled. *Reported to Michaël* quotes #606's KB-only wording and tells him not to approve if he meant the ruling for KB only.

The Block 2 change ("against `artifact_sha256` above") is true and removes the old ambiguity with the commit hash that comes right before it.

**Item 6's design-doc note is true.** The canonical tuple has nine fields: ME's five fields plus the `served` artifact, which holds the four served fields (`cognitive.rs:182-198`), and KB's `IDENTITY_FIELDS` lists nine. Line 80 of `docs/design/embedding-identity-and-tei-qwen-migration.md` is the blank line after the table's last row, and Sphinx includes the file (`docs/design/index.md:10`).

### [LOW] Optional suggestions

1. **Put the expected digest in the plan instead of recording it later.** The amended ADR is fully determined by the base digest plus the three blocks. Applying them in memory gives `586ea4c89cabbe2f2dc7fa1be1162aa83be28562cc5bb48ec35260e5d81d00cd`; confirm it against your own trial copy. Writing it into the plan would make Michaël's approval cover the whole file, not only the blocks and the line counts. Item 4 would then end with "`sha256sum "$ME/$ADR"` prints `586ea4c8…`", and `APPROVED_ADR_SHA` would be that value rather than something recorded after approval.
2. **ME's operator docs don't state the new duty that clients must agree.** `docs/reference/mcp-server.md:76` covers the server's override and `docs/reference/cli-inspector.md:101` covers the CLI's, each on its own. Nothing contradicts the ADR, and the default meets the duty: every client left unset uses the pinned instruction. If you want the duty visible where an operator changes the setting, append this to `mcp-server.md:76`, as a separate ME commit: "A CLI `query` against the same store takes the same `--query-instruction` (ADR-0015 §6)." Leaving it out is defensible under "less is more".

## Round 3

**ADR-0015 lockstep plan, round-3 review of the r3 delta**

Altitude report SHA-256: 366ec3030d08ee599c2aa168f0409a184e72ce4b9876ff83f555d05718429525

**Verdict: LGTM.** The two rewritten Block 3 paragraphs are true of both code trees, and the pinned `APPROVED_ADR_SHA` is correct. Three LOW wording fixes remain, none of them in the ADR text. Confidence is high: I recomputed the digest in memory and wrote no file.

**Altitude: `proceed` acknowledged.**
- The report's plan digest (f7a6ccfd…) matches `.review/plan.md`.
- The plan is 332 lines and 25,219 bytes, with 7 fences (71 body lines) and 9 tasks, as the report says.
- `.review/plan-delta-r3.diff` is exactly the change from `last-reviewed-plan.md` to `plan.md`.
- The 8 lines added since round 2 hold a pinned digest and a rewritten sequence, not duplicated content.

**`APPROVED_ADR_SHA` is correct.** I applied the three blocks in memory to each base copy: ME and KB both hash to `b4b6e60f…` and use LF line endings. I first checked that the replaced lines sit where the plan says (3, 148-153, 157-161). Both copies then hash to `2a13502a3a24211bfff7e2627faf4963bd1acd05ba1db024587474cddf080b1b`, the value pinned in the plan. Other checks on the result:
- 31 lines added and 9 deleted.
- All three blocks are found, and the §5 sentence is still word for word.
- KB's `^\| Knowledge layer \| …` regex still matches the unchanged row.
- No new line is over 90 columns, and `§` is the only non-ASCII character.
- The leak grep finds nothing in the plan or in `resolution-r2.md`.

**Block 3, the rewritten paragraphs, checked against the code**

| Sentence | Code | Verdict |
|---|---|---|
| "Each embedding client reads `query_instruction` from its own configuration, and the store does not record it." | ME: MCP `mcp/src/config.rs:66,126-129,219-222` (TOML, flag, env); CLI `cli/.../embedding_args.rs:54,228-229` (flag); no `instruction` in `me-backend-sqlite` or `me-backend-postgres` | True |
| "A deployment therefore gives every client … the same instruction and revisits it when a reconstruction (§4) changes the model." | These are deployment duties, not claims about the code; `engine/reconstruct.rs:129-133` uses the embedder the caller passes in | Says nothing the code contradicts |
| KB: "NULL means its model's default, the pinned instruction when the model name contains `qwen3-embedding` (ignoring case) and none otherwise; `""` means none; any other text is the space's own." | `embeddings.py:38-40` (`"qwen3-embedding" in model.lower()`), `db.py:642-645` (only NULL falls back to the default; `""` and any text are returned as recorded) | True, and exact |

**Item 8's successor order matches `address-issue/scripts/state.sh`.**
- `supersede-plan` requires an approved predecessor, a different successor path (so the `-v2` key works) and its evidence from the plan review.
- It then clears `plan_issue_url`. That is why the successor plan issue has to be published after `supersede-plan` and before `approve-plan --plan-issue-url`, which is the plan's order.
- Checking the old expected digest, then replacing the block by exact match, then checking the new digest is sound.

### [LOW]

1. **Item 8, plan line 246, still names the old plan issue.** Line 243 now says "`/finish-pr` archives on the active plan issue", but line 246 says "Item 5 archives on item 2's plan issue". After a successor, that is the wrong issue. Replace with: "Item 5 archives on the active plan issue (item 2's, or the successor's) and removes `docs/plans/` in one commit".
2. **The exposure sentence in *Reported to Michaël* uses "none" in a way that can be misread.** It says the deployed server "sets none". In §6, "none" means `""`, which would mean the server sends no instruction, the opposite of what happens. The dutiona/infra quadlet leaves the setting unset, and its deploy test (`test_memory_deploy.py:305`) keeps it unset, so the server uses the pinned instruction. Replace with: "the deployed ME server leaves it unset, so it uses the pinned instruction (dutiona/infra's `memory-engine.container`, kept unset by its deploy test)".
3. **Cosmetic: plan line 245 has no indent.** It begins "two PR numbers, each in its own repository)…" at column 0. Markdown still treats it as part of the list item, but indent it two spaces to match the rest. While editing item 8, it is also worth saying that the successor plan and its issue body get the same leak check and read-through that item 2 gives the first plan.

## Round 4

**ADR-0015 lockstep plan, round-4 review of the r4 delta**

Altitude report SHA-256: a461be532dd33e060ec99f25cad48ff055090837d6e5e4c3305bdcb6c1e5aabd

**Verdict: LGTM.** All five round-3 findings are fixed correctly in the delta, and the ADR text and its pinned digest have not changed. One optional LOW remains, in item 8's successor path.

**Altitude: `proceed` acknowledged.**
- The report's plan digest (95be6d15…) matches `.review/plan.md`.
- The plan is 339 lines and 25,695 bytes, with 7 fences (71 body lines) and 9 tasks, as the report says.
- `.review/plan-delta-r4.diff` is exactly the change from `last-reviewed-plan.md` to `plan.md`.
- The growth is 7 lines, all in item 8's contingency path. Nothing in the delta duplicates content.

**What I checked in the delta:**
- **ADR text unchanged.** All four fenced deliverables (the three `text` blocks and the `markdown` note) are byte-identical to round 3. So `APPROVED_ADR_SHA` = `2a13502a3a24211bfff7e2627faf4963bd1acd05ba1db024587474cddf080b1b`, which I recomputed in round 3, still holds.
- **Exposure sentence is true.**
  - dutiona/infra's `memory-engine.container` sets no `MEMORY_MCP_EMBED_QUERY_INSTRUCTION` and no `MEMORY_MCP_CONFIG`.
  - `hosts/dutiona/tests/test_memory_deploy.py:305` asserts that neither key is set.
  - With no setting, `http.rs:219` makes the server use the pinned instruction.
  - The "none" wording that could be misread is gone.
- **Archive target is fixed.** Item 8 now archives on "the plan issue recorded for the active approval (item 2's, or the successor's)". This agrees with item 2 (line 189), and the stale "item 2's plan issue" is gone.
- **Successor order still matches `state.sh`.** The steps run in this order:
  1. `supersede-plan`, which requires a different path and clears `plan_issue_url`.
  2. The leak check and read-through, then the successor plan issue is published.
  3. `approve-plan --type github-comment`.
  4. Only then are the ADRs changed: old digest checked, block replaced by exact match, new digest checked, approved-text check run against the successor.
  5. Commit and push in both worktrees, read the heads back, update the PR bodies, rebind the review targets, re-run `/super-review`.
  6. At the final HEAD, both ADRs must match the active plan's digest.
- **Layout fixed.** The sub-bullets and the "Before READY" paragraph are properly indented under item 8.
- **Leaks.** The leak grep finds nothing in the plan or in `resolution-r3.md`.

### [LOW] Item 8: say the successor carries all three blocks (optional)

The approved-text check fails unless it finds exactly three `text` blocks (`len(blocks) != 3`). "carries the new block" could be read as a successor holding only the changed block. Replace with: "carries all three blocks (the changed one replaced) and its new expected ADR digest, and is reviewed by `/super-plan`". A versioned successor is normally a full plan anyway, so this only removes a possible misreading.

## Round 5

**ADR-0015 lockstep plan, round-5 review of the r5 delta**

Altitude report SHA-256: d48ba36512a55fbe8715f794a04f9bb6acbc92d489bfca7e0b7f481394724de1

**Verdict: LGTM.** The delta only changes prose and is correct. I have no findings.

**Altitude: `proceed` acknowledged.**
- The report's plan digest (27da3da7…) matches `.review/plan.md`.
- The plan is 340 lines and 25,734 bytes, with 7 fences (71 body lines) and 9 tasks, as the report says.
- `.review/plan-delta-r5.diff` is exactly the change from `last-reviewed-plan.md` to `plan.md`.

**Round-4 arbitration accepted.** `resolution-r4.md` says plan-audit only accepts a complete versioned successor, never a partial one. That fully covers my optional LOW, and I don't raise it again.

**Delta checks:**
- **Deliverables unchanged.** All four fenced deliverables are byte-identical to round 4: the three `text` blocks and item 6's `markdown` note. `APPROVED_ADR_SHA` = `2a13502a…080b1b`, which I recomputed in round 3, still holds.
- **The three re-qualified references are correct.**
  - `dutiona/knowledge-base#612` is the KB PR that merged on 2026-11-17 with merge commit `a518ab9` ("feat(embeddings)!: enforce the shared ADR-0015 identity…"), which is what the Constraints line says.
  - `dutiona/memory-engine#1017` and `dutiona/knowledge-base#606` name the right repositories.
- **No bare `#N` left in prose.** I stripped fenced blocks, then code spans (including ones that wrap across lines), then searched for issue references. None remain. The references left in the Constraints quotes and in `Closes #1078` / `Closes #607` sit inside code spans, and GitHub does not turn code-span references into links.
- **Leaks.** The leak grep finds nothing in the plan or in `resolution-r4.md`.
- **Rewrapping.** The two rewrapped lines only change where lines break; the words are unchanged.

## Resolution

Five rounds (plan digests 5b442d86, 0f4b532e, f7a6ccfd, 95be6d15, 27da3da7). Final plan SHA-256
27da3da724cf0c8603096ce1844ca56f43ddebfaffd7165ecef6f110e16fe162; final altitude report SHA-256
d48ba36512a55fbe8715f794a04f9bb6acbc92d489bfca7e0b7f481394724de1. Round 5: Codex LGTM, agy LGTM, subagent LGTM, no
findings.

Round 1 (Codex 4 HIGH / 4 MEDIUM / 1 LOW; agy LGTM; subagent 1 BLOCKER / 3 MEDIUM / 7 LOW):
- [BLOCKER/HIGH] Block 3 overclaimed for ME and dropped "recorded with the space" → the shared paragraph states the
  ruling whole, adds "The Memory layer departs from this rule as its paragraph states.", says "prepends the
  instruction, if any", "document vectors under one identity"; the Memory-layer paragraph states the deployment's
  duties for both gaps. Codex's "Other models default to no instruction" not taken: false for ME's code as a shared
  default; the departure sentence carries the gap.
- [MEDIUM] Ruling scope (KB-scoped in #606) → *Reported to Michaël* says so and offers a KB-scoped successor.
- [HIGH] Approval record / post-approval text change → item 2 records each marker with `approve-plan --type
  github-comment`; item 8 routes a text change through a successor plan.
- [HIGH] False post-merge parity → item 9 checks each blob read and compares digests with `APPROVED_ADR_SHA`.
- [MEDIUM] Rebase and line numbers → the base ADR digest is pinned; a moved base stops for Michaël.
- [MEDIUM] Approved-text check after Item 5 → runs before Item 5; final HEAD runs parity plus digest.
- [MEDIUM] Tesseract → required; absent = INCOMPLETE.
- [MEDIUM] Leak check → read-through before every publication; grep is a supplement with stated exit codes and wider
  patterns.
- [MEDIUM] Stale ME design-doc table → item 6, an ME-only boy-scout commit with exact text.
- [MEDIUM] Reconstruction/recommendation wording → adopted.
- [LOW] `artifact_sha256` in Block 2; evidence rows; grep exit note; open-PR re-check; KB re-read list → adopted.
  Subagent LOW 3 (library providers) covered by "the HTTP client".

Round 2 (Codex 2 HIGH / 1 MEDIUM / 1 LOW; agy LGTM; subagent LGTM + 2 LOW):
- [HIGH] Item 8 successor order → rewritten in the state machine's order (supersede-plan, successor plan issue,
  approve-plan; then exact-match replacement).
- [HIGH] Open-PR re-check would match the pair's own PRs → excludes only this pair's two PR numbers.
- [MEDIUM] Memory-layer paragraph read as current behaviour; unproven deployment claim → paragraph opens with what the
  code does and states agreement as a duty; the claim now cites dutiona/infra's quadlet and deploy test.
- [LOW] Knowledge-layer default → stated by model name containing `qwen3-embedding`, ignoring case.
- [LOW] Pin the expected ADR digest → `APPROVED_ADR_SHA` = 2a13502a3a24211bfff7e2627faf4963bd1acd05ba1db024587474cddf080b1b,
  confirmed independently by the subagent and Codex.
- [LOW] Operator-doc sentence in ME's mcp-server.md → not taken: nothing contradicts the ADR, the default meets the
  duty, the pair stays minimal; reported as optional.

Round 3 (Codex 2 HIGH / 1 LOW; agy LGTM; subagent LGTM + 3 LOW):
- [HIGH] Archive target after a successor → "the plan issue recorded for the active approval".
- [HIGH] Successor handoff skipped commit/push → commit, push, read back heads, update bodies, rebind, re-review.
- [LOW] "sets none" misreadable → "leaves it unset, so it uses the pinned instruction".
- [LOW] Successor publication hygiene; indentation → adopted.

Round 4: [LOW] (subagent, optional) "carries the new block" could be misread as a partial successor → arbitrated, not
applied: plan-audit requires a versioned successor to be "a complete plan, not a diff", and applying it would void
three fresh LGTMs over a contingency path for a wording that cannot change the outcome. The subagent accepted the
arbitration in round 5.

Round 5 (revision 5, found by the worker while dry-running the plan issues): three bare issue references in prose
repository-qualified (dutiona/knowledge-base#612, dutiona/memory-engine#1017, dutiona/knowledge-base#606), two lines
rewrapped; ADR blocks and `APPROVED_ADR_SHA` unchanged. All three reviewers LGTM with no findings.

Degradations: `advisor()` unavailable in this harness (Full ran as Codex + agy + subagent).
