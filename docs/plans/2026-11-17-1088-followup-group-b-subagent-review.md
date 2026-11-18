**Plan review: chore/1088 follow-up group B**

Altitude report SHA-256: 017b6e7018fdd3b0e259765b53983f6493dcc8e3db55a8a612bacd824ea13080

**Verdict:** not LGTM as written, but close. There are 2 HIGH, 3 MEDIUM and several LOW findings, and no BLOCKER. Every fix is a local edit to the plan. Confidence is high: each finding below was checked against the code at 9b9d676, the rusqlite 0.43.0 source and the bundled SQLite 3.54.1 source. I ran no builds or tests.

### Altitude and duplication (assessed first)

- I checked the altitude report against the plan. The plan SHA-256 `7f1e3bae…a2d9e` matches, and so do its 254 lines and 22,365 bytes. Its "proceed" verdict holds.
- The only literal block is the #1071 text that goes verbatim into four files. That block is the product itself, so embedding it is justified, and I accept that exception.
- The Rust work is described as rules, not code. The Documentation and Testing sections summarise the item list rather than copying it.
- The #1071 block will exist in four places (AGENTS.md, CLAUDE.md, GEMINI.md, SKILL.md). That follows the repository's existing gate-matrix convention, which `gate_matrix_parity` already relies on.

### HIGH

**H1. Item 1 (#1067): the `malformed_metadata_under_a_valid_query_is_a_storage_error` test cannot be set up as written.**
- `facts.metadata` is declared `TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata))` (`crates/me-backend-sqlite/src/store/schema/ddl.rs:38`).
- So `UPDATE facts SET metadata = '{'` fails with a CHECK-constraint error. The test fails at its setup `unwrap`, not on the `Ok` the plan expects. That is a false red, and the test could never go green.
- The fix already exists in the codebase: `crates/me-backend-sqlite/src/store/facts.rs:1756-1774` turns on `PRAGMA ignore_check_constraints` to simulate on-disk tampering. The plan should name that pragma.
- The decision table's reason for the probe also needs rewording. Bad metadata cannot be written through the library; it only appears through tampering, an outside writer, or a backend without the CHECK. That is exactly the fault class #366 treated as worth surfacing, so the probe is still justified (see Over-engineering).

**H2. Verification step 3: the "clean export" fails by construction.**
- `git archive HEAD docs pyproject.toml` leaves out `crates/`.
- `docs/design/embedding-identity-and-tei-qwen-migration.md` already links to `../../crates/memory-engine/src/engine/ingest.rs` and `../../crates/memory-engine-mcp/src/tools/mod.rs`. Item 12 change 5 repoints seven more links into `../../crates/…`.
- MyST turns a relative link to an existing non-document file into a download link, and a missing target into a `myst.xref_missing` warning. In a docs-only export all nine targets are missing, so `-W` exits 1.
- A clean checkout is the whole tree. Use `git archive HEAD | tar -x -C <dir>`. Otherwise the implementer may "fix" it by unlinking the code references.

### MEDIUM

**M1. #1046: the new `-W` gate runs on unlocked docs dependencies.**
- `uv.lock` is gitignored (`.gitignore:46`) and pyproject only sets floors (`sphinx>=8.0`, …).
- So a new Sphinx, MyST, rtd-theme or mermaid release can add a warning and turn the gate red with no change in this repository. That is the "test that breaks because upstream shipped a release" case Michaël's rules call a broken test.
- The ideal fix is to commit `uv.lock` and run `uv run --locked` in the gate. `.readthedocs.yaml`'s `uv sync` would then be locked too.
- This reverses an earlier decision to ignore the lock, whose reason I don't know. Ask Michaël, or record the risk as accepted in the PR.
- Related: `.readthedocs.yaml` and `docs/Makefile` already build with `-W`, so Read the Docs is red today and this PR turns it green. Worth saying in the PR.

**M2. Item 9 (#1041): the drift check can pass silently.**
- The rule handles a variant line ending in `,` and one ending in `{`, but says nothing about anything else.
- If rustfmt ever wraps a long tuple variant (`Foo(` … `),`), an implementation that just skips the line drops that variant from the enum side. If its table row was also forgotten, the check passes.
- The repository's own `gate_matrix_parity.rs` header says a guard with blind spots "fails open". Specify that any line starting with four spaces and an uppercase letter that ends in neither `,` nor `{` makes the test panic.

**M3. Item 12, change 8 (`myst.header`): no rule for closing #1046 if suppressed warnings exist.**
- #1046's second acceptance criterion says warnings are fixed at their cause, "not suppressed globally".
- If step 0 counts more than zero hidden warnings, keeping `suppress_warnings` does not meet that criterion. The plan reports the count but still writes `Closes #1046`.
- Make the close depend on Michaël's decision: either fix the headings, or have him rule the old suppression out of scope.
- The anchor-change worry is also weaker than stated. MyST slugs come from heading text, not level, so only headings that cross level 3 (`myst_heading_anchors = 3`) gain or lose an anchor.

### LOW

- **Item 9 mutation proof.** `MemoryError` derives `thiserror::Error` with no enum-level attribute, so a bare `Dummy,` fails to compile with a missing-`#[error]` error. That is a red, but not the drift assertion. Use `#[error("dummy")] Dummy,`.
- **Item 9 table parsing.** The line after "`MemoryError` variants:" is blank, and the header and separator rows have no backticks. The rule should say to skip leading blank lines and rows without a backticked first cell. It fails loudly today, so this is wording only.
- **Item 9 output.** The derived first cell for `EmbeddingModelMismatch` will show `Box<crate::types::EmbeddingFingerprint>`, a path inside me-types, in public docs. Either accept that or strip `crate::` on both sides of the comparison.
- **Item 4 (B3).** `crates/memory-engine-embed/tests/proposer_integration.rs` contains no `.unwrap()` (it uses `expect`). Adding the allow to all four test files leaves one that does nothing; add it to three.
- **Item 10 (B4).** Both claims the plan says to "check" are wrong today:
  - `PersistenceClassifier` is re-exported at the crate root (`crates/memory-engine/src/lib.rs:190`).
  - `serialize_embedding` and `deserialize_embedding` are not; they live at `me_backend_sqlite::store` (`crates/me-backend-sqlite/src/store/mod.rs:32`).
  - The Traits line lists only `EmbeddingProvider`, while the root re-exports about 15 `me_traits` items.
  - State the corrections outright instead of leaving them conditional.
- **Item 3 green check.** `git grep -n -e CycleAnomaly -e empty_enums` also matches the plan file once `docs/plans/` is committed. Use `-- '*.rs'` or exclude `docs/plans`.
- **Item 5 commit subject.** The commit also moves me-backend-sqlite and embed onto the workspace table. "move the fuzzing cfg into the workspace lint table" would describe it better.
- **Item 12, gate step.** `CONTRIBUTING.md:38` still has the non-`-W` Sphinx command; update it alongside CLAUDE.md's Commands line. AGENTS.md also says `ci.yml` "runs these same commands", which stops being true for the new `uv run` line. Add a "local only" note or a docs step in `ci.yml`.
- **Step 0.** Run `git fetch origin` before `git log HEAD..origin/main -- …fts.rs`, or the check compares against a stale ref.
- **PR body.** Explain 31 warnings (#1046) versus the 26 measured, and the 9 orphans in #960 versus 6 left: three `reference/*` orphans were already added to a toctree.
- **#1071.** Dropping `--cpuset-cpus` departs from the issue's acceptance text. Make sure Michaël's plan approval explicitly covers it, since only #1038 = DELETE is recorded as confirmed.

### Over-engineering questions

**Probe versus code-only rule (#1067): keep the probe.** Error code 1 really does arise outside the MATCH string:
- the filter's `json_extract`/`json_type` on tampered metadata;
- a bad JSON path, for example a consumer metadata key containing a NUL byte;
- any future runtime SQL function added to the filter.

A code-only rule would turn each of these into a silent empty result, which breaks #1067's first acceptance criterion. The probe runs only after a failure and is about 10 lines. Its logic is sound: a MATCH string that fails on its own cannot match anything, so calling that "no match" is never wrong.

I also checked the error codes it relies on:
- An FTS5 syntax error reaches rusqlite as `SqliteFailure` with code 1: the error surfaces at the step, and `query_map` and `query_row` don't step until rows are read (`statement.rs:274/370`, `row.rs:215-230`).
- Extended result codes are on (`inner_connection.rs:87`).
- Index corruption yields `SQLITE_CORRUPT_VTAB` (267) in 3.54.1.
- A prepare-time error arrives as `SqlInputError`, not `SqliteFailure`, so a probe whose prepare fails correctly falls through to a fault.

**Text-derived drift test (#1041): sound against today's `error.rs`.**
- The rule derives all 20 variants correctly: single-line struct variants such as `EmbeddingDimension`, `UnsupportedEpoch` and `IndexInconsistent`; multi-line ones with `///` lines such as `EmbeddingModelMismatch` and `SnapshotTooLarge`; and the `#[from]` stripping.
- rustfmt's `struct_variant_width` (35) fixes both layouts. No string line inside an `#[error]` attribute sits at four-space indent.
- It is worth its cost, because payload drift was the defect #1041 found. It needs M2 and the parsing LOWs fixed.

**The six boy-scout commits are all real rot.**
- B1: stale `fts.rs:30-83` and `:43-45` references.
- B2: the `ScopeStore::insert` doc credits `validate_segment` with the whitespace rule.
- B3: embed is the only library crate without `unwrap_used`.
- B4: wrong re-export claims in api.md.
- B5: dead `docs/plans/` paths in two `.rs` files.
- B6: `templates_path` names a directory that doesn't exist.

B3 is also a precondition that keeps #1085 a pure refactor, so its place in the order (item 4 before item 5) is right.

### Does each member's acceptance map to an item?

| Issue | Met? | Notes |
| --- | --- | --- |
| #1038 | Yes | Deletion matches the ruled DELETE; no public-API snapshot gate exists. |
| #1041 | Yes | Once M2 and the parsing LOWs are fixed. |
| #1046 | Partly | "On a clean checkout" needs H2; criterion 2 is conditional on M3; criterion 3 is covered. |
| #1052 | Yes | Both the rename and the slash test. |
| #1067 | Yes | Criteria 1–3, once H1 is fixed. Errors propagate with `?` through `me-query/src/hybrid.rs:146` and `execute.rs:419` up to `recall`. |
| #1071 | Yes, with one deviation | The `--cpuset-cpus` change is justified. |
| #1077 | Yes | Removing the dependency drops exactly one line from Cargo.lock; nothing forwards `me-test-support/backend-postgres`. |
| #1085 | Yes | — |
| #960 | Yes | The STOP trigger does not fire, because `roadmap` is dropped rather than repointed. |

### Structure and feasibility

- The Documentation, Testing and Verification sections are all present.
- The order works: B3 before #1085, B1 after #1067, #1077 changes the lock before the image build in Verification.
- Spot-checked claims that hold:
  - File and line references: `cycle_report.rs:16-21` and `:228-232`, `me-cognitive/src/lib.rs:117`, `memory-engine/src/lib.rs:310`, `fts.rs:87-90`, `me-test-support/Cargo.toml:32`.
  - The named existing tests all exist.
  - The 26-warning breakdown (1 + 2 + 6 + 7 + 7 + 3) matches the docs tree.
  - The 7 plain mermaid fences and 7 `../../src` links.
  - All repoint targets exist.
  - The parity test filters to `cargo` lines, so the new `uv run` line and image block don't trip it.

## Resolution

Applied in plan revision 2 (SHA-256 2e4e2b3993930e0f95eb594819fe9164421bd3d015a31055683e031f4031feb8, altitude report
0267ae88f8b54011fb88fc3cd56af47cc26bfc81fbbad31e6f1c50c0017b9b4a); this reviewer re-checks that delta in round 2.

- [HIGH] H1 metadata test cannot be set up (CHECK(json_valid)) → item 1 tampers through `PRAGMA ignore_check_constraints`
  as facts.rs:1756-1774 does; the #1067 decision's reason now names tampering, an outside writer, another backend, and a
  bad JSON path from a consumer key.
- [HIGH] H2 docs-only clean export fails by construction → Verification 3 exports the whole tree (`git archive HEAD | tar -x`).
- [MEDIUM] M1 unlocked docs dependencies → new boy-scout item 12 (B7) commits `uv.lock` and the gate runs
  `uv run --locked`; it reverses the #21 ignore, so it is flagged for Michaël's approval (strike B7 to accept the risk).
  RTD already building with `-W` goes into the PR body.
- [MEDIUM] M2 drift check can fail open → any variant line ending in neither `,` nor `{` panics.
- [MEDIUM] M3 `myst.header` → measured: a full export of 9b9d676 with `suppress_warnings = []` emits 26 warnings, none
  `myst.header`; the suppression hides nothing and is deleted (item 13, change 8). Step 0 no longer counts it.
- [LOW] Dummy mutation → `#[error("dummy")] Dummy,`.
- [LOW] table parsing → skip blank lines and rows whose first cell is not backticked.
- [LOW] `crate::types::` in public docs → accepted: the cell is the derived declaration verbatim (stated in item 9);
  normalizing would add code for cosmetics.
- [LOW] B3 three test files, not four → applied.
- [LOW] B4 conditional claims → stated outright (PersistenceClassifier is re-exported; serialize/deserialize_embedding
  are not; Error and Traits lines completed).
- [LOW] item 3 grep matches the plan → `-- '*.rs'`.
- [LOW] item 5 subject → `refactor(build): move the facade, sqlite and embed crates onto the workspace lint table`.
- [LOW] CONTRIBUTING.md:38 and ci.yml claim → CONTRIBUTING updated; the gate line says "local only, ci.yml has no docs job".
- [LOW] step 0 fetch → `git fetch origin` first.
- [LOW] PR body counts → 31→26 and 9→6 explained in item 13 and the PR item.
- [LOW] #1071 deviation needs explicit approval → the decision row says Michaël's plan approval covers it; the worker
  report asks for it explicitly.

---

# Round 2 (plan 2e4e2b39…1feb8, altitude 0267ae88…9b4a)

**Round 2: plan review of the r2 delta (group B, #1088)**

Altitude report SHA-256: 0267ae88f8b54011fb88fc3cd56af47cc26bfc81fbbad31e6f1c50c0017b9b4a

**Verdict: LGTM.** Every round-1 finding is resolved in the plan text, not only in the Resolution notes. The delta adds four LOW wording and consistency problems, none of which blocks approval. Confidence is high: I checked each point against the r2 plan file, the repository and the uv/podman documentation, and ran no builds or tests.

### Altitude (assessed first)
The report matches the plan:
- The plan SHA-256 is `2e4e2b39…1feb8`.
- The plan has 277 lines and 25,438 bytes, and 22 top-level tasks.
- The only fenced block (lines 207–213) is still the exact #1071 text to be inserted. I accept that exception again.

The new "Surfaced, not done" section only points elsewhere and copies nothing. Proceed.

### Round-1 findings, checked against the revision
- **H1 (malformed-metadata test):** fixed. Item 1 now tampers the way `facts.rs:1756-1774` does (`PRAGMA ignore_check_constraints` ON, then the UPDATE, then OFF). The decision row now names tampering, an outside writer, another backend, and a bad JSON path built from a consumer key.
- **H2 (clean export):** fixed. Verification step 3 now archives the whole tree and runs from inside that directory.
- **M1 (unlocked docs dependencies):** fixed by the new B7 item (assessed below).
- **M2 (drift check failing open):** fixed. Item 9 now says any other line ending panics.
- **M3 (`myst.header` suppression):** fixed. The measurement of 0 such warnings justifies deleting the suppression, and the full-export count of 26 matches the in-tree count, which is consistent with what H2 assumed.
- **LOW items:**
  - The mutation is now `#[error("dummy")] Dummy,`.
  - The table parsing skips blank lines and the header and separator rows.
  - The test unwrap allowance now goes in only the three embed test files that call `unwrap`.
  - B4 states its corrections outright.
  - The item 3 grep is restricted with `-- '*.rs'`.
  - Item 5 has the new commit subject.
  - CONTRIBUTING.md is updated, and step 0 runs `git fetch` first.
  - The PR body explains the counts and asks for explicit approval of the #1071 deviation.
  - Keeping `crate::types::` verbatim in the table is an acceptable resolution; I offered it as an option.

### What the delta adds
**B7 (commit `uv.lock`, run `uv run --locked`): sound.**
- `uv lock --check` and `uv run --locked` both exist in uv 0.13.7.
- The local lock uses format version 1, revision 5.
- `.readthedocs.yaml`'s `uv sync` will pick the committed lock up automatically.
- The `.containerignore` allowlist keeps the lock out of the image.
- The history claim holds: the ignore line came in with 692a6ca (#21).

**Other additions are also fine:**
- The #1067 rule now states exactly what the probe checks and that the original error is what propagates.
- The corrupt-index test checks its own fault injection before asserting.
- Pinning "each malformed string fails with extended code 1" in the characterization test is acceptable. That code is the contract the classification depends on, not incidental output, and the bundled SQLite only changes through a reviewed lock bump.
- The `.containerignore` point stays a fair "I think". `podman-build(1)` says an archive context is extracted to a temporary directory, and that a `.containerignore` in the context directory is read. It doesn't explicitly cover a tar read from stdin (`-`). You could cite the man page in place of "not measured".

### New findings
- **[LOW] Item 13, gate comment.** The comment "local only, ci.yml has no docs job" is false. `.github/workflows/ci.yml:46` defines a `docs` job, which runs rustdoc intra-doc link checks. Change it to "ci.yml runs no Sphinx step".
- **[LOW] Item 12 (B7), the "if struck" clause is incomplete.** `--locked` appears in three places, not only the gate line:
  - item 13's red command (line 170);
  - the gate line (line 188);
  - Verification step 3 (line 261).

  If B7 is struck, the archive has no `uv.lock` and `uv run --locked` fails there. The clause should say to drop `--locked` everywhere. Separately, B7's own check (`uv lock --check`) passes against the gitignored lock that already sits in the worktree. What actually proves the lock is tracked is Verification step 3, or `git ls-files --error-unmatch uv.lock`.
- **[LOW] Item 13, the 31→26 explanation covers only three of the five warnings.** 9b9d676 (#1086, which closes #1022) added the three `reference/*` entries. The other two are not attributed. Either say so ("3 from #1086, 2 unattributed"), or drop the causal wording from the item and the PR body.
- **[LOW] "Surfaced, not done": two of the api.md drift items are one-line rot.** The duplicated `### Bootstrap` heading (api.md lines 36 and 189) and `execute_query`'s documented return type (it really returns `Result<QueryResponse>`, `engine/query.rs:45`) are in a file this PR already edits. Under the boy-scout rule they would be fixed here, each in its own commit. Only the systemic async-shown-as-sync rewrite needs the proposed issue. This is Michaël's call; surfacing it as the plan does is compliant.

## Resolution (round 2)

Applied in revision 3 (0925754c…7313, altitude 1c7769ef…b260):
- [LOW] gate comment → "local only, no CI job runs Sphinx".
- [LOW] B7 → `--locked` dropped everywhere if struck; green adds `git ls-files --error-unmatch uv.lock`.
- [LOW] count → 31 (issue) → 29 (measured on 7a22c07 before the rebase; two unattributed) → 26 (9b9d676).
- [LOW] api.md rot → split: new boy-scout B8 (item 14a) removes the duplicated Bootstrap section. `execute_query`'s
  return type stays with the method-table drift (async shown as sync) as one surfaced follow-up: the table hand-copies
  rustdoc signatures, so fixing one row while the rest drift is not the fix.

---

# Round 3 (plan 0925754c…7313, altitude 1c7769ef…b260)

**Round 3: delta review (r2 → r3), follow-up group B (#1088)**

Altitude report SHA-256: 1c7769ef9d21e1123af14973e8eb5ac9ea8903d40bf52e89857093d42897b260

**Verdict: LGTM.** All five round-2 LOW findings are fixed in the plan text, and the new B8 item holds up against the file. The delta adds one cosmetic LOW and one optional wording nit; neither blocks approval. I'm highly confident, having checked each point against the r3 plan and `docs/reference/api.md`; no tests, builds or gates were run.

### Altitude report (checked first)
- The report's figures match the plan: SHA-256 `0925754c…aa7313`, 284 lines, 26,199 bytes, and 23 top-level tasks (r2's 22 plus B8).
- The only fenced block is still the #1071 product text, now at lines 213–219. I accept it as before.
- The delta is wording plus one boy-scout item. Its disposition, "proceed", stands.

### Round-2 findings, checked against r3
- **Gate-line comment:** now says "local only, no CI job runs Sphinx". That is true: the `docs` job in `ci.yml` runs rustdoc, not Sphinx.
- **B7 "if struck" clause:** now drops `--locked` from all three places it appears (item 13's red command, the gate line, Verification 3). B7's green check now includes `git ls-files --error-unmatch uv.lock`, which shows the lock is tracked rather than just present on disk. Fixed.
- **Warning count:** now reads 31 → 29 (on 7a22c07, two unattributed) → 26 (9b9d676). This is consistent: 7a22c07 is the parent of 9b9d676, and #1086 added the three `reference/*` entries, so 29 − 3 = 26. The PR item carries the same 31→29→26 count. Fixed.
- **B4:** now cites the `pub use me_traits::{…}` block instead of a line number, so it survives line drift. Fixed.
- **api.md drift:** split correctly.
  - The duplicated Bootstrap section becomes boy-scout B8.
  - The method-table drift is described as systemic and stays a surfaced follow-up. That's sound: `execute_query`'s return type and async-shown-as-sync affect the whole hand-copied page.
- **`.containerignore` note:** now cites `podman-build(1)` with the right hedge (an archive context is extracted and its `.containerignore` read; stdin `-` is not named explicitly).

### B8 checked against api.md
- **The two sections:** api.md has `### Bootstrap` at line 36 and again at line 189.
- **No content lost:** the second section's two rows cover everything in the first: savepoint transactions, event-based idempotency, aggregated report, failures logged and skipped. Deleting the first loses nothing.
- **Anchors:** with `myst_heading_anchors = 3`, the surviving heading's slug changes from `bootstrap-1` to `bootstrap`. Nothing in `docs/` links to either anchor, so no reference breaks.
- **Drift test:** the #1041 test reads only the table after "`MemoryError` variants:", so B8 doesn't affect it.

### New findings
- **[LOW] Item label and position:** B8 is labelled 14a but sits before item 14 (B6). It's also unrelated to the Sphinx items around it. Since Verification 6 checks that the commits appear "in order", renumber it 10a and place it after B4 (item 10, the other api.md fix), or renumber the items that follow.
- **[LOW, optional] Gate comment wording:** `.readthedocs.yaml` does run Sphinx with `-W`, so "no CI job runs Sphinx" is slightly loose if the Read the Docs build counts as CI. "no ci.yml job runs Sphinx" would be exact. The PR body already mentions Read the Docs, so this is cosmetic.

## Resolution (round 3)

Converged: all reviewers LGTM on revision 3; only LOW/cosmetic items remain, arbitrated without a further revision
(an edit would only re-stale every acknowledgement for no change of intent):
- [LOW] B8 label/position → accepted as written: "in order" means file order, where 14a precedes 14; the implementer
  lands B8 between items 13 and 14 exactly as listed.
- [LOW, optional] gate comment → accepted: Read the Docs is not a CI job of this repository, and the PR body names it.
- [Vibe LOW] "#1022 took three" vs "#1086 then listed three" → both true: PR #1086 is the change that closed issue
  #1022. The PR body will say "#1086 (closing #1022)".
