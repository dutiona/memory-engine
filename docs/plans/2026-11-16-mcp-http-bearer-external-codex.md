# Codex review (headless, gpt-6.1-sol), thread 01a1e664-a07f-7d21-8954-3298c2649c5d

## Round 1 (plan f634253c2b022da755eabe9dc4587fdfd02766a6dfb7e7c8c7ff670bfe5b6864)

Michaël, two [MEDIUM] findings remain.

Altitude is justified: the plan describes security decisions and commit boundaries without duplicating the implementation. The ADR-based interpretation allowing authenticated MCP requests to return 403/405 is justified; ADR-0020’s lifecycle status table governs those lifecycle routes. Documentation, Testing, and Verification sections are present. The supplied digests match.

Altitude report SHA-256: 9822dbd63a6f2b7acde35d9d4b1e6dd175c96b5029438d9bd2af858f37306836

- **[MEDIUM] Item 5’s specified behavioral red test is already green after item 2.** Item 2 explicitly appends `allowed_hosts` and implements `start_with_hosts`; therefore item 5’s `configured_host` test cannot initially receive the claimed 403 when given `memory.example`. Move that transport test into item 2. For item 5, establish red/green evidence through the binary’s flag/environment plumbing and parser rejection cases.

- **[MEDIUM] Decision 4 / item 3 leaves the fail-closed handler guard untested.** Every planned HTTP request reaching `call_tool` already carries authenticated `Parts` and `Machine`. Removing the missing-identity rejection would therefore leave all planned tests green. Add negative handler tests for missing `Parts` and for `Parts` without `Machine`, asserting `internal_error`, no embedding request, and no fact written. Keep the authenticated write as the positive control.

Read-only source inspection only; no tests, builds, or gates were run.

REVIEW COMPLETE

## Round 2 (plan 042c2fe7884876cbc3e48d6ae917861d94aa6626e1c8dc42c7407018c69f8983)

Michaël, LGTM for the round-2 delta. No new findings.

- Item 5 now has a valid CLI-plumbing red test; Host transport tests correctly belong to item 2.
- Item 3 adds both missing-identity cases and a positive control. Testing the extracted guard is proportionate, with its invocation explicitly required before dispatch.
- The separate documentation fix is justified by the source.

Altitude remains proportionate; report and plan digests match.

Altitude report SHA-256: f08abd7a4631b09c914e8a6ad5578b82e61f29b0e82238b165bc81c4b358d2a8

Read-only review; no tests, builds, or gates run.

REVIEW COMPLETE

## Round 3 (plan 611a7deb309d1f0d722cba35bb23920187f2d0c975792994c5c7646e53261f74, the final revision)

Michaël, LGTM for the round-3 delta. No new findings.

Altitude remains proportionate; the report and plan digests match. Removing the empty item is justified by the source. Retaining the shutdown write guard prevents queued calls from committing after the flush. The Testing and leak-guard updates are consistent with the revised plan.

Altitude report SHA-256: 52a3efe3d3a5c61e8638f35bcc31aa9b1088a1466f4c96889f97b020992084a7

Read-only review; no tests, builds, or gates run.

REVIEW COMPLETE