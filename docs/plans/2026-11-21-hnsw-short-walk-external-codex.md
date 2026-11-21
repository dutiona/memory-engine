Reviewer: Codex gpt-6.1-terra (medium) via codex-headless-round.sh (headless exec + resume, one thread, 4 rounds)

~~~~
LGTM, Michaël. The delta qualifies the upstream references without changing the plan’s meaning. The refreshed altitude report’s **proceed** disposition remains justified.

Altitude report SHA-256: b21b9c62c87b0bf6fbeea40da75a070f751fbd716e78aa84148e6268b282c528
REVIEW COMPLETE
~~~~

## Resolution

Plan reviewed: four rounds on one resumed thread. Round 4 reviewed the final plan bytes
(`Plan SHA-256: d3fe837fdd7e6ae9b6bd8461dd718571b4a8436aa0ef3dab79c85bb49410ae94`) and gave LGTM.

Round 1 (plan 933b3a39…):
- [HIGH] A short walk does not necessarily widen; widening depends on filtered survivors, and raising `ef` can change
  candidates. **Fixed in revision 2.** The Outcome now describes post-filtering a short prefix. A new "No query that
  succeeds today changes" paragraph proves the point: item 4 only touches calls with `found < dest.len()`, and item 5
  only raises `ef` when `dest_len > ef`; both calls panic today. In Testing, T1 at limit 34 and T1 at limit 120 now
  separate "enough survivors" from "too few".
- [HIGH] D1 alternatives' acceptance boundary and per-option sequences. **Fixed in revision 2.** D1 now states, per
  option, the Outcome it meets and which items change. A pick of (e) or (c) requires a complete successor plan before
  implementation. Not adopted: inlining full red/green sequences for every alternative, which would put a second plan
  inside this one.
- [MEDIUM] T1/T2 query vectors and RNG range unspecified. **Fixed in revision 2.** The complete seeded inputs and the
  query `[1, 0, 0, 0]` are now spelled out. Item 3 keeps the observed red runs required.

Round 2 (plan 72efef53…):
- [MEDIUM] A direct git fork cannot leave items 3–5 unchanged. **Fixed in revision 3.** The fork pins the 0.11.0
  release commit for the red run; item 4 pushes the fix to the fork and bumps `rev`; the PR links the fork commit.
- [MEDIUM] T2 checks the result, not the widen-then-brute-force path. **Fixed in revision 3.** T2 and Testing now
  describe it as a result check only. Asserting the path would pin the walk length, which depends on the D2 defect.

Round 3 (plan 3955a638…): LGTM, no findings.

Round 4 (plan d3fe837f…): LGTM, no findings. The only change was qualifying the upstream references
as `rust-cv/hnsw#N`, so the published plan issue does not autolink unrelated memory-engine issues.

Degradations: none. The reviewer model is `gpt-6.1-terra`, per Michaël's 2026-11-21 roster ("if you need codex,
gpt 6.1 terra only").
