Altitude report SHA-256: 2808691b58d1a90a1629368905d497e562f32eb06c1902861c2d3970d449cd85

### 1. Altitude
The re-issued altitude report (.review/altitude.md) is **acknowledged and the justified override is re-accepted**. The plan delta (+30 lines, +3.5 KB) strictly incorporates necessary refinements from round 2 (atomic transactions, invertible escaping, query candidate union, and re-filing replay invariants) without expanding into unnecessary prose.

---

### 2. Resolution of Round-2 Changes & Invariant Verification
The delta in `.review/plan-delta-r3.diff` successfully resolves all round-2 feedback and introduces no new defects:

1. **Decisions for Michaël at plan approval:**
   - **Decision 1 (Equal ADR-0020 keys):** The framing accurately cites the verbatim rationale from line 178 of `inputs/epic-spec.md` ("the same project on the laptop and the desktop must share memory"), transparently presents the tension with brainstorm decisions (non-git aliases confirmed by a human; machine-qualified locators), and outlines the exact consequences of Options A (strict subset, hints on reads/writes) and B (machine-qualified keys before release 1, clean slate with no migration).
   - **Decision 2 (ADR-0005 amendment):** Clearly framed as a documentation consistency step ensuring consolidation pass 2 and 3 summaries in the service are explicitly restricted to visibility meets under §10.
2. **Recall Candidate Union (§5 Step 1 & 2):**
   - Searching the union of query text as written and its tokenized form guarantees that literal-path memories (e.g. from release 1) and tokenized memories are both reliably matched by full-text search (C32).
   - Resolving ambiguous keys by preferring the caller machine's own binding eliminates spurious errors when a local binding exists.
3. **Locator Resolution & Transactional Operations (§8):**
   - Preserves ADR-0020's remote fallback chain: local-path or `file:` remotes are ignored rather than immediately treated as "no Locator", falling through to origin or dev-relative resolution (C22).
   - `merge` and `add` execute the locator redirect and memory re-filing within a single approved transaction, preventing intermediate states where memories become unreachable (C23).
4. **Portable Paths & Invertible Escaping (§9):**
   - `${PROJECT_ROOT}` is anchored strictly to the session's own checkout top-level, preventing paths in linked worktrees or sibling checkouts from being erroneously remapped (C27).
   - Escaping via `$`-run incrementation ($n \to n+1$) is mathematically invertible and cleanly preserves literal agent tokens (C30).
5. **Re-filing Fold Semantics (§12):**
   - Replay is formally defined as a fold in log order over initial placement and subsequent filing decisions (`set_level`, moves, approved re-filings), ensuring intentional actions like `set_level(id, user)` are never silently undone by replay (C35).
6. **Corpus Structures & Verification Rigor (§10, Verification):**
   - ADR-0016 diffusion kernels and curvature structures are keyed by visibility class, preventing cross-visibility leakage.
   - P1 token-shape scanning runs universally across all files, commit messages are vetted via a saved log file, and H1 checks required acceptance anchors with failure exits.

[Antigravity Review] LGTM

REVIEW COMPLETE

## Resolution

LGTM; no findings to action. The round-4 candidate (plan SHA-256 `6135c0c810e39fd720fbe4d45d686525920ab46cbff03f80f4c8223eecd37a33`) changes the plan for Codex's and the subagent's round-3 findings and goes back for a delta round.
