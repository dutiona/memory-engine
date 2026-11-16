# Clean-slate subagent review (Claude, Opus), round 1

Reviewer: fresh `general-purpose` subagent on Opus, no conversation context, read-only. Reviewed plan SHA-256
`4006dc1c801b5e7bee6d76abd2693a3893deedea4ea25a7bf4f52b39882b1b2a`.

---

**Plan review: #1018 MCP over HTTP behind per-machine Bearer auth**

Altitude report SHA-256: 6467a948116538ab5d596392f1d6860bae4e9fa6332524b25fdf07e203b4cacc

**Verdict: not LGTM as written. There is one blocker, and it is a trivial fix: private infrastructure values in the Verification section.** The design itself is sound. I checked every load-bearing claim about rmcp, axum and reqwest against the 1.8.0, 0.8.9 and 0.12.28 source, and they hold. I'm highly confident in those checks; the test-precision findings below are my own inference.

**Provenance**
- Plan SHA-256 `4006dc1c…2b1a` matches the altitude report's header.
- The `.review/inputs/` copies are not in the worktree, so I hashed the source files instead. `issue-1018.json` (`97eb531f…5489095`) and `draft-context.md` (`6222df2f…47dd`) match the report.
- The report's measurements are right: 355 lines, 30,087 bytes, 12 items, and the file line counts.

### Altitude and duplication (assessed first)
- I agree with the report's `proceed`. The plan carries decisions, signatures, test names and red/green commands. It contains no product code, no frozen payloads and no duplicated validator, and its length comes from ten acceptance criteria plus three boy-scout commits.
- Constraints lines 18–24 restate ADR-0020 §4. That duplicates a merged contract, but it is compact, it works as item 1's test checklist, and the plan says ADR-0020 and ADR-0021 win over any restatement. Acceptable.
- Justified overrides, acknowledged:
  1. GET and DELETE with a valid token get 405, not the acceptance's "200", because stateless rmcp serves only POST (`tower.rs:877-891`). Asserting 200 on `initialize`, `tools/list` and `tools/call` keeps the criterion's intent.
  2. A 403 on `/mcp` for an authenticated request with a wrong Host, despite ADR-0020's "never 403" row. That table describes the lifecycle routes, and the constraints forbid `disable_allowed_hosts`.
  3. `metadata.machine` instead of a column. This follows from "no new SQL schema" and the call record arriving with #1020.
  4. The accepted residuals (no body limit, all 26 tools exposed, no drain deadline). See L8 for two additions.

### Findings

**[BLOCKER] Verification, lines 348–349: the `rg` guard leaks the private values it is meant to catch.**
- Two of its patterns encode private infrastructure. One is a character class that matches exactly the ME and KB deploy ports. The other is the leading characters of the real tailnet name. I confirmed both against the private infra repo and will not repeat them here.
- The constraint on line 33 ("no tailnet name, address, deploy port … anywhere") is broken by the plan itself, and the plan is set to go public three ways:
  - committed under `docs/plans/` on a pushed branch;
  - published by `archive-plan-issue.sh --repo` to the public tracker *before approval* (Operational steps);
  - pasted into the PR body with the gate output.
- Fix:
  - Move the infra-specific patterns to a private file outside the repo, for example under `~/.local/state/agent-work/<lane>/`, and run `rg -f <file>`.
  - Keep only generic shapes in the plan: `\.ts\.net\b`, the CGNAT `100.64.0.0/10` range, and the `me_` token shape.
  - Then re-hash the plan and regenerate the altitude report.

**[MEDIUM] Item 4 red/green command filters out the in-process tests.**
- `--test http_shutdown --test server_process sigterm`: the filter `sigterm` matches nothing in `http_shutdown.rs` (`shutdown_lets_…`, `shutdown_waits_…`), so those tests never run under the stated command.
- The "flush alone, watch `shutdown_waits_for_an_abandoned_call` stay red" step has no command at all.
- Fix: run `--test http_shutdown` without a filter, plus `--test server_process sigterm`.

**[MEDIUM] Item 2 matrix: the "two `Authorization` headers" case has no stated values.**
- If one copy is invalid, a `headers.get()` (first-wins) implementation passes whenever the bad copy comes first.
- To pin Decision 3's exactly-one rule, both copies must be the valid token. A second variant could pair the laptop and desktop tokens.

**[MEDIUM] Item 4 fallback ("if it passes on the flush alone … drop the fence") reasons the wrong way.**
- The hazard is structural, and I verified each piece:
  - rmcp `tokio::spawn`s the stateless call (`tower.rs:1250`).
  - Its handler runs in `spawn_service_task` with the JoinHandle dropped (`service.rs:1178`).
  - hyper's HTTP/1 dispatcher closes a busy connection on EOF when half-close is off, which is the default.
  - So axum's graceful future returns while the write is still running.
- A green run on the flush alone means the test did not actually abandon the request, for example because the future was not dropped or the connection was reused. Replace the fallback with "fix the test until it goes red; keep the fence."

**[LOW]**
- **L1, Decision 6.** The write guard drops when `serve` returns. A detached call that was still waiting for its read guard behind the writer can then commit after the flush, before the runtime tears down. This is safe today: the fingerprint is counts and max ids (`snapshot.rs:163`), so the sidecar is rejected and rebuilt. Holding the guard until `main` exits closes it at no cost.
- **L2, item 6.** `backup_dir` is not checked at startup. `VACUUM INTO` into a missing or unwritable directory fails only when a migration is pending, so a typo shows up at the next upgrade as a refusal to start. An `is_dir()` check at startup moves that failure to deploy time, and the same process test can cover it.
- **L3, item 6 red.** Asserting `config.engine.backup_dir` gives a compile error, not an unknown-field failure. It is still red; only the wording is wrong.
- **L4, item 2.** The harness uses tokio `sync` (oneshot), `time` and `net`, but `sync` is declared only in item 4. It compiles anyway through feature unification. Declare the features where they are first used.
- **L5, item 2, `a_bad_token_file…`.** "Missing file → non-zero exit" also passes on a clap exit 2, the same trap item 11 guards against. Assert exit code 1 or that stderr names the path.
- **L6, item 5.**
  - `MEMORY_MCP_ALLOWED_HOSTS="a, b"` is refused because of the leading space. That is correctly fail-closed, but the docs should say "no spaces".
  - `user@host` parses as an `Authority` and silently equals `host`. Refusing `@` is optional.
- **L7, Decision 4.** `memory_flush_insights` (`p0.rs:497-509`) already implements the rule "absent/null → `{}`, object → stamp, other → reject". `record_machine` re-implements it at another layer; one shared helper would do.
- **L8, item 11 residuals.**
  - Name `memory_dump_state`: every token holder on the network can now make the server write files into its temp dir, where before only a local stdio parent could. It is bounded by `validate_dump_path`, but the trust model has changed.
  - The body-limit residual has a catch. rmcp 3.x has a default 4 MiB limit (RUSTSEC-2026-0461 text), while 1.8 has none (`expect_json` → `body.collect()`). `memory_bootstrap_session` accepts 50 MB of JSONL, so #1025's limit must exceed that or wait for #1020 to remove the tool.
- **L9, item 1, "trailing space".** This works as a byte-level unit test. On the wire, though, httparse trims trailing whitespace (`httparse-1.10.1/src/lib.rs:1228`), so the same case over HTTP gets 200, not 401. Never add it to the HTTP matrix.
- **L10, the release-checklist question.** A note in the PR body does not last. The skill says the promoted binaries are "the only artifacts the harness consumes", which becomes false at merge; one line in the skill is the durable fix. This is Michaël's call.
- **L11, items 9 and 10.** Both rewrite the Architecture section of `mcp-server.md`. Make sure item 9's commit is not wholly overwritten by item 10.

### Structural completeness
Documentation, Testing and Verification are all present, and Verification covers the full `AGENTS.md` matrix plus MSRV (a dependency changes).

### Feasibility
- The commit order builds.
- Each red is real:
  - item 1 panics on `todo!`;
  - item 2 fails to compile, then fails with the layer removed;
  - item 3 stores `"forged"`;
  - item 5 returns 403;
  - item 6 finds no `.bak`, in both the TOML and env cases;
  - item 7 exits 101 in debug, confirmed in reqwest `blocking/wait.rs` `enter()` via `ClientHandle::new`.
- The only gap is item 4's command filter (MEDIUM above).

### Verified as stated (no action needed)
- **rmcp defaults and hosts.** The loopback default is at `tower.rs:108-121`, and an empty host list means allow-all, so "never empty" is load-bearing. `with_allowed_hosts` replaces the list, so appending means mutating the pub field.
- **rmcp request path.** The Host check (403) runs before method dispatch. Stateless POST moves `Parts` into the request extensions, which reach `RequestContext.extensions`. The `Extensions` Debug impl prints nothing. Cancelling the token turns a JSON call into a 500 ("empty response"). No rmcp 401 exists. No advisory hits the 1.8 server transport: 0461 and 0457 leave this version unaffected, and 0189 is patched as of 1.4.
- **axum and the lock.** axum 0.8.9 `Router::layer` wraps the path router, the fallback router and the default `NotFound` catch-all. The lock has a single `http` 1.5.0, so `axum::http::request::Parts` is the type rmcp inserts.
- **Engine.** `verify_embedding_identity` checks the fingerprint only, so the harness embedder sees no startup request. `flush_snapshot` snapshots the projections before reading the fingerprint, so the hazard in Decision 6 is real. `migrate` writes `<db>.v<N>.bak` before any step.
- **Call sites.** There are 115 `dispatch(` call sites, and `dispatch` already has 7 parameters.

## Resolution

Round-1 findings, applied to plan revision `f634253c2b022da755eabe9dc4587fdfd02766a6dfb7e7c8c7ff670bfe5b6864`:

- [BLOCKER] leak guard encodes private values → Verification now holds generic shapes only (`\.ts\.net\b`, the
  CGNAT 100.64.0.0/10 range, the `me_` token shape); the deployment-specific patterns moved to a private file under
  `~/.local/state/agent-work/.../memory-engine-1018/private-leak-patterns.txt`, run with `rg -f` over the diff, the
  commit messages and the PR body. The planning brief input (`draft-context.md`) was redacted the same way; the lens
  drafts were moved out of the repository.
- [MEDIUM] item 4 filter → `--test http_shutdown` runs unfiltered; the flush-alone red step has its own command.
- [MEDIUM] two-header case → both copies hold the valid token, plus a laptop+desktop pair.
- [MEDIUM] item 4 fallback → replaced by "fix the test until it is red; keep the fence", with the structural reason.
- [LOW] L1 → accepted as a documented residual in Decision 6 (a queued call committing after the flush yields a
  fingerprint mismatch and a safe rebuild); holding the guard past `serve` would need a guard-returning signature
  for no correctness gain.
- [LOW] L2 → item 6 refuses to start when a set `backup_dir` is not a directory, with a process test.
- [LOW] L3 → item 6 red wording now says compile failure.
- [LOW] L4 → tokio `sync` (and dev `time`) declared in item 2, where first used.
- [LOW] L5 → the bad-token-file test asserts exit code 1 (not clap's 2) and that stderr names the path.
- [LOW] L6 → `parse_allowed_host` also refuses `@` and surrounding whitespace; tests added.
- [LOW] L7 → declined: `memory_flush_insights` reports bad metadata into its per-entry `failed` list, not an
  `invalid_params` error, and #1020 removes it; the reason is recorded in Decision 4.
- [LOW] L8 → both residuals (`memory_dump_state` reach, the body-limit vs 50 MB bootstrap cap) named in item 11.
- [LOW] L9 → the trailing-space case is kept unit-only, with the reason, in item 2's matrix.
- [LOW] L10 → kept as Michaël's call; the question now names the one-line skill precondition as the durable option.
- [LOW] L11 → item 9 now covers only stale claims outside the Architecture section item 10 rewrites; it makes no
  commit if none remain.

A delta re-review of the final revision, with its altitude acknowledgement, is appended below as round 2.

---

# Round 2 (delta), plan `042c2fe7884876cbc3e48d6ae917861d94aa6626e1c8dc42c7407018c69f8983`

**Round-2 delta review: #1018 plan (third revision)**

Altitude report SHA-256: f08abd7a4631b09c914e8a6ad5578b82e61f29b0e82238b165bc81c4b358d2a8

**Verdict: LGTM, with four LOW cleanups that block nothing.** Every round-1 finding is resolved or declined for a stated reason, and the Codex/agy changes check out against the source. I found no new BLOCKER, HIGH or MEDIUM.

### Altitude report (assessed first)
- **Digests match.**
  - The plan's SHA-256 is `042c2fe7…8f983`, as the report's header says.
  - `issue-1018.json` (`97eb531f…`) and the redacted `draft-context.md` (`1d5fc126…`) match their sources.
  - `tower.rs` (`1664c5bf…`) and `service.rs` (`87af5ef0…`) match the copies in the cargo registry.
- **Measurements are right:** 400 lines, 34,487 bytes, 12 items, and the fenced block at lines 367–382.
- **The revision history is accurate.** Predecessors are `4006dc1c…` (what I reviewed) and `f634253c…`. No item was dropped or re-scoped, and the lens drafts are out of the repository.
- **I agree with `proceed`.** The growth is tests, one startup check and sharper commands, not copied product code.

### Round-1 findings
- **[BLOCKER] leak guard: resolved.** Verification now uses generic shapes only. I ran the private pattern file over the plan, every file in `docs/plans/` and the redacted brief, with zero hits. The brief no longer contains the port.
- **[MEDIUM] item 4's command filter: resolved.** `--test http_shutdown` now runs without a filter, and the flush-alone step has its own command.
- **[MEDIUM] two `Authorization` headers: resolved.** The matrix now sends both copies holding the valid token, plus a laptop and desktop pair.
- **[MEDIUM] item 4's "drop the fence" fallback: resolved.** It now says to fix the test and keep the fence, and gives the structural reason.
- **L1, holding the write guard: accepted as a residual.** The outcome is fine, but the stated reason is wrong; see N3.
- **L2–L6, L8–L10: resolved.** Each is applied as the Resolution section describes.
- **L7, the shared metadata helper: declined.** The reason holds: `flush_insights` reports into a `failed` list, and #1020 removes it.
- **L11, items 9 and 10 overlapping: resolved.** It is now moot; see N4.

### Delta since the second revision: verified
- **Item 3's unit test of `calling_machine(&Extensions)`.** The premise is true: `Peer::new` is `pub(crate)` (`service.rs:574`), so `call_tool` cannot be driven outside rmcp. `rmcp::model::Extensions::new` and `insert` are public (`extension.rs:52,70`). `pub(crate) TokenTable::parse` plus `authenticate` builds a real `Machine` without a test-only constructor.
- **Item 5's red.** Before item 5, clap rejects the unknown `--allowed-host` flag with exit 2. The env-var half goes red too, because the unknown variable is ignored and the Host check returns 403. The `'*'` case goes red because the server starts instead of exiting 2.
- **The in-process Host tests moved to item 2.** That is consistent: `serve` appends `allowed_hosts` from item 2 on.
- **Item 7's second commit.** The "Build the embedding provider…" paragraph does sit above `KNOWN_PROVIDERS` (`main.rs:123-129`).

### New findings
- **[LOW] N1: the generic leak guard has false positives.**
  - `docs/plans/2026-11-16-mcp-http-bearer-subagent-review.md` lines 38 and 101 contain the CGNAT range in dotted form, which the guard's `100\.(6[4-9]…)\.` pattern matches. If the review or external-review files are committed, the guard fails on them. Keep them off the branch or reword.
  - Item 1's ADR vector test and the "line holding a raw token" test would also trip `me_[0-9A-Za-z]{32}` if written as literals. Build them with `"0".repeat(32)` or the run-time generator; Constraints line 34 already implies this.
- **[LOW] N2: the Testing section is stale against the delta.**
  - The unit list leaves out `src/server.rs` `calling_machine` (item 3).
  - The acceptance map says `allowed_hosts → 5`; it should now say `→ 2, 5`.
  - "Never log text, except the line number" now contradicts the new stderr assertions (path, directory, value). Reword it to "stderr names the offending input in our own message".
  - For item 5's `'*'` case, assert on our parser's own message, not just exit 2 plus a `*` somewhere in clap's output.
- **[LOW] N3: L1's rationale is wrong; the residual itself is fine.**
  - Holding the guard needs no signature change: a `std::mem::forget(guard)` after the flush in `serve`'s tail keeps it held until the process exits.
  - The residual is still safe for every write reachable over MCP. Edge updates only set `t_expired`, which changes the active count, so the fingerprint changes and the next open rebuilds.
  - Either correct the reason or take the one-line fix; it is the owner's call.
- **[LOW] N4: item 9 is provably empty.** Every stale claim in `mcp-server.md` (lines 261, 262 and 270) sits inside § Architecture (line 256), which item 10 rewrites. Delete item 9 rather than carry a conditional no-op.

## Resolution (round 2)

Applied to plan revision `611a7deb309d1f0d722cba35bb23920187f2d0c975792994c5c7646e53261f74`:

- [LOW] N1 → the generic leak guard skips the review artifacts (`-review.md`, `-external-`), which quote the guard's
  own range; the private guard still covers them, and `/finish-pr` removes `docs/plans/` before merge. Testing now
  requires every token-shaped test value to be built at run time.
- [LOW] N2 → Testing lists `calling_machine`, maps `allowed_hosts` to items 2 and 5, and says stderr assertions
  check only our own messages; item 5's `*` case asserts `parse_allowed_host`'s own message.
- [LOW] N3 → taken: `serve`'s tail `std::mem::forget`s the write guard after the flush, with one comment why; the
  residual is gone.
- [LOW] N4 → item 9 deleted; its claims go with the docs rewrite (now item 9), whose commit body names them.

---

# Round 3 (delta), plan `611a7deb309d1f0d722cba35bb23920187f2d0c975792994c5c7646e53261f74` (the final revision)

**Round-3 delta review: #1018 plan (fourth revision)**

Altitude report SHA-256: 52a3efe3d3a5c61e8638f35bcc31aa9b1088a1466f4c96889f97b020992084a7

**Verdict: LGTM.** All four round-2 findings (N1–N4) are resolved. No new BLOCKER, HIGH or MEDIUM; one optional LOW wording fix.

### Altitude report (assessed first)
- **Plan digest matches:** the file hashes to `611a7deb…61f74`, as the report's header says.
- **Measurements match the file:** 404 lines, 34,795 bytes, 11 items (0–10) and the fenced block at lines 369–384.
- **The revision note is accurate.** The third candidate was `042c2fe7…`. This delta folds the old item 9 into the docs item, adds the `mem::forget`, and updates the Testing section and the leak guard.
- **The rest still holds.** "Three boy-scout fixes" is still true (item 7's fix and its doc commit, plus item 8). I agree with `proceed`.

### N1–N4
- **N1, leak-guard false positives: resolved.**
  - I reran the generic patterns over `docs/plans/`. The only hits are lines 38 and 101 of `-subagent-review.md`. The new `grep -v -e '-review\.md$' -e '-external-'` filter excludes that file, and I tested the filter against sample paths.
  - The new `-advisor.md` file is not excluded, but it has no hits.
  - The private patterns give zero hits across `docs/plans/`.
  - The Testing section now requires every token-shaped value to be built at run time (`format!("me_{}", "0".repeat(32))`, `token(machine)`).
- **N2, stale Testing section: resolved.** The unit list now includes `calling_machine`. The acceptance map reads `allowed_hosts → 2, 5`. stderr assertions are limited to our own messages, and item 5's `*` case asserts `parse_allowed_host`'s own text.
- **N3, holding the write guard: resolved.** The guard is now `std::mem::forget`-ed in `serve`'s tail, so the signature is unchanged and the residual is gone.
  - It is safe in the harness: each `TestServer` has its own `in_flight`. No test calls a tool after `stop()`. The abandoned-call test's call has already finished before the writer takes the lock.
  - In the binary, any call still waiting on the lock is dropped when the runtime shuts down after `main` returns.
  - clippy's `mem_forget` lint is in the restriction group, which the crate does not enable.
- **N4, the empty item 9: resolved.** The old item 9 is deleted and the docs item (now 9) takes its claims. The items are renumbered consistently: the only cross-reference left is "item 9 adds it" in Questions, and no stale "item 10" or "item 11" remains.

### New finding
- **[LOW] Verification, the private-guard sentence.** It runs "over the same files and messages", right after the *filtered* file list. The parenthetical says the private guard still covers the review artifacts. Say "over the unfiltered `git diff --name-only origin/main` list" so nobody reuses the filtered one.

## Resolution (round 3)

- [LOW] private-guard wording → arbitrated without a plan change (exit condition: the round raised no new
  load-bearing finding, and Codex and agy returned LGTM on the same bytes). The worker runs the private patterns over
  the unfiltered `git diff --name-only origin/main` list, the commit messages and the PR body, and the PR's
  verification evidence shows that exact command; the plan's parenthetical already states that the private guard
  covers the review artifacts.
