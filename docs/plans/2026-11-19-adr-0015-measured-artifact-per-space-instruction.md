# docs(adr): ADR-0015 §5 measured serving facts and §6 per-space query instruction (dutiona/memory-engine#1078, dutiona/knowledge-base#607)

One plan for the lockstep pair, published with the same bytes as the plan issue of each repository.

## Outcome

After both PRs merge, `docs/design/adr/0015-cross-layer-embedding-identity-policy.md` is byte-identical in
dutiona/memory-engine and dutiona/knowledge-base (`diff` prints nothing) and differs from today's copy in exactly the
three places quoted under *Approved ADR text*: the Status line records the amendment; §5 carries dutiona/infra#140's
measured serving sentence word for word, then its source and date; §6 states the 2026-11-17 ruling whole as the shared
rule, says that the Memory layer departs from it, and states each layer's mechanism as its code has it. Headings,
section numbers, §6's Notation paragraph and table are unchanged. No code changes. ME's departure from the ruling goes
to Michaël for a decision (*Reported to Michaël*). One ME-only boy-scout commit fixes a stale design-doc table.

## Constraints

- `ME's and KB's ADR-0015 are byte-identical after both PRs (diff prints nothing), and each PR links the other.`
- `Each amended section names its source (infra#77 / #140; KB#573 / #606) and the date.`
- `Two §6 sentences change: "every query" and "is not recorded in the store". The wording follows what #573 implements.`
- The ruling: `the query instruction is per-space configuration recorded with the space (Michaël, 2026-11-17, on dutiona/knowledge-base#573's plan #606): a Qwen3-Embedding space carries the pinned instruction, another model none or its own.`
- `Lands after dutiona/knowledge-base#573 merges.` It has: dutiona/knowledge-base#612, merge commit a518ab9, 2026-11-17.
- Michaël approves the ADR text and merges ADRs. Approving this plan approves the three `text` blocks below; both PRs
  carry them byte for byte. A change to them after approval needs a successor plan and a new approval (item 8).
- §5 keeps the measured numbers verbatim: 2,048 tokens, 538-token chunks, 4 parallel slots, 1,158 MB, llama.cpp b12048.
  The sentence, as dutiona/infra#140 reports it:

> 2,048 tokens is well above the benchmark's 538-token chunks. dutiona/infra#77 serves it with 4 parallel slots: the server embeds an input of up to 2,048 tokens as it counts them (`usage.prompt_tokens`, which includes the end-of-sequence token it appends) and refuses a longer one with HTTP 400 `exceed_context_size_error`; its memory footprint was 1,158 MB while the Mac served a 27B model, with llama.cpp build b12048.

- §6 says nothing ME's code contradicts; a divergence is reported, never fixed by code in this pair.
- §5 and §6 keep their numbers and headings (code and docs in both repositories cite them). The §6 table stays byte for
  byte: KB's `tests/test_embed_identity.py::test_the_instruction_is_the_adrs` matches `` ^\| Knowledge layer \| `(.+)` \|$ ``.
- Every issue reference in the ADR names its repository (the ADR's own rule: the copies are byte-identical).
- Both repositories are public: no tailnet names or addresses, home paths or tokens in the ADR, commits, PR bodies,
  plan issues or anything committed under `docs/plans/`.
- Route: Documentation / Lightweight / Super planning / Full post-implementation review, Default severity.
- Both merge gates run on the host at the final clean HEAD, even for a docs-only change. CI is out of credit and never
  cited.
- Branch commits: Conventional Commits, imperative, body says why, no Co-Authored-By line. Never merge.

## Approved ADR text

The base is the ADR whose SHA-256 is `b4b6e60ff7820525bba7ac74b49c87ab0f78e90b4064d22d8a5643ca68346016` in both
repositories today (ME origin/main e14b188, KB origin/master 2ccc5d3); line numbers are that file's. LF line endings,
no trailing whitespace, at most 90 columns except the Status line, new text ASCII except `§`. The approved-text check
(Verification) finds each block in the file. The base with the three blocks applied (a trial copy) has the SHA-256
`2a13502a3a24211bfff7e2627faf4963bd1acd05ba1db024587474cddf080b1b`, called `APPROVED_ADR_SHA` below: approving this
plan approves that whole file.

**Block 1: line 3, the Status line.**

```text
**Status:** Accepted (2026-06-16); amended 2026-11-16 (dutiona/memory-engine#1017: served artifact, pinned identity, per-store query instructions); amended 2026-11-19 (dutiona/memory-engine#1078, dutiona/knowledge-base#607: measured serving facts, per-space query instructions)
```

2026-11-19 is the day this text was written, as dutiona/memory-engine#1017's entry carries the day its text was
written (its PR merged on 2026-11-17): a later approval or merge day does not change it.

**Block 2: §5's last paragraph, lines 148-153.** The first three lines are unchanged. The fourth now names the digest
it checks against (`artifact_sha256`, not the commit just before it). The old last clause ("dutiona/infra#77 confirms
it fits in the Mac's memory with the chosen `--parallel`") is replaced by the measurement, in its own paragraph so a
re-measurement amends only that paragraph.

```text
The served file is `Qwen3-Embedding-0.6B-Q8_0.gguf` (639,150,592 bytes) from the official
Hugging Face repository `Qwen/Qwen3-Embedding-0.6B-GGUF` at commit
`370f27d7550e0def9b39c1f16d3fbaa13aa67728`, the Q8_0 file the 2026-11-02 benchmark used.
The deployment verifies the installed file's SHA-256 against `artifact_sha256` above.

2,048 tokens is well above the benchmark's 538-token chunks. dutiona/infra#77 serves it
with 4 parallel slots: the server embeds an input of up to 2,048 tokens as it counts them
(`usage.prompt_tokens`, which includes the end-of-sequence token it appends) and refuses
a longer one with HTTP 400 `exceed_context_size_error`; its memory footprint was 1,158 MB
while the Mac served a 27B model, with llama.cpp build b12048. dutiona/infra#140 reported
these measurements on 2026-11-17.
```

From `2,048 tokens` to `b12048.` is dutiona/infra#140's sentence word for word; its line breaks are wrapping. The date is
dutiona/infra#140's (opened and merged 2026-11-17).

**Block 3: §6's first paragraph, lines 157-161**, becomes these three paragraphs. The heading, the Notation paragraph
and the table follow unchanged.

```text
Asymmetric models embed queries with an instruction and documents without one. The query
instruction is per-space configuration recorded with the space (owner's decision,
2026-11-17, on dutiona/knowledge-base#573's plan dutiona/knowledge-base#606): a
Qwen3-Embedding space carries its store's pinned instruction, verbatim below, unless
configured otherwise, and a space of another model carries none or its own. The Memory
layer departs from this rule as its paragraph states. The embedding client prepends the
instruction, if any, to each query; callers never build it. Documents never carry an
instruction, so document vectors under one identity stay comparable across stores. The
instruction is policy, not identity: the identity tuple (§1) leaves it out, so no
mismatch check compares it, and changing it re-embeds nothing.

**Memory layer.** Each embedding client reads `query_instruction` from its own
configuration, and the store does not record it. A deployment therefore gives every
client of one store (the MCP server, the CLI) the same instruction and revisits it when a
reconstruction (§4) changes the model. Unset, the HTTP client uses the pinned instruction
whatever the model, so a deployment of another model sets it to `""` (none) or to its
own.

**Knowledge layer.** Each space records its instruction in
`embed_spaces.query_instruction`: NULL means its model's default, the pinned instruction
when the model name contains `qwen3-embedding` (ignoring case) and none otherwise; `""`
means none; any other text is the space's own.
```

The two sentences the issue names: "its embedding client prepends it to every query automatically" becomes "The
embedding client prepends the instruction, if any, to each query"; "it is not recorded in the store" becomes "the
identity tuple (§1) leaves it out, so no mismatch check compares it", and whether a store records it moves to each
layer's paragraph. "Owner's decision" follows ADR-0020's wording for the same kind of source.

Each statement against the code (ME e14b188, KB 2ccc5d3):

| Statement | ME | KB |
| --- | --- | --- |
| Per-space configuration, recorded with the space | departs, as stated: no backend schema holds it (`me-backend-sqlite`, `me-backend-postgres`); each client sets it (`crates/memory-engine-mcp/src/config.rs`, `crates/memory-engine-cli/src/commands/embedding_args.rs`) | `embed_spaces.query_instruction` (schema v2, `migrate.py`), `db.space_query_instruction` |
| Qwen3-Embedding space: pinned unless configured | `HttpEmbeddingProvider::new` sets `DEFAULT_QUERY_INSTRUCTION` (`crates/memory-engine-embed/src/http.rs`) | `embeddings.default_query_instruction`: `"qwen3-embedding" in model.lower()` |
| Another model: none or its own | departs: only when the deployment sets it | `default_query_instruction` returns `""`; a recorded value wins |
| Client prepends the instruction, if any; callers never build it | `embed_query`, `embed_query_batch` prepend the client's own (none when `""`); callers `mcp/src/tools/recall.rs`, `cli/src/commands/query.rs` | `embeddings.embed_query`, the only place a query input is built; `search.py` passes `space_query_instruction(queried)` |
| Documents never carry one | `embed`, `embed_batch` | `embeddings.embed` |
| Not identity; no mismatch check compares it | `fingerprint()` / `EmbeddingFingerprint` have no such field | `settings.IDENTITY_FIELDS` leaves it out; startup does not compare it |
| Changing it re-embeds nothing | `docs/reference/mcp-server.md` | `tests/test_embed_identity.py::test_a_space_records_its_own_instruction` updates the column and searches with no re-embed |
| Memory-layer paragraph | MCP `query_instruction` (TOML, `--embed-query-instruction`, `MEMORY_MCP_EMBED_QUERY_INSTRUCTION`), CLI `--query-instruction`; `reconstruct` takes the caller's provider (`crates/memory-engine/src/engine/reconstruct.rs`) | n/a |
| Knowledge-layer paragraph | n/a | `db.space_query_instruction`, `default_query_instruction` |

## Reported to Michaël

Both PR bodies carry this section.

**ME and the 2026-11-17 ruling.** KB implements the ruling. ME at e14b188 departs from it twice, and the amended §6
says so in the shared paragraph and states ME's mechanism, with the deployment's duties, in its own:

1. **Not recorded with the space.** The instruction is each embedding client's configuration. The MCP server and the
   CLI are configured separately, so two clients of one store can use different instructions (a CLI `query
   --query-instruction ''` against the MCP server's store, today), and after a reconstruction each client uses the
   instruction from its own configuration.
2. **Pinned for every model by default.** A space of another model carries none or its own only if the deployment sets
   it; ME's docs say "A symmetric model must opt out" (`docs/reference/mcp-server.md`).

Past decision it touches: dutiona/memory-engine#1017 made the pinned instruction every provider's default, with `""`
opting a symmetric model out, to implement §6 as it then read. The ruling came a day later, on KB's plan
dutiona/knowledge-base#606, whose text is KB-scoped ("Qwen3-Embedding spaces get KB's pinned one"); both issues' text
applies it to the shared §6.

Today's exposure: clients that serve §5's model with the setting unset use the same pinned instruction. The deployed
ME server leaves it unset, so it uses the pinned instruction (dutiona/infra's `memory-engine.container`, kept unset by
its deploy test). Clients with different settings can disagree even for that model. Nothing is filed. Options, ideal first:

- **Ideal: ME records the instruction with its space, as KB does** (unset = the model's default, empty = none, text =
  its own). One rule and one storage in both layers; every client of a store agrees; a reconstruction to another model
  gets that model's default. Cost: an ME schema change (and its Postgres parity entry) plus a precedence rule between
  configuration and the recorded value; then a lockstep pair drops the "departs" sentence and folds the two layer
  paragraphs into the shared one.
- **Pragmatic: ME defaults by model only.** A non-Qwen3 ME deployment stops sending Qwen's prefix when it forgets to opt
  out; gap 1 stays; a lockstep pair rewrites the Memory-layer paragraph.
- **Accept:** the amended §6 records ME's departure and its duties; nothing further.

Recommendation: **accept now**, and take the ideal when ME first serves a model other than §5's or a second client
needs another instruction. Michaël decides.

If the ruling was meant for KB only, do not approve: say so, and a successor plan scopes the shared sentence to the
Knowledge layer ("In the Knowledge layer, the query instruction is per-space configuration recorded with the space")
and drops the "departs" sentence.

**ME's stale identity table (boy-scout, item 6).** `docs/design/embedding-identity-and-tei-qwen-migration.md` labels
its five-field table "Identity tuple (canonical, both layers)", while ADR-0015 §1 has nine fields since
dutiona/memory-engine#1017. Item 6 adds a status note in its own ME commit; striking it at approval leaves the pair
otherwise unchanged.

## Implementation sequence

`$ME` and `$KB` are the two worktree roots (shell variables, never written out);
`ADR=docs/design/adr/0015-cross-layer-embedding-identity-policy.md`; `$PLAN` is this plan's repository-relative path,
the same bytes in both worktrees. Every command runs with an explicit working directory.

- [ ] **1. Worktrees and baseline.** No commit. The locked worktrees exist: ME branch
  `docs/1078-adr-0015-measured-artifact` at origin/main e14b188, KB branch `docs/607-adr-0015-measured-artifact` at
  origin/master 2ccc5d3. Checks: `git -C "$KB" merge-base --is-ancestor a518ab9 HEAD` exits 0; `sha256sum "$ME/$ADR"
  "$KB/$ADR"` prints the base digest above twice; no open PR in either repository touches `$ADR` (`gh pr list --state
  open --limit 200 --json number,files`). After any rebase, the base digest must still match in both; if it does not,
  stop and take the new base text to Michaël, because the approved blocks and line numbers assume it.

- [ ] **2. Plan issues, then approval.** No commit yet. `sha256sum "$ME/$PLAN" "$KB/$PLAN"` prints one digest twice;
  the leak check (Verification) is clean on the plan. Publish one plan issue per repository with `/finish-pr`'s
  `archive-plan-issue.sh` without `--pr` (its `--dry-run` output read in full first). Michaël receives the two marker
  lines, `ADDRESS-ISSUE-PLAN-APPROVAL v1 issue=1078 plan_sha256=<digest> APPROVED` for the ME plan issue and the same
  with `issue=607` for the KB one, and posts each on its plan issue; the worker never posts them. Each is recorded with
  `state.sh approve-plan --type github-comment --plan-issue-url <that plan issue>` against its own repository's state
  record; `/finish-pr` Item 5 later archives on the same issue. Nothing below starts before both are recorded.

- [ ] **3. Commit the plan.** In each worktree, the plan and its review artifacts under `docs/plans/` (same bytes in
  both; the drafts stay ignored). Check: the leak check is clean on every staged file, and the staged files are read
  for the forbidden classes in Constraints. Commit (both): `docs(plans): plan the ADR-0015 §5 and §6 lockstep
  amendment`.

- [ ] **4. ME: write the approved text.** File: `$ADR` in `$ME`. Red: the approved-text check prints `3 blocks, 3
  missing` and exits 1. Change: line 3 becomes block 1, lines 148-153 become block 2, lines 157-161 become block 3;
  nothing else. Green: the check prints `3 blocks, 0 missing, §5 sentence verbatim` and exits 0; `git diff --numstat`
  lists only `$ADR`, 31 added and 9 deleted; `git diff -U0 -- "$ADR" | grep -c '^[-+]|'` prints 0 (exit 1 there means
  no table line touched); `uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build` succeeds (the only
  ME gate that reads the ADR). `sha256sum "$ADR"` prints `APPROVED_ADR_SHA`. Commit, `git add -- "$ADR"` only:
  `docs(adr): amend ADR-0015 §5 with the measured serving facts and §6 with per-space query instructions`. Body: §5 was
  left for filling when dutiona/memory-engine#1017 merged, and dutiona/infra#140 reports the facts; §6 states the
  2026-11-17 ruling as dutiona/knowledge-base#573 implemented it, and ME's departure as its code has it. One commit,
  not one per section: the Status line records both. No closing keyword in the body.

- [ ] **5. KB: mirror by copy, never by a second edit.** File: `$ADR` in `$KB`. Red: `diff "$ME/$ADR" "$KB/$ADR"`
  prints item 4's hunks. Change: `cp "$ME/$ADR" "$KB/$ADR"`. Green: `diff` prints nothing and `sha256sum` prints
  `APPROVED_ADR_SHA` twice; the approved-text check passes in `$KB`; `uv run pytest tests/test_embed_identity.py -q -k
  adrs` reports `1 passed`. Commit, `git add -- "$ADR"` only, with item 4's message byte for byte.

- [ ] **6. ME only: the stale design-doc table.** File: `docs/design/embedding-identity-and-tei-qwen-migration.md`.
  Red: `grep -c 'superseded by ADR-0015' <file>` prints 0. Change: after the table under "Identity tuple (canonical,
  both layers)" (line 80, the blank after its last row), insert the `markdown` block below and one blank line. Green:
  that grep prints 1; `git diff --numstat` lists only this file, 4 added; Sphinx as in item 4 succeeds. Commit:
  `docs(design): point the five-field identity table at ADR-0015 §1`. Body: the table predates the served artifact
  (dutiona/memory-engine#1017); boy-scout fix, separate from the ADR so the lockstep commits stay identical.

```markdown
> **Status: superseded by ADR-0015 §1 (#1017).** The canonical tuple now has nine fields:
> these five plus the served artifact (`artifact_sha256`, `pooling`, `normalization`,
> `max_input_tokens`).
```

- [ ] **7. Gates and PRs.** Both merge gates (Verification), every line, unpiped, at each branch's clean HEAD; every
  command exits 0, and a failure goes to Michaël with its output and a recommended fix. Push both branches; open ME's
  PR, then KB's, then `gh pr edit` ME's body to link KB's; read both back. Title in both: `docs(adr): ADR-0015 §5
  measured serving facts and §6 per-space query instruction`. Each body: `Closes #1078` (ME) or `Closes #607` (KB) on
  its own line and no other closing keyword, negated or not; `Refs dutiona/infra#37`; the other PR as
  `dutiona/knowledge-base#<n>` / `dutiona/memory-engine#<n>`; the plan issue; `Classification: Documentation
  (user-confirmed)`, `Execution path: Lightweight`, `Planning tier: Super`, `Post-implementation review: Full (Default
  severity)`; one line per amended section with its source and date; *Reported to Michaël*; the gate summary lines
  (never paths); `APPROVED_ADR_SHA` and the parity `diff`; a `Collateral findings` section. Before each body is
  published, it is read for the forbidden classes and the leak check is clean on it.

- [ ] **8. Review and finish.** `/super-review` at Full, Default severity, on each PR. A finding that would change
  approved text goes to Michaël with the exact replacement. On his OK:
  - A versioned successor plan (this plan's key plus `-v2`, the same bytes in both worktrees) carries the new block and
    its new expected ADR digest, and is reviewed by `/super-plan`; `state.sh supersede-plan` records it against each
    repository's approved record; it gets item 2's leak check and read-through and is published on a successor plan
    issue in each repository; Michaël's two new markers are recorded with `state.sh approve-plan --type
    github-comment`.
  - Only then: both ADRs are checked against the predecessor's `APPROVED_ADR_SHA`; the changed block is replaced by
    exact match of the predecessor's block (never by line number); both ADRs are checked against the successor's
    digest; the approved-text check runs with the successor as `$PLAN`. The successor plan, its review artifacts and
    the ADR change are committed in both worktrees and both branches pushed; the new PR heads are read back, both PR
    bodies updated with the successor plan issue and digest, each review target rebound to its new head, and
    `/super-review` re-run on both.

  Before READY, both repositories are checked again for any other open PR touching `$ADR` (excluding only this
  pair's two PR numbers, each in its own repository), and the approved-text check runs while `$PLAN` still exists.
  Then `/finish-pr` to READY on each: Item 5 archives on the plan issue recorded for the active approval (item 2's,
  or the successor's) and removes `docs/plans/` in one commit; at each final HEAD the gates re-run and both ADRs match
  the active plan's `APPROVED_ADR_SHA` (parity `diff` and `sha256sum`). Never merge.

- [ ] **9. Merge: Michaël's.** He squash-merges both PRs back to back. Then, in a scratch directory outside both
  worktrees: `git -C "$ME" fetch origin main`, `git -C "$KB" fetch origin master`, `git -C "$ME" show
  origin/main:"$ADR" > me.md`, `git -C "$KB" show origin/master:"$ADR" > kb.md`, each exiting 0; `sha256sum me.md
  kb.md` prints `APPROVED_ADR_SHA` twice.

## Documentation

The ADR is the deliverable, plus item 6's ME design-doc note. Re-read against the amended text and left unchanged
because they agree with it and no section number or heading moves: ME `docs/reference/mcp-server.md`,
`docs/reference/cli-inspector.md`, the rustdoc of `crates/memory-engine-embed/src/http.rs`,
`crates/memory-engine-mcp/src/config.rs` and `crates/memory-engine-cli/src/commands/embedding_args.rs`; KB `README.md`,
`docs/usage/searching.md`, `docs/usage/embedding-spaces.md`, `docs/reference/schema.md`, `docs/reference/mcp-tools.md`,
`docs/design/adr/0018-provider-abstraction.md`, `src/knowledge_base/embeddings.py`,
`src/knowledge_base/routes/embeddings.py`. No samples: no API changes.

## Testing

No new test: the change is prose with no executable behaviour, and a test of ADR prose would be a snapshot that breaks
on the next amendment. A cross-repository parity test was rejected in dutiona/knowledge-base#606 (Decision 10), so
parity is the `diff` and digest shown in both PRs. What guards the change: KB's `test_the_instruction_is_the_adrs`
(the Knowledge-layer row is untouched); ME's Sphinx build with `-W`, which renders `docs/design/`; the approved-text
check and the numstat (items 4-6), run but not committed; both full suites in the gates, which pass as at base since
no code changes.

## Verification

ME, at the final clean HEAD, every line of AGENTS.md "Verify", unpiped, cargo under the host's heavy-build lock with
`CARGO_BUILD_JOBS=16`. N/A: the image build, the plugin gate and the MSRV job (no `Containerfile`, `Cargo.lock`,
plugin or dependency change).

```bash
cargo build --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo check -p memory-engine
cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine
cargo doc --no-deps -p memory-engine --all-features
cargo deny check
uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build
cargo +nightly fuzz build
```

KB, at the final clean HEAD, every line of AGENTS.md "Verify", in order, pytest under the heavy-build lock.
`command -v tesseract` must print a path first (the gate's once-per-host prerequisite; without it the OCR tests skip
and the gate is INCOMPLETE, not passed).

```bash
uv lock --check
uv sync
uv run ruff check src/ tests/
uv run ruff format --check src/ tests/
uv run pytest tests/ -q -m "not slow"
uv sync --all-groups
uv run basedpyright src/ tests/
```

Lockstep checks, from each worktree root. The leak grep supplements, never replaces, reading each text before it is
published; its exit status 1 means clean, 0 a match, 2 an unreadable input. The approved-text check needs `$PLAN`, so
it runs before `/finish-pr` Item 5; at the final HEAD the parity and digest lines run.

```bash
leaks() { grep -nE '[/]home[/]|[~][/]|[.]ts[.]net|\btail[0-9a-f]{4,}\b|\b100[.](6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])[.]|gh[pousr]_[A-Za-z0-9]{20,}' "$@"; }
python3 -I - "$PLAN" "$ADR" <<'EOF'
import re, sys
plan, adr = (open(p, encoding="utf-8").read() for p in sys.argv[1:])
blocks = re.findall(r"^```text\n(.*?)^```$", plan, re.S | re.M)
missing = [b.partition("\n")[0] for b in blocks if b not in adr]
sentence = re.search(r"^> (2,048 tokens is well above.*)$", plan, re.M)[1]
verbatim = sentence in " ".join(adr.split())
print(f"{len(blocks)} blocks, {len(missing)} missing" + (", §5 sentence verbatim" if verbatim else ""), *missing, sep="\n")
sys.exit(len(blocks) != 3 or bool(missing) or not verbatim)
EOF
diff "$ME/$ADR" "$KB/$ADR" && sha256sum "$ME/$ADR" "$KB/$ADR"   # no diff; APPROVED_ADR_SHA twice
git -C "$ME" diff --name-only origin/main...HEAD                # the ADR, item 6's file (and docs/plans/ until Item 5)
git -C "$KB" diff --name-only origin/master...HEAD              # the ADR (and docs/plans/ until Item 5)
```

Acceptance map: byte-identical copies and mutual links, items 4, 5, 7, 8, 9; each amended section names its source and
date, block 2 (dutiona/infra#77, dutiona/infra#140, 2026-11-17) and block 3 (dutiona/knowledge-base#573,
dutiona/knowledge-base#606, 2026-11-17).
