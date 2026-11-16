Altitude report SHA-256: b415d2fdcee00b728ac18348e24476a5d3bf1be0f404cab64cb7d2c41632d34c

### 1. Altitude
The re-issued altitude report (.review/altitude.md) is **acknowledged and the justified override is re-accepted**. The minor growth (+11 lines, +1.6 KB) in round 4 consists purely of targeted rule and test case refinements resulting from round-3 review findings.

---

### 2. Resolution of Round-3 Changes & Delta Review
The changes in `.review/plan-delta-r4.diff` are verified and introduce no regressions:

1. **Decisions for Michaël at plan approval:**
   - Decision 1 now incorporates the scoping context from epic spec decision 19 ("for one project known under two keys, such as a non-git folder nested differently on two machines"), giving Michaël the complete design context behind the trade-offs of Options A and B. The framing remains impartial, objective, and focused on consequences.
2. **SessionStart Status Line Grammar Integrity (§7):**
   - The unconfirmed "same key" hint is placed as an advisory line inside `additionalContext` rather than modifying the closed status-line BNF grammar from ADR-0020 §L, preserving parser backwards compatibility.
3. **Query Embedding Efficiency (§5 Step 1):**
   - In recall step 1, embedding the tokenized query is conditionally bypassed if tokenization produced no changes to the query text, conserving latency within ADR-0020's 500 ms embed budget.
4. **Transactional Safety for Merge (§8, C23):**
   - Quoting ADR-0020's maintenance ruling (offline CLI execution) and binding the merge execution to the preview's exact log position prevents race conditions: any modification to the absorbed project between preview and apply safely refuses the transaction until re-previewed.
5. **Fail-Closed Checkout Path Semantics (§9, C27):**
   - `${PROJECT_ROOT}` strictly requires the client's checkout fact; without it, paths fall back to `${DEV_ROOT}` without creating incomplete tokens. Tokens are also forbidden directly following a `$` prefix.
6. **Re-filing Replay Re-resolution (§12, C35):**
   - `set_level(project)` properly resolves its target project from its own call record context during replay, and earlier re-filings are re-derived rather than replayed, ensuring subsequent `correct` operations take precedence without being shadowed by stale re-filing decisions.
7. **Verification & Privacy Scans (Verification):**
   - P1 now matches Carrier-Grade NAT (RFC 6598) / Tailscale IP ranges (`100.64.0.0/10`) across all files and scopes the scanning of artifacts (`pr-body.md`, `kb-513.md`) immediately prior to their respective publication points.

The plan is complete, sound, and fully reconciled with all upstream and sibling constraints.

[Antigravity Review] LGTM

REVIEW COMPLETE

## Resolution

LGTM; no findings to action. The round-5 candidate (plan SHA-256 `6334123a9cc4e7874d701834ed6d853629c8698edecb906913626aea57b24db3`) changes the plan for Codex's and the subagent's round-4 findings.
