# #1162: pair tool results by id (#1159), hold guides and field lists to the code (#1160), keep the fuzz corpus out of the worktree (#1161), keep thinking out of bootstrap facts (C1, folded)

## Outcome

The CLI bootstrap and the backfill reader attach the same result to the same call on any Claude Code transcript: a
`tool_result` block answers the call its `tool_use_id` names, a user row holding such a block is a result row whether
or not it carries `toolUseResult`, and a bootstrap turn takes the calls of every assistant row linked beneath it up to
the next prompt. Sessions in Claude Code's real shape (one content block per row) therefore stop losing their calls,
and the outcome evidence those calls carry, before #1098's real writes. `tests/api_doc_parity.rs` also holds every
guide `trait`/`impl` fence and every api.md `` `X` fields`` list to the code, failing closed; the drifted guides and
api.md are fixed. The bounded fuzz run writes its corpus under `fuzz/target/`, a cargo-marked build output, so a
worktree that ran the full verification list holds no ignored file except build outputs. Folded collateral C1: a
bootstrap turn's `assistant_text`, and so every bootstrap fact, is the visible text of all the turn's assistant rows,
never their thinking. One PR, five commits.

## Constraints

- #1159: "A failing test first: two parallel calls answered in reverse order pair by id in both readers."
- #1159: "One pairing rule shared by both readers, by `tool_use_id`, or a documented reason the bootstrap cannot use it."
- #1160: "The guide signatures match the traits, and the parity test (or a sibling) checks them, failing on drift."
- #1160: "api.md's field lists are checked too, or generated."
- #1161: "The bounded run writes its corpus outside the worktree (a temporary directory passed as the corpus argument) or under a path the cleanup treats as regenerable, in AGENTS.md, ci.yml and the gate matrices alike."
- #1161: "A worktree that ran the full verification list holds no ignored file except build outputs."
- Michaël: "#1159 is the load-bearing member: the CLI bootstrap and the backfill reader must pair tool results the same way (by tool_use_id) before #1098's real writes; prove it with a test that fails today. #1160 and #1161 are small."
- One PR; it closes #1162, #1159, #1160 and #1161, one `Closes #N` per line; each member in its own commit (#1159 in
  two, item 2's reason). Conventional Commits, imperative, the body says why, no Co-Authored-By line. TDD: red first.
- The AGENTS.md verification block is the merge gate (CI has no credit). The repository is public.
- Michaël, on the first plan (relayed by the orchestrator, 2026-11-26): "Approve + fold C1 in"; the relay's terms:
  "bootstrap facts take the turn's visible assistant text, never its thinking; prove it with a failing test first,
  and state what changes for already-stored facts and for --reprocess". Every other part of the plan stays.

## Ground truth (read in the code)

- A bootstrap writes one marker event per session (`bootstrap/mod.rs:125-139`, checked again under the write lock,
  `engine/bootstrap.rs:84-97`) and one fact per candidate episode; no event id derives from a turn. Fact content is
  `User: <user_text>` / `Assistant: <assistant_text>` (`bootstrap/extract.rs:108-132`); tool calls reach a session only
  through stderr keywords (`filter.rs:504-527`), the session outcome (`outcome.rs:79-112`) and a custom
  `SessionExtractor` (`CandidateEpisode::turns`).
- Today a turn is a prompt plus ONE assistant row (`filter.rs:197-274`); results pair with that row's `tool_use`
  blocks by position (`:405-418`); a user row is a result only if it carries `toolUseResult` (`:118-130`, `:155`).
- The backfill reader (`memory-engine-mcp/src/adapters/claude_code/transcript.rs`) keeps a file-wide pending-call map:
  a `tool_use` with an id inserts (a later duplicate replaces), a `tool_result` removes the call its `tool_use_id`
  names, and a block naming nothing pending is an "orphan tool_result" (`:363-370`, `:427-441`). A result row is a user
  row with a `tool_result` block (`:340-349`).
- Claude Code's real shape (the reference transcript
  `crates/memory-engine-mcp/tests/fixtures/claude-code-transcripts/-work-fixture-p/52c6c7b7-….jsonl`, and the
  synthetic `tests/fixtures/{success,failed}_session.jsonl` too): one content block per row, each row's `parentUuid`
  the row before; a result row's parent is the row holding its call; `system`/`attachment`/`progress` rows sit in the
  chain; line 37 is a result row without `toolUseResult`. The bootstrap pairs the reference's first prompt with its
  thinking row only and sees none of its four calls; it sees one of `failed_session`'s three.
- The bounded run line lives in `AGENTS.md:38`, `CLAUDE.md:60`, `GEMINI.md:40`, `.claude/skills/qa-sweep/SKILL.md:42`,
  `.github/workflows/ci.yml:124` and `fuzz/seeds/backfill_upload/README.md:20`; `tests/gate_matrix_parity.rs` holds the
  four matrices to a superset of ci.yml's cargo lines (comments stripped, `norm` `:32-39`). cargo marks `fuzz/target/`
  with a signed `CACHEDIR.TAG`, the `cache-tag` row of the cleanup's `regenerable.py`; a bare `fuzz/corpus/` has no mark.

## Implementation sequence

Landing order: #1161 first (every later full run is then also its acceptance evidence), #1159 next (the load-bearing
member, its neutral test reshaping before its rule change), C1 after it (it needs item 3's turn rows), #1160 last (no runtime risk; its checker then reads the
guide #1159 edits). Heavy commands, one at a time: `H<n> <cmd>` =
`flock /run/user/1000/memlane-heavy.lock systemd-run --user --scope -q -p MemoryMax=24G -p MemorySwapMax=0 -- env CARGO_BUILD_JOBS=<n> <cmd>`;
`H4` for workspace test, clippy `--all-targets` and fuzz build/run, `H8` otherwise. Never piped.

- [ ] **0. Worktree (no commit).** It exists, locked, on `fix/1162-import-docs-tail` from `main`; `git worktree list
  --porcelain` shows it `locked`, `git status` is clean. `docs/plans/` is excluded locally: plan files are added with
  `git add -f`.

- [ ] **1. #1161: write the bounded run's corpus into `fuzz/target/`.**
  - Files: the six above.
  - Red: edit `ci.yml:124` first; `H8 cargo test -p memory-engine --all-features --test gate_matrix_parity` fails, each
    of the four matrices "omits cargo commands that ci.yml runs". Before any edit, `git status --porcelain=v1
    --ignored=matching --untracked-files=all -- fuzz` after the old line lists `!! fuzz/corpus/`.
  - Change: in all six files the corpus argument `fuzz/corpus/backfill_upload` becomes
    `fuzz/target/corpus/backfill_upload`; the rest of each line is byte-identical (`-create_missing_dirs=1` stays: the
    directory does not exist on a first run). The four matrices' trailing comment gains `; corpus under fuzz/target, a
    build output (#1161)`. The seeds README's sentence on the corpus directory says the run writes new inputs to
    `fuzz/target/corpus/backfill_upload`, under the directory cargo marks with `CACHEDIR.TAG`, so they go with the build
    output. `fuzz/.gitignore` keeps `corpus`/`artifacts` for the manual campaigns of the other seed READMEs.
    - Why not `"$(mktemp -d)"`: a failed `mktemp` passes an empty corpus argument, it leaks a directory per run under
      `/tmp`, and it is shell syntax in a list every harness copies. `fuzz/target/` is deleted whole by the cleanup.
  - Green: the parity command passes. Acceptance probe: `rm -rf fuzz/corpus fuzz/artifacts` (the baseline run's
    output), run the three fuzz lines, then `head -c 43 fuzz/target/CACHEDIR.TAG` prints
    `Signature: 8a477f597d28d172789f06886806bc55`; `git status --porcelain=v1 --ignored=matching
    --untracked-files=all -- fuzz` lists only `!! fuzz/target/`; `python3 -I
    ~/.claude/skills/housekeeping/scripts/regenerable.py clean "$PWD" --dry-run` prints `fuzz/target` as ACTION and no
    KEEP line under `fuzz/`. Decided: the artifact prefix stays cargo-fuzz's default. The empty
    `fuzz/artifacts/backfill_upload/` it creates holds no file, so git lists nothing for it and the cleanup's scan,
    which reads git's records, sees nothing; a crash artifact there is a failing gate's evidence, and blocking cleanup
    on it is right. A probe that shows otherwise stops the item for a plan amendment.
  - Commit: `chore(fuzz): write the bounded fuzz run's corpus into fuzz/target`.

- [ ] **2. #1159, 1/2: give the synthetic transcripts Claude Code's tool ids (behaviour-neutral).**
  - Files: `crates/memory-engine/src/bootstrap/filter.rs` tests (helpers `assistant_with_tool`, `user_with_tool_result`,
    `user_with_interrupted_tool_result`; the JSONL of `meta_summary_and_interrupt_lines_are_not_prompts`; the blocks of
    `reconstruct_pairs_multiple_tool_results_in_order`), `tests/fixtures/success_session.jsonl`,
    `tests/fixtures/failed_session.jsonl`.
  - Red: none by design. Proof of neutrality: the unchanged suite stays green on the unchanged code with no assertion
    edited.
  - Change: every synthetic `tool_use` gets an `id` (`toolu_<assistant uuid>` in the helpers; `t-first`/`t-second` in
    the two-call test; `t1` in the `isMeta` JSONL; `toolu_s1`, `toolu_s2`, `toolu_f1`…`toolu_f3` in the fixtures); every
    synthetic result row gets `message: {role: user, content: [{type: tool_result, tool_use_id: <id>, content: <its
    stdout, else stderr, else "">, is_error: <isError, else false>}]}` and keeps its `toolUseResult`. The helpers drop
    any parameter the new shape leaves unused (clippy's 7-argument limit).
  - Green: `H8 cargo test -p memory-engine --all-features --lib bootstrap`,
    `H8 cargo test -p memory-engine --all-features --test bootstrap_test --test retraction_test --test bootstrap_dryrun`.
  - Commit: `test(bootstrap): give the synthetic transcripts Claude Code's tool ids`. Why separate: a reviewer can tell
    a test that changed shape from a test that changed verdict.

- [ ] **3. #1159, 2/2: pair the bootstrap's tool results by `tool_use_id` across the whole turn.**
  - Files: `crates/memory-engine/src/bootstrap/parse.rs`, `crates/memory-engine/src/bootstrap/filter.rs`,
    `crates/memory-engine-mcp/src/adapters/claude_code/transcript.rs` (a test, one doc line), `tests/bootstrap_test.rs`,
    `docs/usage/bootstrapping-sessions.md`.
  - Red (write all, run, watch each fail where stated):
    - a. `filter.rs` `reconstruct_pairs_results_by_tool_use_id` replaces the positional test (it asserts the old rule):
      one row holds `t-first` Bash and `t-second` Read, the results answer `t-second` then `t-first`; expects
      Bash←`out-first`, Read←`out-second`. Fails today (swapped).
    - b. `filter.rs` `the_reference_transcript_pairs_every_call_by_id` reconstructs the reference transcript: the same
      turns as today (same count and `user_text`s), and per turn the `(tool, interrupted)` list written out as a
      literal, derived from the tool rows of the fixture README's table (each row's line number placed in the turn of the
      prompt before it); turn 1 is `[Bash, Read, Read, mcp__me__recall]` with `tool_calls[1].stdout ==
      Some("Error: File does not exist.")`. Fails today (turn 1 has no call).
    - c. `filter.rs` `reconstruct_turns_stays_linear_on_hostile_links`: 100,000 rows sharing one uuid and one parent,
      and a 100,000-deep prompt/call/result chain, each reconstructed in under 10 s (the ceiling of
      `tests/redaction_conformance.rs:232-256`). Green today; it fails a walk that re-reads a uuid's children or
      recurses (stack overflow). The measured debug-build time goes in the commit body; a run above 2 s stops for a
      look at the walk before the ceiling is trusted.
    - d. `transcript.rs` `both_readers_pair_results_by_tool_use_id`, modelled on
      `both_readers_agree_on_an_interrupted_call`: per case one engine, prompt `u1` "Fix the parser." (a Bug
      candidate), a recording `SessionExtractor` collecting `(tool_name, stdout)` of each candidate's calls; each result
      row's `toolUseResult` and block content are the string `out-<tool>`. Both readers are held to one expected table
      (two readers wrong the same way would agree with each other):

      | Case | Rows (parent) | Expected |
      | ---- | ---- | -------- |
      | native reverse | `a1(u1){Read t_read, Grep t_grep}`, `r(t_grep)(a1)`, `r(t_read)(a1)` | each answered by its own |
      | split rows | `a0(u1){thinking}`, `a1(a0){Read}`, `a2(a1){Grep}`, `r(t_grep)(a2)`, `r(t_read)(a1)` | each answered by its own |
      | no toolUseResult | native reverse, `r(t_read)` without `toolUseResult` | backfill Read←`out-read`; bootstrap Read←None (no stdout), Grep←`out-grep` |
      | orphan | `a1{Read}`, `r(t_none)`, `r(t_read)` | Read←`out-read`; backfill counts one orphan |
      | duplicate id | `a1{Read t_dup}`, `a2(a1){Grep t_dup}`, `r(t_dup)(a2)` | the later call holds the id: Grep answered, Read not |
      | never answered | `a1{Read, Grep}`, `r(t_read)` | Read answered, Grep not |
      | result before call | `r(t_read)(u1)`, then `a1(u1){Read}` | orphan: Read not answered |

      The bootstrap also reports `turns_reconstructed == 1` in every case. Fails today in the first five cases.
    - e. `tests/bootstrap_test.rs`: `bootstrap_success_session` asserts `outcome_counts.success == 1`,
      `bootstrap_failure_session` `outcome_counts.failure == 1` (both `indeterminate` today: the turn misses calls).
    - Red commands: `H8 cargo test -p memory-engine --all-features --lib bootstrap::filter`,
      `H8 cargo test -p memory-engine-mcp --all-features --lib both_readers`,
      `H8 cargo test -p memory-engine --all-features --test bootstrap_test`.
  - Change:
    - `parse.rs`: `ContentBlock::ToolUse` gains `id: Option<String>` (the block's string `id`). Test patterns take `..`.
      The `tool_use_result` doc loses the positional wording. `ContentBlock::ToolResult` is not used for pairing: its
      `content` is a lossy string (`parse.rs:505-516`), while `records_an_interrupt` needs the raw value
      (`parse.rs:95-107`; the "block array" and "block flag" cases of `both_readers_agree_on_an_interrupted_call`).
    - `filter.rs`, `reconstruct_turns(&[SessionEntry]) -> Vec<ConversationTurn>` keeps its signature:
      - **Raw result blocks:** `fn tool_results(entry: &SessionEntry) -> impl Iterator<Item = &Value>` yields the raw
        `tool_result` objects of `message.content` (it replaces `result_content`, which found only the first). Each
        block's `tool_use_id`, `content` and `is_error` are read from that raw object.
      - **Result row:** `fn is_result_row(&SessionEntry) -> bool` = a user row with a `tool_result` block (by `tool_results`) or a
        `toolUseResult`; used by `takes_part` and every pass; a result row is never a prompt. The block is the backfill's
        and the API's rule; the `toolUseResult` arm keeps today's guarantee that a block-less result row opens no turn.
      - **Turn selection unchanged** (the first unused assistant child of the prompt, else the #82 fallback), so
        `user_text`, `assistant_text`, `timestamp` and `uuid` stay as today in this commit: existing facts' content and
        hashes are unchanged (what a `--reprocess` run may add is risk 1). Item 4 then changes `assistant_text` on
        purpose, as its own commit.
      - **Turn rows:** one pass over ALL entries (non-turn rows sit in the chain) builds
        `children: HashMap<&str, Vec<usize>>`. The selected assistant is seeded first (`owner[its index] = Some(turn)`,
        as today it is marked `used`); from it an explicit-stack walk enters each child unless it is a genuine prompt
        (takes part, not a result row), has no uuid, or its uuid is already in `used`; an entered row gets
        `owner[i] = Some(turn)`, its uuid goes into `used`, and its children are expanded once (O(n), #406; a fork's
        repeated uuid joins at most one turn). `turn` is the index into the turns vector as built, and pairing runs
        before the final `sort_by_key` (`filter.rs:276`), so `owner` indices stay valid.
      - **Pairing**, `fn pair_calls(entries, owner, n_turns) -> Vec<Vec<ToolCallRecord>>`, one file-order pass, the
        backfill's rule: an owned assistant row's `tool_use` blocks become its turn's calls in file order; every
        `tool_use` with an id, owned or not, is inserted into `pending: HashMap<&str, Option<(usize, usize)>>` (a later
        duplicate replaces); each `tool_result` block removes the entry its `tool_use_id` names and fills that call
        (`stdout`/`stderr` from the row's `toolUseResult`, no fallback to block text — `parse.rs:130-132`'s
        `has_short_sha` reason; `is_error` = the block's `is_error` or `toolUseResult.isError`, the backfill reading
        the block; `interrupted` = the `toolUseResult` flag or `records_an_interrupt` of this block's `content`); a block
        naming nothing pending fills nothing.
      - **Deleted:** `tool_results_by_parent`, `collect_tool_result_entries`, `extract_tool_calls`, `result_content`
        (replaced by `tool_results`); `build_turn` takes the turn's calls.
      - **Rustdoc:** `ConversationTurn`, `takes_part`, `reconstruct_turns` state this rule and name the backfill reader
        (`memory-engine-mcp` `adapters::claude_code::transcript`) as its other reader, and say where the two differ in
        payload, not pairing: for a result without `toolUseResult` the backfill's result text falls back to the block
        text, the bootstrap's `stdout` stays `None`.
    - `transcript.rs`: no code change; `Reader::result`'s doc gains "The CLI bootstrap pairs by this same rule
      (`bootstrap/filter.rs`, #1159)."
    - Why no shared function: the readers walk different types (`SessionEntry` versus a `serde_json` map) and the
      backfill emits records as it walks, so a shared helper would wrap one map insert and remove. Test d holds both to
      one table, as #1154's test holds the interrupt rule. This is #1159's "documented reason", stated on
      `reconstruct_turns`.
    - `bootstrapping-sessions.md` pipeline step 2: "Reconstruct: a turn is a prompt, its first assistant entry and every
      entry linked beneath it up to the next prompt; each `tool_result` answers the call its `tool_use_id` names, the
      backfill reader's rule (#1159)."
  - Green: the red commands pass; `H8 cargo test -p memory-engine-mcp --all-features --lib adapters::claude_code::transcript`
    passes unchanged (README table, digest, orphan tests).
  - Commit: `fix(import): pair the bootstrap's tool results by tool_use_id across the whole turn`. Body: positional
    pairing mis-paired reordered results, and the one-row turn missed every call in Claude Code's one-block-per-row
    shape, so session outcomes were wrong before #1098.

- [ ] **4. C1 (folded): a bootstrap turn's assistant text is its visible text, never its thinking.**
  - Files: `crates/memory-engine/src/bootstrap/parse.rs`, `crates/memory-engine/src/bootstrap/filter.rs`,
    `tests/bootstrap_test.rs`, `docs/usage/bootstrapping-sessions.md`.
  - Red (write all, run, watch each fail):
    - f. `filter.rs` `a_turns_assistant_text_is_its_visible_text`: `u1` "Fix the parser.", `a0(u1){thinking
      "PRIVATE-REASONING"}`, `a1(a0){text "first"}`, `a2(a1){tool_use t1}`, `r(t1)(a2)`, `a3(r){text "done"}`; expects
      `assistant_text == "first\ndone"`. Fails today: the text is `"PRIVATE-REASONING"`, the first row's thinking
      (`filter.rs:335-352` joins `Text` and `Thinking`).
    - g. test b (item 3) gains: on the reference transcript, turn 1's `assistant_text` contains its final answer
      ("The help text lists it.") and no turn's `assistant_text` contains any non-empty `thinking` string of the
      fixture (lines 3 and 28, read from the file by the test, not copied; the test asserts it found at least one). Fails today.
    - h. `tests/bootstrap_test.rs` `bootstrap_facts_never_store_thinking`: the body of test f through
      `bootstrap_session` with a `SessionExtractor` that delegates to `KeywordExtractor` and records the `content` of
      every `ExtractedFact` it returns (public API); expects at least one fact, every content containing `first` and
      `done` and none containing `PRIVATE-REASONING`. Fails today.
    - Red commands: `H8 cargo test -p memory-engine --all-features --lib bootstrap::filter`,
      `H8 cargo test -p memory-engine --all-features --test bootstrap_test`.
  - Change:
    - `filter.rs`: `assistant_text` is built in item 3's file-order pass: each owned assistant row's non-empty
      visible text, in file order, joined by `\n` (the backfill's join of a message's texts, `transcript.rs`
      `message.texts`). `user_text` is unchanged (user rows carry no thinking). Rows Claude Code writes itself add no
      text, as in the backfill (`transcript.rs` skips `<synthetic>` and makes `isApiErrorMessage` a stop record): an
      owned row whose `message.model` is `<synthetic>` or whose `isApiErrorMessage` is true is skipped for text (its
      `tool_use` blocks, if any, still count). `SessionEntry` gains `is_api_error_message: Option<Value>` and
      `MessagePayload` gains `model: Option<String>` (both crate-private, `#[serde(default)]`); every struct literal of
      the two types (about 21, in `parse.rs` and `filter.rs`, mostly test helpers) gains the field as `None` in this
      commit. No `Default` derive: it would add a trait to two wire types to save one line per literal. Test f gains a
      `<synthetic>` row `"No response requested."` and an API-error row, neither in the expected text.
    - `parse.rs`: the `ContentBlock::Thinking` variant is deleted; a `thinking` (or `redacted_thinking`) block parses
      to `ContentBlock::Other`, so no bootstrap path can read it. `parse_content_blocks_thinking` becomes
      `parse_content_blocks_skip_thinking` (asserts `Other`). Why delete rather than skip in `extract_text`: the only
      reader of the variant was the join that leaked it, and a variant nobody reads is dead code clippy would flag.
    - `bootstrapping-sessions.md`: step 2 adds "its assistant text is the visible text of those entries, never their
      thinking"; a short note under the pipeline states the stored-fact consequence below, and that a fact's content is
      cut at 2000 characters from its end, so a long multi-row turn can lose its last words (the cap is unchanged here).
  - What changes for facts (named in the PR body and in the docs note):
    - **Already-stored facts: nothing.** No migration rewrites them. A session with a bootstrap marker is skipped
      (`skip_existing` defaults to true), so its facts keep the content they were stored with, thinking included where
      the old first row was a thinking block.
    - **`--reprocess`** (`skip_existing: false`, `memory-engine-cli/src/commands/bootstrap.rs:207`): the session's
      facts are rebuilt from visible text, so their content differs from the old facts' and no twin matches
      (`facts.rs:383-397` reinforces equal content and scope only). Each candidate turn therefore mints a new fact
      beside its old one, and the old, thinking-bearing fact stays active. Cleaning it is a supervised erase of that
      session's old bootstrap facts before the `--reprocess`, a decision for Michaël outside this PR, asked as an open
      question in the PR body (item 8). The worker does not know how many bootstrap facts live stores hold.
    - **New bootstraps:** facts hold the turn's visible text, longer than before in multi-row turns; `build_content`'s
      2000-character cap (`extract.rs:108-132`) still bounds it. The keyword pre-filter now reads the final answer, so a
      session can yield candidates it did not before.
  - Green: the red commands pass; `H8 cargo test -p memory-engine --all-features --lib bootstrap` passes.
  - Commit: `fix(bootstrap): keep thinking out of bootstrap facts`. Body: the turn's text was its first row's, often
    a thinking block, so facts stored the model's reasoning, which the backfill never captures; now the turn's
    visible text. Names the stored-fact consequence.

- [ ] **5. #1160: hold the guides' trait fences and api.md's field lists to the code.**
  - Files: `tests/api_doc_parity.rs`, the `[[test]] api_doc_parity` comment in `crates/memory-engine/Cargo.toml`,
    `docs/reference/api.md`, `docs/reference/glossary.md`, and the guides the red run names (known:
    `docs/advanced/consolidation.md`, `docs/advanced/extensibility.md`, `docs/getting-started/quickstart.md`,
    `docs/usage/adding-facts.md`, `docs/usage/bootstrapping-sessions.md`).
  - Red: new `every_guide_trait_fence_is_the_codes` and `every_api_field_list_is_the_codes`, plus guards on inline input
    `a_trait_fence_mismatch_fails`, `an_unreadable_or_unknown_trait_fence_fails`, `a_field_list_mismatch_fails`
    (passing: a renamed or `_`-prefixed parameter, the `Result<T, MemoryError>` form, an omitted fn with a default body,
    a hidden-line fence; failing: a changed parameter type, a missing required fn, an unknown method, and a field
    missing, extra, `pub(crate)` or mistyped). `H8 cargo test -p memory-engine --all-features --test api_doc_parity`
    fails, listing at least the known drift (`summarize`, two `should_pin`, `notify_insert` returning `Result<()>`,
    `fingerprint` missing from the `EmbeddingProvider` excerpts and impls, and the short `BootstrapConfig`/
    `BootstrapReport` lists).
  - Change (same file; reuses `repo_root`, `code_span`, `Normalize`/`normalized`, `parse_src_dir`; no new dependency):
    - **Pages:** `README.md` and `docs/**/*.md` except `docs/design/`, `docs/audits/`, `docs/plans/`, `docs/_build/` and
      `docs/ROADMAP.md` (dated records, working plans, generated output).
    - **Fences:** every fence whose info string starts with `rust`, rustdoc hidden lines (`# `, a lone `#`) un-hidden;
      parsed as a `syn::File`, else as a block (its nested items).
    - **Code side:** every `pub trait` and `pub struct` under `crates/*/src`, outside `#[cfg(test)]` modules, by name;
      a name two crates define is an ambiguity error.
    - **Trait rule:** for each fence `trait X` or `impl X for T` (X the path's last segment): each fn shown must be X's
      fn of that name with an equal signature after `normalized`, parameter patterns erased (an impl may rename them)
      and `Result<T, MemoryError>` read as `Result<T>` (the alias); an `impl` must show every fn of X without a default
      body (an incomplete impl does not compile); supertraits and attributes are not compared; inherent `impl T` is
      skipped.
    - **Field rule (api.md only; the guides' struct excerpts are C3):** a line opening with a code span then ` fields`
      heads a list of `- ` bullets (indented lines
      continue a bullet); each bullet's code spans before its first ` -- ` are `name: Type` or a bare `name`; the list
      must name exactly X's `pub` named fields (a list headed "fields" claims completeness), a given type equal to the
      field's after `normalized`.
    - **Fail closed:** F1 no trait fn checked anywhere fails (the reader broke); F2 a `rust` fence holding `trait ` or
      `impl ` that parses neither way fails, naming file and line; F3 an X no crate defines fails unless listed in
      `FOREIGN_TRAITS` (starts empty; takes only the std/external traits the first red run names); F4 a fn missing on
      either side fails; F5 an api.md line holding `` ` fields`` or `Fields: `` ` `` that is not a list header fails; a
      bullet without ` -- ` or a list with no field fails. `FOREIGN_TRAITS` stays although it may stay empty: without
      F3 a renamed trait would make every fence naming it skip silently, which is the fail-open #1160 exists to stop.
  - Doc fixes: `consolidation.md:92` →
    `fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String>;`. `extensibility.md`: `fingerprint`
    added to the `EmbeddingProvider` excerpt and example; `should_pin(&self, input: &ClassifierInput) -> bool` in the
    excerpt and example (which reads `input.content` and imports `ClassifierInput`); the `:139` caveat says classifiers
    receive a `ClassifierInput` (`content`, `fact_type`, `base_importance`, `metadata`), not a synthetic `Fact`;
    `notify_insert(…) -> Result<()> { Ok(()) }`. Every other `EmbeddingProvider` impl gains `fingerprint` (hidden `#`
    lines where the guide hides boilerplate). `glossary.md:66` describes `should_pin`'s `ClassifierInput`. api.md:
    the `EngineConfig` entry drops "Fields:" (its fields are `pub(crate)`, `engine/mod.rs:107-127`) and names
    `EngineConfig::new(path, embed_dim)` and its `with_*` setters; `period_start`/`period_end` become two typed spans;
    the missing fields get bullets from their rustdoc (every entry the red run names); lines 3-4 gain "Each
    `` `X` fields`` list names every public field of `X`, typed where a bullet gives a type, held by the same test." The
    test's module doc and the Cargo.toml comment name the two new checks.
  - Green: the same command passes (6 existing tests, 5 new); `uv run --locked sphinx-build -b html -W --keep-going -E
    docs docs/_build` passes.
  - Commit: `docs: hold the guides' trait fences and api.md's field lists to the code`.

- [ ] **6. Full verification (no commit).** The Verification block on the final tree, then item 1's acceptance probe.
- [ ] **7. Plan issue.** Exactly one, published by `~/.claude/skills/finish-pr/scripts/archive-plan-issue.sh` (before
  approval, for the fleet handoff; `/finish-pr` Item 5 reuses it). Never a second one.
- [ ] **8. PR** from `fix/1162-import-docs-tail`: one `Closes #N` line each for #1162, #1159, #1160, #1161;
  `Refs dutiona/infra#37`; the outcome shift (risks 1 and 1b below), the folded C1 with its stored-fact consequence,
  the open question "erase the old thinking-bearing bootstrap facts before any `--reprocess`?", and the remaining
  collateral findings, with no closing keyword near them.
- [ ] **9. `/super-review`** at the confirmed Full gate; fixes in new commits; affected checks re-run.
- [ ] **10. Merge:** squash, by Michaël, never the worker.

## Risks, riskiest first

| Risk | Disposition |
| ---- | ----------- |
| 1. #1159 changes what a re-bootstrap stores | Retired by design for item 3: the marker guards whole sessions; item 3 leaves turn texts unchanged, so an existing fact's content and hash are. Accepted and named in the PR: sessions bootstrapped after the fix (or `--reprocess`ed) get the outcome and Bug categories their calls imply (more success/failure, fewer indeterminate), and a turn that becomes a candidate only through its calls' stderr keywords mints a new fact on `--reprocess` (a fact the old reader missed, not a duplicate: a twin of equal content and scope is reinforced, `me-backend-sqlite/src/store/facts.rs:383-397`); a custom `SessionExtractor` now receives every call of a turn, redacted as before (`mod.rs:224-235`). |
| 1b. C1 (item 4) changes fact content | Accepted on Michaël's fold: stored facts are not rewritten; a `--reprocess` mints new visible-text facts beside the old ones (item 4, "What changes for facts"); the PR body says so. |
| 2. Green on synthetic shapes, red on the real one | Item 2 converts the synthetic shapes; tests b and d "split rows" run the real shape. |
| 3. Wrong turn boundaries | A row has one parent and the walk stops at every genuine prompt; non-turn rows are walked through. Test b pins the reference transcript's turns. An assistant row parented on the prompt instead of the previous row is missed: accepted, no observed writer does it. |
| 4. #406's O(n) on hostile input | Each uuid's children read once, explicit stack; test c. |
| 5. Edge pairings | Test d's table. `isMeta` result rows: the backfill skips meta first, the bootstrap pairs (accepted, Claude Code writes none). |
| 6. A backfill regression | Its code is untouched; its README-table, digest and orphan tests stay green. |
| 7. #1160's checker fails open | F1-F5 and the three guards. |
| 8. #1161's path is not regenerable, or a copy is missed | Item 1's `CACHEDIR.TAG`, git-status and cleanup dry-run probe; `gate_matrix_parity` holds the four matrices to ci.yml. `CARGO_TARGET_DIR` overridden: accepted, the repository sets none and the probe would show a KEEP line. |

## Collateral findings for Michaël

- **C1, folded (Michaël, 2026-11-26): item 4.** The bootstrap's `assistant_text` was the first assistant row's text
  only (`filter.rs:290`), in the real shape often a thinking block, so facts stored the model's thinking (which the
  backfill never captures, `thinking_is_not_captured`) and the final answer never reached the keyword pre-filter.

Not in this PR (fold candidates):

- **C2.** Guide call examples drift in ways no signature check covers: sync calls of async methods
  (`consolidation.md:107`, `extensibility.md:319`, `conflict-resolution.md:85`), `&` where `Arc` is taken
  (`bootstrapping-sessions.md:29-35,59-65`), struct literals of `#[non_exhaustive]` configs (`consolidation.md:103-106`).
  The fix that holds them is compiling the fences (doctests), a docs-wide PR.
- **C3.** Guide struct excerpts and field tables are unchecked and have drifted (`consolidation.md`'s
  `ConsolidationConfig`/`ConsolidationStats`, `extensibility.md`'s `SearchConfig`, the `BootstrapConfig` table in
  `bootstrapping-sessions.md`). #1160's field rule could take them with a "shown fields must exist" variant.

## Documentation

- #1161: the four matrices' comments and the seeds README (item 1); no prose change to AGENTS.md's paragraph beyond that.
- #1159: `bootstrapping-sessions.md` step 2; the rustdoc of item 3. No api.md change: no public signature changes
  (`bootstrap::parse` and `bootstrap::filter` are `pub(crate)`, `bootstrap/mod.rs:12,16`; `ContentBlock` is not
  re-exported).
- C1: `bootstrapping-sessions.md` step 2 and its stored-fact note (item 4); the PR body names the `--reprocess`
  consequence.
- #1160: api.md (header, `EngineConfig`, field bullets), glossary, the guides, the test's module doc (item 5).

## Testing

- New: tests a-c and f in `filter.rs` (and g's assertions in b), d in `transcript.rs`, the two outcome assertions and
  h in `bootstrap_test.rs`, five tests in `api_doc_parity.rs`. `parse_content_blocks_thinking` becomes
  `parse_content_blocks_skip_thinking`. Changed: the `filter.rs` helpers and the two workspace fixtures (item 2, neutral). Removed:
  `reconstruct_pairs_multiple_tool_results_in_order`, replaced by test a.
- #1161 has no automated test: one would have to run cargo-fuzz. `gate_matrix_parity` holds CI and the four matrices
  to one line; item 1's probe is the behaviour evidence.
- Whole suite: `H4 cargo test --workspace --all-features`; the bounded fuzz run drives the upload route over the
  reshaped readers.

## Verification

On the final tree, each command unpiped, each exit 0:

```sh
H8 cargo build --workspace --locked
cargo fmt --all --check
H4 cargo clippy --workspace --all-targets --all-features -- -D warnings
H8 cargo build --workspace --all-features
H4 cargo test --workspace --all-features
H8 cargo check -p memory-engine
H8 cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
H8 cargo doc --no-deps -p memory-engine
H8 cargo doc --no-deps -p memory-engine --all-features
H8 cargo deny check
uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build
H8 cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml
H4 cargo +nightly fuzz build
H4 cargo +nightly fuzz run backfill_upload fuzz/target/corpus/backfill_upload fuzz/seeds/backfill_upload -- -create_missing_dirs=1 -max_total_time=60 -timeout=10
```

Then item 1's acceptance probe: `CACHEDIR.TAG` signed; `git status --porcelain=v1 --ignored=matching
--untracked-files=all` lists only toolchain-marked outputs (`target/`, `fuzz/target/`, `docs/_build/`, `.venv/`,
`__pycache__/`); the cleanup dry run has no KEEP line. Any other line is a failure of #1161's second criterion.
N/A: the image build (no `Containerfile`, `.containerignore` or `Cargo.lock` change), the plugin gate (`plugins/`
untouched), the MSRV job (no dependency or edition change).
