# Antigravity plan review, all rounds

## Round 1

Michaël,

Here is my review of the implementation plan for `dutiona/memory-engine#1019` (`docs/plans/2026-11-16-1019-plugin-outbox.md`), evaluated against ADR-0020 (§3, §4, §5, §10, §11, §12) and the issue acceptance criteria.

### Altitude and Contract Assessment
- **Altitude & Duplication**: The plan is tightly scoped at 502 lines across 10 implementation items, containing no fenced code blocks or reproduced transcripts. Inline literals are restricted to one-line contract definitions. The altitude checkpoint is verified and sound.
- **Justified Contract Overrides**: Where issue #1019 loosely states "leave only on 2xx" and "park on any other 4xx", the plan explicitly acknowledges and follows the binding contract of ADR-0020 §3: only HTTP 204 deletes an event (a 200 from an intermediary or wrong endpoint holds), while 400, 409, 413, and 422 park; 404, 405, 3xx, and unclassified 4xx hold.
- **Structural Completeness**: The `Documentation`, `Testing`, and `Verification` sections are all present, detailed, and directly map every acceptance criterion and verification gate (G1–G5).

### Technical Analysis
1. **POSIX sh Portability (dash & macOS bash 3.2)**:
   - D7's byte-by-byte percent-encoding under `LC_ALL=C` using `r=${s#?}; c=${s%"$r"}` correctly strips the signed-char 64-bit extension observed on bash 3.2 via `%${h#"${h%??}"}` on `printf %02X "'$c"`. Single-quotes (`0x27`) and ASCII `0x21–0x7E` (excluding `"`, `%`, `\`) remain unencoded per ADR-0020 §3.
   - Glob expansion `"$O"/[0-9]*` under `LC_ALL=C` sorts monotonically in `(epoch, seq, pid)` order-key sequence due to fixed-width 10-digit epoch and 6-digit zero-padded seq fields.
   - Non-zero exit on `read -r` when trailing LF is missing does not trip the shell because `set -e` is explicitly forbidden by D3.
2. **Locking & Lost-Wakeup Protocol (D8)**:
   - Atomic lock publishing via `mv O/tmp/$$.lock O/lock` eliminates race windows where a lock directory could exist without an owner record. If `O/lock` already exists, `mv` places `$$.lock` inside, which is immediately recognized as unheld and purged.
   - Stale lock detection safely validates that the owner record comprises two digit strings before invoking `kill -0 <pid>`, preventing syntax errors.
   - The flush loop adheres strictly to ADR-0020 §12: an empty drain releases the lock and performs a single second-look check to prevent lost wakeups, whereas a retry or hold halts immediately without re-checking.
3. **Security & Token Isolation**:
   - The token is read from `$H/.config/memory-engine/secrets/token` via `read -r`, never exported, never passed in command-line arguments, and never stored in `headers` or `outbox/`. It enters curl's stdin strictly via builtin `printf` through `-H @-`.
   - `umask 077` ensures outbox directories are `0700` and queued files are `0600`.
4. **Curl Invocation & Network Transfer (D8)**:
   - Starts with `-q` to bypass `~/.curlrc`, omits `-L` to reject redirects, and bounds stalls rather than total time (`--connect-timeout 5 --speed-limit 8192 --speed-time 60`).
5. **Test Harness & Real Run Isolation**:
   - `run.sh` enforces the minimal utility whitelist via restricted `PATH=$T/rbin` (symlinks only).
   - `real-claude.sh` isolates `claude -p` inside a scratch `$HOME`, sanitizes environment variables via `env -i`, scrubs all `CLAUDE_CODE_*` variables, and does not touch your personal credentials file.

Altitude report SHA-256: 2f2df9c27a42c3c0d788389e42688fd37b966a91105fac49ee8670ab2095cdda

[Antigravity Review] LGTM

## Round 2

Michaël,

Here is my review of the Round 2 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r2.diff`, from `.review/last-reviewed-plan.md` to `.review/plan.md`), evaluating the changes made to resolve Round 1 findings.

### Altitude and Delta Assessment
- **Altitude Checkpoint**: The plan grew by 79 lines (502 to 581 lines) solely to incorporate the protocol hardening, invariant specifications, and test cases identified in Round 1. It remains concise, literal-free (no fenced blocks), and well under the estimated implementation surface.
- **Delta Scope**: The delta strictly addresses the 16 items identified in the Round 1 synthesis without introducing scope creep or unneeded abstractions.

### Analysis of the Delta Fixes
1. **D8 Lock Protocol Hardening**:
   - Replacing direct `rm -rf O/lock` with an atomic rename `mv O/lock O/tmp/$$.stale` ensures that exactly one contender breaks a given directory. Losers fail cleanly via `ENOENT`, eliminating races against concurrent acquisitions.
   - Deleting unreadable or malformed locks is correctly justified: under D8's protocol, a lock is published whole, so an unreadable record indicates an interrupted or corrupted state rather than a valid live writer.
   - Stating the flusher invariant (verifying `owner` matches `$$` immediately prior to dispatch) alongside accepted residuals (bounded at most to one duplicate request, handled idempotently by ADR-0020 §5) provides clear bounds for test cases `lock_steal` and `release_race`.
2. **D5 Reordering & Resilience**:
   - Moving endpoint recording (step 2) and stable copy publication (step 3) ahead of event publication (step 8) prevents a newly spawned flusher from encountering a queued event before `O/endpoint` exists.
   - Adding `<id>` collision avoidance in step 7 (`[ ! -e "O/<id>" ]`) prevents `mv` from silently nesting the event directory inside an existing collision.
   - `mkdir -p` is deferred to a single retry fallback on failure, keeping the happy path fast while allowing partly deleted trees to heal (`tree_heals`).
   - Redirecting git calls `</dev/null` in D7 guarantees that pre-body git queries cannot consume stdin before `cat > O/tmp/$$/body` executes.
3. **Curl & Token Handling (D8)**:
   - Adding `-g` prevents URL globbing on IPv6 literals.
   - Adding `--noproxy '*'` guarantees that the Bearer token is never routed through an ambient HTTP proxy, ensuring identical network behavior between session hooks and watcher flushes (`env -i`).
   - The token read idiom `tok=; IFS= read -r tok < "$TOKEN" || [ -n "$tok" ]` robustly handles both EOF-terminated and LF-terminated tokens without aborting or leaking variables.
4. **Environment Isolation & Security**:
   - In `real-claude.sh`, constructing the environment in Python via `os.execve` ensures that `CLAUDE_CODE_OAUTH_TOKEN` is passed via `envp` rather than command-line arguments, preventing exposure in process listings (`real_no_token_argv`).
   - Resetting `PATH` unconditionally to `/usr/bin:/bin` under the stable copy closes the vulnerability on macOS where `env -i` leaves a default `PATH` ending in `.`.
   - Committing `bin/me-hook` as mode `100644` prevents it from being exposed as an executable binary on the Bash tool's `PATH`.

The delta fixes are verified, mathematically sound, and introduce no new defects.

Altitude report SHA-256: 8dd03d8e910877c2e36d10fc1c07d64b88428f904300edf4f1fc421fa410d778

[Antigravity Review] LGTM

## Round 3

Michaël,

Here is my review of the Round 3 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r3.diff`, from revision 2 to revision 3 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 3 adds 28 lines (581 to 609 lines), with zero fenced code blocks and no literal expansions. Calibration remains tight and well below the expected implementation surface.
- **Delta Scope**: Focuses strictly on eliminating edge-case races around lock renewal, closing the clock-stepback residual to guarantee globally unique event IDs, generalizing the on-disk heal rule, handling non-standard git worktree layouts, and decoupling git-index assertions from tarball test runs.

### Analysis of Revision 3 Fixes
1. **D8 Lock: Immutable Owner & Per-Flusher Renewal Stamps**:
   - `O/lock/owner` is written once at acquisition and never modified by renewals.
   - Renewals publish to a private, per-flusher file `O/lock/<pid>.stamp` via atomic rename (`mv O/tmp/$$.stamp O/lock/$$.stamp`). Even if another flusher displaces `O/lock` between check and renewal, the stamp lands under the displaced flusher's PID and is never read by the new owner.
   - Staleness evaluates `max(owner_epoch, <owner_pid>.stamp)`. This completely decouples owner verification from renewal writes and eliminates renewal-race hazards (`renew_race`).
2. **D5 Step 6 & D8 Prune: Monotone Epoch & Clock Step-Back Resilience**:
   - The high-water mark `h` is extracted from the last entry of `O/seq/[0-9]*` via `eval "last=\${$#}"` (zero forks).
   - If the system clock steps back below `h`, the hook clamps the epoch to `h` and formats `ME-Event-Time` using `date -u -r "$h"` (BSD) or `date -u -d "@$h"` (GNU).
   - D8 prunes reservations only below `h - 3600` while preserving the newest second's reservations. Consequently, `h` survives arbitrary idle periods, epochs never regress, and `<epoch>-<seq>-<pid>` IDs are strictly unique per machine for ever, fully honoring ADR-0020 §3 without residuals (`clock_back`).
3. **D5 Generalized Directory Healing**:
   - The healing rule (on failure, run one `mkdir -p` of `O/tmp`, `O/seq`, `O/rejected`, and `S/bin`, then retry once) is extended to every step that writes into these directories. If any single directory is deleted out-of-band, the capture heals cleanly at the step requiring it (`tree_heals`).
4. **D7 Worktree Root Resolution for Non-Standard Layouts**:
   - Standard checkouts continue to resolve `Root` with zero extra forks by stripping `/.git` from `--git-common-dir`.
   - In layouts where `--git-common-dir` does not end in `/.git` (`--separate-git-dir`, bare repositories with linked worktrees, submodules), the plan now queries `git worktree list --porcelain` and reads the first line (`worktree <path>`). This ensures that linked worktrees in bare or split layouts consistently resolve to the same root (`facts_separate_gitdir`, `facts_bare_worktrees`).
5. **Decoupling Git Mode Assertion from Non-Git Harness Runs**:
   - Moving the `bin/me-hook` 100644 mode check from `run.sh` to the Verification gate ensures that `run.sh` succeeds when executed on `macos-mini` from an unversioned `tar` archive over ssh, while preserving the strict mode gate before merge.

All Round 3 fixes are verified, robust, and introduce no new defects.

Altitude report SHA-256: 990193f4d846ad3b1e4316dceb2f37abf5f300a8fa614e23ee0c80232eaa130c

[Antigravity Review] LGTM

## Round 4

Michaël,

Here is my review of the Round 4 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r4.diff`, from revision 3 to revision 4 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 4 adds 16 lines (609 to 625 lines), maintaining zero fenced code blocks and keeping all inline literals to one-line contract definitions. Calibration remains well within target limits.
- **Delta Scope**: Tightly focused on three technical refinements: making `(epoch, seq)` reservations strictly monotonic from the high-water glob without octal hazards, simplifying git root derivation to avoid fragile porcelain parsing, and excluding non-replayable stdin streams from the directory heal retry.

### Analysis of Revision 4 Fixes
1. **D5 Step 7: Monotone `(epoch, seq)` Counter & Reservation Robustness**:
   - Reserving `O/seq/<epoch>.<seq6>` with a fixed 6-digit zero-padded sequence suffix guarantees that glob sorting under `LC_ALL=C` strictly matches numeric `(epoch, seq)` ordering.
   - Starting candidate evaluation above the high-water pair `(h, m)` reduces reservation overhead in the standard path to a single `mkdir`.
   - Stripping leading zeros before `$(( m + 1 ))` eliminates octal interpretation bugs across POSIX shells (such as dash reading `000009` as invalid or `000012` as octal 10), tested via `seq_leading_zeros`.
   - Bounding overflow at 999999 by rolling over to `(epoch + 1, 000000)` ensures that a permanently stepped-back clock never drops an event or runs out of sequences (`seq_overflow`).
   - The post-reservation re-read check safely detects if a process stalled for over an hour between glob reading and `mkdir`, dropping and restarting the reservation if it fell below `new_h - 3600` (`frozen_reserve`).
2. **D7: Robust Git Root Derivation Without Porcelain Subprocesses**:
   - Replaces `git worktree list --porcelain` with a direct rule on `git-common-dir`: Root is the common dir minus `/.git` when it ends so, else the common dir itself.
   - This eliminates an unnecessary process fork, bypasses git's quoted-path escaping, and fixes the bug identified on git 2.53 where `--separate-git-dir` layouts incorrectly reported the git directory as the first worktree. All linked worktrees in bare, split, or submodule layouts continue to derive the identical `ME-Project-Root` (`facts_separate_gitdir`, `facts_bare_worktrees`).
3. **D5: Stream-Safe Heal Rule**:
   - Explicitly exempting step 5 (`cat > O/tmp/$$/body`) from the heal retry prevents corrupted or truncated bodies resulting from partially consumed stdin pipes (`body_write_fail`).

All Round 4 fixes are verified, clean, and introduce no new defects.

Altitude report SHA-256: 91d17b9dbfce73436c5649fd12cb4ddf850e1885b8d1eb4c319aa80937f769c8

[Antigravity Review] LGTM

## Round 5

Michaël,

Here is my review of the Round 5 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r5.diff`, from revision 4 to revision 5 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 5 stands at 630 lines (+5 net lines over revision 4). Inline literals remain strictly confined to one-line definitions, with zero fenced blocks. Calibration is verified and sound.
- **Delta Scope**: Reverts revision 4's monotonic counter in favor of adhering strictly to ADR-0020 §12's literal reservation rule, refactors D7 root derivation into three clean hierarchical cases, and cleanly escalates the two underlying ADR-0020 specification tensions to you as Open Questions 7 and 8.

### Assessment of Escalated Decisions (Q7 & Q8)
Both questions are formulated accurately, thoroughly, and with explicit trade-offs and recommendations:

1. **Question 7 (Event-ID Uniqueness vs. Clock Step-Back & Bounded Pruning)**:
   - **Accurate framing**: Highlights the specification tension between ADR-0020 §3 ("unique per machine for ever") and §12 (`mkdir <outbox>/seq/<epoch>.<n>` from $n=0$ upward with 1-hour pruning).
   - **Options & Consequences**:
     - *(a) Recommended*: Follow §12 literally, retain 1-hour pruning, and document the residual in ADR-0020 via a dedicated `docs(adr)` commit. Consequence: re-minting requires an extreme confluence (a backward clock step $> 1$ hour combined with PID reuse at the exact same second and sequence), and the server handles it safely via 409 (park) or duplicate (drop identical).
     - *(b)*: Keep all reservations indefinitely (unbounded directory growth).
     - *(c)*: Client-side monotonic high-water counter (amends §12's time/seq rules).
   - **Status**: The plan cleanly implements (a) by default pending your approval.

2. **Question 8 (`ME-Project-Root` in Disconnected / Non-Standard Layouts)**:
   - **Accurate framing**: Addresses the edge case where Git cannot discover the main working tree from a linked worktree (such as `--separate-git-dir`, bare repositories with worktrees, or submodules, as verified by probe on Git 2.53 where `git worktree list --porcelain` lists the gitdir).
   - **Rule & Recommendation**: D7 case 3 sends the linked worktree's own top level, with the exception documented in ADR-0020 §3 in the same `docs(adr)` commit. For repositories with remotes, `origin`/`upstream` remote keys unify the projects regardless; for path keys, treating isolated linked worktrees as independent roots is safe and deterministic.

### Analysis of Technical Changes in the Delta
- **D7 Hierarchical Root Derivation**:
  1. If `--git-common-dir` ends in `/.git`: strips `/.git` to yield the main working tree (covers standard checkouts and their linked worktrees with zero extra forks).
  2. If `--git-dir` equals `--git-common-dir`: we are in the primary/only working tree of the repository (submodule or main tree of `--separate-git-dir`), returning `--show-toplevel`.
  3. Otherwise: linked worktree of a split/bare layout, returning that worktree's top level per Q8.
  - Verified by test case `facts_layouts` across all three configurations.
- **D5 Step 7 & D8 Prune Reversion**:
  - Reverts cleanly to literal §12 loop (`mkdir O/seq/<epoch>.<n>` starting at $n=0$ with 6-digit zero-padding), bound at $n=1000$.
  - Preserves the collision check on `O/<id>` to prevent directory nesting.
  - Test harness cases (`seq_taken`, `seq_bound`, `prune`, `latency.py`) are properly realigned with the literal §12 loop.
- **Documentation & Commits**:
  - The plan cleanly isolates the necessary ADR-0020 amendment into its own `docs(adr)` commit, preserving atomic commit discipline.

No defects or regressions were found in the delta.

Altitude report SHA-256: c9f63be5c71978f5da18694e7c38825d228635d5fc709131b4cca438a6e5cca2

[Antigravity Review] LGTM

## Round 6

Michaël,

Here is my review of the Round 6 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r6.diff`, from revision 5 to revision 6 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 6 stands at 635 lines (+5 lines over revision 5), maintaining zero fenced code blocks and strictly confined inline literals. Calibration is verified and sound.
- **Delta Scope**: Modifies only the descriptions of Open Questions 7 and 8 to provide full transparency on consequences and design readiness for your decision.

### Review of the Reworded Open Questions
1. **Question 7 (Event-ID Uniqueness vs. Clock Step-Back)**:
   - **Option (a)**: Accurately clarifies that if a re-minted ID is byte-identical to an earlier event, the server acknowledges it as `duplicate` (204) and drops it, meaning that second distinct occurrence and its side-effects (e.g., a second final message or tool call) are not stored.
   - **Option (c)**: Accurately characterizes the client-side monotonic counter as not a ready design—highlighting that it would require another review cycle to resolve edge cases (such as a hook frozen after its post-reservation check, or bounded retry exhaustion)—which reinforces the recommendation for (a).
2. **Question 8 (`ME-Project-Root` in Non-Standard Worktree Layouts)**:
   - **Alternative Analysis**: Accurately clarifies that adopting the common git directory as root would also require changing D7 case 2 (the main tree) to match, and that whenever the git directory resides outside the dev root, ADR-0020 §7 would derive no path key, causing remote-less projects to fall back to the `unscoped` bucket. This makes the consequence of the alternative clear and confirms the recommended approach.

The rewordings are accurate, transparent, and introduce no new defects.

Altitude report SHA-256: f78c6a36ce009d63588f2f0d58f90ec42df261656bfd21a1304a6315c0dab78e

[Antigravity Review] LGTM

## Round 7

Michaël,

Here is my review of the Round 7 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r7.diff`, from revision 6 to revision 7 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 7 stands at 649 lines (+14 net lines over revision 6). It remains strictly scoped with no fenced blocks, no transcript leaks, and confined inline literals.
- **Delta Scope**: Hardens side duties to best-effort so auxiliary file writes never abandon an event, secures parking against Unix `rename(2)` path behavior when `rejected/` is deleted, refines D7 case ordering to avoid separate-gitdir false positives, details the G6 cache comparison, and cleans up residual stale text.

### Analysis of Revision 7 Fixes
1. **D5: Best-Effort Side Duties & Event Prioritization**:
   - Publishing the stable helper copy is moved to step 8 (after event publication in step 7), and both endpoint recording (step 2) and stable copy publication (step 8) are explicitly marked **best effort**. A failure in writing `O/endpoint` or updating `S/bin/me-hook` will not abandon the queued event (`side_duties_fail`).
   - The sequence reservation loop in step 6 now explicitly tests both `[ ! -e "O/<id>" ]` and `[ ! -e "O/rejected/<id>" ]`, preventing an ID clash if an event with that ID is already parked (`rejected_collision`).
2. **D8: Full Target Path on Parking (`$O/rejected/${ev##*/}`)**:
   - Specifying the full destination path prevents Unix `rename(2)` from renaming the event directory itself into `O/rejected` when the directory was deleted out-of-band. The full target name causes `mv` to fail cleanly with `ENOENT`, which activates the heal rule to recreate `O/rejected` and retries the park (`tree_heals`).
3. **D7: Robust Case Ordering for Root Derivation**:
   - Evaluating `git_dir == common_dir` first guarantees that the primary working tree of a `--separate-git-dir` repository resolves to `--show-toplevel` even if the external gitdir path ends in `/.git`. Case 2 only evaluates for linked worktrees (`git_dir != common_dir`).
4. **Test Harness & Hygiene**:
   - `real-claude.sh` adds `--marketplace <source>`, resolving the cached plugin path via `$H/.claude/plugins/cache/memory-engine/memory-engine/*/bin/me-hook` for the G6 verification step.
   - Stale references (R9 fallback text and D2 mode check references) are corrected.

All Round 7 refinements are verified and introduce no defects.

Altitude report SHA-256: c59ea5fed69e8f4654f93ae38ea88aea0a523ada822fd15020bdb35a05358612

[Antigravity Review] LGTM

## Round 8

Michaël,

Here is my review of the Round 8 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r8.diff`, from revision 7 to revision 8 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 8 stands at 664 lines (+15 net lines over revision 7). It contains no fenced code blocks, no transcript material, and retains tightly scoped inline contract literals. Calibration is verified and sound.
- **Delta Scope**: Hardens security against stale-endpoint leakage when an endpoint update fails, fully classifies Git layout behavior for `.git`-named gitdirs in D7 and Q8, and corrects test fixture mechanics to ensure authentic failures.

### Analysis of Revision 8 Fixes
1. **D5 Step 2: Prevention of Stale-Endpoint Delivery**:
   - If writing the updated endpoint fails, D5 step 2 now explicitly removes `O/endpoint` (`rm -f`).
   - Because a missing or unreadable `O/endpoint` acts as an immediate **hold** for `me-hook flush`, this ensures no flush will mistakenly deliver newly captured events or Bearer tokens to the previous URL. Delivery remains held until a subsequent hook successfully writes the new endpoint (`endpoint_update_fail`).
2. **D7 & Q8: Classification of `.git`-Named Gitdirs**:
   - Accurately details the behavior of rarer layouts where a gitdir itself is named `.git`:
     - Bare repository cloned as `proj/.git`: sends the container `proj/`, shared by all linked worktrees (case 2).
     - `--separate-git-dir` repository where the external gitdir is named `.git`: linked worktrees send the gitdir's parent, matching Git's native view where `git worktree list` reports that parent first.
   - Fully enumerates each layout's behavior in Q8 so you have complete visibility when approving the ADR-0020 amendment. Verified in test case `facts_layouts`.
3. **Test Fixture Realism (`side_duties_fail`)**:
   - Replaces the directory-stub fixture with authentic failures (a read-only `S/bin` directory and an `mv` wrapper returning non-zero on `*.endpoint`). This eliminates false passes caused by `mv` nesting a file inside an existing directory rather than failing.

All Round 8 fixes are verified, mathematically sound, and introduce no new defects.

Altitude report SHA-256: 736380950394fcd62b4996fb1ded5c58fbadb63f421c5f29c2023691bad5db15

[Antigravity Review] LGTM

## Round 9

Michaël,

Here is my review of the Round 9 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r9.diff`, from revision 8 to revision 9 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 9 stands at 670 physical lines (+6 lines over revision 8). It contains zero fenced code blocks and maintains strict inline contract literal bounds. Calibration is verified and sound.
- **Delta Scope**: Closes the mid-flush endpoint divergence window by requiring the flusher to re-read `O/endpoint` before every request, and proves it via `endpoint_changed_mid_flush`.

### Analysis of Revision 9 Fixes
1. **D8: Per-Request Endpoint Verification**:
   - Inside the drain loop, the flusher now re-reads `O/endpoint` before each request using the `read` shell builtin (zero subprocess overhead).
   - If `O/endpoint` is absent or differs from the URL the flush started with, the flusher halts immediately with a **hold**, leaving all subsequent events in the queue.
   - This ensures that if `me_url` is reconfigured or withdrawn mid-drain, or if an endpoint update fails and removes `O/endpoint`, the running flusher will not continue sending queued events or Bearer tokens to the stale URL.
   - The check-to-act window matches the existing lock ownership invariant (at most a one-request residual before halting, with no event loss).
2. **D5 Step 2: Unwritable State Root Safety**:
   - D5 step 2 now explicitly notes that if `rm -f O/endpoint` fails during an endpoint update failure, the outbox directory `O` itself is non-writable, which causes step 7's publication (`mv O/tmp/$$ O/<id>`) to fail as well—ensuring no event is queued or dispatched in an unwritable filesystem state.
3. **Test Coverage (`endpoint_changed_mid_flush`)**:
   - Verified by test case `endpoint_changed_mid_flush`, where `oncall.1` simulates both removing `O/endpoint` and rewriting it to another URL, verifying that in both scenarios the flusher stops without dispatching a second request.

The fix is verified, robust, and introduces no new defects.

Altitude report SHA-256: 7eb0903638a270322032a78680b3d6238c7b25bf862c6e786d92e64daa632405

[Antigravity Review] LGTM

## Round 10

Michaël,

Here is my review of the Round 10 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r10.diff`, from revision 9 to revision 10 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 10 stands at 675 physical lines (+5 lines over revision 9). It contains zero fenced code blocks and strictly adheres to inline literal bounds. Calibration is verified and sound.
- **Delta Scope**: Accurately documents the per-machine endpoint residual in D5 step 2 and the README, and moves two delivery-dependent test cases from item 2 to item 5.

### Analysis of Revision 10 Fixes
1. **Per-Machine Endpoint Residual Documentation (D5 Step 2 & README)**:
   - Accurately captures the consequence of the issue's requirement for a single per-machine state file (`O/endpoint`): if multiple sessions run simultaneously across a configuration change, the recorded endpoint reflects whichever session wrote last.
   - Since both endpoints are explicitly configured by you and use the same machine token, no unauthorized URL can be contacted and no events are lost.
   - The operational mitigation is cleanly documented in the README ("restart running sessions after changing `me_url`").
2. **Test Case Realignment (Item 2 → Item 5)**:
   - `endpoint_changed_mid_flush` and `endpoint_update_fail` verify the flusher's delivery behavior under endpoint mutation and removal using `fake-curl`.
   - Moving these cases from item 2 (which only implements enqueue and envelope generation) to item 5 (which implements `flush` and introduces `fake-curl`) resolves the dependency ordering so tests are introduced alongside the code they exercise.

The delta is verified, accurate, and introduces no defects.

Altitude report SHA-256: e95939ca4f741e00b243ed380723f19efc80f6a73774676248b71efbbbd12bd9

[Antigravity Review] LGTM

## Round 11

Michaël,

Here is my review of the Round 11 delta for `dutiona/memory-engine#1019` (`.review/plan-delta-r11.diff`, from revision 10 to revision 11 in `docs/plans/2026-11-16-1019-plugin-outbox.md`).

### Altitude and Delta Assessment
- **Altitude Checkpoint**: Revision 11 retains identical physical line count (675 lines). Inline literals remain tightly bounded, with zero code blocks. Calibration is verified and sound.
- **Delta Scope**: Strictly text portability—replaces machine-local directory paths in Operational steps 1 and 2 with portable repo-relative phrasing to satisfy the `archive-plan-issue.sh` public-issue linter.

### Analysis of Revision 11 Changes
- Operational step 1 replaces `~/dev/memory-engine/.worktrees/feat-1019-plugin-outbox` with `under the repository's .worktrees/: branch feat/1019-plugin-outbox`.
- Operational step 2 replaces the explicit path with `under docs/plans/`.
- No technical, functional, or contract details are altered.

The delta is verified, clean, and introduces no defects.

Altitude report SHA-256: 8d448789593d8542fb5d407011beb187ab6752053c091968bf22d785a645e166

[Antigravity Review] LGTM

REVIEW COMPLETE
