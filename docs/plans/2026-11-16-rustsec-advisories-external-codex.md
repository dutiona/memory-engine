# External review: Codex (headless, persistent thread 01a1e579-96af-7ef0-bac7-bd5ff45dbd52)

Round 1 ran on the Codex config default (gpt-6.1-terra), before Michaël's 2026-11-16 preference for gpt-6.1-sol arrived; rounds 2-4 resumed the same thread on gpt-6.1-sol (Codex warned that the session was recorded with gpt-6.1-terra). Each round's final message, verbatim. Absolute paths of the private review snapshot are shortened to `<review-tree>/`; nothing else is edited.

---

## Round 1

Michaël, I accept the altitude report’s **proceed** disposition: the plan is detailed, but its literal text mostly specifies decisions and verification rather than duplicating implementation.

Altitude report SHA-256: 812499d868adf06a99492f592edb2263253d3ae33def5d1d855eabf5a7bb2fb9

- **[HIGH] Verification and delivery — the plan permits READY with an unmet acceptance criterion.** [Verification step 2](<review-tree>/.review/plan.md:204) expects clippy to fail unless #1032 has landed, yet acceptance requires every other matrix command to pass. [Delivery step 4](<review-tree>/.review/plan.md:231) still directs `/finish-pr` to READY. Attribution to #1032 explains the failure; it does not satisfy the gate. Hold READY until clippy passes, or obtain an explicit change to the acceptance criterion.

- **[HIGH] Implementation sequence — required verification after each change is missing.** The project instructions require the entire verification matrix after every change. [Items 1–10](<review-tree>/.review/plan.md:46) prescribe only selected checks, with the complete matrix deferred to final HEAD. Add the required matrix at the applicable change boundaries and report failures; focused checks alone do not establish the mandated evidence.

- **[MEDIUM] Item 6 — mutation cleanup’s clean-status condition cannot pass as written.** [Item 6](<review-tree>/.review/plan.md:125) requires an empty `git status --porcelain` after each mutation, but the intended `deny.toml` change remains uncommitted until afterward. Capture the pre-mutation status and compare it after restoration, alongside the byte comparison of the mutated manifest.

The plan contains Documentation, Testing and Verification sections. This was a read-only review; I did not run tests or gates.

REVIEW COMPLETE

---

## Round 2

Michaël, I accept the refreshed altitude report’s **proceed** disposition. The explicit allowance resolves my clippy/READY finding; item 6’s before/after status comparison resolves the mutation-cleanup finding.

Altitude report SHA-256: e6ce3857655094cd39dd5d440c5cc05e078486cfef384e2d42264cb13f6f4560

- **[HIGH] Verification — the revised cadence still contradicts the project instruction.** [Lines 217–220](<review-tree>/.review/plan.md:217) defer the full matrix for items 1–4 and 8–10, despite `AGENTS.md` requiring it “after every change.” A later run verifies the combined result, leaving the intermediate commits without the required evidence. Run the matrix at each item boundary or obtain an explicit cadence exception.

- **[MEDIUM] Verification after item 5 has an impossible expected result.** The new cadence runs steps 1–2 after item 5, while [step 2](<review-tree>/.review/plan.md:231) expects every command except clippy to return zero. At that point, items 6–7 have not added the ignores, so `cargo deny check` must still fail on 0440 and 0462. Specify those expected intermediate failures separately from final acceptance.

- **[MEDIUM] Item 10’s green grep matches its prescribed replacement.** [Item 10](<review-tree>/.review/plan.md:182) searches for `` `async` feature `` and expects no matches, but its replacement for `api.md` explicitly says “There is no `async` feature”. Narrow the check to obsolete claims or explicitly allow that corrective sentence.

Read-only inspection only; no tests or gates were run.

REVIEW COMPLETE
---

## Round 3

Michaël, **LGTM on the round 3 delta**. I accept the altitude report’s **proceed** disposition.

Altitude report SHA-256: 2e1eb9df6fa30b7d9c31a1cffc934d60033dc2fd66c6a6e9138671cd20300a8e

The intermediate deny expectations and item 10’s grep are corrected. The cadence disposition is acknowledged; no new evidence warrants reopening it. No new findings.

Read-only inspection only; no tests or gates were run.

REVIEW COMPLETE
---

## Round 4

Michaël, **LGTM on the round 4 delta**. I accept the altitude report’s **proceed** disposition. The script reference preserves the publication sequence and single-issue requirement. No new findings.

Altitude report SHA-256: 7ad9572577d491fe9392eaab08d656c8bff66ae410ed610070d71aa47dd1c8ae

Read-only inspection only; no tests or gates were run.

REVIEW COMPLETE