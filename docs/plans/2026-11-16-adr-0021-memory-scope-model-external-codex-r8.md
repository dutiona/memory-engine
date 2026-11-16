Michaël, **the delta addresses the round-7 lookup and label gaps, with one correction needed in the key rationale**. Confidence: high.

I re-acknowledge the altitude report’s justified override: the outline remains the assigned ADR review artifact, with duplication bounded by archival.

Altitude report SHA-256: 8b027d88192d5cbb363ea8628bdb2faf7306da39cde260c6f0c2ba637d048465

- **[MEDIUM] Constraints — A normalized remote key can contain a colon.** Removing the scheme and port does not remove a colon from the URL’s path: `https://github.com/example/a:b` yields a key containing `a:b` under the stated normalization. The proposed path-key distinction may still hold because its separator precedes the relative path, but the claimed proof that remote keys never contain `:` is false. Specify the remote-key grammar and prove the namespaces disjoint under it; add a colon-in-path case.

The machine-qualified lookup, cross-machine cases, and advisory-only alias observation otherwise address my round-7 findings.

REVIEW COMPLETE
## Resolution

Addressed in the round-9 candidate (plan SHA-256 `21e8b3a50998918bdae0a70c1b1b8f2f394d57dc21f1d76674693b7bdb62160b`).
- [MEDIUM] a normalized remote key can contain `:` → Constraints now state the narrower invariant: a path key's first `:` comes before any `/`, right after a DNS-label machine; a remote key has no `:` before its first `/` (port dropped, scp form rewritten, IPv6 host bracketed) though its path may hold one; a path key splits at its first `:`. C21 adds `github.com/example/a:b` against `laptop:a:b`.
