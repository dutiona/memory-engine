# Clean-slate subagent review

Altitude report SHA-256: 2f2df9c27a42c3c0d788389e42688fd37b966a91105fac49ee8670ab2095cdda

Reviewed: `.review/plan.md` (SHA-256 `376f2795…bbd7c`, the same bytes as the worktree's
`docs/plans/2026-11-16-1019-plugin-outbox.md`), against ADR-0020 §3, §4, §5, §10, §11, §12, the issue body,
`planning-context.md` and `cc-docs/`. The input hashes match the ones the altitude report lists.

**Verdict: approve once the five MEDIUM findings are resolved. There is no BLOCKER and no HIGH.** The design
(D1–D9) is sound. The lock protocol, the seq reservation, the second look and the encoding hold up under the
races I traced. Every acceptance criterion and every §12 client rule maps to an item and a test. The MEDIUM
findings are about how credentials and proxies are handled, a platform behaviour the plan does not mention
(`bin/`), an install path that no gate exercises, and one wrong fallback.

## 1. Altitude and duplication (assessed first)

I agree with the altitude report's **proceed**. The plan describes about 1,500 lines of code in 502 lines,
mostly as rules. It reproduces no canonical file, validator or transcript. These overrides are justified, and
I accept each one explicitly:

- **Constraints restates ADR-0020 verbatim.** Justified: the text is binding, quoted rather than paraphrased,
  and it is the reference the tests are checked against.
- **D1–D9 inline shell idioms** (`${s#"${s%??????}"}`, `r=${s#?}; c=${s%"$r"}`, the exact curl line). These
  carry real weight: each one encodes a portability fact (bash 3.2's signed-char `printf`, forking nothing on
  the hot path) that an implementer cannot infer.
- **The Testing table repeats the case names from items 2–9.** Justified: it is the mapping from the issue's
  bullets to the cases.
- **The synthesis rationale** is a session record, but it records rejected alternatives (the `-nt` check, PATH
  in `endpoint`). That stops them from being reopened. It can stay.

No duplication finding.

## 2. Mapping check

- **Acceptance criteria:**
  - validate and marketplace: item 1, G1.
  - Utilities, `LC_ALL=C`, token path, no argv: D3, the `rbin` PATH, `token_channel`, `encoding` on both shells.
  - userConfig, exec form and `endpoint`: D1, D2, D5.8, D6.
  - Synchronous hooks: `registration`, the real run. Budget: item 9.
  - The twelve bullets: the Testing table.
  - Stable copy: item 7.
  - dash and macOS: G2, G3.
  - Real run: items 3 and 7.
- **ADR §12 rules:**
  - Utilities and outbox modes: `modes`.
  - The token is read at send time and never reaches argv: `token_channel`.
  - Seq: time after body, `mkdir` from 0, bounded, ID shape, one `date` call: `envelope`, `seq_taken`,
    `seq_bound`.
  - Capture is synchronous, silent, exits 0, has a 5 s timeout: `registration`, `silent`.
  - One flusher, renewal, breaking rules: item 6.
  - Order, park and continue, retry or hold stops: `status_classes`.
  - No lost wakeup: `second_look`, `no_second_look`.
  - Transfer limits: `token_channel` argv.
  - Detached redirect: `detached`.
  - Watcher: `watcher_env_i`.
- **§3, §4, §10, §11:** all covered (`userinfo`, `dev_root`, `newline`, `status_classes`, `redirect`,
  `session_start_*`, `registration`).

Two gaps sit inside these mappings: L7 (the issue-versus-ADR lock semantics are not listed as a resolution) and
M5 (no unborn-HEAD case).

## 3. Findings

### [MEDIUM] M1 — Item 3 / Q1: the Claude OAuth token *is* in an argv

Item 3 runs every call as `env -i HOME="$H" … CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" … claude …`.
That assignment is an argument of the external `env` program. It can be read in `/proc/<pid>/cmdline` and
`ps` until `env` execs `claude`, and it appears on every `claude plugin marketplace add`, `plugin install` and
`-p` call. So the plan's claim that "the token is in no argv" is false, and the token is a long-lived
`setup-token` credential.

Fix: build the environment without putting the token on a command line.

- One way is `python3 -I -c 'import os,sys; os.execve(sys.argv[1], sys.argv[1:], {...,"CLAUDE_CODE_OAUTH_TOKEN": os.environ["CLAUDE_CODE_OAUTH_TOKEN"]})' "$claude" …`.
  `python3` is already a prerequisite.
- Another is `env -i HOME=… PATH=… /bin/sh -c 'IFS= read -r CLAUDE_CODE_OAUTH_TOKEN < "$1"; export CLAUDE_CODE_OAUTH_TOKEN; shift; exec claude "$@"' sh <0600 file> …`,
  which uses Q1's file alternative.

Add a self-check that `real-claude.sh` contains no `CLAUDE_CODE_OAUTH_TOKEN=` on a command line.

### [MEDIUM] M2 — D8: proxy variables are undecided, and they reach the Bearer token

The curl line honours `http_proxy`, `https_proxy`, `ALL_PROXY` and `NO_PROXY` from the hook's environment. The
watcher's `env -i` flush has none of them. Two consequences follow:

- With an `http://` `me_url` (plausible on a tailnet) and a proxy set (plausible on the WSL laptop on an
  institutional network), the `Authorization: Bearer` line goes to the proxy in cleartext.
- The two flush paths behave differently: hooks go through the proxy and the watcher goes direct.

`-q` only removes `~/.curlrc`; it does nothing about environment proxies. Decide this now.

- Recommended: add `--noproxy '*'`. The server is reached directly, both paths behave the same, and no
  intermediary ever sees the token.
- Otherwise, document that proxies are honoured, and why.

Test it: `token_channel` asserts `--noproxy` in argv, and a `real_curl` variant with
`http_proxy=http://127.0.0.1:<second stub>` asserts that the second stub receives nothing.

### [MEDIUM] M3 — Items 3 and 10: no gate exercises the install path real users get

`real-claude.sh` installs from `claude plugin marketplace add <repository root>`, which is a local-path
marketplace. `plugins_loading.md` says such a plugin "loads in place from its path inside the marketplace
folder", with `CLAUDE_PLUGIN_ROOT` pointing at the source directory. Every real install uses the README's
`claude plugin marketplace add dutiona/memory-engine` instead. That clones the repository and copies the plugin
to `cache/memory-engine/memory-engine/0.1.0/`, with only committed files.

So no gate runs the README's first command, and item 10's own check ("every command the README shows is one a
gate runs") cannot pass as written.

Fix: once the branch is pushed (the repository is public), add a G4 phase, or a recorded pre-merge step, in a
fresh scratch HOME:

- `claude plugin marketplace add dutiona/memory-engine#feat/1019-plugin-outbox`, then `install`.
- Assert that a capture lands and that the stable copy is `cmp`-equal to the cached `bin/me-hook`.

Otherwise, weaken item 10's check and say so in the PR.

### [MEDIUM] M4 — D2 / item 1: `bin/` is a Claude Code component directory, and the plan never mentions it

`plugins_manifest-reference.md` (Standard layout) says:

> Files here are on the Bash tool's `PATH` while the plugin is enabled, so Claude runs them as bare commands.
> claude.ai and Cowork don't install a plugin that has this directory.

The issue fixes the path as `bin/me-hook`, so the location is given. Its consequences still need a decision:

- Commit `bin/me-hook` as mode 100644. With no exec bit, the shell's PATH lookup skips it, so the model cannot
  run `me-hook` as a bare command (for example, inject fake events).
- Assert that mode in the `registration` or `version` case (`git ls-files -s`).
- State in the README that the plugin cannot be distributed through claude.ai or Cowork.
- Check that G1 `--strict` does not warn about a non-executable file in `bin/`. If it does, that is a choice to
  bring to Michaël: move the helper to `scripts/`, which departs from the issue's wording.

### [MEDIUM] M5 — Item 9: the "if it bites" fallback reintroduces a bug D7 rejects

The fallback is to fold the branch in with `--abbrev-ref HEAD`. Three problems:

- It abbreviates like `symbolic-ref --short`, which prints `heads/x` when a tag shares the name. D7 avoids
  exactly that.
- It prints a literal `HEAD` when detached, which would need its own handling.
- On an unborn HEAD (a fresh `git init`) it very likely fails the whole `rev-parse` with exit 128. D7's rule
  "Failure: Root is D" would then drop the common-dir root, and linked worktrees would stop sharing it.

Fix:

- Keep `symbolic-ref`. If a fork must go, choose another one or measure first.
- Or specify `--symbolic-full-name HEAD`, parse the printed lines whatever the exit status, and accept that an
  unborn HEAD has no branch.
- Either way, add a `facts_unborn` case to item 4 (Root and Branch for a repository with no commits). New
  projects start that way, and nothing covers it today.

### [LOW] L1 — D3: `PATH=${PATH:-/usr/bin:/bin}` never takes effect

dash and bash both set a compiled-in PATH when none is inherited. Probed here: `env -i /bin/sh -c 'echo $PATH'`
and the same with bash both print `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`. So under the
watcher's `env -i`, curl is found through the shell's default, not `/usr/bin:/bin`. Upstream bash's default
ends in `.`, and macOS bash 3.2's default is unverified.

Probe `ssh macos-mini "env -i /bin/sh -c 'echo \$PATH'"`. Then either delete the line and state that the plan
relies on the shell default, or set `PATH=/usr/bin:/bin` unconditionally when the D6 `$0` rule matches (the
watcher path). `watcher_env_i` already uses real curl.

### [LOW] L2 — D5.1: one sentinel directory gates `mkdir -p`

`mkdir -p` runs only when `O/rejected` is missing. If `O/tmp`, `O/seq` or `S/bin` is deleted while `O/rejected`
remains (a user "cleaning tmp"), every capture fails silently and permanently, or the watcher's copy stops
updating. Fix: when `mkdir O/tmp/$$`, the seq reservation or the copy fails, run the `mkdir -p` once and retry.
It costs nothing on the hot path.

### [LOW] L3 — D8: curl globbing

curl globs `[]{}` in a URL unless given `-g`. An IPv6-literal `me_url` (`http://[fd7a:…]:port`) passes D1's
validation, then fails every request: curl exits non-zero, the code is `000`, so it retries for ever. Add `-g`
(`--globoff`) to the request line and to `token_channel`'s argv assertions.

### [LOW] L4 — D5.2: git calls inherit the unread hook stdin

The facts are derived before `cat` reads the body. No git subcommand used reads stdin today, but if one did, the
body would be truncated with no sign. `</dev/null` on each `git -C` call costs nothing and removes that risk.

### [LOW] L5 — Item 3 and Verification: isolation leaks

- `PATH="<dir of claude>:/usr/bin:/bin"` puts Michaël's real `~/.local/bin`, if that is where `claude` lives,
  ahead of `/usr/bin` for every hook and git call. Symlink `claude` into a scratch `bin` instead.
- The root from `mktemp -d` must not contain a space: `dev_root=$H/dev` would fail D1, unlike `run.sh`'s
  deliberate space.
- Failure diagnostics must never print queued bodies, because those are raw hook payloads in PR output.
- Verification's `ls -ld ~/.local/state/memory-engine` "absent" check fails spuriously if that directory
  legitimately exists, and checks nothing about `~/.claude`. Record its state before and after and compare
  them, and add a read-only real-HOME `claude plugin list` that shows no `memory-engine`.

### [LOW] L6 — Item 3: Stop versus StopFailure

A transient API error (rate limit, overload) during `claude -p` produces `StopFailure`, not `Stop`. That trips
"stop the work and go to Michaël" as if R1 had bitten. The case should report which of the two was queued.
Either one proves capture across teardown.

### [LOW] L7 — Constraints: the list of ADR-over-issue resolutions is incomplete

The issue says "a killed sender's stale lock is broken after a fixed age, and a live sender's lock is never
broken". The ADR, which the plan and tests follow (`lock_dead`, `lock_age`), breaks a dead pid's lock at once
and a live pid's lock after one hour. Add this to the resolution list and to the PR.

Also, D8 breaks a lock whose pid is readable and alive but whose stamp is malformed, while §12 says "only …
when neither can be read". State the justification next to the rule: a lock is published whole, so a malformed
record has no live writer of this protocol.

### [LOW] L8 — Documentation: GEMINI.md is left out

`GEMINI.md` has Commands and Project Structure sections parallel to the two files the plan updates (`AGENTS.md`,
`CLAUDE.md`). Either add the plugin line there too or say why not.

### [LOW] L9 — Over-engineering in the test doubles

- `fake-curl`'s `${0%/*}/../curl.d` fallback ("so it works under `env -i`") can never be reached. Under `env -i`
  the helper's PATH is the shell default, so fake-curl is never found, and every `env -i` case uses real curl.
  Drop it.
- `waves` adds little over `simultaneous` plus `second_look`. Keeping it is optional.

### [LOW] L10 — Hygiene checks are narrower than the public-repository rule

- Add a `gitleaks` scan of the branch. The repository ships `.gitleaks.toml` "for CI and pre-flight", and
  userinfo fixtures (`user:secret@`, `ci-user:not-a-secret@`) are exactly what scanners flag.
- Add a grep for tailnet CGNAT addresses: `100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.`.
- Write the committed AGENTS.md/CLAUDE.md gate block as "a macOS host over ssh", not `ssh macos-mini`.

### [LOW] L11 — D7: the root in some layouts

In a bare repository with worktrees, or with `--separate-git-dir`, the common dir does not end in `/.git`. Each
linked worktree then gets its own top level, which contradicts §3's "so that linked worktrees share it". This
only matters for projects with no remote, which get a path key. Accept it explicitly in the risk table, or use
the first entry of `git worktree list --porcelain` (one more fork).

### [LOW] L12 — D8 prune: a backward clock jump

Seq reservations are pruned after 3,600 s. A backward clock step of more than an hour (WSL resyncing after
sleep is a known pattern) can then hand out the same `(time, seq)` twice. §5's per-time uniqueness would rest
on the pid alone (the IDs still differ). Record this as an accepted residual in the risk table.

### [LOW] L13 — Sequencing clarity

- Q1 blocks item 3's green. "Each item's gates green before its commit" then stalls items 4–10. Say whether
  items 4–6 may proceed while item 3's commit waits for the credential.
- Item 2 must make `session-start` behave as `capture`, because item 3 expects SessionStart to be queued. D5
  implies this, but item 2 does not say it.
- The `run.sh` harness says each case runs with `PATH=$T/rbin`. The case's own assertions need `cmp`, `find`,
  `python3`, `pgrep` and `sleep`, so the restricted PATH must apply only to the helper's process tree.

## 4. Structural completeness

The Documentation, Testing and Verification sections are all present and concrete. Open questions are
labelled; Q1 is correctly marked BLOCKING with a default that keeps Michaël's credentials file untouched.

## 5. Feasibility

The order is achievable. Each item builds only on earlier ones: the stub first appears in item 3, fake-curl in
item 5, the lock in full in item 6, the stable copy in item 7. Putting the real run right after enqueue
(item 3) is the right risk order. The only scheduling dependency is Q1 (see L13).

## Counts

BLOCKER 0 · HIGH 0 · MEDIUM 5 · LOW 13

## Resolution

Applied to plan revision 2 (SHA-256 `ac2e10b72a66f56365d2f5ba11b0e24ac071b67b3ea6783e45da393cbe50c8f8`), severity mode Default (all findings addressed):

- [MEDIUM] M1 token in `env`'s argv → item 3 builds the `claude` environment with `python3 -I -c 'os.execve(...)'`, copying the token environment-to-environment; a `real_no_token_argv` case greps the script for an assignment. (Codex raised the same as HIGH.)
- [MEDIUM] M2 proxies → D8 adds `--noproxy '*'`; `token_channel` asserts it; new `proxy_bypassed` case; Q5 records the trade-off.
- [MEDIUM] M3 GitHub install path unexercised → new G6: `real-claude.sh --marketplace dutiona/memory-engine#feat/1019-plugin-outbox` once pushed, asserting capture, delivery and the stable copy equal to the cached helper; risk R13.
- [MEDIUM] M4 `bin/` semantics → D2: `bin/me-hook` committed 100644, asserted by the `version` case; README states claude.ai/Cowork cannot install it; a `--strict` warning goes to Michaël; Q6 offers `scripts/`.
- [MEDIUM] M5 wrong latency fallback → item 9 no longer tunes; it stops with per-step numbers (`--steps`). New `facts_unborn` case in item 4.
- [LOW] L1 PATH default never applies → D3: PATH inherited, set to `/usr/bin:/bin` unconditionally under the stable-copy `$0` rule; macOS bash 3.2's `env -i` default (ends in `.`) probed and recorded.
- [LOW] L2 sentinel `mkdir -p` → D5 step 1: `mkdir -p` once and retry when `mkdir O/tmp/$$` fails; `tree_heals` case.
- [LOW] L3 curl globbing → `-g` in D8 and in `token_channel`.
- [LOW] L4 git stdin → every git call `</dev/null` (D7).
- [LOW] L5 isolation leaks → `claude` symlinked into a scratch bin; root without a space; failure output never prints bodies; Verification compares before/after records of the real state root and `installed_plugins.json` instead of asserting absence.
- [LOW] L6 Stop vs StopFailure → item 3 accepts either and reports which fired.
- [LOW] L7 incomplete resolutions → Constraints adds the lock-semantics resolution; D8 justifies breaking an unreadable record with a live pid.
- [LOW] L8 GEMINI.md → added to item 10 and Documentation.
- [LOW] L9 over-engineering → fake-curl's `env -i` fallback dropped; `waves` dropped.
- [LOW] L10 hygiene → grep extended to CGNAT addresses and the host name (outside `docs/plans`); committed docs say "a macOS host over ssh"; gitleaks is not installed here (checked), so the grep and a fixture read stand in, stated in Verification.
- [LOW] L11 bare-repository root → accepted residual stated in D7.
- [LOW] L12 backward clock jump → accepted residual stated in D8 prune and risk R12 (a 409 parks: kept, not lost).
- [LOW] L13 sequencing → item 3 states items 4–6 may land while the credential is pending; item 2 states `session-start` runs the capture path; the harness restricts PATH to the helper's process tree only.

## Re-review of revision 6

Altitude report SHA-256: f78c6a36ce009d63588f2f0d58f90ec42df261656bfd21a1304a6315c0dab78e

Reviewed plan revision 6 (SHA-256 `2c708536…d08`, the same bytes as the worktree copy), the altitude report above,
the advisor file's round summaries and this file's `## Resolution`. Same read-only rules as before; the only probe
run was a one-line `mv` behaviour check in a scratch directory (see N2).

**Verdict: not LGTM yet.** Two new MEDIUM findings, each fixed by one sentence of the plan (N1, N2). With those
fixed, LGTM. All 18 original findings are resolved.

### Altitude (assessed first)

I agree with **proceed**. The plan grew from 502 to 635 lines (`wc -l`), and all of the growth answers review
findings: the D8 lock rules, the D5 heal and collision rules, the D7 Root cases, the new test cases, and Q5–Q8. It
still reproduces no file; the new text is contract-level rules plus the cases that drive them. Q7 and Q8 are
justified escalations: both are tensions inside ADR-0020 that a client design cannot settle without amending the
binding contract.

Small inaccuracies in the report (they do not change its disposition):

- It says "+128 lines"; `wc -l` gives 502 → 635, which is +133.
- Its target-file list still names only three edited docs. Revision 6 also edits `GEMINI.md`, and, per Q7/Q8,
  `docs/design/adr/0020-harness-neutral-lifecycle-api.md` in its own commit.

### Disposition of the original findings

| Finding | Disposition | Note |
|---|---|---|
| M1 token in `env`'s argv | resolved | `os.execve` with an environment built from nothing; the `real_no_token_argv` grep finds no `CLAUDE_CODE_OAUTH_TOKEN=` (the Python dict literal contains none) |
| M2 proxies | resolved | `--noproxy '*'` in D8, `token_channel`, `proxy_bypassed`, Q5 |
| M3 GitHub install path | resolved | G6 and R13; see N4 for the script option G6 needs |
| M4 `bin/` semantics | resolved | 100644, checked in Verification with `git ls-files -s`, README note, Q6; wording nit in N5 |
| M5 latency fallback | resolved | no tuning, `--steps`, `facts_unborn`; R9's row is stale (N5) |
| L1 PATH default | resolved | fixed to `/usr/bin:/bin` under the stable-copy `$0` rule; the macOS default (ends in `.`) is recorded |
| L2 sentinel `mkdir -p` | resolved | the heal rule plus `tree_heals`; but see N2 for the park step |
| L3 curl globbing | resolved | `-g` |
| L4 git stdin | resolved | `</dev/null` on every git call |
| L5 isolation | resolved | scratch `bin`, root without a space, no bodies in output, before/after records |
| L6 Stop/StopFailure | resolved | either one accepted, the one that fired is reported |
| L7 ADR-over-issue lock resolution | resolved | Constraints and D8 |
| L8 GEMINI.md | resolved | |
| L9 test-double extras | resolved | fallback and `waves` dropped |
| L10 hygiene | resolved | grep extended; `gitleaks` is unavailable, and that substitution is stated explicitly, not silent |
| L11 D7 root | resolved, as an owner decision | three-case rule plus Q8; refinement in N3 |
| L12 clock jump | resolved, as an owner decision | Q7. Its option (a) consequences (409 park, or a `duplicate` that drops a distinct identical-body second occurrence) are stated accurately |
| L13 sequencing | resolved | items 4–6 may land while Q1 is pending; `session-start` runs the capture path from item 2; the restricted PATH covers the helper's process tree only |

### Later changes checked

- **D5 reorder.** Recording the endpoint before publication closes the first-use window Codex found (residual in
  N6). It also pulled the stable copy ahead of publication (N1).
- **Heal rule.** Correct for every `mkdir` and for any `mv` whose target path names the final entry. Keeping it off
  the `cat` step is right. It never fires for the park step as D8 writes it (N2).
- **ID collision (D5 step 7).** Sound. "No live process can create `O/<id>`" holds, because the ID ends in this
  hook's own pid. `id_collision` is feasible if the test spawns the helper through `sh -c '…; exec /bin/sh helper …'`
  so that it knows the pid in advance. One edge remains (N7).
- **D8 lock (immutable `owner`, per-flusher `<pid>.stamp`, break by rename).** I traced acquire against an existing
  lock, break against a concurrent re-acquire (`lock_steal`), renewal against replacement (`renew_race`) and release
  against a fresh lock (`release_race`):
  - At any instant at most one flusher passes a check.
  - A displaced stamp is never read as another flusher's.
  - Exactly one breaker's rename succeeds.
  - The stated residual (at most one duplicate request per interleaving, nothing lost) is correct.
  - Stamp files of displaced flushers go away with the next `rm -rf O/lock`.
  - No defect found.
- **D7 Root rule.** Correct for the ordinary clone, linked worktrees, submodules and the main tree of a
  `--separate-git-dir` repository. It also gives the popular bare-repository-in-`.git` layout a shared value (the
  container directory). One ordering refinement (N3).
- **Q7/Q8 wording.** Accurate to ADR-0020 §3, §5, §7 and §12, with options and consequences stated by their effect.
  Their placement is right: decided at approval, with the ADR line in its own `docs(adr)` commit.

### New findings

#### [MEDIUM] N1 — D5: a failure in a side duty now drops the capture

Revision 2 moved two steps ahead of publication: step 2 (record the endpoint) and step 3 (publish the stable copy).
D5's preamble says "every failure removes `O/tmp/$$` and exits 0". So a persistent failure in either side duty
(`S/bin` read-only, `S/bin/me-hook` or `O/endpoint` turned into a directory, a failed `cat "$0"`) now abandons every
event, silently, even after the heal rule. Before the reorder, both steps came after the `mv` and could not cost an
event.

Fix: state that steps 2 and 3 are best effort (on failure after the heal, continue to step 4). Alternatively, move
step 3 back after step 8: only the endpoint has to precede publication. Add a case: `S/bin/me-hook` is a directory,
and a capture still queues.

#### [MEDIUM] N2 — D8: the park command turns a missing `rejected/` into the event itself

D8 writes the park as `mv "$ev" "$O/rejected/"`. When `O/rejected` has been deleted, that is not a failure. rename(2)
accepts a trailing slash on a missing target when the source is a directory, so the event directory *becomes*
`O/rejected`. Probed here with GNU coreutils: `mv O/ev1 O/rejected/` → exit 0, leaving `O/rejected/body`. BSD `mv`
goes through the same rename(2), so I expect the same there (likely, not probed).

The consequences:

- The heal rule never runs.
- The parked event drops out of `O/rejected/[0-9]*`, so out of #1030's count.
- The README's replay (`mv O/rejected/<id> O/`) cannot name it.
- The next park nests beside its files.

`tree_heals`' wording ("a later park still lands in `rejected/`") could pass on this broken state.

Fix: `mv "$ev" "$O/rejected/${ev##*/}"`, which fails when `rejected/` is missing, so the heal rule applies. Make
`tree_heals` assert `O/rejected/<id>/body` and `headers`.

#### [LOW] N3 — D7: check case 2 before case 1

Take a `--separate-git-dir` repository whose git dir path happens to end in `/.git` (for example
`~/gitdirs/proj/.git`). In its main tree, case 1 sends `~/gitdirs/proj`, which is not a working tree.

Testing case 2 first ("git dir equals the common dir: the top level") fixes this at no cost. An ordinary clone is
unchanged, since its top level equals the parent of `.git`. Case 1 then serves only linked worktrees.

#### [LOW] N4 — G6 needs a script option that no item adds

G6 runs `real-claude.sh --marketplace dutiona/memory-engine#feat/1019-plugin-outbox`. Items 3 and 7 describe
`real-claude.sh` without that option, or without saying where the cached `bin/me-hook` is found for the `cmp`. Name
it in item 7's change: `--marketplace <source>` replaces the local-path `marketplace add`, and the assertion compares
the stable copy with `$H/.claude/plugins/cache/memory-engine/memory-engine/<version>/bin/me-hook`.

#### [LOW] N5 — Stale and contradictory text

- R9's row still says "The fallback of item 9, then Michaël"; item 9 no longer has a fallback.
- D2 says "the `version` case asserts the mode in Verification", but item 1 moved that check out of `run.sh`.
  Reword it as "Verification asserts the mode".
- The altitude-report inaccuracies listed above.

#### [LOW] N6 — The first-use wakeup is narrowed, not closed

Take events queued while `me_url` was unset. A flusher that read "no endpoint" and holds, just before a newly
configured hook writes `O/endpoint`, stops without a second look, while that hook's own flush finds the lock taken.
The new event then waits for the next hook or the watcher. That is a delay, not a loss, and it is consistent with
§12's no-second-look-after-a-hold. State it as accepted next to D5 step 2.

#### [LOW] N7 — The collision check could also cover `rejected/`

D5 step 7 checks only `O/<id>`. Under Q7's scenario, a new event can take the ID of a *parked* one; the README's
replay `mv O/rejected/<id> O/` would then nest the parked event inside the queued one, and that event's 204
`rm -rf` would delete it. Testing `[ -e "$O/rejected/<id>" ]` too is a builtin and free. Alternatively, fold this
into Q7(a)'s stated residual.

### Counts (new findings)

BLOCKER 0 · HIGH 0 · MEDIUM 2 · LOW 5

## Resolution (re-review of revision 6) → revision 7 (SHA-256 `a2aa2b8b9eca156b1a93258862a83af604c67057e0e8d7233a3d046f6fcc0af8`)

- [MEDIUM] N1 side duties drop a capture → the stable copy moved back after publication (D5 step 8) and both side duties are best effort; `side_duties_fail` case.
- [MEDIUM] N2 park with a trailing slash → D8 parks with the full target `"$O/rejected/${ev##*/}"`, which fails when `rejected/` is missing, so the heal rule applies; `tree_heals` asserts `O/rejected/<id>/body` and `headers`.
- [LOW] N3 D7 case order → the git-dir-equals-common-dir case is tested first.
- [LOW] N4 G6's script option → item 7 adds `--marketplace <source>` and the cache glob for the `cmp`.
- [LOW] N5 stale text → R9 row and D2 wording fixed; the altitude report's counts and target-file list corrected.
- [LOW] N6 first-use wakeup narrowed, not closed → stated as an accepted delay at D5 step 2.
- [LOW] N7 collision with a parked ID → D5 step 6 also tests `O/rejected/<id>`; `rejected_collision` case.

## Re-review of revision 7

Altitude report SHA-256: c59ea5fed69e8f4654f93ae38ea88aea0a523ada822fd15020bdb35a05358612

Reviewed plan revision 7 (SHA-256 `a2aa2b8b…0af8`, matching the request) through `.review/plan-delta-r7.diff`
against revision 6, plus the current altitude report. Every D5 step number cross-referenced from items 2, 4, 5 and
7 matches the renumbered steps.

**Verdict: LGTM.** All seven N-findings are resolved. The two new findings are LOW. Neither blocks approval; both
can be fixed while implementing.

The altitude disposition **proceed** still holds: 649 lines, +147 since revision 1, all of it from resolving
findings. The report's line count and its list of target files are now correct. Its phrase "except three docs" is a
leftover and contradicts its own list; that is cosmetic.

### Disposition of N1–N7

| Finding | Disposition | Note |
|---|---|---|
| N1 side duties drop a capture | resolved | The endpoint (step 2) and the stable copy (step 8, now after publication) are best effort; see N8 on the test fixture |
| N2 park with a trailing slash | resolved | `mv "$ev" "$O/rejected/${ev##*/}"` fails with ENOENT when `rejected/` is missing, so the heal rule runs and the park is retried; `tree_heals` asserts `rejected/<id>/body` and `headers` |
| N3 D7 case order | resolved | "git dir equals the common dir" is tested first; Q8's wording now lags (N9) |
| N4 G6's script option | resolved | `--marketplace <source>` plus the cache glob, requiring exactly one match |
| N5 stale text | resolved | R9's row, D2's wording, and the report's counts and file list |
| N6 first-use wakeup | resolved | Stated as an accepted delay at D5 step 2 |
| N7 collision with a parked ID | resolved | D5 step 6 also tests `O/rejected/<id>`; `rejected_collision` case |

### New findings

#### [LOW] N8 — `side_duties_fail` does not exercise a failure

The case replaces `S/bin/me-hook` and `O/endpoint` with directories. `mv` onto an existing directory does not fail,
though; it moves the file inside it. So:

- the stable-copy step "succeeds" as `S/bin/me-hook/<pid>.me-hook`;
- the endpoint step "succeeds" as `O/endpoint/<pid>.endpoint`. Because `read` from a directory fails, the hook
  thinks the endpoint changed and does this on every hook.

The case passes without ever reaching the best-effort path it is meant to prove. In that state the helper also adds
one file per hook to those directories and never prunes them.

Fix: use fixtures that really fail.

- A read-only `S/bin` (the test may `chmod`; the helper may not).
- An `mv` wrapper in `rbin` that fails on a `*.endpoint` source.

Optionally, write both side duties as `mv` onto the full target name, as the park now does. That variant cannot
apply here, because the target *is* the full name, so instead accept the nesting as a state that cannot arise
without manual damage. Say which choice is made.

#### [LOW] N9 — Q8 and D7 case 3 misdescribe two layouts after the reorder

Since N3, a linked worktree whose common dir ends in `/.git` always takes case 2. That covers the popular
bare-worktree pattern (`git clone --bare <url> proj/.git`, then `git worktree add`) and a separate git dir named
`…/.git`.

- **Bare repository named `.git`:** every worktree shares `proj/`, the container directory. That is a reasonable
  value, and it does not match Q8's "the plan sends that worktree's own top level".
- **Separate git dir named `…/.git`:** the main tree sends its own top level (case 1), while its linked worktrees
  send the git dir's parent (case 2), which is not a working tree.

Only bare or separate git dirs *not* named `.git`, and submodules, reach case 3. Amend Q8 and the parenthesis of
case 3 to say this, so that Michaël decides Q8 on the actual behaviour. No code change is needed.

### Counts (new findings)

BLOCKER 0 · HIGH 0 · MEDIUM 0 · LOW 2

## Resolution (re-review of revision 7) → revision 8 (SHA-256 `b73b9a2ceca38acad1438128728dc019168e0360043e013b82d7eae63c044546`)

- [LOW] N8 `side_duties_fail` cannot fail → fixtures that really fail (a read-only `S/bin`; an `mv` wrapper failing on a `*.endpoint` source); the directory-replacement state is named as manual damage the helper accepts as it is.
- [LOW] N9 Q8 and D7 case 3 misdescribe two layouts → D7 case 2 and Q8 now describe the bare repository cloned as `proj/.git` (container shared by all worktrees) and the separate git dir named `.git` (linked worktrees send its parent, the main tree its own top level); case 3's parenthesis names only git dirs not named `.git` and submodules.

## Re-review of revision 8

Altitude report SHA-256: 736380950394fcd62b4996fb1ded5c58fbadb63f421c5f29c2023691bad5db15

Reviewed plan revision 8 (SHA-256 `b73b9a2c…4546`, matching the request) through `.review/plan-delta-r8.diff`
against revision 7. The altitude disposition **proceed** still holds: the delta only clarifies text and adds two
cases.

**Verdict: LGTM.** There are no new findings.

### Disposition

| Finding | Disposition | Note |
|---|---|---|
| N8 `side_duties_fail` did not fail | resolved | Real failures now: a read-only `S/bin` makes the copy's `mv` fail with EACCES, and an `mv` wrapper fails the endpoint's `mv`. The leftover `O/tmp/<pid>.me-hook` is pruned once the pid is dead. A path replaced by a directory nests the file; the plan names this explicitly and accepts it as manual damage |
| N9 Q8 and case 3 wording | resolved | D7 case 2, case 3 and Q8 now give each layout's actual value. `facts_layouts` adds the separate-git-dir-named-`.git` worktree |

### Codex round-7 changes, checked

- **D5 step 2 removes `O/endpoint` when an update fails.** This fails safe: the token and the new event never go to
  the previous URL, and the queue holds instead. It can also delete a good endpoint that a concurrent hook recorded a
  moment earlier. That costs only a delay: the next hook sees "missing, differs from `me_url`" and records it again.
  It stays within the "holds until a later hook records the new one" the plan states. An unset or invalid `me_url`
  never reaches this step, so the old endpoint is kept, consistent with D1. `endpoint_update_fail` covers the path.
- **D7 case 2, git's own view.** This is consistent with git: `git worktree list` derives the main worktree by
  stripping `/.git` from the common dir, so the plan sends what git itself reports first. Fixing that value in a test
  is sound.

### Counts (new findings)

BLOCKER 0 · HIGH 0 · MEDIUM 0 · LOW 0

## Re-review of revision 9

Altitude report SHA-256: 7eb0903638a270322032a78680b3d6238c7b25bf862c6e786d92e64daa632405

Reviewed plan revision 9 (SHA-256 `726e6707…89ce9`, matching the request) through `.review/plan-delta-r9.diff`
against revision 8. The altitude disposition **proceed** still holds: the change is a few lines and one test case.

**Verdict: LGTM.** There is one new LOW finding, about sequencing only. Fix it while implementing.

### Codex round-8 change, checked

- **Re-reading the endpoint before each request** is sound and costs no fork, since the `read` builtin does it. A
  URL withdrawn or changed mid-flush gets at most the one request already past its read. That is the same residual
  as the lock's, stated as such.
- A legitimate reconfiguration mid-flush therefore holds once. The events wait for the next hook or the watcher.
  That is a delay, not a loss, and it is consistent with §12's rule of no second look after a hold.
- **"If the `rm` fails as well, `O` is not writable"** is true except under manual damage. If `O/endpoint` is a
  directory, `rm -f` fails while publication succeeds. Even then `read` fails, so the flush holds, and nothing
  reaches the old URL. No defect.

### New findings

#### [LOW] N10 — Two item-2 cases need item 5's flush and fake curl

`endpoint_update_fail` (since revision 8: "the flush makes no request") and the new `endpoint_changed_mid_flush`
(fake curl's `oncall.1`) are listed under item 2. Item 2 has no flush, no step-10 spawn and no `fake-curl`, so
neither case can go red and then green there, as the TDD sequence requires.

Fix: either move both cases to item 5, or split `endpoint_update_fail` in two. The capture half stays in item 2:
the event is queued and `O/endpoint` is gone. The flush half moves to item 5: no request is made.

### Counts (new findings)

BLOCKER 0 · HIGH 0 · MEDIUM 0 · LOW 1

## Resolution (re-review of revision 9) → revision 10 (SHA-256 `1013f249f1e534125e8142d4c9b29cc44e27ffb427037aa5509d95754942e077`)

- [LOW] N10 two endpoint cases need item 5's flush → `endpoint_update_fail` and `endpoint_changed_mid_flush` moved to item 5.

## Re-review of revision 10

Altitude report SHA-256: e95939ca4f741e00b243ed380723f19efc80f6a73774676248b71efbbbd12bd9

Reviewed plan revision 10 (SHA-256 `1013f249…e077`, matching the request) through `.review/plan-delta-r10.diff`
against revision 9. The altitude disposition **proceed** still holds: this delta moves two cases and adds two
sentences.

**Verdict: LGTM.** There are no new findings.

### Disposition

| Finding | Disposition | Note |
|---|---|---|
| N10 item-2 cases needing the flush | resolved | `endpoint_changed_mid_flush` and `endpoint_update_fail` now sit in item 5, beside the flush and the fake curl they need |

### The accepted residual, checked

While a session started before a change of `me_url` still runs, its hooks record the old URL again, so the
per-machine `O/endpoint` is whichever URL was written last.

- This follows from the issue's own design, a per-machine endpoint file that the watcher can read with no
  arguments, so it is correctly stated rather than designed away.
- Both URLs are ones the owner configured, and both get the same machine token, so no foreign host ever sees the
  token. The `--noproxy` and redirect rules still hold.
- Each flip also trips D8's re-read before a request: a flush running at that moment holds. That costs a delay,
  never an event, consistent with §12.
- The README's remedy, restarting running sessions after changing `me_url`, matches Claude Code's own
  `plugin configure` message, "Restart Claude Code to apply it".

### Counts (new findings)

BLOCKER 0 · HIGH 0 · MEDIUM 0 · LOW 0

## Re-review of revision 11

Altitude report SHA-256: 8d448789593d8542fb5d407011beb187ab6752053c091968bf22d785a645e166

Reviewed plan revision 11 (SHA-256 `25aca0c4…6fad`, matching the request) through `.review/plan-delta-r11.diff`.
The delta only rewords Operational steps 1 and 2: the worktree path becomes "under the repository's `.worktrees/`",
and the plan file's path becomes "under `docs/plans/`". The branch name, the base commit and the "never install
into the real HOME" rule are unchanged, and no design, test or gate is touched. The altitude disposition
**proceed** still holds.

**Verdict: LGTM.** There are no new findings (BLOCKER 0 · HIGH 0 · MEDIUM 0 · LOW 0).
