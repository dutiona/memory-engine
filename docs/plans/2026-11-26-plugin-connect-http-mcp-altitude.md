Plan SHA-256: 091d579025096c15228173b45f49f4aa7e800430b862febeba8cb83ba06b1620

# Altitude checkpoint: #1028 plan (plugin HTTP MCP connection and memory skill)

- Plan: `docs/plans/2026-11-26-plugin-connect-http-mcp.md`, 453 physical lines, 41,260 bytes (`wc -lc`).
- Top-level implementation tasks: 12 (`grep -c '^- \[ \]'`; items 0 to 11, of which 0 and 11 are operational and
  1 to 10 each one commit). The acceptance map is a table, not tasks.
- Fenced/literal volume: one fence (Verification, lines 437-445), 7 body lines, 739 bytes: commands, not a canonical
  file. Inline literals: the two `mcpServers` entries (item 5) and the ADR sentences (item 7) are the exact contract
  text a reviewer must judge, each a few lines; no complete product file, validator, generator, manifest or mutant
  catalogue is reproduced.
- Named target files and measured existing surface: `plugins/memory-engine/bin/me-hook` 390 lines,
  `tests/run.sh` 1,211, `tests/real-claude.sh` 209, `tests/stub_server.py` 81, `README.md` 87,
  `.claude-plugin/plugin.json` 22, `.claude-plugin/marketplace.json` 12, ADR-0020 545, `docs/reference/mcp-server.md`
  728, `AGENTS.md` 234. New: `tests/stub_embedder.py`, `skills/memory/SKILL.md`.
- Estimated new or changed surface (estimates, uncertain to about a factor of two): me-hook +60 to +90; run.sh +300 to
  +450 (about 15 new cases); real-claude.sh +300 to +450 (bring-up, phases M and L); stub_embedder.py about 60;
  SKILL.md about 40; README +50 to +80; plugin.json +25; ADR-0020 and mcp-server.md about +12. Roughly 900 to 1,200
  product lines against a 453-line plan.
- Revision 2 (round-1 findings): the plan grew by 31 lines of decided detail (stub counters, per-turn preconditions,
  ADR §3 wording, the `CLAUDE_PROJECT_DIR` probe, `t_envelope`), not by a change of role. Regeneration: none; this is
  the first plan for #1028. Lens drafts (ephemeral, not reviewed evidence) are under the
  ignored `docs/plans/2026-11-26-plugin-connect-http-mcp-drafts/`.
- Required fallback/provenance inputs: none. The probe results the plan relies on are stated in its Constraints; the
  reviewers judge them as recorded facts and read every other input from the repository tree.
- Calibration: an implementation plan for a coordinated change across a shell client, a manifest, tests, a skill and
  an ADR. Its detail sits in rules, test names and exact contract strings, which the issue and ADR-0020 make
  load-bearing (one key on two channels, a token printed into JSON, shell-parsed helper strings). Plan bytes stay well
  below the product they describe, no artifact is duplicated, and planning maintenance does not exceed
  implementation. No structural stop.
- Disposition: **proceed**.
