# fix(search): hnsw 0.11 panics when a scoped ANN walk returns fewer neighbours than dest holds (#1074)

## Outcome

With the `ann` feature on, `HnswStrategy::search` can no longer panic, scoped or not. `hnsw` 0.11.0 is vendored at
`vendor/hnsw` with a two-line fix, so a walk shorter than `dest` returns its filled prefix. The copy is wired as a
direct path dependency of `me-backend-sqlite`, so every build that compiles `ann` gets the fix: this workspace, the
image, the detached `fuzz/` workspace, and git consumers who enable `ann` as `README.md` and
`docs/getting-started/installation.md` describe. ME also keeps HNSW's `ef >= k` contract: each attempt's `ef` is at
least the candidate window, so a window wider than `ef` is no longer cut to `ef`. A walk can still fall short of the
window (the window equals the index size, or duplicate clusters). Its prefix is then post-filtered like any candidate
set: if at least `limit` candidates survive, the attempt is served as is; otherwise the existing loop widens and
finally falls back to brute force. Before this change such a walk ended as a join error, and the query failed. Each
trigger has a unit test that fails on today's code. The HNSW variant of the filter-before-top-k test that #1020
dropped is restored and passes.

**No query that succeeds today changes.** Item 4 only changes a call where `found < dest.len()`, which panics today.
Item 5 only raises `ef` when `dest_len > ef`; the walk's pool never exceeds `ef`, so that call also panics today.
Graph topology (insertion is untouched), the snapshot format, the public API and default-feature builds do not change.

## Constraints

- Acceptance: A failing test reproduces the panic (the HNSW variant #1020 dropped).
- Acceptance: Fix by one of: (a) size `dest` to what the walk can return and handle a short result in ME's code; (b) a `[patch]` of the crate with the fix, pinned with its reason; (c) replace the crate with a maintained one (e.g. `hnsw_rs`, `instant-distance`). The PR compares them and says why.
- Acceptance: The dropped HNSW test variant is restored and passes.
- The plan's core is the (a)/(b)/(c) comparison; it states a recommendation and the alternatives; Michaël picks at approval.
- Judge each option on correctness, maintenance, blast radius in ME, dependency hygiene (`cargo deny`).
- The deployed image builds default features without `ann`; the served path is unaffected today. Keep the fix inside the `ann` feature unless the plan shows otherwise.
- Restore the HNSW variant of the filter-before-top-k test #1020 dropped (PR #1069). It must fail first.
- The repo's AGENTS.md verification block runs with `--all-features` (the panic lives behind `ann`).
- The PR carries `Closes #1074` and `Refs dutiona/infra#37`. memory-engine is public: no home paths, host names or tokens.
- Every cargo and podman command runs under the machine's heavy-build lock with `CARGO_BUILD_JOBS=16`.
- Review: `/super-review` at the Full gate. Michaël squash-merges; the implementer never merges.

## Root cause and triggers

In hnsw 0.11.0, `search_layer` (`src/hnsw/hnsw_const.rs`, which `nearest` calls) ends with
`let found = min(dest.len(), searcher.nearest.len()); dest.copy_from_slice(&searcher.nearest[..found]);`.
`copy_from_slice` panics unless `found == dest.len()`. The walk's pool holds at most `ef` nodes, and only the nodes
the layer-0 walk reaches. That walk skips nodes already marked `seen` on the upper layers, and duplicate clusters cut
it further. `Searcher`'s fields are private, so a caller cannot learn the walk's size in advance. ME sizes
`dest = min(3·limit·2^attempt, n_items)` against `ef = 100·2^attempt`. That gives two triggers, both measured with
a seeded probe of hnsw 0.11.0 using ME's parameters:

1. **`dest > ef`.** A strategy limit of 34 or more panics on the first attempt over more than 100 facts (probe:
   `dest` 102 against `ef` 100 panics; 99 passes). The hybrid path asks the vector channel for three times the user's
   `limit` or `rerank_depth`, so a user limit of 12 or more hits this, scoped or not.
2. **`dest` larger than the walk.** A filtered query widens `dest` up to `n_items`, which the walk rarely reaches
   (probe at 67 nodes: `dest` 67 at `ef` 400 panics on random vectors; with 50 duplicates, `dest` 60 at `ef` 200
   already panics). This is the path of #1020's test.

Upstream (rust-cv/hnsw) has not released since 0.11.0 (2021-07-19). Its `main` still carries both lines. Issues
rust-cv/hnsw#24 and rust-cv/hnsw#31 report this panic and are open. PR rust-cv/hnsw#40, the same one-line fix, was
closed unmerged on 2026-08-17.

## Options

| Option | Trigger 1 | Trigger 2 | Reaches | Maintenance | Blast radius in ME | `cargo deny` |
|---|---|---|---|---|---|---|
| (a) ME sizing only (`ef >= dest`, clamp `dest`) | removed | **remains**: the walk size is private; only `catch_unwind` (panic-hook noise, aborts under `panic = "abort"`) could absorb it | all builds | none | `ann.rs` | unchanged |
| **(b) vendored copy + two-line fix, direct path dependency, plus (a)'s `ef >= dest` — recommended** | removed | removed by construction: both slices have length `found` | every `ann` build, `fuzz/` and git consumers included | ME owns a frozen ~760-line copy of a dormant crate; it changes only when ME changes it | `vendor/hnsw/`, 3 manifests, `.containerignore`, lock, one rule and one comment in `ann.rs` | no `deny.toml` change: a path crate is not a source; its MIT licence ships with it |
| (b′) the same copy through a root `[patch.crates-io]` | removed | removed | only builds rooted here: Cargo reads `[patch]` from the root workspace only, so `fuzz/` (which enables `ann`) and git consumers keep the panic | as (b) | root manifest | as (b) |
| (b″) pinned git fork | removed | removed | as (b′) via `[patch]`, or as (b) as a direct git dependency | a public fork outside this repo, reviewed outside this PR | one manifest | needs a `[sources] allow-git` exception to `unknown-git = "deny"` |
| (c) `hnsw_rs` 0.3.5 (the only maintained pure-Rust incremental candidate) | removed | removed (`search` returns a `Vec`) | all builds | maintained (releases 2025-06 to 2026-10) | rewrite of `HnswInner` build, insert, search and notify; topology, snapshot-rebuild and recall tests re-baselined | 16 non-optional deps (rayon, mmap-rs, env_logger, anyhow, lazy_static…), 98 `unsafe`, and `std::process::exit` plus `println!` in library code (`Hnsw::new` exits the process when `max_nb_connection > 256`) |
| (e) port the walk into ME (`search/ann/graph.rs`, ~300 lines, MIT notice kept), drop `hnsw` and `space` | removed | removed: `nearest(query, k, ef) -> Vec<usize>` | all builds | ME's own code under ME's lints and tests; future HNSW fixes become ordinary ME changes | `search/ann/` split into a module, a temporary differential test against hnsw 0.11, docs | removes `hnsw`, `space`, `ahash` 0.7, `hashbrown` 0.11, `libm` from the lock |

Not viable: `instant-distance` (repository archived, batch build only, so it cannot serve `notify_insert`), `usearch`
(C++ core through `cxx` and `build.rs`; its 3.0 redesign landed on `main` 2026-11-10), `arroy` (LMDB on-disk model),
and deleting `ann` (SQLite loses its only scaling path, and the restored test needs HNSW).

**Recommendation: (b).** Confidence: high that it removes the panic; medium-high on the vendoring plumbing until item
2's checks run.

- **Why:** it is the smallest provably correct fix. Two lines in code ME already ships. Every query that succeeds
  today copies the same bytes and returns the same neighbours (see Outcome). ME's loop already reads the returned
  slice, never `dest`. It keeps the reasons `docs/design/design-choices.md` gave for choosing `hnsw`: pure Rust, no `unsafe`, few
  dependencies. It keeps `deny.toml`'s crates.io-only sources.
- **Why a path dependency rather than the issue's literal `[patch]`:** the `[patch]` would leave `fuzz/` and every
  git consumer on the panicking crate. The PR states this deviation.
- **The zero-compromise ideal is (e).** It removes the frozen copy and two crates from the build. The structure is
  cleaner, and the next HNSW fix becomes ordinary ME code. It costs a ~300-line port review and a differential test
  now, for a feature that no deployment builds. Nothing persisted couples ME to the graph implementation: the
  snapshot holds `(fact_id, embedding)` only and every load rebuilds the index. So (e), or (c), stays open later at
  no extra cost, for instance when ANN is first deployed.
- **Future cost of (b):** a path dependency blocks `cargo publish` of `me-backend-sqlite` and the facade. Both are
  `publish = false` today.

**Decisions for Michaël at approval.**

- **D1, the option.** (b) is recommended.
  - (b″) as a direct git dependency meets the same Outcome, but the fix then lives in the fork.
    - Item 2 forks rust-cv/hnsw and pins `rev` to the 0.11.0 release commit `a50060296360d9dc9ba0f714d24991a12714f604`,
      with a `[sources] allow-git` entry for the fork.
    - Item 3 runs red against that pin.
    - Item 4 pushes the two-line fix to the fork and bumps `rev` to that commit; the PR links the fork commit as the
      reviewable diff.
    - Items 3 and 5 are otherwise unchanged.
  - (b′), and (b″) through `[patch]`, keep items 3–5 (with (b″)'s fork mechanics for item 4). They fail the
    Outcome's every-build claim:
    `fuzz/` and git consumers keep the panic. Choosing either narrows the Outcome to builds rooted in this
    workspace, and the PR states that.
  - (e) or (c) changes the approved intent. The plan is then rewritten as a complete successor with its own red/green
    items before any implementation. Items 1, 3 (the tests), 6 and 7 carry over.
- **D2, upstream's unreleased `seen` fix** (rust-cv/hnsw 4ce5e6d) and the duplicate-cluster starvation in hnsw 0.11's
  neighbour pruning. Both shorten walks and cost recall silently, but neither is needed for the panic fix. The fix
  changes topology, recall and the seeded tests. Recommended: keep it out of this PR and report it as a follow-up
  with a recall baseline.

## Implementation sequence

Every cargo and podman command runs under the heavy-build lock (see Verification). Branch commits carry no co-author
line.

- [ ] **0. Worktree, baseline and plan issue.**
  - Isolation: the worktree `.worktrees/fix-1074-hnsw-short-walk` on `fix/1074-hnsw-short-walk` already exists and
    is locked. `git worktree list --porcelain` shows it `locked`, and `git status --short` is clean.
  - Baseline: run `cargo test -p me-backend-sqlite --all-features`,
    `cargo test -p memory-engine --all-features --test eval --test ann_recall_test --test all_features_store` and
    `cargo deny check`. All pass, and their output is kept, so every later red is attributable.
  - The lifecycle publishes this plan as the one plan issue. No other is opened. No commit.

- [ ] **1. Drop the facade's unused `hnsw`, `space`, `rand` (boy-scout, own commit).**
  - Files: `crates/memory-engine/Cargo.toml`, `crates/me-backend-sqlite/Cargo.toml`, `Cargo.lock`.
  - Red: N/A, no behaviour. `git grep -nE '(^|[^[:alnum:]_])(hnsw|space|rand)::' -- crates/memory-engine tests
    benches examples fuzz` prints nothing today.
  - Change: delete the facade's three optional dependencies and set `ann = ["me-backend-sqlite/ann"]`. In
    `me-backend-sqlite`, the comments that say they mirror the facade's dependencies and feature "exactly" now say
    that the facade's `ann` forwards here.
  - Green: these exit 0: `cargo build --workspace --all-features`, `cargo check -p memory-engine --features ann`,
    `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
    `cargo test -p memory-engine --all-features --test ann_recall_test`. `git diff Cargo.lock` touches only the
    facade's dependency list. If a build fails, a facade target uses one of them: drop this item and say so.
  - Commit: `chore(deps): drop the facade's unused hnsw, space and rand dependencies`

- [ ] **2. Vendor `hnsw` 0.11.0 unmodified as a path dependency.**
  - Files:
    - new: `vendor/hnsw/{Cargo.toml,LICENSE,src/**}`;
    - edited: `crates/me-backend-sqlite/Cargo.toml`, `Cargo.toml`, `.containerignore`, `Cargo.lock`.
  - Source: download `hnsw-0.11.0.crate` from `static.crates.io` into a fresh scratch directory outside the
    repository. Its `sha256sum` must equal the lock's
    `2b9740ebf8769ec4ad6762cc951ba18f39bba6dfbc2fbbe46285f7539af79752`. Copy `src/` and `LICENSE` byte for byte.
  - Manifest: `vendor/hnsw/Cargo.toml` keeps upstream's `[package]`, `[dependencies]` and `[features]` verbatim. It
    drops `[dev-dependencies]`, `[[bench]]` and `[profile.*]`, because their files are not vendored. A header comment
    records the crate checksum, the reason (#1074) and the deviation list.
  - `crates/me-backend-sqlite/Cargo.toml`: `hnsw = { path = "../../vendor/hnsw", optional = true }`. Next to it goes
    the pin's reason: no upstream release since 2021-07-19, and rust-cv/hnsw#40 closed unmerged. It is a path
    dependency because `[patch]` only applies at the root workspace. Replace it when a crates.io release carries the
    fix.
  - `Cargo.toml`: `exclude = [".worktrees", "fuzz", "vendor"]`. A path crate under the workspace root otherwise
    joins as a member, so its comment names `vendor/`: third-party crates carried with a fix, path dependencies, not
    members.
  - `.containerignore`: add `!vendor/` after `!crates/`, and name it in the header comment. The image's
    `cargo fetch --locked` reads every path dependency's manifest, even with `ann` off.
  - Red: N/A, no behaviour change. `diff -r <scratch>/hnsw-0.11.0/src vendor/hnsw/src` and the `LICENSE` diff print
    nothing.
  - Green (commit first: the image build reads `HEAD`):
    - `cargo metadata --format-version 1 --no-deps` (to a file, then `jq`) still lists 18 workspace members.
    - `cargo tree -p me-backend-sqlite --features ann -e normal -i hnsw` shows one `hnsw v0.11.0 (…/vendor/hnsw)`.
    - These exit 0: `cargo build --workspace --locked`; `cargo build --workspace --all-features`, logged to a file;
      `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
      `cargo deny check`, with no finding beyond the baseline's.
    - These pass too: `cargo +nightly fuzz build` and the AGENTS.md image build with both `--version` runs.
  - Remedies, applied only if their check fails, never by editing the vendored `src/`:
    - The build log shows rustc warnings from `hnsw` (Cargo does not cap a path dependency's lints): allow exactly
      those lints in `[lints.rust]` of `vendor/hnsw/Cargo.toml`.
    - `cargo fmt --all --check` lists `vendor/` files: add `vendor/hnsw/rustfmt.toml` with
      `disable_all_formatting = true`. `workspace.exclude` alone does not prevent this, because `cargo fmt --help`
      says `--all` formats "all packages, and also their local path-based dependencies". The option is stable:
      rustfmt 1.11.0-stable's `--help=config` lists it, and lists no unstable option. The same green check proves
      it is honoured.
  - Commit: `chore(deps): vendor hnsw 0.11.0 as a path dependency`

- [ ] **3. Reproduce both triggers and restore the #1020 variant (red).**
  - Files: `tests/eval/conformance/scope_isolation.rs`; `crates/me-backend-sqlite/src/search/ann.rs`
    (`hnsw_strategy_tests`).
  - **E1** `exact_set_filters_before_top_k_on_every_channel_with_hnsw`:
    - `#[cfg(feature = "ann")] #[tokio::test]`, with its imports inside the function.
    - It calls `assert_filtered_before_top_k` on
      `MemoryEngine::builder(DIM).search_config(SearchConfig { ann_threshold: 0 }).build()`.
    - This is trigger 2: 67 facts, 55 sharing the query vector, channel limit 30, widened `dest` capped at 67.
  - **T1** `hnsw_search_serves_a_window_wider_than_ef_in_one_attempt` (trigger 1):
    - Fixture: 400 Semantic facts. Each 4-d embedding has components `rng.gen_range(-1.0_f32..1.0)`, drawn in fact
      order from one `SmallRng::seed_from_u64(7)` (`gen` is reserved in edition 2024).
    - Query `[1.0, 0.0, 0.0, 0.0]`, no `fact_type`, no scope.
    - For `limit` in `[34, 120]`, `search` inside the existing `count_batch_queries` helper returns `limit` results
      in exactly one batched query.
    - At `limit = 34` after item 4, the walk fills at most 100 of 102 slots and the unfiltered 100 survivors cover the
      limit. So this is the "short window, enough survivors, no widening" case. `limit = 120` is the "short window,
      too few survivors" case that item 5 removes.
  - **T2** `hnsw_search_widened_to_the_whole_index_returns_every_match` (trigger 2):
    - Fixture: insert 55 Procedural facts at `[1, 0, 0, 0]` first, then 12 Semantic facts at `[0.1·k, 1, 0, 0]` for
      k = 1..=12.
    - `search(conn, &[1.0, 0.0, 0.0, 0.0], DIM, 12, Some(&FactType::Semantic), None)` returns exactly the 12
      Semantic ids, compared as a set. It checks the result only: no panic, and every match is found. It does not
      assert which path served it, widening or brute force.
  - Fixtures use `me_types::test_util::new_fact_with_type` and lossless conversions: clippy pedantic runs on tests
    under `-D warnings`.
  - Red: `cargo test -p me-backend-sqlite --all-features hnsw_search_` and
    `cargo test -p memory-engine --all-features --test eval exact_set_filters_before_top_k`.
    - The brute-force variant passes.
    - E1, T1 and T2 fail with `source slice length (…) does not match destination slice length (…)`. E1 shows it as
      `query failed: Pool("task join error: …")`.
    - The output is kept for the PR.
    - If T2 passes here, it does not exercise trigger 2: stop and report rather than tune the fixture blindly.
  - Commit (intentionally red: the branch is squash-merged, and this commit is the checkable "fails first"):
    `test(search): reproduce the hnsw short-walk panic`

- [ ] **4. Copy only the filled prefix (the fix).**
  - Files: `vendor/hnsw/src/hnsw/hnsw_const.rs`, `vendor/hnsw/Cargo.toml` (deviation list).
  - Red: item 3's commands, unchanged.
  - Change: at both returns of `search_layer`, `dest.copy_from_slice(&searcher.nearest[..found]);` becomes
    `dest[..found].copy_from_slice(&searcher.nearest[..found]);`. Each gets the comment
    `// memory-engine #1074: copy only the found prefix; upstream panics on a short walk.` Nothing else in the crate
    changes. The crate then keeps its own doc promise on `nearest`, that a short pool fills only part of `dest`.
  - Green:
    - E1 and T2 pass.
    - T1 no longer panics. It still fails at `limit = 120`, with 2 batched queries: the walk returns at most `ef`.
    - The rest of `cargo test -p me-backend-sqlite --all-features` passes.
    - `cargo test -p memory-engine --all-features --test ann_recall_test --test all_features_store` passes.
    - `diff -r` shows exactly the two hunks.
  - Commit: `fix(search): return a short hnsw walk's filled prefix instead of panicking`

- [ ] **5. Keep `ef` at least the candidate window.**
  - Files: `crates/me-backend-sqlite/src/search/ann.rs`.
  - Red: `cargo test -p me-backend-sqlite --all-features hnsw_search_serves_a_window_wider_than_ef_in_one_attempt`
    fails at `limit = 120` with 2 batched queries.
  - Change:
    - `HnswStrategy::search` passes `ef.max(dest_len)` to `nearest`. The doubling of `ef` and `overfetch` stays.
    - The comment above `n_items` ("dest length must not exceed the number of indexed items") states the wrong
      bound. It becomes: a walk fills at most `ef` slots and can reach fewer nodes than `dest` holds; the vendored
      `hnsw` returns the filled prefix, and the widening and fallback below serve a short result; `ef >= dest_len`
      keeps the overfetch the window asks for.
  - Green:
    - `cargo test -p me-backend-sqlite --all-features` passes in full.
    - `cargo test -p memory-engine --all-features --test ann_recall_test --test eval` passes with no threshold edited.
      Editing a threshold is a stop signal.
    - `cargo clippy -p me-backend-sqlite --all-targets --all-features -- -D warnings` is clean.
  - Commit: `fix(search): keep the hnsw ef at least the candidate window`

- [ ] **6. Drop stale line references from the ann tests (boy-scout, own commit).**
  - Files: `crates/me-backend-sqlite/src/search/ann.rs`.
  - Red: N/A, comments only. The test comments cite `(ann.rs:248-253)` and `(ann.rs:230-235)`, which point at moved
    code.
  - Change: name the code they mean instead (the `n_items == 0` early return; the dimension check).
  - Green: `grep -n 'ann\.rs:[0-9]' crates/me-backend-sqlite/src/search/ann.rs` prints nothing, and
    `cargo fmt --all --check` exits 0.
  - Commit: `chore(search): name the code that the ann test comments cite`

- [ ] **7. Documentation.**
  - Files and texts: see Documentation below.
  - Red: N/A, prose.
  - Green: `uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build` exits 0.
    `cargo test -p memory-engine --test gate_matrix_parity` passes; it parses `AGENTS.md`.
  - Commit: `docs(search): record the vendored hnsw and the ef rule`

- [ ] **8. PR.**
  - Run the whole Verification section on the final HEAD. Then push and open
    `fix(search): stop hnsw panicking when a walk returns fewer neighbours than dest holds`.
  - The body carries:
    - `Closes #1074` and `Refs dutiona/infra#37`;
    - the classification and route lines;
    - the condensed Options table, with why (b) and why a path dependency rather than `[patch]`;
    - item 3's red output, and item 4's `diff -r`;
    - the Verification results;
    - the D2 follow-up;
    - the collateral findings.
  - The body uses repository-relative paths only.

- [ ] **9. Review.** Run `/super-review` at the Full gate (Codex + agy). Each accepted finding gets its own commit,
  with the Verification lines it touches re-run. Then run `/finish-pr` to READY. It archives this plan on the one plan
  issue and removes `docs/plans/` from the branch.

- [ ] **10. Merge.** Michaël squash-merges. The squash commit carries exactly one co-author line.

## Documentation

- `vendor/hnsw/Cargo.toml` header and `crates/me-backend-sqlite/Cargo.toml`: provenance, reason, deviations and exit
  condition (items 2 and 4).
- `docs/design/design-choices.md`: the ANN-backend decision (§3) gains "vendored with one fix (#1074): upstream
  panicked on a short walk, has not released since 2021 and closed the fix unmerged; a path dependency, so every
  build gets it". The widening description states `ef >= window`.
- `docs/advanced/hybrid-search.md`: the HNSW bullet names `vendor/hnsw`. The widening loop gives per attempt
  `ef = max(100·2^attempt, window)` with `window = min(limit·3·2^attempt, index size)`, and says that a walk shorter
  than the window widens like a filtered one.
- `docs/getting-started/installation.md`: `ann` adds `space`, `rand` and the vendored `hnsw`. The image allowlist
  includes `vendor/`.
- `AGENTS.md` and `GEMINI.md`, Project Layout: `vendor/  # third-party crates carried with a fix (path deps, not
  workspace members)`.
- No public API change, so no samples. `docs/ROADMAP.md` is frozen and stays as it is.

## Testing

- **Unit** (`me-backend-sqlite`, `ann`):
  - T1 covers trigger 1. Its two limits also separate the two outcomes of a short window: enough survivors, served
    in one attempt (`limit = 34`), and too few, which needs `ef >= dest_len` to stay one attempt (`limit = 120`).
  - T2 covers trigger 2. It asserts the result (no panic, every filtered match returned), not whether a widened
    attempt or the brute-force fallback served it. Asserting the path would pin the walk's length, which depends on
    the D2 defect.
- **Integration** (`memory-engine`): E1 runs engine → `SqliteBackend::vector_search` → `spawn_blocking` → strategy
  on all three channels, and restores C16's HNSW coverage.
- **Regression guards, unchanged and green:**
  - every seeded `ann.rs` test (topology, rebuild, snapshot round trip, tombstones, batching);
  - `tests/ann_recall_test.rs`, with unchanged thresholds;
  - `tests/all_features_store.rs`;
  - the Stage B dispatch tests in `crates/me-backend-sqlite/src/sqlite/search_index.rs`;
  - `benches/search_bench.rs`, compiled by clippy `--all-targets`.
- **Not added:**
  - a property test over random graphs: the fix is total by construction;
  - a test pinning the vendored bytes or version: tests check behaviour, and the `diff -r` is a review step;
  - an image e2e: the image builds without `ann`.
- **Acceptance map:** "a failing test reproduces the panic" is item 3. "Fix by one of (a)/(b)/(c)", with the
  comparison, is items 2, 4 and 5 plus the PR body. "The dropped variant is restored and passes" is item 4's green
  run of E1.

## Verification

On the final HEAD, every cargo and podman command runs under the machine's heavy-build lock with
`CARGO_BUILD_JOBS=16` exported. Commands run unpiped, with long output redirected to files. CI is manual-dispatch
only, so this matrix is the merge gate. The AGENTS.md Verify block, exactly:

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

Dependencies, `Cargo.lock` and `.containerignore` change, so the MSRV job and the image build run too:

```bash
cargo +1.89 build --workspace --tests --examples
cargo +1.89 build --workspace --all-features --tests --examples
git archive --format=tar HEAD | podman build --pull=newer --layers=false --memory=32g \
    --cpu-period=100000 --cpu-quota=1600000 -t memory-engine:verify -
podman run --rm --network=none memory-engine:verify --version
podman run --rm --network=none --entrypoint memory-engine-cli memory-engine:verify --version
podman rmi memory-engine:verify
```

Expected results:

- Every command exits 0.
- The test log lists E1, T1 and T2 as `ok`; a cfg-gated test that never ran is a false green.
- `cargo deny check` reports nothing beyond the baseline.
- If the Postgres parity tests (testcontainers) time out under load, re-run them alone under the lock before calling
  it a regression, and report both runs.

The plugin gate is N/A, since `plugins/` is untouched.
