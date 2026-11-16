Plan SHA-256: 611a7deb309d1f0d722cba35bb23920187f2d0c975792994c5c7646e53261f74

# Altitude checkpoint: 2026-11-16-mcp-http-bearer

- **Plan:** `docs/plans/2026-11-16-mcp-http-bearer.md`, 404 physical lines, 34,795 bytes. Top-level implementation
  tasks: 11 (items 0-10), counted as lines matching `^- \[ \]` outside fenced blocks (`awk` over the file).
- **Literal volume:** one fenced block (lines 369-384, the Verification command list), 14 lines / 729 bytes. It is a
  command list copied from `AGENTS.md`, not a canonical file, validator or generated payload. Inline code spans
  carry signatures, flag names and test names only; no complete product file is embedded.
- **Target files, measured existing surface:** `crates/memory-engine-mcp/src/main.rs` 291 lines, `config.rs` 284,
  `server.rs` 131, `lib.rs` 21, `tools/definitions.rs` 416 (one schema line changes), `Cargo.toml` 56; `deny.toml`
  92 (comment only); `crates/memory-engine/src/engine/mod.rs` 1,052 (doc-comment only, item 8);
  `docs/reference/mcp-server.md` 270; `docs/getting-started/installation.md` 76.
- **New surface, estimates (uncertain, not measured):** `src/auth.rs` ~250 lines with its unit tests; `src/http.rs`
  ~150; `tests/common/mod.rs` ~250; `tests/http_auth.rs` ~250; `tests/http_shutdown.rs` ~100;
  `tests/server_process.rs` ~250; `main.rs`/`server.rs`/`config.rs` deltas ~120; docs ~120. Total ~1,500 lines,
  roughly 4x the plan.
- **Revision:** fourth candidate, after the clean-slate subagent's round-2 delta review (LGTM with four [LOW]:
  leak-guard false positives on the review artifacts, a stale Testing section, the guard-holding rationale, and an
  empty item 9). The third candidate had SHA-256 `042c2fe7884876cbc3e48d6ae917861d94aa6626e1c8dc42c7407018c69f8983`
  (Codex and agy LGTM on it); this delta folds the old item 9 into the docs item (11 items now), holds the drain
  guard with `std::mem::forget`, and updates Testing and the leak guard. The third candidate followed the Codex/agy
  round 1 (Codex: two
  [MEDIUM] on item 5's red test and the untested fail-closed guard; agy: LGTM). The second candidate had SHA-256
  `f634253c2b022da755eabe9dc4587fdfd02766a6dfb7e7c8c7ff670bfe5b6864`; this delta moves the in-process Host tests
  to item 2, gives item 5 a CLI-plumbing red, adds unit tests for the guard, and adds one doc boy-scout commit to
  item 7. The first-round revision followed the clean-slate review (its findings and resolution are in
  `docs/plans/2026-11-16-mcp-http-bearer-subagent-review.md`). The predecessor candidate had SHA-256
  `4006dc1c801b5e7bee6d76abd2693a3893deedea4ea25a7bf4f52b39882b1b2a`; the delta adds tests, one startup check
  (`backup_dir`), sharper commands and residuals, and replaces the leak guard; no item was dropped or re-scoped.
  `draft-context.md` was redacted (the deploy port removed), so its digest changed. The three lens drafts (moved out of the repository to the worker's private artifact
  directory, `plan-drafts/`) are ephemeral working inputs, not predecessors.
- **Required provenance inputs (copied into the review tree under `.review/inputs/`):**
  - `issue-1018.json`, the fetched issue record (title, body, comments) — source
    `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1018/scratch/issue-1018.json`,
    copy `.review/inputs/issue-1018.json`, SHA-256
    `97eb531f77346475c6539d5ffe96e48be3d44a5ad2c82e962e0c3c8eb5489095`.
  - `draft-context.md`, the planning brief (constraints from the owner and the fleet) — source
    `~/.local/state/agent-work/2026-11-02-me-kb-claude-code/fleet/artifacts/memory-engine-1018/scratch/draft-context.md`,
    copy `.review/inputs/draft-context.md`, SHA-256
    `1d5fc1261d761d8b7a6ef5724be5259c3e1ca33fd1298f2f280442df2fa277b6`.
  - `rmcp-1.8.0/tower.rs`, rmcp's streamable-HTTP server (config defaults, Host check, stateless POST path) — source
    `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-1.8.0/src/transport/streamable_http_server/tower.rs`,
    copy `.review/inputs/rmcp-1.8.0/tower.rs`, SHA-256
    `1664c5bf193b2e185da26be63f4712f4ae354c874afd770cc0b0d282036393ca`.
  - `rmcp-1.8.0/service.rs`, rmcp's request context and extensions — source
    `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-1.8.0/src/service.rs`, copy
    `.review/inputs/rmcp-1.8.0/service.rs`, SHA-256
    `87af5ef04e9a35c4614d382f196c93c3d313b6a2d3ae8fd545062abdc13f2162`.
  - ADR-0020 and ADR-0021 are committed at the review tree's HEAD (`docs/design/adr/`); the rest of the rmcp 1.8.0,
    axum 0.8.9 and reqwest 0.12.28 sources are in the cargo registry (not copied).
- **Calibration.** The plan's job is to assign one security-relevant, multi-file feature (transport swap, auth
  layer, shutdown semantics, configuration, a shared harness later tickets reuse) to commits with failing tests. Its
  detail sits in Decisions (ten load-bearing choices with their reasons, each verified against rmcp/axum/reqwest
  source) and per-item red/green commands. Signals checked: no complete product file, no duplicated validator or
  generator, no frozen manifest or transcript, no mutation catalogue; planning maintenance is well below
  implementation. The plan is longer than a typical Lean plan because the acceptance has ten criteria and three
  boy-scout fixes, each needing its own commit; that is coordinated necessary work, not a second copy of the product.
- **Disposition:** `proceed`.
