# Implementation audit (scope: pre-review) — #1033

- Approved plan: `2026-11-16-rustsec-advisories.md`, SHA-256
  `cb4ed9008bdb0eb05feb7455efe513c9a6162ad72baee4b21a88164f28b3989b` (approved on the plan issue #1039).
- Audited head: `288279c9618ebd15209a64f3df2bccaa754fb1fc`, on `origin/main` `a5d756b` (#1032 merged before
  implementation, so the branch was rebased first and the clippy row is expected green with no sibling exception).

| # | Item | Status | Commit |
|---|---|---|---|
| 1 | Stale 1.85 comment | Implemented | `6ef5656` |
| 2 | Facade's dead rusqlite `trace` dev-dependency | Implemented | `2df39d5` |
| 3 | Members inherit `rust-version` | Implemented | `52418b7` |
| 4 | Saturate `list_recent_by_scope`'s LIMIT (red: `Storage(Backend("number too large to fit in target type"))`) | Implemented | `b36585b` |
| 5 | rusqlite 0.43 + MSRV 1.89, `count_batch_queries` helper | Implemented | `e9f0231` |
| 6 | RUSTSEC-2026-0462 ignore + `[[bans.features]]` rmcp ban | Implemented | `bc5c296` |
| 7 | RUSTSEC-2026-0440 ignore | Implemented | `bf88dc0` |
| 8 | MSRV prose to 1.89 | Implemented | `aec1127` |
| 9 | `deny.toml`'s stale lists | Implemented | `9bbcd9f` |
| 10 | Removed-`async` docs (6 lines, 5 files) | Implemented | `288279c` |

Counts: Implemented 10, Modified 0, Dropped 0, Incomplete 0, Added 0. Divergence: 0%.

Intent-preserving variations (not Modified):
- Item 6's mutation script first reported a false failure: `printf … | grep -q` under `pipefail` exits 141 on
  SIGPIPE. The check now reads a here-string. Both mutations (`transport-streamable-http-client-reqwest`, `client`)
  then printed `feature-banned`, gate rc 2, `RESTORE PROVEN (bytes)`, `STATUS UNCHANGED` and `MUTATION PROVEN`.
- Item 10's `api.md` sentence keeps the old entry's note that `MemoryEngine`'s DB methods are `async fn` awaited in
  a tokio runtime. `architecture-overview.md` now reads "The runtime is the caller's; `tokio` is a non-optional
  dependency, so no feature gates it."
- Item 8's CLAUDE.md wording reads "(minus the documented exceptions in `deny.toml`; against the full all-features
  graph)".
- `cargo install cargo-fuzz --locked` found cargo-fuzz 0.13.2 already installed, so nothing was installed.
- Verification step 3 (clippy attribution) did not run: #1032 is on main, and the plan requires clippy rc 0 in that
  case. Clippy was rc 0 at every full run.

Full matrix runs (AGENTS.md rows + MSRV job), each row unpiped into its own log:
- After item 5 (`e9f0231`): every row rc 0 except `cargo deny check` rc 1, with exactly RUSTSEC-2026-0440 and -0462
  and `bans ok, licenses ok, sources ok`. That is the plan's stated intermediate result.
- After item 7 (`bf88dc0`): every row rc 0.
- Final, at `288279c` with a clean tree and `origin/main` unmoved: every row rc 0. The tests show 1998 passed,
  0 failed, 81 ignored. `cargo deny check` ends `advisories ok, bans ok, licenses ok, sources ok`.

Reworded before review: item 7's commit message spelled "upstream fix testcontainers/testcontainers-rs#1006", a
closing-keyword shape. It was reworded with a non-interactive rebase that left every tree unchanged
(`git diff 189358d HEAD` is empty), which moved the SHAs from item 7 on. The full-matrix runs above ran on trees
identical to the cited commits.
