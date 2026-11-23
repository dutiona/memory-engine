Reviewer: Claude Sonnet (clean-slate general-purpose subagent, medium effort, no conversation context), via Agent tool, continued by message for rounds 2-3; stands in for Codex (degraded: quota)

Final review (round 3, plan SHA-256 578afdc576bb0c1cb6f2b730141f58d639de6cf1f44bd34ad93b16173c157aec):

~~~~markdown
# Round 3 review of the #1156 backfill-ready plan (claude-shared)

Altitude report SHA-256: 5ce9d5d4a4c0e8cb530c03a634d6dc540e12503c36ec66fc9aff4925fe8a7f6a

Verdict: LGTM. All four round-2 LOWs are resolved and the delta introduces no new gap. No finding above LOW.
Read-only: nothing run.

## Altitude (assessed first)

The hash matches the current `.review/altitude.md`. Revision 3 adds 7 plan lines, all round-2 resolutions, with no new
item; the task count (6) and the fenced literal volume are unchanged. Nothing duplicated, nothing transcribed.
Disposition "proceed" stands. No override.

## Round-2 findings against the code

- L1 (builder rows): confirmed. `declared_identity` is at `engine/builder.rs:284` in `impl MemoryEngineBuilder<File>`
  (`:237`), `machines` at `:170` in `impl<B: sealed::Backing> MemoryEngineBuilder<B>` (`:127`). Owner = self type's last
  path segment without generic arguments gives `MemoryEngineBuilder` in both cases, so the qualified first cells match.
  `builder` is `pub fn builder(embed_dim: usize) -> MemoryEngineBuilder<InMemory>` in `impl MemoryEngine`
  (`:107`, `:114`), so the bare-name rule resolves it. The api.md line numbers in the Red list (`:22-23`) are right
  (`api.md:22` `declared_identity`, `:23` `machines`; my round-2 note said 20-21, which was off by two).
- L2 (a) header and separator rows skipped: specified. (b) Unparseable file fails by name: specified. (c)
  `#[cfg(test)]` modules excluded: specified; `engine/builder.rs:335` is such a module, so the exclusion is exercised.
- L3: "six sessions" fixed in the fallback.
- L4: the multiple-candidate failure names each candidate's owner.

## New gaps from the delta

None that matter. Two notes, both LOW and optional:

[LOW] N1. `impl sealed::Backing for InMemory {}` (`builder.rs:80,92`) and other trait impls contribute no `pub fn`,
so owner collection needs no special case; but `pub fn` inside an `impl Trait for Type` block would carry the Type as
owner with no `pub` keyword normally, so nothing is picked up. Fine as written.

[LOW] N2. The `#[cfg(test)]` exclusion should match the attribute on the `mod` item (including `cfg(all(test, ...))`
only if the repo uses it; `builder.rs:335` uses the plain form). A plain-form match is enough; no change needed.

## Structural completeness and risks

Documentation, Testing and Verification are present and consistent with the delta. No new security, performance or
migration risk.

REVIEW COMPLETE
~~~~

## Resolution

Round 1 (plan 0325a663), verdict LGTM with tightenings; every finding addressed in revision 2 (plan 7471bc7e):

- M1 (#1154, the predicate widens the flag test to a block `content` object): accepted. Item 2 states the one
  widening and pins it with test shape (f); both readers now treat it alike.
- M2 (#1154, fixture fields and raw content source): accepted. Item 2's Red spells out `uuid`, `parentUuid`,
  `sessionId`, `timestamp`, a fresh engine per shape, and reads the block `content` from the raw `message.content`
  `Value`, never the lossy `ContentBlock::ToolResult` `String`.
- M3 (#1092, four fail-open holes in the parity check): accepted. Owner-type scoping, "no such fn" failure, an
  explicit type-row list, and the one-direction limit stated on the page and in the surfaced list.
- M4 (#1092, use `syn`): accepted. The check parses with `syn` 2 + `quote` (both already in the root lock);
  consequence recorded: root `Cargo.lock` edges, so the image build and MSRV rows join Verification.
- M5 (#1093, cargo-fuzz flag set unverified; order not enforced by the set-based parity test): accepted. The flag
  check (read 2026-11-23, re-read at item 5) is recorded; the order is held by ci.yml and the justfile and stated as a
  convention in the doc matrices.
- L1 (missing deploy note): accepted; written under Documentation and referenced from item 6.
- L2 (`review()` not defined): accepted; defined in item 1's Red.
- L3 (Outcome overclaim): accepted; Outcome narrowed to "one call gets one verdict", the pairing divergence surfaced.
- L4 (bootstrap outcome counts change): accepted; item 2's commit body notes it.
- L5 (fuzz lock outside cargo-deny; no release procedure): accepted; the fuzz header notes it, surfaced item 4 says the
  rule lives in the header only.
- L6 (replay-only assertion): no change; the plan already scopes it to the replay.
- L7 (heavy commands under the host lock): execution note, no plan change; the worker runs every heavy command under
  the host's lock and memory cap.

Round 2 (plan 7471bc7e), verdict LGTM with four LOWs; all addressed in revision 3 (plan 578afdc5):

- L1 (builder rows need qualification): accepted (same as agy's round-2 MEDIUM); Red list and Change step.
- L2 (table header/separator rows; unparseable files; `#[cfg(test)]` modules): accepted, all three parts.
- L3 (fallback said five sessions): fixed to six.
- L4 (name the candidates' owners on an ambiguous bare name): accepted.

Round 3 (plan 578afdc5), verdict LGTM; N1 and N2 are LOW notes that ask for no change: no action.

Degradations: this reviewer is the stand-in for Codex, the Full gate's named reviewer, which is out of quota until about
2026-11-25 (owner's 2026-11-22 roster: a Sonnet subagent at medium effort in Codex's slot, preferred over Opus because
Opus drafted the plan).
