---
name: qa-sweep
description: "Use when running a super-qa sweep, clearing a [super-qa] Auto-fix rollup issue, addressing a batch of QA/audit findings, or doing any multi-PR quality pass in memory-engine. Holds the memory-engine-specific verification gate, the 7-class failure taxonomy distilled from the #248 super-qa epic (forgetting/retrieval/mcp/temporal/storage/core/cli/consolidation sweeps), the triage→waves→orchestrate→close-out playbook, and the build-hygiene + PR-orchestration gotchas. Triggers on /super-qa in this repo, 'clear the super-qa rollup', 'address these findings', 'qa sweep', 'sweep the audit issues'."
---

# qa-sweep — memory-engine QA sweep playbook

Canonical, hard-won reference for running a quality sweep in this repo. Distilled from the
**#248 super-qa epic** (the area sweeps #251/#253/#254/#255/#256/#257/#258/#259 and the
#739/#879 doc/sim-graph tail). The committed `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` carry the
gate + the short trap list for _every_ session; **this skill carries the full playbook** so those
files stay lean. Read it before starting a sweep.

Pairs with the global `super-qa`, `finish-pr`, and `address-issue` skills (those live in
`dutiona/my-dotfiles`; file issues there for cross-repo improvements, don't fork them here).

---

## 0. The authoritative gate (YOU run it — #989)

⚠️ **CI is `workflow_dispatch`-only. It does NOT run on push or PR.** So the commands below are
not a pre-check with CI as the backstop — **they are the gate**, and nothing runs automatically to
catch a sweep that skipped them. Run the **exact** commands; a local pass that diverges (weaker
features, narrower scope, piped output — trap #1 masks the exit code) is a **false pass**, and now
an *undetected* one. Treat a PR with no dispatched run as **unverified by machine**.

```bash
cargo fmt --all --check                                                # Format
cargo clippy --workspace --all-targets --all-features -- -D warnings   # Clippy (deny warnings)
cargo build --workspace                                                # Build (default features)
cargo build --workspace --all-features                                 # Build (all features)
cargo test  --workspace --all-features                                 # Test
cargo check -p memory-engine                                           # Facade-alone, default — the TRUE archive-OFF gate (#978)
cargo check -p memory-engine --no-default-features --features backend-sqlite  # Facade-alone, no-default
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine                                   # Docs, default features (core crate only; #915 widens to --workspace)
cargo doc --no-deps -p memory-engine --all-features                    # Docs, all features
cargo deny check                                                       # Supply-chain
cargo +nightly fuzz build                                              # Fuzz gate (#993) — detached fuzz/ consumes the facade API; needs nightly + cargo-fuzz
```

The last line is the **facade-API gate** (#993): `fuzz/` is a detached workspace that no `--workspace` command compiles, yet it consumes the facade public API by path — a removal or rename can break it silently. Needs nightly + cargo-fuzz (`just fuzz-build` wraps it); incremental (a seconds-long no-op when the facade is unchanged).

The two facade-alone `cargo check`s are **not optional padding**: the `--workspace` builds cannot
cover them, because feature unification turns `archive` on for the whole workspace. They are the
only thing that compiles the configuration downstream consumers get by default — added in #978
after exactly that coverage vanished silently.

**Dispatching and monitoring CI** (it will not run itself):

```bash
gh workflow run ci.yml --ref <branch>       # dispatch
gh run list --workflow=ci.yml --limit 1     # find the run
gh run watch <run-id>                       # follow it
```

MSRV is **1.88** (let-chains). The workflow's MSRV job builds `--workspace --tests --examples` on
exactly 1.88, default _and_ all-features — a dev-dep needing newer Rust fails it. Reproduce it
locally if you touch let-chains or edition-sensitive code; it will not run on its own either.

---

## 1. The failure taxonomy (what bit us, ranked)

Every sweep finding and every rework cycle reduced to one of these seven classes. Read a finding,
name its class, apply the countermeasure.

### Class 1 — Verification false-green (most frequent; the reason this skill exists)

The gate reported pass while tests were dark or never ran.

- **Piping a cargo gate through `head`/`tail`** truncates output (hides RED) _and_ discards
  cargo's exit code (PIPESTATUS reflects the pager, not cargo). `head -N` masked 8 RED insta
  snapshots (#254); a `tail` + `$PIPESTATUS` capture came back empty (#259). → **Run unpiped, or
  redirect to a file and read the file.** The `cargo-gate-guard.sh` hook blocks the `head`/`tail`
  forms mechanically. `grep` is allowed (it doesn't truncate) but _also_ masks the exit code —
  check `${PIPESTATUS[0]}` or redirect if you filter a gate through it.
- **`clippy --all-features` compiles tests but does not run them** — a clippy-green diff can have RED
  tests (#255).
- **`cargo build` green ≠ tests green** for file moves / `include_str!` / `[[test]]` registration —
  only `cargo test` catches a dropped target. **56 eval tests were dark since #814** (a `[[test]]`
  target wasn't registered; CI was false-green for weeks) → fixed by #903/#916.
- **Proptests pass "lucky" at low case counts** — a decay-underflow flake was green at 256 cases,
  RED at 200k (#255). Raise `PROPTEST_CASES` before trusting a property.

### Class 2 — Gate incompleteness (local gate ≠ CI gate)

- **Doc-links need BOTH gates**: `rustdoc -D broken_intra_doc_links` _and_ `clippy --all-features`
  (the nursery `doc_link_with_quotes` lint). `cargo test --doc` checks failing examples, **not
  broken links** (#739/#259).
- **`clippy --fix` runs default features** and silently breaks `--all-features` (it `const fn`-ed a
  fn that's non-const under `ann`) and misses cfg-gated nests (`ann`/`archive`). Always re-run the
  exact CI clippy after any `--fix` (#667, build-hygiene).
- **An MSRV bump is a lint iceberg**, not one lint — declaring 1.88 unlocked `collapsible_if`→let-chain
  suggestions codebase-wide (#667).

### Class 3 — Genuine coverage gaps (real bugs that PASSED `cargo test`)

Tests existed and were green; the bug lived in an unexercised path. Only **adversarial review** with
a mutation/edge lens found these:

- edge swap-remove sibling corruption needing descending-sort+dedup (#257, #833 — a BLOCKER);
- `IndexInconsistent` post-commit contract (#257, #837);
- a TOCTOU in the atomic Add arm (#259, #335);
- the #312 cascade canary that was a _coverage gap_, not a bug (#255).
  → On any behavior-changing finding, add the failing test FIRST (TDD), and run ≥2 adversarial lenses.

### Class 4 — Snapshot staleness (the issue describes code that no longer exists)

- The #628/#631 reorg made **every** file:line in #259's snapshot stale
  (`conflict/temporal.rs` → `engine/conflict.rs`). Triage against **current main**, not the issue.
- A long-parked issue may already be fixed elsewhere — **recurred 3×** (#251). `gh issue view N` +
  `grep` before resuming.
- A "magic constant" may be a **named sentinel** (`Fact::UNSCORED_IMPORTANCE` = 0.5 — PRESERVE, don't
  "fix" it, #259); "dead code" may be **epic-foundation** (`remove_edge_by_id`/`expire_edge` are E0
  sim-graph scaffolding, #879). Pickaxe (`git log -S`) + check the roadmap (Projects #6) before
  changing or deleting.

### Class 5 — PR orchestration hazards (concurrency + merge mechanics)

See §3. The big ones: `closingIssuesReferences` auto-close trap, `mergeable=CLEAN` ≠ semantically
safe, subagent cwd-leak, `git add -A` sweeping build artifacts, hot-file serialization, god-split-last.

### Class 6 — Review / evidence discipline

- The gemini-code-assist bot (and now the Codex connector bot) re-post already-fixed FPs on each
  push — **verify against current bytes** before acting, and the bots are type-blind.
- **Always post refuting/fixing evidence on issue closure** (SHA / file:line / canonical #) and
  re-verify before `gh issue close`.
- Under budget, prefer **2–3 in-house adversarial subagents with diverse lenses** over `/super-review`
  (saves external-model quota).

### Class 7 — Instruction/doc drift (the guidance itself lies)

- A migration that **relocates data** breaks raw-SQL consumers elsewhere — grep the WHOLE workspace
  for consumers of the old location (#622).
- ADR parity ME↔KB is **semantic, not byte** (#853).
- These very instruction files drifted (1.85 vs 1.88, 3 vs 4 crates, a deleted `async` feature). When
  you touch the code that an `.md`/skill describes, update the doc in the same PR.

---

## 2. Sweep execution playbook

**1. Triage BEFORE implementing, against current `main`.** Fan out a read-only agent per finding →
classify {still-open / already-fixed / moot-FP / changed}. Collapse duplicates that share a location.
On past rollups this killed ~30–40% of findings before any code was written.

**2. Bucket by KIND, risk-tiered into waves.**

- _Wave 1_ behavior-preserving/additive (docs, tests, refactor, perf): parallel, light review.
- _Wave 2_ behavior-changing (correctness, security, schema, soundness): **TDD-first** (failing test →
  fix → green), ≥2–3 adversarial lenses each, a **persisted-format-compat** lens on any
  schema/serialization change.
- **Serialize hot files**: one PR per wave may touch a given hot file (e.g. `memory_graph.rs`,
  `hybrid.rs`, `ann.rs`) — otherwise rebases thrash. One PR/wave/hot-file.
- **God-split goes LAST.** A module split's real conflict isn't git — it's the test-mod `use super::{}`
  list and **duplicate test-fn names → E0428 compile error** that no textual merge detects.

**3. Orchestrate with worktrees; agents return PATCHES, you gate.** Use `isolation: 'worktree'` or
inline worktree edits. Agents' self-gates are advisory — **you** apply each patch to a controlled
`.claude/worktrees/` branch, run the authoritative gate (§0) yourself, commit, PR. Watch for:
subagents inheriting the **main** cwd (their relative-path edits land in `main`, not the worktree —
use absolute worktree paths); schema/`insta` agents leaking into the main checkout (`git -C <main>
status --porcelain` after). Throttle bursts to ~9 concurrent agents (18 got rate-limited).

**4. Re-gate every branch against THEN-CURRENT main right before merge.** `mergeable=CLEAN` ≠
compiles: a sibling can drift an indirect dependency in a file your patch never touched (main moved
~7× during one sweep). `git diff --name-only <base> origin/main` for overlap; `git rebase
origin/main` + re-run the full gate on the combined state. Commit `fmt` BEFORE gating.

**5. Close-out with evidence.** Non-final PRs use `Refs #N` / `Part of #N` (never `Closes/Fixes #N`
— that auto-closes the umbrella and orphans remaining waves). The final PR uses exactly one
`Closes #N`. Close the rollup with a disposition table mapping every finding →
{merged PR / already-fixed / FP / carved-out issue}. File epic-scale/needs-design findings as their
own labeled issues (per the PM contract in CLAUDE.md), don't fold them in.

---

## 3. PR-orchestration gotchas (Class 5, expanded)

- **`closingIssuesReferences` auto-close trap.** GitHub parses `close|fix|resolve(+s/+d) #N` from PR
  title, body, **and commit messages**. Verify before merge:
  `gh api graphql -f query='…pullRequest(number:N){ closingIssuesReferences{ nodes{ number } } }'`
  — want `[]` (non-closing) or `[N]` (the intended final close).
- **Subagent cwd-leak.** Fan-out agents inherit the main session cwd, not a worktree. Do worktree
  edits inline with absolute paths; read-only reviewers must use `gh pr diff <N>` + absolute paths.
- **`git add -A` after a build-running subagent** swept a 4.1 MB ELF onto main (#726). Stage explicit
  paths, or `git status --porcelain` and eyeball every untracked entry first.
- **Coordinate with concurrent sessions.** The user runs parallel sessions that merge siblings and
  edit shared files (incl. MEMORY.md) mid-sweep. Re-check each PR `state` before merging; respect
  fenced-off PRs.
- **`gh pr edit` label changes** sometimes fail via GraphQL → use the REST path.
- **`isolation:'worktree'` leaks branch-locking worktrees** — clean them up (`git worktree remove`)
  or the branch can't be deleted post-merge.

---

## 4. Build-hygiene gotchas (Class 2, expanded)

- **`significant_drop_tightening` is ~always a FALSE POSITIVE** on the store wrappers (they borrow
  `&conn` transitively; clippy misses it). Use a scoped `#[allow(…, reason = "…")]`.
- **The clippy gate is `-D warnings` workspace-wide now** (pedantic/nursery cleared to 0 + ~19
  justified `#[allow]`s). Keep changed code 0-warning or add a justified `#[allow]`.
- **`Cargo.lock` is gitignored** — a `cargo update -p X` commits nothing; CI resolves fresh. A
  RustSec advisory is closed by raising the MSRV floor (which excludes the vulnerable version via the
  MSRV-aware resolver), _not_ by a committed pin. Say that in the PR.
- **Mechanical fix patterns**: `as`→`try_from` (truncation/sign); count→`f64` keeps `as f64` + scoped
  `cast_precision_loss` allow (NOT saturating `try_from().unwrap_or(MAX)` — not lossless); >3-bool
  struct → one bool becomes a meaningful enum; `too_many_lines` → extract a cohesive helper.

---

## 5. The mechanical guard

`utils/scripts/hooks/cargo-gate-guard.sh` is a PreToolUse/Bash hook that **blocks** piping a
`cargo (test|clippy|build|doc)` through `head`/`tail` (Class 1, trap 1). Its logic is committed and
reviewable; its activation lives in the gitignored `.claude/settings.json` (local-only — the
committed `.md` files remain the guardrail that reaches every clone). Wiring is documented in the
script header and `CLAUDE.md`.
