Reviewer: Claude Sonnet (medium effort) clean-slate subagent via the Agent tool, standing in for Codex (2 rounds, resumed)

~~~~
Altitude report SHA-256: e264eb389dcb12b90a5cd1d81516b97cd3aefb836bf773389d3141db01ab1bb9

## Round 2 review (delta only)

Michaël, LGTM. I read the delta only and ran nothing. The delta adds no blocker, contradiction or public-safety problem, and I accept both dispositions that were not changed.

### Round-1 findings

| Round-1 finding | Status |
|---|---|
| MEDIUM: `CLAUDE_PROJECT_DIR` physical or logical | Resolved. Item 1's probe records it, with `PWD` set and a scratch SessionStart hook, before item 3. Decision 1 and the README upgrade note carry the consequence. |
| MEDIUM: model-dependent real run | Resolved. Exact per-turn prompts, a `tool_use` precondition that reports `model did not call &lt;tool&gt;`, and `--max-turns 40`. |
| MEDIUM: Decisions 3 and 5 not up for a ruling | Resolved. Both are flagged for the owner, and item 7 amends the ADR-0020 §3 MCP-channel paragraph ("Requests to memory-engine carry the same project facts…"). |
| LOW: stub GETs | Resolved. Non-event requests, GET included, get their own counter, `DIR/other/` and `other.log`. `DIR/log` and `DIR/req` stay 1:1 with the event route, so `only_503` and `delivered` are unaffected. |
| LOW: redundant `unset` | Resolved. Replaced by "never reads it from the environment". |
| LOW: embed provider named | Resolved. See the check below. |
| LOW: macOS `/bin/sh` | Resolved. It is now its own Verification command, on a macOS host over ssh. |
| LOW: private repo name | Resolved in the acceptance map ("dotfiles follow-up"). |
| LOW: `--debug` exposure | Resolved. A README sentence. |
| LOW: desktop-only real run | Resolved. AGENTS.md says it runs on a maintainer's desktop, not in CI. |
| LOW: `hostile_git_config` | Kept, as I said was acceptable. |
| LOW: M5 trigger | Not changed. Accepted. |

### Dispositions not changed
- **M5 does not distinguish a 401 from a reconnect: accepted.** The acceptance outcome is "a rotated token is picked up without restarting". M5 proves that, and the trigger is Claude Code's mechanism. The new M5 wording is honest about it.
- **`hostile_git_config` kept: accepted.** It is a behaviour test and cheap.

### Delta checks
- **`--embed-provider openai` (item 1).** I could not confirm from `config.rs` whether the `openai` label needs an API key or https. The only provider label there is `DEFAULT_PROVIDER`, and the `openai` semantics live in `memory-engine-embed`'s `http.rs`, which I only grepped. The plan covers this: a start-up failure shows in `DIR/log` or the ME stderr, and the worker adjusts. The "label, not protocol switch" claim is unverified, so I would not treat it as settled. [LOW]
- **Item 10's `t_envelope` change.** I confirmed `t_envelope` hard-codes `"0.1.0"` at run.sh line 228. The plan's fix (read `ME_VERSION` from `me-hook`) is right and correctly ordered. Red is "bump both, `envelope` fails", which is sound.
- **Item 3, `F_OK` set only once `F_ROOT` is non-empty.** This is a small behaviour change on the hook channel. The existing capture cases are the oracle, and a non-empty root is guaranteed for an existing absolute directory. Fine.
- **Item 6, phase stops on a failed precondition.** It reports `not ok - model did not call &lt;tool&gt;` and stops, which is right. The preconditions apply per turn, including the two-call turns, since both tool names are checked.
- **Plan size.** It grew 31 lines (422 to 453), all decided detail. The altitude note justifies it, and I see no structural change.
- **Public safety.** The delta adds nothing private. Only "dotfiles follow-up" appears, and no host, path or token.

REVIEW COMPLETE
~~~~

## Resolution

Round 1 (plan bf4e06c9…, altitude 5ed71744…), every finding:
- [MEDIUM] `CLAUDE_PROJECT_DIR` physical or logical is unknown before item 3: item 1's probe now records it (scratch SessionStart hook, `PWD` set), before item 3; Decision 1 and the README upgrade note carry the consequence.
- [MEDIUM] the real run depends on the model calling the exact tools: item 6 now gives exact per-turn prompts, `--max-turns 40`, and a per-turn precondition reporting `model did not call <tool>`.
- [MEDIUM] Decisions 3 and 5 reinterpret issue text without a ruling, and ADR-0020 §3's MCP-channel paragraph would contradict Decision 3: both decisions are flagged for the owner beside Decision 1; item 7 amends that paragraph.
- [LOW] M5 does not observe the 401 itself: not changed. The acceptance outcome is that a rotated token is picked up without restarting the session, which M5 proves; whether a 401 or a reconnect re-ran the helper is Claude Code's mechanism. The reviewer accepted this in round 2.
- [LOW] phase A wire checks cover POSTs only: fixed, item 5 (GETs are recorded too).
- [LOW] redundant `unset CLAUDE_PROJECT_DIR`: fixed, item 4.
- [LOW] `hostile_git_config` is the weakest test: kept as a behaviour test, as the reviewer recommended.
- [LOW] the `--embed-provider` value unnamed: item 1 names `openai`.
- [LOW] the macOS `/bin/sh` run needs a macOS host: Verification lists it as its own command on a macOS host over ssh.
- [LOW] public hygiene ("my-dotfiles follow-up"; the desktop-only real run): fixed in the acceptance map and item 9's AGENTS.md change.
- [LOW] `--debug` can reveal the `Authorization` header: README sentence, item 9.

Round 2 (plan 091d5790…, altitude e264eb38…): LGTM. Its one [LOW], that `--embed-provider openai` being a label rather than a protocol switch was unverified, was checked by the plan owner against `crates/memory-engine-embed/src/http.rs`: the provider is an operator-declared label recorded in the embedding fingerprint, and only the value `llama.cpp` changes behaviour (token counting); `openai` changes nothing on the wire. No plan change.

Degradation: this reviewer stands in for the Full gate's Codex leg (gpt-6.1-terra), which refused on round 1 with a usage limit (retry after about 20:39Z on 2026-11-26), per the fleet roster of 2026-11-26 ("Sonnet stands in only when Codex refuses").
