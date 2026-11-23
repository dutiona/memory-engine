# #1156 backfill-ready: a stable preview for future-dated items, one interrupted-call rule, a checked API page, a locked fuzz build

## Outcome

One PR on main (9ed349a, after #1155) closes #1153, #1154, #1092, #1093 and #1156, one commit per member plus two
boy-scout commits. A reviewed backfill write commits when an item is dated after its dry run's receipt: the preview
stops hashing a memory's validity start, the one receipt-derived value it covered, and the memory stays valid from the
earlier of its date and its write's receipt, never from the future. Both Claude Code transcript readers decide whether
a tool call was interrupted with one public predicate, `memory_engine::bootstrap::records_an_interrupt`, so one
call gets one verdict (the readers' different result pairing stays, surfaced below). Every method signature
`docs/reference/api.md` documents is the code's, and `tests/api_doc_parity.rs` fails when one drifts. `fuzz/Cargo.lock` is committed, seeded from the root lock, and
`cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml` fails the gate before the fuzz build when it is stale.

## Constraints

- #1156 tracks one PR that closes four issues, each in its own commit, with one `Closes #N` line per member plus `Closes #1156`.
- Route: Bug fix, Standard path, Super planning, Full review gate, Default severity.
- Why now: #1153 and #1154 must land before the supervised backfill's (#1098) first real write. The harvest spans four machines' clocks, so a future-dated item is realistic, not theoretical.
- #1153: reproduce it with a test first. Choose between keeping one preview digest across the dry run and the write, and refusing the item with a named reason at the dry run. Weigh which one the supervised flow (dry run -> review -> write) needs; never a 412 at the write for a reviewed item.
- #1153 acceptance: a failing test first: an item dated after the dry run's receipt previews and writes with the same digest (or is refused with a named reason at the dry run, never a 412 at the write); the rule for a future-dated item is stated in the backfill reference.
- #1154 acceptance: a failing test first: one transcript line with an interrupted tool call gets the same outcome from both readers; one shared predicate (or a documented reason the two must differ).
- #1092 acceptance: every signature on the page matches `crates/memory-engine/src/engine/*.rs` (async-ness, return types, parameters); the page stops hand-copying what rustdoc already states: link to the generated API docs per method, or add a check that fails when a documented signature no longer matches.
- #1093 acceptance: `fuzz/Cargo.lock` is committed (and removed from `fuzz/.gitignore`), and the fuzz build uses `--locked`; or the reason it must float is written next to the ignore. It must keep `cargo +nightly fuzz build` and the bounded `fuzz run` row green, and the plan says how the lock is kept fresh.
- The repository is public: nothing committed names a machine path, host or person. Conventional Commits, no co-author line on branch commits. TDD: each red watched before its change. No benchmark. Tests check behaviour, never a dependency's version.

## Decisions

- **#1153: one stable preview; refuse nothing.** For every import `t_valid = min(t_created, receipt)`: memories at
  `engine/backfill.rs:447`, captures at `engine/lifecycle.rs:545` (`t_created` is the item's time for
  `Origin::Backfill | Origin::Copy`, `:536-539`). `t_created` is hashed (`backfill.rs:611`) and the receipt is excluded
  by design (`backfill.rs:78-81`, `:539-543`; the event's `created_at` is already left out). So dropping `t_valid` from
  `effects()` (`:610`) removes the receipt and nothing a reviewer sees: the report shows `t_created`, never `t_valid`
  (`FactPreview`, `:486-503`). Refusal is rejected: it compares the item's time with the receipt, so an item dated
  between the dry run's receipt and the write's is refused by one and stored by the other; the refused list is hashed
  (`:557-574`), so the write answers 412 anyway, and the fast clock's latest history is lost. Also rejected: an
  unclamped `t_valid` (a far-future memory is invisible to `t_valid <= now` reads and surfaces through `list_due`,
  against the live rule at `docs/reference/mcp-server.md:458-460`); the dry run's receipt sent back with the write (a
  client-set bound defeats that rule, and adds a header).
- **Receipt audit (#1153).** Every other preview input and store outcome reads no clock: the store's outcomes
  (duplicate, collision, `live_overlap`, superseded, created/reinforced/blocked) read store state, and change only when
  the store does, which is the 412 the preview exists for; the store's receipt, segment and queue times are never hashed;
  `ME-Backfill-Mtime` is required for a memory file, so `envelope.mtime.unwrap_or(received_at)`
  (`crates/memory-engine-mcp/src/backfill.rs:340`) never falls back. The fuzz oracle (item 1) guards anything missed.
- **#1154: one predicate on one value, in the facade's wire module.** `bootstrap/parse.rs` already reduces
  `toolUseResult` (`:92-116`) and both readers import the transcript vocabulary from `memory_engine::bootstrap`
  (`INTERRUPTED`, `parse_timestamp`, `read_json_lines`); dependencies point from the server down to the facade, never
  back. `INTERRUPTED` moves from `filter.rs:116` to `parse.rs`; its public path is unchanged. A one-value predicate,
  because the bootstrap meets `toolUseResult` at parse time and the block's `content` at pairing time and keeps no raw
  `toolUseResult` (it can hold a whole file's text). Rejected: a documented reason to differ (both readers ask one
  question of the same bytes); a home in the server crate (the bootstrap cannot depend on it).
- **#1092: a check, not links; the check parses with `syn`.** No rustdoc is hosted (docs.rs and the page's own Pages
  link answer 404; the crate is `publish = false`), so a per-method link has no target, and one would rot unchecked on
  a rename as the page's link did. A test inside `cargo test --workspace --all-features` is where every change is
  verified, as `tests/gate_matrix_parity.rs` is for the gate matrix. It compares parsed signatures: `syn` 2 and `quote`
  are already in the root lock (2.0.119, 1.0.47), so two dev-dependency edges replace a hand-written Rust signature
  parser and give owner-type scoping from `impl` blocks for free. Rejected: dropping the signature column (neither of
  the issue's options; the Sphinx site would show no signature); a text normaliser (about twice the code, and it
  fails open on any layout, comment or `cfg` it does not model). Cost: the root `Cargo.lock` changes, so the image
  build and the MSRV rows join Verification.
- **#1093: commit the lock; `fetch --locked` stands for the flag cargo-fuzz lacks.** cargo-fuzz 0.13.2's `build` has no
  `--locked` (its `--help`, read 2026-11-23, lists no lock, offline or frozen option; re-read at item 5 and use the flag
  if a newer cargo-fuzz has one); `cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml` fails on a missing
  lock or one cargo would change (the Containerfile's `cargo fetch --locked` is the precedent), and must run before
  `fuzz build`, which would otherwise rewrite a stale lock silently. That order is held by ci.yml's step order and the
  `fuzz-build` recipe; `gate_matrix_parity` compares sets, so in the doc matrices it is a convention. **Freshness:** the lock records the path crates' versions, so every
  workspace version bump (each release) and every dependency change in a crate fuzz/ reaches fails the row; the fix is
  the re-seed `cp Cargo.lock fuzz/Cargo.lock && cargo +nightly fetch --manifest-path fuzz/Cargo.toml`, committed with
  the change, which also realigns the shared versions with the root lock. Rejected: a post-build
  `git diff --exit-code fuzz/Cargo.lock` (fires after the build used floated versions; `gate_matrix_parity` cannot
  hold a non-cargo line); joining fuzz/ to the root workspace (its targets build only on nightly with `--cfg fuzzing`).

## Implementation sequence

The branch `fix/1156-backfill-ready` is at main 9ed349a (#1155 merged; its tree equals the head these line numbers
were read on). Items 1 and 2 first: they gate #1098.

- [ ] **1. #1153: one preview from dry run to write for an item dated after its receipt**
  - Files: `tests/backfill_import_test.rs`, `crates/memory-engine/src/engine/backfill.rs`,
    `fuzz/fuzz_targets/backfill_upload.rs`, `fuzz/seeds/backfill_upload/README.md`,
    `fuzz/seeds/backfill_upload/transcript-future` (generated), `docs/reference/mcp-server.md`, `docs/backfill.md`.
  - Red (engine): a new `fn review() -> Duration` (10 minutes) beside `received()` (`tests/backfill_import_test.rs:28`);
    the dry-run-then-write helper (`:171-188`) sends its write as a clone of the upload with
    `received_at = received() + review()`, as the route's two requests are; reusing one receipt is what hid the defect. `imported_facts_are_backdated_to_their_event` then expects the write's receipt as `created_at`.
    New `a_future_dated_item_writes_under_its_dry_runs_preview`: a main completed stop and a memory item, each dated
    `received() + 5 min` (between the receipts: the case that sinks a refusal) and `received() + 1 h` (after both),
    through the helper; asserts the write committed with the dry run's `facts`, and each stored memory's
    `t_created == date` and `t_valid == Some(date.min(write receipt))`.
    `cargo test -p memory-engine --all-features --test backfill_import_test` fails in the new test only, on `committed`.
  - Red (route): in the fuzz target's replay branch (`backfill_upload.rs:127-143`) assert the replayed write's status
    is not 412 (a dry run replayed unchanged on the store it rolled back keeps its preview, #1153); add
    `transcript-future` to the seed script (`seeds/backfill_upload/README.md:44-46`): the main transcript with every
    `2026-03-19` replaced by `2099-03-19`, and regenerate it with the script. The bounded `fuzz run` row crashes on it.
  - Change: `effects()` hashes a fact's `text`, `type`, `t_created` and `metadata`, not `t_valid`, with one comment line:
    `// Not t_valid: the earlier of t_created and the receipt, and the preview never holds a receipt time (#1153).`
    The clamp stays. `import_backfill`'s rustdoc (`:78-79`) adds that a memory is valid from the earlier of its date and
    the receipt, and that the preview binds the date, never the receipt.
  - Docs: see Documentation (the rule in the backfill reference, the 412 row).
  - Green: `backfill_import_test` passes whole (`the_preview_changes_with_every_effect_bearing_input` still sees a date
    change through `t_created`); `cargo test -p memory-engine --all-features --lib engine::` passes
    (`a_backfill_capture_is_labelled_and_backdated` still sees `t_valid == Some(delivery.time)`); the bounded fuzz run
    ends with no crash.
  - Commit: `fix(backfill): keep one preview from dry run to write for an item dated after its receipt`

- [ ] **2. #1154: one interrupted-call rule for both transcript readers**
  - Files: `crates/memory-engine/src/bootstrap/{parse,filter,mod,outcome}.rs`,
    `crates/memory-engine-mcp/src/adapters/claude_code/transcript.rs`, `docs/reference/api.md`.
  - Signature: `#[must_use] pub fn records_an_interrupt(value: &serde_json::Value) -> bool`: true for an object whose
    `interrupted` is `true`, or for a text opening with `INTERRUPTED` (a string, or an array's first `text` block with a
    string `text`). That text test equals today's prefix test over `result_text` (`adapters/claude_code.rs:190-204`) on
    every shape: an object's or number's compact JSON never opens with `[`, and in a joined array only the first text
    block can open the result. One widening: today the flag is read from `toolUseResult` only (`transcript.rs:379`), so
    a block `content` that is an object with `interrupted: true` becomes interrupted; Claude Code writes a block's
    `content` as a string or an array, and both readers now treat that shape alike (shape (f) below pins it). Rustdoc rule: a call is interrupted when its
    `toolUseResult` or its `tool_result` block's `content` records an interrupt; `# Examples` over the flag, a string,
    an array, and an object whose `stdout` merely quotes the marker. Re-exported from `bootstrap/mod.rs:29-35`.
  - Call sites. Backfill `result()` (`transcript.rs:376-379`):
    `tool_use_result.into_iter().chain(block.get("content")).any(records_an_interrupt)`. Bootstrap: the `toolUseResult`
    deserializer sets `ToolUseResult::interrupted: bool` (was `Option<bool>`, `parse.rs:79`) to
    `records_an_interrupt(&value)` for every non-null shape before consuming it; `extract_tool_calls` (`filter.rs:404-405`)
    sets `result.interrupted ||` the predicate on the `content` of the paired entry's first `tool_result` block, read
    from the raw `message.content` `Value` (`MessagePayload::content`), never from the parsed
    `ContentBlock::ToolResult`, whose `content` is a lossy `String` (`parse.rs:126-131`). Test
    helpers that build `ToolUseResult` (`filter.rs:675, 700`, `parse.rs:898`) follow the `bool`. Unchanged: the live hook
    (`claude_code.rs:102-111`, `is_interrupt`) and the claude-memory reader (`claude_memory.rs:238`) read other inputs.
  - Red: `both_readers_agree_on_an_interrupted_call` in `transcript.rs`'s tests, over six three-line sessions (a
    prompt; an assistant `tool_use` with `stop_reason` `tool_use`; its `tool_result` entry). Every line carries `uuid`,
    `parentUuid` (the prompt's is null, each next line names the previous), `sessionId` (one per shape) and
    `timestamp`, so the bootstrap pairs by UUID (`filter.rs:179-257`) and takes the session ID from the first entry.
    Shapes: (a) flag `toolUseResult.interrupted: true` only; (b) `toolUseResult` string opening with the marker; (c)
    block `content` opening with the marker beside an `Error: [Request interrupted by user for tool use]` string
    `toolUseResult` (an interrupted non-Bash call); (d) array `content` whose first text block opens with it, beside a
    flagless object; (e) a plain success; (f) an object `content` with `interrupted: true` beside a flagless object (the
    widening). Each body goes to `read()` (is the call `ToolOutcome::Interrupted`?) and to
    `MemoryEngine::bootstrap_session` on a fresh in-memory engine per shape (a zero-vector embedder stub,
    `KeywordExtractor`, `BootstrapConfig::default()`), whose `outcome_counts.failure == 1` exactly when the last call
    was interrupted (`outcome.rs` `detect_interrupted`; the prompt carries no other signal). Both verdicts must equal
    the shape's. `cargo test -p memory-engine-mcp --all-features --lib both_readers_agree` fails on (b), (c), (d) and
    (f): on (b) to (d) the bootstrap reports not failed; on (f) neither reader reports the widening yet. If the red run shows another outcome signal interfering, the fallback is mvp's twin: the
    same six sessions as one `const` in both crates' tests, asserting `ToolCallRecord::interrupted` and
    `ToolOutcome::Interrupted`.
  - Green: that test passes; `cargo test -p memory-engine --all-features --lib bootstrap::`,
    `cargo test -p memory-engine-mcp --all-features --lib adapters::claude_code` (the README table, row 20 stays
    `interrupted`) and `cargo test -p memory-engine --all-features --doc records_an_interrupt` pass.
  - Commit: `fix(import): judge an interrupted tool call by one rule in both transcript readers`; its body notes that a
    re-run bootstrap now counts a session ending in a text-only interrupt as `failure`.

- [ ] **3. #1092: api.md's signatures are the code's, and a test holds them there**
  - Files: `tests/api_doc_parity.rs` (new), `crates/memory-engine/Cargo.toml` (`[[test]] name = "api_doc_parity"`
    beside `gate_matrix_parity`, same comment style; dev-dependencies `syn = { version = "2", features = ["full",
    "visit-mut"] }` and `quote = "1"`), `Cargo.lock` (the two edges), `docs/reference/api.md`.
  - The check. Page: every table body row between `## Public Methods by Category` and the next `## ` (each table's
    header row and `---` separator are skipped); the name is the first cell's code span (`name` or `Type::name`); rows
    in the test's explicit type-row list (`Backoff`, `Settled`) are skipped, and any other row's second cell's first
    code span is its signature, elided `fn name` and all:
    `[async ](params) -> ret`, or `<generics>(params) -> ret`. The test rebuilds `[async ]fn name…` and parses it as a
    `syn::Signature`; a cell that does not parse fails as unreadable; no row at all fails ("the parser broke"). Code:
    `syn::parse_file` over every `.rs` under `crates/memory-engine/src/` (a file that does not parse fails the test by
    name, never skipped); the candidates are the `pub` fns of that name, at module level or in an `impl` block, outside
    `#[cfg(test)]` modules, each with its owner (the `impl`'s self type, its last path segment without generic
    arguments: `impl<B: Backing> MemoryEngineBuilder<B>` owns as `MemoryEngineBuilder`). A `Type::name` row matches
    only fns owned by `Type`; a bare name matches a module-level fn or a `MemoryEngine` method (`builder` is
    `MemoryEngine::builder`, `engine/builder.rs:114`); a row with no candidate fails ("no such fn"), and a bare name
    with several candidates of different signatures must be qualified, the failure naming each candidate's owner. Both
    sides are normalised by one `VisitMut` (every path cut to its last segment, a by-value binding's `mut` dropped,
    `const` dropped) and compared as `quote!` token strings. Every failing row is reported in one assertion with the
    form it should read. Guard tests: an `async` or return-type mismatch fails; `mut self` matches `self` and
    `&mut self` never matches `&self`; a `Type::name` row does not match another type's fn of that name.
  - Red: `cargo test -p memory-engine --all-features --test api_doc_parity` fails and lists at least `execute_query`
    (code `Result<QueryResponse>`, `engine/query.rs:45`), `consolidate`'s `Arc<dyn …>` parameters
    (`engine/consolidation.rs:57-62`), `StoreLock::acquire`'s `Result<Self, StoreLockError>` (`store_lock.rs:66`), the
    untyped `bootstrap_session`/`bootstrap_directory` rows (unreadable: a parameter without a type), the builder rows
    `declared_identity` and `machines` (`api.md:22-23`; "no such fn": they are `MemoryEngineBuilder` methods), and
    every async method shown sync.
  - Change: every reported cell becomes the form the check prints; the two builder rows' first cells become
    `MemoryEngineBuilder::declared_identity` and `MemoryEngineBuilder::machines`; `execute_query`'s description names
    `QueryResponse` (`results`, `diagnostics`); the ad-hoc "(async)" note goes; line 3's dead link becomes "Signatures
    are the code's, held to it by `tests/api_doc_parity.rs` (`async` first for an `async fn`, paths cut to their last
    segment); the check reads the documented rows, so a public fn the page omits is not caught.
    `cargo doc --no-deps -p memory-engine --all-features --open` builds the full API documentation; it is not
    hosted."; the Entry Points line "All public methods take `&self`" names the builder and free-function exceptions.
  - Green: the check and its guards pass; `cargo deny check` passes (no new package); the Sphinx build passes with `-W`.
  - Commit: `docs(reference): state api.md's signatures as the code does, and check them`

- [ ] **4. Boy-scout: two doc fixes, one commit each.** No executable behaviour.
  - `README.md:130` links the unhosted Pages site (404); it links `docs/reference/api.md` and names `cargo doc` for the
    full rustdoc. Commit: `docs(readme): link the API reference page instead of an unhosted site`
  - api.md's `MemoryQuery` field list omits `include_archives` (`me-types/src/types/memory_query.rs:84`); add it with
    its rustdoc's meaning. Commit: `docs(reference): list MemoryQuery's include_archives field`

- [ ] **5. #1093: the fuzz build runs from a committed lock**
  - Files: `fuzz/.gitignore` (drop line 5), `fuzz/Cargo.lock` (new), `fuzz/Cargo.toml` (header: the lock is committed,
    the fetch row holds it, the re-seed command and when it is due, and that `cargo deny` audits the root lock only),
    `.github/workflows/ci.yml` (a step before
    `cargo +nightly fuzz build`), the gate blocks of `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` and
    `.claude/skills/qa-sweep/SKILL.md` (the same line before the `fuzz build` row, commented
    `# Fuzz lock (#1093), before the fuzz build: cargo-fuzz has no --locked; re-seed per fuzz/Cargo.toml`), and
    `justfile` `fuzz-build` (the line before its build).
  - Red: `cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml` exits non-zero (no lock);
    `git check-ignore fuzz/Cargo.lock` prints the path; with only the ci.yml step added,
    `cargo test -p memory-engine --all-features --test gate_matrix_parity` fails.
  - Change: the re-seed command generates the lock (seeding probe, 2026-11-23: it adds only `arbitrary` and
    `libfuzzer-sys` beyond the root lock's packages); then the ignore line, header, CI step, four matrix lines and recipe.
  - Green: the fetch row exits 0; `git check-ignore fuzz/Cargo.lock` exits 1; `gate_matrix_parity` passes;
    `cargo +nightly fuzz build` and the bounded `fuzz run` row pass; `git status --porcelain -- fuzz/Cargo.lock` prints
    nothing after them.
  - Commit: `chore(fuzz): commit fuzz/Cargo.lock and check it before the fuzz build`

- [ ] **6. PR, review, finish.** Push the branch; open the PR against main with `Classification: Bug fix
  (user-confirmed)`, the route lines, one `Closes #N` line each for #1153, #1154, #1092, #1093 and #1156 (no negated
  phrasing), `Refs dutiona/infra#37`, the deploy note (Documentation), and a `Collateral findings` section. `/super-review` at
  Full, then `/finish-pr` (it archives this plan on its plan issue and removes the working copy). The merge is
  Michaël's.

## Documentation

- `docs/reference/mcp-server.md` § Backfill uploads, "One transaction" (`:617-628`): "payloads and facts" becomes
  "payloads and facts but a memory's validity start", and the rule: "An item dated after its upload's receipt (a
  machine whose clock ran ahead, a bad timestamp) keeps its date as its event's time, its order key and its memory's
  date; its memory is valid from the earlier of that date and the receipt of the write that stores it, as a live
  capture's is, so it is never scheduled in the future, and its dry run and its write share one preview."
- `docs/backfill.md`: the `facts` review bullet notes that a date after the harvest means a clock ran ahead or a bad
  timestamp, and the memory keeps it; the 412 row adds "or the server was upgraded".
- Deploy note (PR body): this PR changes the preview of every upload, not only future-dated ones, so a dry run reviewed
  on an older server answers 412 at its write and is run again. No backfill write has happened yet (#1098 is the
  first), so the cost is a re-run; no stored data or schema changes.
- The `records_an_interrupt` rustdoc and example; `OutcomeSignals::was_interrupted` (`outcome.rs:27-29`) and the signal
  table cite it instead of `ToolUseResult::interrupted`; api.md's reader prose (`:185-187`) and re-export bullet
  (`:291-292`) list it beside `parse_timestamp`.
- api.md (item 3), README and the `MemoryQuery` field (item 4), the fuzz header, CI step and four matrices (item 5).

## Testing

- Integration: `a_future_dated_item_writes_under_its_dry_runs_preview`, and the write helper's second receipt, which
  every write-based backfill test now exercises. Route, under fuzz: the replay assertion with the `transcript-future`
  seed. Unit: `both_readers_agree_on_an_interrupted_call` (six shapes through both readers). Doc:
  `records_an_interrupt`. Parity: `api_doc_parity` and its three guards. Gate: the fetch row, held in all four
  matrices by `gate_matrix_parity`.
- Each new test is watched red where the defect is before its change; the `(a)` and `(e)` shapes pass before and after
  and pin the parity the refactor keeps.
- No HTTP test for #1153 beyond the fuzz replay: the route's only receipt input is `Utc::now()` into
  `Upload::received_at` (`memory-engine-mcp/src/backfill.rs:328`), and it maps an uncommitted, uncollided write to 412
  (`:393-398`). No test reads a dependency's version.

## Verification

At the final clean HEAD, every line unpiped, one heavy command at a time; a failed or cut-off run is re-run:

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
cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml
cargo +nightly fuzz build
cargo +nightly fuzz run backfill_upload fuzz/corpus/backfill_upload fuzz/seeds/backfill_upload -- -create_missing_dirs=1 -max_total_time=60 -timeout=10
cargo +1.89 build --workspace --tests --examples
cargo +1.89 build --workspace --tests --examples --all-features
git status --porcelain -- Cargo.lock fuzz/Cargo.lock
```

Then, because item 3 changes the root `Cargo.lock`, AGENTS.md's image build from the committed tree (`git archive
HEAD | podman build …`, both binaries' `--version`, `podman rmi`). Expected: every line exits 0 and the `git status`
line prints nothing. `git log --format=%B origin/main..HEAD` holds no `Co-Authored-By`; the diff names no machine
path. N/A: the plugin gate (no `plugins/` change).

## Surfaced, not in this PR

1. The bootstrap pairs tool results by position and counts only rows carrying `toolUseResult` (`filter.rs:159`,
   `:395-407`); the backfill reader pairs by `tool_use_id` (`transcript.rs:364-370`). A `tool_result` row without
   `toolUseResult`, or parallel calls answered out of order, still diverge. Plausible, not reproduced; follow-up
   candidate: the bootstrap pairs by `tool_use_id`.
2. api.md's field lists are hand-copied and unchecked (item 4 fixes the one omission found), and the parity check
   reads only documented rows, so a public fn the page omits goes unnoticed; follow-up candidate.
3. The bounded fuzz run still leaves an ignored `fuzz/corpus/` in a worktree that ran it (#1155's row).
4. Between releases, a root `cargo update` that moves no manifest leaves the fuzz lock's shared versions behind until
   the next re-seed; each release forces one. No release procedure is written in the repository, so that rule lives in
   the fuzz header only.

## Self-check

- #1153: test first and route oracle (1), rule in the backfill reference (Documentation). #1154: test first, one
  predicate (2). #1092: every signature, and a check (3). #1093: lock committed, ignore dropped, `--locked` through
  the fetch row, both fuzz rows green, freshness rule (5). No criterion is unmapped.
- Names agree: `records_an_interrupt`, `review()`, `api_doc_parity`, `transcript-future`, the fetch row.
