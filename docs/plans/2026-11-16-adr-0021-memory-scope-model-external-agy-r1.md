Altitude report SHA-256: 646ddf1733b961652fb9f87e2bb45de4fcf4960bce2d678c865084aabc88cdcf

### 1. Altitude
The justified override specified in `.review/altitude.md` is **acknowledged and accepted**. The plan for ADR-0021 is appropriately structured as a dense design outline containing normative rule bullets, conformance cases, and mapping contracts prior to drafting full narrative ADR prose. This matches the dispatch brief assignment and bounds duplication since the plan is archived to the issue upon PR completion.

---

### 2. Correctness of the Model
The core model is well-structured: fail-closed defaults are extensive, root scope (`DEFAULT 1`) rows are explicitly isolated and unread by recall and by-ID operations, empty selectors fail closed to empty results, and the filter-first requirement prevents leakage through vector overfetch or external rerankers. However, there is a write-authority ambiguity regarding by-ID operations:

[Antigravity Review] [HIGH] §5 and §7 By-ID write authority leak: §5 states that by-ID operations (forget, set_level) act on memories the caller can recall, which an implementer could read to permit mutating or retracting memories of another project returned via recall(project: "other"); the ADR must explicitly state that by-ID operations evaluate visibility strictly against the session's own Recall context (the session's project/bucket and user level) and return not found for facts belonging to other projects, matching ADR-0020 §J.

[Antigravity Review] [LOW] §10 Derived memory Home meet for machine buckets: §10 defines P ∧ P = P and user ∧ X = X, but does not explicitly state whether two memories in the same machine bucket meet (B ∧ B = B) or whether buckets are excluded from consolidation, leaving an implementation ambiguity for derived pipelines.

---

### 3. Strict-Subset Proof (§11)
The strict-subset mapping (MC-1 through MC-8) against ADR-0020's planned rules is sound:
- First-release scope nodes (`user:<user>/project:<label>`, `user:<user>`, `unscoped:<machine>`) remain unchanged and are adopted directly without relabeling or database `UPDATE`s.
- First-release facts implicitly have default unrestricted audiences, which the schema represents additively by absence of audience rows.
- Recall under ADR-0021 with unrestricted audiences produces identical result sets to ADR-0020's project + user scope queries.
- Re-filing from the ADR-0001 event log operates via append-only filing decisions with a pre-flight visibility diff, preserving the audit trail without rewriting history.

---

### 4. Portable Paths (§9) and Locators (§8)
The locator resolution rules and the three portable path tokens (`${PROJECT_ROOT}`, `${DEV_ROOT}`, `${HOME}`) properly solve machine-path divergence. However, two edge cases in token handling require clarification:

[Antigravity Review] [MEDIUM] §9 and C24 Portable path literal distinction mechanism: §9 requires that a literal ${HOME} written by an agent is recalled as written while server-tokenized paths expand, but specifies no representation (such as escaping as \${HOME} or storing token-offset metadata) for distinguishing literal tokens from replaced tokens in facts.content without schema modifications.

[Antigravity Review] [MEDIUM] §9 and C25 Cross-project PROJECT_ROOT expansion hazard: §9 specifies that ${PROJECT_ROOT} expands only when the Home is the reader's project, but leaves open how recall(project: "target") behaves when the caller's session is in a different project; the ADR must explicitly specify that ${PROJECT_ROOT} remains literal because the caller's request header only provides its own checkout root, preventing path corruption.

---

### 5. Coverage of Acceptance, Open Questions, and Conformance Cases
The plan covers all requirements from issue #1015, settles all three open questions from the brainstorm brief, and defines 29 conformance cases (C1–C29). Two gaps in conformance coverage were identified:

[Antigravity Review] [LOW] Conformance case coverage for §6 registry error and §8 correct: the 29 conformance cases omit test cases for the retryable error when knowledge-base cannot reach the registry (§6 row 12, §13) and for the correct maintenance operation rebinding/retiring a Locator (§8).

---

### 6. Plan Mechanics
- **Documentation, Testing, Verification:** All sections are present, clearly articulated, and proportional for a docs-only PR.
- **Verification Commands:** The D1 (Sphinx build with warning baseline comparison), P1 (public repo privacy scan), and H1 (heading completeness) checks are deterministic, unpiped, and verify red-to-green transitions.
- **Hygiene:** Public repository rules are respected; synthetic identifiers (`alice`, `laptop`, `desktop`, `github.com/example/`) are consistently used.

REVIEW COMPLETE
