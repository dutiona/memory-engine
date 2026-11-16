Michaël, **option B is not yet applied consistently enough to prove the strict subset**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the plan is the assigned ADR outline, with duplication bounded by archival.

Altitude report SHA-256: 75ccddd42d5cde071ff9d8a31fac47bd41e478b47d85b4fb388c5520f52e73bb

- **[HIGH] §5, §8, §11 — `recall(project: …)` still resolves keys across kinds and machines.** §5 matches a key “any kind, any machine,” preferring the caller’s machine only when several projects match. Under option B, a remote-less key observed only on another machine can therefore resolve to that machine’s project; a remote key and a remote-less path with equal bytes can also resolve to the wrong project. Define the amended key’s wire representation and kind-aware, machine-qualified lookup, then cover both cases in C21 and C34.

- **[HIGH] Constraints, §3, §11 — The machine-qualified first-release Home label is unspecified.** The plan retains the rule that the label is the percent-encoded key, but does not define how the machine and key kind enter that key. Equal paths on two machines, or a path equal to a remote key, can therefore still produce the same first-release `project:<label>`. Specify an unambiguous encoding in both ADR lanes, including its 256-byte limit behavior, before claiming each existing label can be adopted unchanged.

- **[MEDIUM] §7, §8 — The new alias hint conflicts with “no automated matcher.”** The server is instructed to detect another machine’s equal dev-relative path and hint that an alias can be proposed, while §8 excludes an automated matcher and the settled decision assigns best-effort matching to the agent. State explicitly whether this equality check is an allowed hint or belongs to the agent; it must not bind projects without confirmation.

REVIEW COMPLETE
## Resolution

Addressed in the round-8 candidate (plan SHA-256 `75ad966bf58e50f6f1afd2a639c074716761e2a69bb57aa16b65df2cd2989ee5`).
- [HIGH] `recall(project)` across kinds and machines → path keys are machine-qualified `<machine>:<relative path>` (Constraints), so a key names at most one project; §5 step 2 matches byte for byte with no ambiguity clause; C5, C21, C34 cover a bare path, a cross-machine key and a path equal to a remote key.
- [HIGH] first-release label unspecified → Constraints pin the form and why it cannot collide (DNS-label machine, no `:` in a normalized remote key, 256-byte limit unchanged); relayed to the ADR-0020 lane in Surfaced #2; item 5 checks the label form against ADR-0020 as written.
- [MEDIUM] alias hint vs "no automated matcher" → §7: the hint is an advisory exact-equality observation, never a binding; proposing the `add` stays the agent's best effort and a human confirms.
