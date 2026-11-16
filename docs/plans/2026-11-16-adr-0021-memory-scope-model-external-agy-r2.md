Altitude report SHA-256: f7aabe4bd08a6e4b34de2eb16a17c2712e6278c1f076a3fed116dd629a5d39e7

### 1. Altitude
The re-issued altitude report (.review/altitude.md) is **acknowledged and the justified override is re-accepted**. The plan delta (+108 lines, +9.7 KB) directly reflects the resolution of round-1 reviewer findings through concrete normative rules and test cases rather than prose drift.

---

### 2. Resolution of Round-1 Findings
All findings raised in round 1 have been completely addressed in the revised plan:
1. **By-ID write authority leak (§5, §7):** Resolved. §5 explicitly restricts by-ID authority (`forget`, `set_level`) to the session's own candidate set `{context Home, user}` and expressly forbids mutation via the `project` swap, returning `not found` to match ADR-0020 §J. Verified in conformance case C17 and mapping row MC-8.
2. **Literal token distinction mechanism (§9, C30):** Resolved. Literal agent tokens in input content are specified to be stored escaped as `$${NAME}` in `facts.content` and recalled unescaped, allowing server-inserted tokens to expand without requiring schema migrations. Verified in case C30.
3. **Cross-project `${PROJECT_ROOT}` expansion hazard (§9, C31):** Resolved. §9 explicitly restricts `${PROJECT_ROOT}` expansion to when the memory's Home matches the session's own context Home, ensuring cross-project reads via `project: P` leave `${PROJECT_ROOT}` literal and avoid path corruption. Verified in case C31.
4. **Home meet for machine buckets (§10, C19):** Resolved. §10 grounds the meet in reader-set intersections: `user ∧ X = X` for any Home `X`, `X ∧ X = X`, and disjoint meets (producing no derived memory) for two distinct projects or buckets. Verified in case C19.
5. **Conformance gaps (§6, §8):** Resolved. Conformance coverage expanded from 29 to 35 cases, adding explicit coverage for `correct` locator rebinding/retirement (C24) and retryable failures when KB cannot reach ME's registry (C25).

---

### 3. Review of the Delta & New Invariants
- **Decision for Michaël at plan approval:** The framing of Options A (recommended: keep ADR-0020 key equality with receipts/recall hints) and B (machine-qualify remote-less keys in ADR-0020) is clear, balanced, and appropriately highlights user-facing consequences and future trade-offs.
- **Inert root writers (§3):** Distinguishes deliberate internal root writers (co-session edges and outcome signals) from legacy unfiled memories, excluding them from review lists and cutover warning counters.
- **Query tokenization & graph expansion (§5):** Tokenizing query text against the reader's roots enables lexical matching of absolute paths, and restricting graph expansion to the visible subgraph prevents transitive visibility leaks.
- **ADR-0005 amendment (§14, Implementation Task 4):** Properly links consolidation pass 2 and 3 overrides to ADR-0021's meet rules.
- **Verification mechanics:** D1, P1, and H1 now enforce proper nonzero exit statuses on error; P1 scans `/home` and `/Users` and guards `deny.ere`; H1 validates required acceptance anchors; and the full `AGENTS.md` verification matrix (including `cargo +nightly fuzz build`) is verified at the final pushed head.

No remaining blockers, leaks, contradictions, or unhandled edge cases were observed.

[Antigravity Review] LGTM

REVIEW COMPLETE

## Resolution

LGTM; no findings to action. The round-3 candidate (plan SHA-256 `dbaa23f25a3040aa8e228d5adc9d0858e0991d36adcbeeabe5c2b53e8f9ff612`) changes the plan for Codex's and the subagent's round-2 findings and is sent back for a delta round.
