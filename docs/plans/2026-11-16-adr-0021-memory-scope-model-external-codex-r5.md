Michaël, **the approved Audience restriction now survives replay, but the revised privacy check has a gap**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the plan is the requested ADR outline, and its duplication is bounded by archival.

Altitude report SHA-256: d03c3008dfa41a8cda4fdee10c142d383536776077867d872420657ee3f2f35a

- **[MEDIUM] Verification — The address exception suppresses an entire matching line.** `grep -vF '100.64.0.0/10'` removes any line containing the documented range, including one that also contains a private address in that range. Exclude only the range literal itself before checking the remaining text, and verify a line containing both the literal and a private address is rejected.

The §12 change and C35 address my round-4 finding by preserving the approved machine restriction during replay.

REVIEW COMPLETE
## Resolution

Addressed in the round-6 candidate (plan SHA-256 `350a4bfcdcceef60c02541d51168b0f583a4019fcc32b44df5ba26c0e520d6b4`).
- [MEDIUM] the address exception suppresses a whole line → P1 strips only the literal `100.64.0.0/10` per line (`sed`) before the address grep; a probe line holding the literal and an address in the range returns 1; a line with the literal alone returns 0 (both run, results as stated).
