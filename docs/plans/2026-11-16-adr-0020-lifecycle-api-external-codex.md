# External review (Codex, headless): ADR-0020 plan

One persistent Codex thread, read-only sandbox, seven rounds: round 1 in full, then the delta of each
revision. Each round's final message is reproduced as returned; absolute paths into the review tree are
made relative. The last round reviewed the final plan bytes.

## Round 1

Michaël, I would **not approve the contract yet**. Confidence is high in the findings below.

I acknowledge the altitude report’s proposed override: keeping the contract values in this plan is justified for a review before parallel implementation. The temporary duplication is proportionate to that purpose.

Altitude report SHA-256: a4e9077c25a385fcf73be402fde3b055f439aacfe84aa324fa4b064e23ce0c2c

- **[HIGH] §D, §M — Concurrent flushes can violate delivery order.** Every capture hook starts a detached flush, but the plan specifies no exclusive flush ownership. Two hooks can upload successive events concurrently; a later request can commit before an earlier retry or hold response, defeating the stated stop-on-failure rule and changing segment assignment. Define a crash-safe single-flusher protocol and recovery behavior. [Plan](.review/plan.md:121)

- **[HIGH] §F — Late-event and segment-opening rules conflict.** An event before the latest segment’s start is declared late, even if its timestamp is after an earlier segment’s end; the ticket says *any* event timestamped after the end opens a new segment. A delayed `SessionStart` with `source: resume` is also both an unconditional opener and potentially late. Specify precedence and assignment across multiple segments, including delayed delivery and same-second timestamps, so implementations produce the same checkpoint and uptake. [Plan](.review/plan.md:135)

- **[HIGH] §D, §M — Sequence allocation does not establish the promised creation order.** Concurrent hook processes can obtain timestamps in one order and win `mkdir` in the opposite order. A process can also reserve a lower sequence and stall before enqueueing while a later event is flushed. The server may consequently finalize a segment before its earlier event arrives. Define when an event becomes ordered and how publication and flushing account for reservations that are incomplete or abandoned. [Plan](.review/plan.md:220)

- **[HIGH] §B, §I — Remote credentials can be persisted as raw project facts.** `git remote get-url` may contain URL userinfo; §I strips it for the *key* but also says every event retains decoded raw facts. Thus a credential-bearing remote can enter durable event metadata even though the existing redactor recognizes URL credentials. Require stripping or redacting userinfo before any persistence, including provenance and logs. [Plan](.review/plan.md:179)

- **[HIGH] §I — The project label limit omits the `project:` prefix.** The plan permits an encoded key of 256 bytes, but the actual scope segment is `project:<label>` and the store validates the whole segment against 256 bytes. Keys of 249–256 bytes therefore pass the proposed contract and fail storage instead of falling back to the unscoped bucket. Apply the limit to the complete segment. [Plan](.review/plan.md:181)

- **[MEDIUM] §G, §M — The SessionEnd timeout claim contradicts the supplied hooks reference.** The plan says a plugin cannot raise the 1.5-second budget; the reference says a longer configured per-hook timeout raises it, up to 60 seconds. Correct the rationale and retain the short capture path if desired. [Plan](.review/plan.md:223)

- **[MEDIUM] Verification — The merge gate silently weakens the project’s explicit matrix.** It says to run every Verify command but skips the fuzz build and MSRV; the supplied project instructions require the fuzz gate after every change and explain that it protects the detached facade consumer. Run the required fuzz gate or report it as incomplete if unavailable; distinguish the conditional MSRV requirement. [Plan](.review/plan.md:351)

This was a read-only review; no project tests or gates were run.

REVIEW COMPLETE

## Round 2

Michaël, the revised plan still has delivery and segment cases to resolve before approval. Confidence is high.

I acknowledge the refreshed altitude report’s proposed override. Despite the growth, retaining the values in one plan remains justified for this pre-implementation contract review.

Altitude report SHA-256: d4030770dbc0bf81b38dc116a9fb07c68647fdb5d74480a20c585afb94aac460

- **[HIGH] §D, §M — A published event can be stranded after the last flush.** A new hook can publish an event while the existing flusher is finishing its queue scan. Its own flush exits because the lock is held; the existing flusher can then release the lock without seeing the new event. If no later hook fires, the event remains queued indefinitely. Specify an atomic handoff or recheck protocol that guarantees another flush. [Plan §D](.review/plan.md:141)

- **[HIGH] §D — The stale-lock rule is not yet an implementable contract.** A fixed age alone cannot distinguish a killed flusher from a live one: §M permits a slow upload with no total time limit. The plan requires breaking the former but never the latter, without defining liveness, ownership or a race-safe takeover. Competing stale-lock breakers can also remove a newly acquired lock. Specify those cases before relying on the lock for ordering. [Plan §D](.review/plan.md:141)

- **[HIGH] §F — The revised late-event rule loses derived work.** If a Stop arrives after its segment has ended, its episodic memory is created, but it changes no segment and never re-runs derived work. The checkpoint consequently omits that Stop and §K creates no uptake row for it. This contradicts §F’s claim that a wrong split loses nothing and the requirement for derived work per segment. Define how late events are assigned and how their checkpoint and uptake effects are reconciled. [Plan §F](.review/plan.md:158)

- **[MEDIUM] §B — The new userinfo rule does not cover all accepted remote forms.** It removes `user:password@` from `scheme://` URLs, while §I also accepts SCP-style `user@host:path`. The latter’s userinfo can therefore leave the machine despite the stated guarantee. Specify sanitization for every accepted remote form on both channels before transmission. [Plan §B](.review/plan.md:106)

The revised scope-length limit and required fuzz gate resolve my corresponding round-1 findings. I also withdraw my SessionEnd timeout finding: the reference explicitly says plugin-provided hook timeouts do not raise that budget.

REVIEW COMPLETE

## Round 3

Michaël, the round 3 delta still needs changes before approval. Confidence is high.

I acknowledge the altitude report’s proposed override: reviewing the contract values in one plan remains justified.

Altitude report SHA-256: 1879fb1a2de2fce865e2b8c32f799ca67c6492001ae9d5d9f62def157159bcc2

- **[HIGH] §B — The MCP fallback can execute a project path as shell code.** The revision correctly recognizes that a single quote breaks the quoted substitution, but calling such a path unsupported does not prevent the shell from parsing it before the helper can reject it. A checkout path containing a quote and shell syntax can execute commands when the helper starts. Use a transport that passes the path as data, or fail safely before constructing the shell command. The deferred observation in #1028 does not make the fallback safe if the environment variable is absent. [Plan §B](.review/plan.md:108)

- **[HIGH] §D — The post-unlock rescan conflicts with retry and hold.** On a 401, 429 or connection error, the event remains queued and the flush must stop. The new rule then sees that event after releasing the lock and starts again, potentially creating an immediate, unbounded retry loop; alternatively, an implementation that stops has no specified timer schedule or watcher lifecycle to guarantee retry. Distinguish newly published work from a stopped head and define the backstop’s ownership and retry behavior. [Plan §D](.review/plan.md:142)

- **[MEDIUM] §F, §K — Late uptake is added, but its segment and injection association remain undefined.** A late Stop can precede the latest segment’s start or fall between earlier segments. §F says it changes no segment, while §K requires the session’s latest injection *before it* without defining the time/order basis or the case where that injection arrives later. Implementations can produce different uptake links, or no correct link. Specify the association and reconciliation for those cases. [Plan §F](.review/plan.md:160)

The revised remote sanitization addresses the SCP-style userinfo case I raised in round 2.

REVIEW COMPLETE

## Round 4

Michaël, the round 4 delta resolves the immediate retry-loop and shell-command execution findings, but introduces two contract issues. Confidence is high.

I acknowledge the altitude report’s proposed override: keeping the contract values in one reviewable plan remains justified.

Altitude report SHA-256: 33babd47fa84b0768a0f59f8d27d3e7599b0fbf634ba7354e4ef32bd2513a4b0

- **[HIGH] §B — A delimiter in the project path can silently select the wrong project.** The here-document can truncate a path at a delimiter line. The helper may then receive a valid prefix and derive a different project from it, causing MCP tools to read or write the wrong project’s memories while the hook channel uses the actual path. The server’s control-character check cannot detect characters that never reach it. Require an unambiguous framing or a safe failure when the complete path cannot be conveyed. [Plan §B](.review/plan.md:108)

- **[MEDIUM] §D, §K — Uptake now depends on arrival order despite the stated guarantee.** If a Stop is applied before its earlier SessionStart context read finishes, its uptake row names no injection and is never rewritten; if the context read finishes first, the same Stop links to it. Define reconciliation, or explicitly accept this loss of association and narrow §D’s claim that correctness never depends on delivery order. [Plan §K](.review/plan.md:237)

REVIEW COMPLETE

## Round 5

Michaël, the round 5 delta still leaves the uptake association dependent on timing. Confidence is high.

I acknowledge the altitude report’s proposed override: the contract values remain appropriate to review in one plan.

Altitude report SHA-256: bdb76b6006b37041b0a582992f85001138b3ae9d66d26e6cc7f019100c5f2650

- **[MEDIUM] §K, §L — A synchronous context request does not guarantee an injection exists before the next event.** The hook proceeds after its 2.5-second client timeout or a connection error. The server may commit the context read later, after a Stop has been applied and its uptake row written without an injection; that row is never rewritten. Clarify how late context completion is reconciled, or explicitly accept the missing association and narrow the arrival-order claim. [Plan §K](.review/plan.md:238)

- **[MEDIUM] Decision 10 — The proposed alternative needs fail-closed cases to support its claim.** The SessionStart event can still be queued or its project-root fact dropped because it contains a control character. In either case there may be no server-side root to compare when a model-tool call arrives. Specify that a missing session or root refuses the call; otherwise the stated check does not close the residual in those cases. [Decision 10](.review/plan.md:72)

The recommended residual itself is explicitly described for Michaël’s approval.

REVIEW COMPLETE

## Round 6

Michaël, the round 6 delta resolves the missing-root case in decision 10’s alternative, but introduces a contradiction and leaves the uptake evidence claim too strong. Confidence is high.

I acknowledge the altitude report’s proposed override: the contract values remain appropriate to review in one plan.

Altitude report SHA-256: 802b2a2b5e5c126cfdd427c50a8940486a3629b8ba6531736819476aa7ccc392

- **[HIGH] §B, decision 10 — The line-break rule contradicts the accepted residual.** §B now says a project path containing a line break yields no project fact on either channel. Decision 10 says a delimiter line can truncate that same path into a valid prefix that the MCP helper uses as another project. The helper cannot detect the discarded suffix. State the exception in §B so both descriptions prescribe the same behavior. [Plan §B](.review/plan.md:105)

- **[MEDIUM] §K — Server duration cannot establish that a block reached the session.** A response can be lost or delayed after the server records a short duration, including when the client times out. An analysis therefore cannot reliably identify every unreceived block from that duration alone. Narrow the claim or record evidence of client receipt; otherwise an uptake analysis can treat an unseen block as injected. [Plan §K](.review/plan.md:238)

REVIEW COMPLETE

## Round 7

Michaël, **LGTM for the round 7 delta**. Both round 6 findings are resolved.

I acknowledge the altitude report’s proposed override: keeping the contract values in one plan remains justified.

Altitude report SHA-256: 15c7398b1fd5e16ea69b684d85f794e9e7917d1377a3abf7748836189c8d1fd1

REVIEW COMPLETE
