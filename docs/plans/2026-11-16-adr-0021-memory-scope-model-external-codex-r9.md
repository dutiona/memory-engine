Michaël, **the revised rationale still contains a contradiction for IPv6 remotes**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the plan is the assigned ADR outline, with duplication bounded by archival.

Altitude report SHA-256: db422fdb638f5215205a0d6ff22442bf6cb877f47341331d64ea3b1f8050f3e0

- **[MEDIUM] Constraints — A bracketed IPv6 host has colons before the first slash.** For example, a normalized key beginning `[2001:db8::1]/…` contradicts the new invariant even though the same sentence explicitly permits a bracketed IPv6 host. The namespaces can still be distinguished from a DNS-label-prefixed path key because the IPv6 key starts with `[`. State that case in the proof and add it to C21.

The added colon-in-path case addresses my round-8 example.

REVIEW COMPLETE
## Resolution

Addressed in the round-10 candidate (plan SHA-256 `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a`).
- [MEDIUM] a bracketed IPv6 host has colons before the first slash → Constraints: a remote key either starts with `[` (not a DNS label) or has no `:` before its first `/`; C21 adds `[2001:db8::1]/example/x`.
