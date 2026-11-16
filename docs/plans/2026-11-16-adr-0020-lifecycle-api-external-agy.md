# External review (Antigravity): ADR-0020 plan

One persistent agy conversation, seven rounds: round 1 in full, then the delta of each revision. Each
round's accepted review is reproduced as returned. The last round reviewed the final plan bytes.

## Round 1

Michaël, here is the review of the implementation plan for ADR-0020 (`.review/plan.md`).

Altitude report SHA-256: a4e9077c25a385fcf73be402fde3b055f439aacfe84aa324fa4b064e23ce0c2c

#### Altitude and Duplication Assessment

I have assessed the altitude and duplication measurements in `.review/altitude.md`. Because ADR-0020 defines the shared wire, scope, token, and tool contracts that seventeen downstream tickets across multiple repositories implement in parallel, fixing the concrete contract values in the plan before drafting the prose prevents deferring crucial contract decisions to PR review. Furthermore, the duplication between the plan and the eventual ADR is short-lived, as `/finish-pr` archives the plan into a tracking issue and removes the working copy upon merge. I explicitly acknowledge and accept the proposed override to proceed with the plan as a single artifact.

Structural completeness: The plan contains the required `Documentation`, `Testing` (with explicit N/A justification for code behavior), and `Verification` (V1–V7 and merge gate) sections.

---

[Antigravity Review] [HIGH] Section I (Project key and minimal scope contract) and Decision 3: The 256-byte label cap permits scope path segments that overflow the codebase's MAX_SEGMENT_LEN. Section I states: "Project: user:<user>/project:<label> ... A label over 256 bytes gives no key." However, in `crates/me-types/src/types/scope.rs` (where `MAX_SEGMENT_LEN = 256` and `validate_segment` is defined) and `crates/me-backend-sqlite/src/store/scopes.rs` (`ScopeStore::ensure_path`), the 256-byte limit is enforced on each individual slash-separated segment (`project:<label>`), not on `<label>` alone. Because the `"project:"` prefix is 8 bytes, any label between 249 and 256 bytes produces a segment of 257 to 264 bytes, causing `ensure_path` and `ScopeTree::resolve_path` to reject the scope with `ConflictError::ScopeLabel`. The label cap must be 248 bytes (256 - 8), ensuring the total segment `project:<label>` does not exceed `MAX_SEGMENT_LEN`.

[Antigravity Review] [HIGH] Section M (Claude Code client) contradicts Section L (Budgets and SessionStart output): Section M states: "Print the body only on a 200; otherwise a fixed JSON template in which only integers are substituted." However, Section L explicitly defines dynamic string status values that the client itself must generate when offline or when the server errors: "Client-made: memory: unreachable, memory: token rejected, memory: no token file, memory: error <status>. Suffix ; outbox <q> queued, <p> parked when either is non-zero." Section L also requires repeating the status line on the first line of the context block. If the client fallback template only substitutes integers, it cannot produce these string status lines in `systemMessage` or `additionalContext`. The ADR must reconcile this by specifying a shell fallback template that formats the client-generated status strings without JSON breakage.

[Antigravity Review] [MEDIUM] Section B (Envelope) and Section I: Percent-encoding non-ASCII bytes in project facts exceeds the POSIX sh client utility set. Section B requires: "A client must encode every byte outside printable ASCII (0x21 to 0x7E) and each of %, \" and \, as %HH", yet Decision 20 restricts the client utility set strictly to "POSIX sh, mkdir, mv, rm, cat, date, git and curl. It never parses JSON." While ASCII characters can be replaced using pure POSIX sh pattern loops, portable POSIX sh has no built-in mechanism to extract raw byte values from multi-byte UTF-8 sequences (e.g. from non-ASCII paths in `CLAUDE_PROJECT_DIR` or git branch names) without external tools (such as `od`, `xxd`, `awk`, or `sed`). Decision 8 restricted dev root to ASCII for an MCP-channel limitation; the ADR must similarly clarify whether the shell utility set is granted an encoding tool (like `od`/`awk`) or if non-ASCII path encoding is handled under a specified helper convention.

[Antigravity Review] [MEDIUM] Section M (Claude Code client) and Section D (Identity, order and delivery): Unpadded sequence numbers in event IDs cause client outbox flush reordering. Section D requires the client to flush "Oldest first by (time, seq), one request at a time... skipping ahead on a retry would reorder events and make late ones." Section M allocates sequence numbers sequentially from `n = 0` via `mkdir <outbox>/seq/<epoch>.<n>`, giving event IDs `<epoch>-<seq>-<pid>`. In POSIX sh, iterating over outbox files via globbing (`for f in outbox/queued/*`) sorts lexicographically: unpadded numbers cause `<epoch>-10-...` to sort and flush before `<epoch>-2-...`. Under parallel tool batches where >10 events fire in the same second, this reorders the flush sequence. The ADR should specify that `<seq>` is zero-padded (e.g. `%06d` or `%09d`) in outbox filenames to keep shell glob order identical to numerical sequence order.

[Antigravity Review] [LOW] Verification step V7 (every hook placed) relies on outbound network access. V7 runs `curl -sL https://code.claude.com/docs/en/hooks.md` and compares against §11. Repository verification and CI frequently run in sandboxed or offline environments where network requests fail. V7 should run against the local review reference (`.review/hooks-reference.md`) or a checked-in fixture list of the 33 lifecycle hook events, treating the live curl fetch as an optional online-only audit.

[Antigravity Review] [LOW] Section F (Session segments): Phrasing regarding subagent events opening segments is potentially ambiguous. Section F states: "Subagent events never open or end a segment by themselves; the 'after the end' rule still applies to them." Stating in one sentence that they "never open... a segment by themselves" while immediately adding that "the 'after the end' rule still applies to them" can be read as contradictory. The ADR should explicitly clarify: `SubagentStart` and `SubagentStop` do not demarcate segment boundaries the way `SessionStart` and `SessionEnd` do, but any subagent event arriving after an ended or idle segment opens a new segment under the general "any event after the end" rule.

REVIEW COMPLETE


## Round 2

Michaël, here is the Round 2 review of the revised implementation plan for ADR-0020 (`.review/plan.md`).

Altitude report SHA-256: d4030770dbc0bf81b38dc116a9fb07c68647fdb5d74480a20c585afb94aac460

#### Altitude and Duplication Assessment

I have reviewed the refreshed altitude report in `.review/altitude.md`. The plan grew slightly (from 41 KB to ~52 KB) due to necessary precision added across contract definitions: the 4-rule session segment sequencing, the formal status-line grammar, explicit client outbox locking, and exact rejection test vectors. This level of detail remains justified because ADR-0020 serves as the authoritative specification across seventeen downstream tickets, two public repositories, and the shell client. Given that the plan's duplication is temporary and will be archived upon merge by `/finish-pr`, I explicitly acknowledge and reaffirm acceptance of the proposed override to proceed with the plan as a single artifact.

#### Round 1 Delta Review

All Round 1 findings have been resolved cleanly and soundly:
- **Scope segment limit (§I, Decision 3):** Resolved. The length cap is now correctly checked against the entire `project:<label>` segment exceeding 256 bytes (`MAX_SEGMENT_LEN`), preventing any segment length overflow in `ScopeStore::ensure_path` and `validate_segment`.
- **Client fallback status output (§L, §M):** Resolved. A formal status-line grammar is defined in §L, and the client fallback specifies four fixed literal templates substituting only integers (HTTP status code, queued count, parked count) into `systemMessage` with no `additionalContext`, guaranteeing string-safety in POSIX shell without JSON escaping.
- **Client percent-encoding (§B, §M):** The "not changed" adjudication is sound. Processing single bytes under `LC_ALL=C` with parameter expansion and the `printf` builtin was verified across `dash`, `bash`, and `busybox sh`, and §B adds that unparseable facts are dropped and flagged in provenance rather than aborting or parking the event.
- **Outbox sequence ordering (§B, §D, §M):** Resolved. `ME-Event-Seq` is zero-padded to 6 digits (`<epoch>-<seq as 6 digits>-<pid>`), guaranteeing that lexical filesystem globbing in POSIX shell matches numerical sequence order.
- **Verification step V7:** Resolved. V7 now compares against the local saved hooks reference (`$HOOKS_REF`) rather than making an external `curl` network request, ensuring reliability in sandboxed or offline environments.
- **Subagent segment boundary phrasing (§F):** Resolved. §F was restructured into four ordered rules (lateness check, non-late event classification, idle sweep, derived work), clearly stating that subagents and `compact` session starts are not boundary events by kind and follow general event rules.
- **All other Round 1 fixes** (credential stripping from remotes in §B/§I, bounded 422 parking vs 5xx in §C, `mkdir` flusher lock in §D, tool call failures as tool results in §J, advisory maintenance lock in §J, bounded capture loop in §M, and merge-base warning diff in V1) are well-specified and introduce no contradictions or undefined states.

[Antigravity Review] LGTM The delta resolves all Round 1 findings across reviewers, grounds every shell and server contract in codebase and hooks-reference behavior, introduces no new contradictions or undefined edge cases, and provides a tight, robust specification for ADR-0020.

REVIEW COMPLETE


## Round 3

Michaël,

Here is my Round 3 review of the revised implementation plan for ADR-0020 (`docs/plans/2026-11-16-adr-0020-lifecycle-api.md`).

Altitude report SHA-256: 1879fb1a2de2fce865e2b8c32f799ca67c6492001ae9d5d9f62def157159bcc2

#### Altitude & Scope Override
I explicitly acknowledge and support the proposed altitude override to proceed with the plan as a single artifact. The plan's concentration of exact contract values, route paths, HTTP statuses, and tool mappings across sections A through N is justified: these decisions unblock seventeen downstream tickets across two repositories and the shell client. Splitting the design or prematurely writing the ADR would duplicate effort or shift design review to PR review. The temporary duplication between the plan and the eventual ADR is short-lived, as `/finish-pr` archives the plan upon merge.

#### Round 2 Delta Verification
The revisions in `.review/plan-delta-r3.diff` successfully resolve all Round 2 findings without introducing new contradictions or undefined edge cases:
- **Flusher Stranding & Concurrency (§D):** The revised flusher algorithm loops until a pass sends zero events, drops the lock, and rescans. Hooks publish to the pending directory before acquiring the lock, eliminating stranded-event windows. Best-effort PID liveness checks (`kill -0`) prevent lock abandonment. Furthermore, the plan explicitly establishes that server correctness never depends on client delivery order.
- **HTTP Status Exhaustiveness (§C, Decision 2):** The total status table comprehensively maps all responses: 204 deletes; 400, 409, 413, and 422 park; 408, 429, 5xx, and transport errors retry; unlisted 4xx, 3xx, and non-204 2xx hold the queue.
- **Late Stop Uptake & Segment State (§F, §K):** Uptake recording for late stops upon segment application is explicitly documented; segment checkpoints remain immutable.
- **URI Sanitization (§B):** The updated regex (`[^@/:]+@`) strips all userinfo up to the final `@` preceding the host for both URI schemes and SCP-like git syntax.
- **Maintenance Lock (§J):** Expanded across all CLI commands that open the engine, ensuring SQLite migrations never race against concurrent access.
- **Credential Safety (§M):** Passing tokens via standard input (`curl -H @-`) eliminates process table leakage.
- **Adjudicated "Not Changed" Items:** The decision to verify Claude Code's helper environment variable behavior during #1028 implementation rather than in this documentation planning phase is sound, as the plan specifies behavior for both outcomes.

[Antigravity Review] LGTM The Round 2 revisions completely resolve all raised issues, the wire contract order-independence guarantee is sound, and no regressions or undefined behaviors are present.
REVIEW COMPLETE


## Round 4

Michaël,

Here is my Round 4 review of the revised implementation plan for ADR-0020 (`docs/plans/2026-11-16-adr-0020-lifecycle-api.md`).

Altitude report SHA-256: 33babd47fa84b0768a0f59f8d27d3e7599b0fbf634ba7354e4ef32bd2513a4b0

#### Altitude & Scope Override
I explicitly acknowledge and endorse the proposed altitude override to proceed with the plan as a single artifact. The concrete values, routes, tables, and tool boundaries detailed across sections A through N are essential for aligning the 17 downstream tickets across repositories before drafting prose. The duplication between the plan and the eventual ADR is temporary and self-clearing, as `/finish-pr` archives the plan upon merge.

#### Round 3 Delta Verification
The revisions in `.review/plan-delta-r4.diff` successfully resolve all Round 3 findings without introducing new contradictions or undefined edge cases:

- **Project Path Delivery on MCP Channel (§B, §N, #1028):** Passing the project directory as data on standard input via `exec <helper> <<'DELIMITER'` cleanly circumvents shell argument escaping issues (`'`, non-ASCII, spaces) without relying on undocumented environment variables. Because `exec` replaces the shell process before any subsequent lines can be evaluated, path data containing the delimiter cannot achieve arbitrary command execution.
- **Flusher Second-Look Loop Decoupling (§D):** Re-scanning after lock release is now strictly confined to flushes that terminate because the queue is empty. Encounters with retry-class (408/429/5xx) or hold-class (unexpected statuses) immediately stop the flush without a second look, preventing tight CPU/network retry loops while preserving the no-lost-wakeup guarantee.
- **Watcher Specification & Flusher Stale Lock Handling (§D, §M):** The watcher is formally defined as the per-machine system timer (`dutiona/my-dotfiles#1911`). The outbox lock mechanism is fortified by pairing `kill -0` PID checks with a 1-hour timestamp expiry renewed before every request, and `kill` is explicitly added to the allowed utility set.
- **Late Uptake Determinism (§K, #1029):** The association rule is now unambiguous: an uptake row names the injection whose `SessionStart` possesses the greatest order key strictly below the stop's order key among injections held by the server at write time, is never rewritten, and names none if none are held.
- **Maintenance Lock Boundary (§J, #1020):** Correctly scoped to commands opening the engine writable, matching the true implementation in `crates/memory-engine-cli/src/db.rs` where `open_engine` is migration-free for read-only access.
- **Downstream Ticket Alignment:** Downstream summaries for #1019, #1020, #1028, and #1029 accurately reflect these contract refinements.

[Antigravity Review] LGTM All Round 3 findings have been thoroughly and cleanly resolved, with empirical validation for the MCP path delivery, sound concurrency bounds on the outbox flusher, and no introduced regressions or undefined behaviors.
REVIEW COMPLETE


## Round 5

Michaël,

Here is my Round 5 review of the revised implementation plan for ADR-0020 (`docs/plans/2026-11-16-adr-0020-lifecycle-api.md`).

Altitude report SHA-256: bdb76b6006b37041b0a582992f85001138b3ae9d66d26e6cc7f019100c5f2650

#### Altitude & Scope Override
I explicitly acknowledge and support the proposed altitude override to proceed with the plan as a single artifact. Concrete contract definitions across sections A through N remain necessary to coordinate the 17 downstream tickets without premature prose drafting or artificial design splits. The plan's lifecycle ends at `/finish-pr`, which archives it upon merge.

#### Round 4 Delta Verification
The revisions in `.review/plan-delta-r5.diff` cleanly resolve all Round 4 findings without introducing new contradictions or undefined edge cases:

- **Residual Project Path Truncation & Decision 10 (§B, Decision 10, #1028):**
  - **Exactness:** The residual risk is stated precisely: a directory name containing a line break followed by the delimiter line can truncate the path seen by the helper, causing MCP calls to be filed under the shortened prefix. The helper guards against arbitrary line breaks by requiring exactly one line that resolves to an existing directory; otherwise, it sends no fact.
  - **Option Framing:** Both choices are characterized accurately:
    - *Option A (Recommended):* Accept the residual. Exploiting it requires filesystem write access inside the parent dev root (which already possesses authority over the workspace), while the hook channel rejects paths with line breaks.
    - *Option B (Server Cross-Check):* Have the helper forward `ME-Session-Id` and enforce server-side validation against the `SessionStart` project root. The plan correctly details its trade-offs: it adds header and validation overhead to #1025 and #1020, and depends on `CLAUDE_CODE_SESSION_ID` being exposed to the helper environment (undocumented by Anthropic).
- **Injection Order Independence (§K):** Clarified that uptake linking does not depend on outbox upload order. Context injection is performed synchronously by the `SessionStart` hook upon session initialization, guaranteeing that the injection record exists in the store before subsequent turns or stops are generated.
- **Unreadable Lock Recovery (§D):** If the outbox lock PID or timestamp cannot be read (e.g. truncated or corrupted lock file), the lock is treated as broken, preventing unrecoverable flusher stalls.
- **Stall Limits vs. Lock Renewal (§M):** The `--speed-limit` threshold is explicitly tuned to guarantee that a maximum-sized 16 MiB payload completes within the 1-hour lock renewal budget.
- **Downstream Ticket Consistency:** Downstream alignment for #1028 accurately records Decision 10's residual.

[Antigravity Review] LGTM All Round 4 findings are cleanly resolved, Decision 10 is precisely stated with sound option trade-offs, and no regressions or undefined behaviors exist in the wire contract.
REVIEW COMPLETE


## Round 6

Michaël,

Here is my Round 6 review of the revised implementation plan for ADR-0020 (`docs/plans/2026-11-16-adr-0020-lifecycle-api.md`).

Altitude report SHA-256: 802b2a2b5e5c126cfdd427c50a8940486a3629b8ba6531736819476aa7ccc392

#### Altitude & Scope Override
I explicitly acknowledge and support the proposed altitude override to proceed with the plan as a single artifact. The precise values, grammar definitions, status classifications, and tool mappings across sections A through N are necessary to align the 17 downstream tickets without prematurely writing ADR prose or introducing artificial design splits. The plan will be archived as an issue by `/finish-pr` upon merge.

#### Round 5 Delta Verification
The three revisions in `.review/plan-delta-r6.diff` cleanly resolve all Round 5 findings without introducing contradictions or undefined edge cases:

- **Decision 10 & §B Consistency:**
  - The plan harmonizes the line-break path rule across Decision 10 and §B: a project directory containing a line break yields no project fact on either channel (rather than an ambiguous rejection).
  - Decision 10's alternative (Option B) now explicitly states its fail-closed behavior: the server refuses a model-tool call if the project root differs from the session's recorded `SessionStart` root *or if no `SessionStart` root is held*. It also explicitly flags that the equivalence of `CLAUDE_CODE_SESSION_ID` with the hook input's session ID remains an empirical check for #1028. Both options are stated accurately and comprehensively.
- **Uptake Injection Timing Nuance (§K):**
  - Section K narrows the claim regarding injection precedence to acknowledge client timeouts: if a context read exceeds the client's timeout (2.5 s), the session proceeds without receiving the block, while the server may still complete the write. Because the injection logs its server-side duration, downstream uptake evaluation pipelines can unambiguously detect and filter out these timed-out reads.
- **Contract & Downstream Stability:**
  - The changes preserve wire contract order-independence and introduce no new contradictions or regressions.

[Antigravity Review] LGTM The Round 5 refinements cleanly resolve the timeout nuance in uptake linking, fully harmonize the line-break handling across §B and Decision 10, and leave the plan in a sound, complete, and review-approved state.
REVIEW COMPLETE


## Round 7

Michaël,

Here is my Round 7 review of the revised implementation plan for ADR-0020 (`docs/plans/2026-11-16-adr-0020-lifecycle-api.md`).

Altitude report SHA-256: 15c7398b1fd5e16ea69b684d85f794e9e7917d1377a3abf7748836189c8d1fd1

#### Altitude & Scope Override
I explicitly acknowledge and support the proposed altitude override to proceed with the plan as a single artifact. Fixing contract values, status mappings, and route boundaries across sections A through N is essential to coordinate the 17 downstream tickets in parallel without prematurely expanding ADR prose or creating an artificial design split. The plan will be archived as an issue by `/finish-pr` upon merge.

#### Round 6 Delta Verification
The two sentence updates in `.review/plan-delta-r7.diff` cleanly resolve the Round 6 findings without introducing new contradictions or undefined edge cases:

- **Reconciliation of §B and Decision 10:**
  Section B's rule on line breaks now explicitly references the forged-delimiter scenario: *"the one exception is the forged delimiter of decision 10, which hides the line break from the MCP helper."* This eliminates the contradiction between §B's general rejection of paths with line breaks and Decision 10's residual truncation behavior.
  - **Decision 10 Framing:** Remains stated with precision. Both the recommended acceptance (Option A) and the server-side cross-check alternative (Option B, with its fail-closed case and undocumented environment variable dependency) are accurately framed for the owner's disposition.
- **Uptake Injection Semantics (§K, #1029):**
  Section K removes the incorrect assumption that server-recorded execution duration can reliably prove client receipt across timeouts and slow links. The contract for #1029 is now cleanly defined: an injection record reflects what was *served* by the server, not *seen* by the model.
- **Contract & Downstream Integrity:**
  The server-side wire contract maintains full order-independence, and no regressions or undefined states have been introduced.

[Antigravity Review] LGTM Both Round 6 findings are cleanly resolved, §B and Decision 10 are completely reconciled, the uptake semantic contract ("served, not seen") is sound, and the plan is ready for owner approval.
REVIEW COMPLETE

