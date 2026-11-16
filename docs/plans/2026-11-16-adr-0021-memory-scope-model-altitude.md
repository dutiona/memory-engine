Plan SHA-256: e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a

# Altitude checkpoint: ADR-0021 plan (round 10 candidate: owner ruling B, key form pinned)

## Measurements

- Plan: `docs/plans/2026-11-16-adr-0021-memory-scope-model.md`, 589 physical lines, 53,718 bytes (round 9, 588 lines, 53,635 bytes, SHA-256
  `21e8b3a50998918bdae0a70c1b1b8f2f394d57dc21f1d76674693b7bdb62160b`; round 8, 585 lines, 53,288 bytes, SHA-256
  `75ad966bf58e50f6f1afd2a639c074716761e2a69bb57aa16b65df2cd2989ee5`; round 7, 576 lines, 52,392 bytes, SHA-256
  `2743211cb7d4bc1101aef20613607610d8a58ee7d84b304b1fe99129c65e1f88`; round 6, the reviewed and owner-approved-in-principle revision: 584 lines,
  52,852 bytes, SHA-256 `350a4bfcdcceef60c02541d51168b0f583a4019fcc32b44df5ba26c0e520d6b4`; round 5: 580
  lines, 52,442 bytes, SHA-256 `6334123a9cc4e7874d701834ed6d853629c8698edecb906913626aea57b24db3`; round 4: 574;
  round 3: 563; round 2: 533; round 1: 425).
- Top-level implementation tasks: 9 (unchanged), counted as lines matching `^- \[ \] \*\*` (`grep -c`).
- Fenced code: one `sh` block (Verification), 31 lines, about 2.8 KB: three check helpers (D1, P1, H1). Commands only.
- Other literal volume: one Markdown table (§6, 12 rows). No DDL, no code, no frozen manifest.
- Section volume: Constraints (lines 18–67); "Owner rulings at plan approval" (lines 68–86, replacing the decision
  section); ADR outline (lines 87–432); Surfaced (lines 433–446); Implementation sequence (lines 447–523);
  Documentation, Testing, Verification (lines 524–589).
- Target files unchanged from round 2: the new ADR (estimate 450–550 lines, uncertain), ADR-0007 (+1), ADR-0005 (+1),
  the index (+1).

## Delta from round 9 (bounded)

+1 line: the disjointness proof covers a bracketed IPv6 remote host (it starts with `[`, not a DNS label); C21 adds
`[2001:db8::1]/example/x`.

## Delta from round 8 to round 9

+3 lines, +0.3 KB: the key-disjointness rationale corrected (a remote key may hold `:` in its path but never before
its first `/`; a path key splits at its first `:`), and C21 adds a colon-in-path case.

## Delta from round 7 to round 8

+9 lines, +0.9 KB, from round-7 findings: the machine-qualified path-key form `<machine>:<relative path>` pinned in
Constraints (non-colliding by construction) and relayed to the ADR-0020 lane; §5 step 2 without the ambiguity clause;
C5, C21, C34 cover a bare path, a cross-machine key and a path equal to a remote key; the §7 alias hint stated as
advisory, never a binding; item 5 checks the label form.

## Delta from round 6 to round 7: owner ruling B

−8 lines, −0.5 KB. Michaël chose option B for Decision 1 and accepted Decision 2. The decision section becomes the
recorded rulings; §8 first sight binds a `dev-relative` Locator only to its own (machine, path), never to another
machine's equal path or a same-bytes remote key; joins of remote-less projects are human-confirmed `add`s only; the
"same key" hint, its SessionStart line and its counter are removed, replaced by a hint that an alias can be proposed;
MC key-rule row cites ADR-0020's rule as amended; C21 and C24 rewritten; Consequences, "Not decided here" and the
Surfaced notes follow. Intent change is the owner's own ruling, so this is a successor revision reviewed by a delta
round before approval is recorded.

## Delta from round 5 to round 6 (kept for the record)

+4 lines, +0.4 KB, from round-5 findings: a recorded replay target follows a later `merge` redirect (§12); P1 strips
the documented range literal per line before the address grep, and item 1 proves P1 can fail on probe files.

## Delta from round 4 to round 5 (kept for the record)

+6 lines, +0.7 KB, from round-4 findings: human-confirmed re-filing items replay as recorded (§12, C35); the
write-time `${PROJECT_ROOT}` rule narrowed to paths outside the session's checkout top level, nested worktrees a
known imprecision (§9); P1's address check ignores the documented range literal.

## Delta from round 3 to round 4 (kept for the record)

+11 lines, +1.6 KB, all from round-3 findings landed as rules, cases or framing: `set_level` replay re-resolved from its
call record and re-filings re-derived (§12, C35); no `${PROJECT_ROOT}` without the checkout fact (§9, C27); no token
after a `$`; the "same key" hint as its own SessionStart context line (ADR-0020's status-line grammar unchanged); the
merge preview bound to its log position and the maintenance ruling quoted (§8, C23); embed the tokenized query only
when it differs; P1 adds the 100.64.0.0/10 range and scans each artifact before its own publication; Decision 1
quotes spec decision 19.

## Delta from round 2 to round 3 (kept for the record)

+30 lines, +3.5 KB, all from round-2 findings, each landed as a rule, a case or a corrected framing: ignored local or
`file:` remotes fall back as in ADR-0020; `merge`/`add` apply redirect and memory moves in one approved transaction;
`${PROJECT_ROOT}` has exactly one prefix per session and never remaps across checkouts; replay folds every logged
Filing decision so `set_level` survives; invertible `$`-run escaping; union of literal and tokenized query; caller's
machine binding wins over `ambiguous`; an ambiguous key is treated as no Locator; confirmation is an `add` with
evidence "confirmed"; the ADR-0016 citation and per-visibility-class derived structures; ADR-0005 line worded against
what ADR-0005 states; P1 scans token shapes in every file and takes the commit messages from a saved file; the
decision section now quotes the spec's reason verbatim, names both brainstorm decisions in tension, restates B's cost,
and lists the ADR-0005 file as its own decision.

## Required fallback/provenance inputs

| Source | Review copy | SHA-256 |
| --- | --- | --- |
| brainstorm brief snapshot, `inputs/brainstorm-brief.md` in the issue's artifact dir | `.review/inputs/brainstorm-brief.md` | `14f74304af249f48a4023f151d0b22f5afca3388d85d4efa788a25a80888fb5a` |
| owner decisions snapshot, `inputs/brainstorm-decisions.md` | `.review/inputs/brainstorm-decisions.md` | `de12eadbbb01204e276d8fb8d24e75838abe7aa5b1408beab265bd6222fa171f` |
| ADR-0020 lane's plan snapshot, `inputs/adr-0020-plan-snapshot.md` | `.review/inputs/adr-0020-plan-snapshot.md` | `6cc46ce9221b76c7f96a70e37de514e7f641c0a3522e3d18d6dd67b551af8430` |
| epic spec snapshot (new in round 3, the source of decision 17's quoted reason, line 178), `inputs/epic-spec.md` | `.review/inputs/epic-spec.md` | `cec384b64752435f978f4110c78fc5ff1f6e720a45cef2696866f4d0d5b3cad3` |

The dispatch brief (private, not copied) states the plan's role verbatim: "The plan is the ADR outline: sections, the
minimal-contract → Home/Audience mapping (ADR-0020 decision 18 must be a strict subset), the conformance cases for
KB, and the open questions the brief lists. Keep /super-plan proportionate (Standard engagement): the design is
decided."

## Calibration and disposition

Unchanged reasoning: the product is a design record, the dispatch makes its outline the plan, and every reviewer in
rounds 1 and 2 acknowledged the override without asking for bullets to move out. The round-3 growth is findings landed
as rules and cases. No embedded canonical file, validator or generator; no near-copy successor; planning maintenance
stays below implementation. Duplication with the ADR ends when `/finish-pr` archives the plan.

Disposition: **justified override**, unchanged. It waives no requirement, finding, completeness, review, audit or
approval gate. Reviewers are asked to re-acknowledge it against this report's SHA-256.
