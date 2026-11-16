# Implementation audit (pre-review): #1016 container image

- Scope: pre-review
- Approved plan: `docs/plans/2026-11-16-1016-me-container-image.md`, sha256
  `30e4adfb3868589d28e979ad7c2c617fc61c975db0789071554f53a6a59434d1`
- Audited head: `8b38fa8` (the last implementation commit after rebasing onto `4f3854d`, before this audit)

| # | Plan item | Status | Commit |
|---|---|---|---|
| 1 | Track `Cargo.lock` | Implemented | `9143ae1` |
| 2 | Gate the lock; update the qa-sweep lock policy | Implemented | `b78a6c4` |
| 3 | Boy-scout: duplicate `.gitignore` entries | Implemented | `1f5037b` |
| 4 | Allowlist `.containerignore` | Implemented | `0698d6c` |
| 5 | Multi-stage `Containerfile` | Implemented | `f61d7e0` |
| 6 | Boy-scout: nonexistent `async` feature in the install docs | Dropped: already fixed on `main` by #1043 (`4f3854d`) | — |
| 7 | Server image docs | Implemented | `8b38fa8` |

Counts: 6 Implemented, 0 Modified, 1 Dropped, 0 Incomplete, 0 Added. Divergence: 14% (1 of 7; threshold 30%).
The Dropped item needs no follow-up. Its commit became empty in the rebase because #1043 had already made the same
correction to `README.md` and `docs/getting-started/installation.md`.

Rebase adaptations (#1032 and #1033 merged while this branch was parked; neither changes intent):

- #1033 raised the MSRV to 1.89 (`rusqlite` 0.43). The lock was regenerated against the new manifests. The old lock
  failed `cargo build --workspace --locked` (rc 101), which shows the item-2 gate works; the regenerated lock passes.
  The qa-sweep bullet now says "the MSRV floor" instead of naming 1.88. The plan's `+1.88` verification rows run as
  `+1.89`.
- The `--locked` line stays first in the matrix blocks of all three instruction files.

Intent-preserving variations (not Modified):

- Item 3 removes the top-of-file `.vscode/` and keeps the one under `# IDE`.
- Item 4's probe also plants `crates/memory-engine/target/probe` and `crates/memory-engine/.secrets-denylist`, so it
  proves the `**/` patterns hold at depth.
- The allowlisted build succeeded without the root `tests/`, `benches/` and `examples/`, so they stay out.
- Verification 6: `memory-engine-cli add-fact` refuses a missing database. The probe therefore first creates the
  store by starting the image's own `memory-engine-mcp` with a three-line config (`embed_dim = 3`) and a closed
  stdin. Then it runs `add-fact`, `sqlite3 -readonly … VACUUM INTO`, and `integrity_check` plus the FTS5 count.
