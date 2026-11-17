# Implementation audit (post-review): #1022

- Approved plan: `docs/plans/2026-11-17-1022-project-key.md`, SHA-256
  `dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400`, approved on the plan issue
  https://github.com/dutiona/memory-engine/issues/1082 (marker comment 6959320792).
- Post-review head audited: `64e9ad459e7837d8bee4ce90be73aa88ba918a21`.
- Scope: post-review. The pre-review audit (`-implementation-audit.md`, head `c03f4b9`) stays as written.

## Items

| Item | Status | Evidence |
|---|---|---|
| 0. Plan on the branch | Implemented | `baf9352` |
| 1. Decode the five facts and strip userinfo | Implemented | `2b726ae`; review fixes `6d39692`, `ae6fa07`, `24f8eac`, `cbcc53c`, `b2d28a3`, `64e9ad4` |
| 2. Derive the key, rules v1; one Home type | Implemented | `48936bc`; rules v1 stated in full on the reference page, `8907662` |
| 3. Scoped writes: `insert_event_at`, `move_fact_atomic` | Implemented | `16ece17`; comment placement `b9db7f7` |
| 4. Every call reads its facts and records them | Implemented | `3e43137`; refusal records redacted, `6d39692` |
| 5. `recall` reads the session's project | Implemented | `b79dfdf` |
| 6. `remember(content, level?)`: Home, receipt, hint | Implemented | `16f9f9a` |
| 7. ADR-0021 §8 amendment | Implemented | `fe6c641`, cites dutiona/infra#37 comment 6959376409 |
| 8. `set_level(id, level)` | Implemented | `dc617c9`; error text `e63ffad` |
| 9. Documentation | Implemented | `e6f9f27`; review restatements `8907662`, `ee42303`, `71ff371`, `5dcdc32`, `d54bf93`, `2fbe158` |
| 10. Boy-scout: stale line references | Implemented | `c03f4b9` |
| 11. Reconcile with #1021 and #1023 | Implemented | `origin/main` is still `7a22c07`: nothing to reconcile |
| 12. Verification | Implemented | rerun at the final head under `/finish-pr` |
| 13. PR | Implemented | https://github.com/dutiona/memory-engine/pull/1086 |
| 14. `/super-review` | Implemented | Full gate, Default severity, 7 rounds to LGTM (Claude clean-slate subagent + Vibe), relayed on the PR |
| 15. `/finish-pr` | Implemented | this audit is part of it |
| 16. Merge | Not yet due | Michaël only |
| Added: reference toctree | Added | `98b9fbc`, boy-scout |

Counts: 16 Implemented, 0 Modified, 0 Dropped, 0 Incomplete, 1 Added, 1 not yet due. Divergence: 1 of 17 original
items (Modified + Dropped + Added), 5.9%, under the 30% threshold.

## Intent-preserving variations from review (not Modified)

The plan's intent for items 1 and 4 is ADR-0020 §3's "nothing stored holds userinfo". Review showed the two-step
strip could not meet it alone, so the mechanism grew; the intent did not change.

- Key rules v1 gained rule 11: a scheme-form URL that still holds an `@` after stripping and gives no remote key is
  dropped as `unstripped_userinfo`. It reads the decoded value only and changes no key. Its residual (a client that
  sent an encoded `/` in userinfo whose text before it reads as a host) is named on the canonical page.
- Refusal records keep the refused message through `redact_userinfo`, which removes the userinfo of every URL quoted
  from a tool argument; it is total over UTF-8 (a totality test covers non-ASCII and multibyte whitespace).
- `KEY_RULES_VERSION` stays 1: version 1 is defined by this PR, which has not merged.

## Declined in review

- Embedding before recording a `remember` (round 1): #1021 replaces that path.
- Closing the scp-form residual (round 5): it would extend rule 11 to every shape for a client that breaks two
  ADR-0020 §3 rules; it stays documented.
