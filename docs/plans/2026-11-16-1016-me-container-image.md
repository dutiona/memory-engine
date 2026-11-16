# build(container): build the ME server image from a committed Cargo.lock (#1016)

## Outcome

Any memory-engine tag becomes a self-contained server image built from git alone: `Cargo.lock` is tracked and
checked by the local gate matrix, and `podman build .` on a clean checkout runs a multi-stage, `--locked` release
build of `memory-engine-mcp` and `memory-engine-cli` and ships them in a slim Debian runtime that holds CA
certificates, the `sqlite3` CLI and `curl`, running as a fixed non-root UID. Deploy wiring (quadlet,
`UserNS=keep-id`, volumes, health probe, HTTP transport, SIGTERM handling) stays with the infra and ME-5 tickets.

## Constraints

- "`Cargo.lock` is committed and removed from `.gitignore`; `cargo build --workspace --locked` passes."
- "The `Containerfile`: is multi-stage, with a `--locked` release build of `memory-engine-mcp` and `memory-engine-cli`;"
- "has a slim runtime with CA certificates (TLS to the Mac), the `sqlite3` CLI (read-only `VACUUM INTO`) and `curl` (health probe);"
- "runs as a non-root user with a fixed UID."
- "`.containerignore` keeps `target/`, worktrees and denylist files out of the context. `podman build` succeeds from a clean checkout of a tag, and both binaries answer `--version`."
- Spec D5, paraphrased: one Containerfile per repository, built from committed lockfiles; images are built on the
  deploy host with `podman build` from a release tag, at low priority; no registry, no new CI dependency.
- memory-engine is PUBLIC: no tailnet name, host name, address, port mapping or token in any file here.
- Decided here (facts found while planning, confirmed by the plan review):
  - `reqwest` 0.12 default TLS is `native-tls` (OpenSSL) for both binaries (`cargo tree -i openssl-sys`), so the
    builder needs `pkg-config` + `libssl-dev` and the runtime needs `libssl3t64`. Switching to rustls is out of scope.
  - The build context is an allowlist (`Cargo.toml`, `Cargo.lock`, `crates/`): a new untracked file is excluded by
    default. If the `--locked` release build of the two binaries refuses the missing root `tests/`, `benches/`,
    `examples/` target paths declared in `crates/memory-engine/Cargo.toml`, those three are re-included; the item-4
    build decides it, nothing else.
  - `fuzz/.gitignore` already ignores `fuzz/Cargo.lock`, so dropping the root `Cargo.lock` line tracks only the root lock.
  - Builder and runtime both name Debian trixie (`rust:1-slim-trixie`, `debian:trixie-slim`): the binaries link
    the builder's glibc and OpenSSL, so the two stages must share one Debian release. `rust:1` is a range, not a pin;
    the toolchain floor is the workspace MSRV 1.88 and the lockfile is the reproducibility mechanism.
  - Fixed UID/GID `10001` (`memory-engine`). Any stable value works; it only has to stay fixed for the quadlet's
    `UserNS=keep-id:uid=10001,gid=10001`. Each quadlet maps the host service user onto its own image UID, so the KB
    image need not match it, though matching is simpler.
  - Tracking the lock reverses the repository's "lock is gitignored, CI resolves fresh" policy: an advisory is now
    closed by committing `cargo update -p X` (the MSRV-aware resolver keeps it at or below 1.88), raising the MSRV
    only when no compatible fixed version exists.

## Implementation sequence

- [ ] **Track `Cargo.lock`.**
  - Files: `.gitignore`, `Cargo.lock` (new in git).
  - Red: `git check-ignore -v Cargo.lock` prints `.gitignore:7:Cargo.lock`; `git ls-files Cargo.lock` is empty.
  - Change: delete the `Cargo.lock` line under `# Rust`; commit the lock produced by `cargo generate-lockfile`
    (resolver 3, so the resolution honours `rust-version = "1.88"`).
  - Green: `git check-ignore Cargo.lock` exits 1; `cargo build --workspace --locked` passes.
  - Commit: `build: track Cargo.lock so images build with --locked`.
- [ ] **Gate the lock and update the lock policy text.**
  - Files: `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` (matrix block: a new first line
    `cargo build --workspace --locked   # Lockfile is current`, placed first because any later unlocked cargo
    command would rewrite a stale lock before it is checked), `.claude/skills/qa-sweep/SKILL.md` (the
    "`Cargo.lock` is gitignored" bullet rewritten to the policy in Constraints).
  - Red/green: `tests/gate_matrix_parity.rs` stays green (the matrix only grows, so it remains a superset of
    `ci.yml`): `cargo test -p memory-engine --all-features --test gate_matrix_parity`.
  - Commit: `docs: gate the committed lockfile and update the advisory policy`.
- [ ] **Boy-scout: drop duplicate `.gitignore` entries** (`/target` at line 56 under the empty `# Added by cargo`
  block duplicates `/target/` at line 5; `.vscode/` at line 14 duplicates line 2).
  - Files: `.gitignore`. Green: `git check-ignore target/x .vscode/x` still matches both; `git status` unchanged.
  - Commit: `chore(gitignore): drop duplicate target and .vscode entries`.
- [ ] **Add `.containerignore`.**
  - Files: `.containerignore` (new). Exact content, in order: `*`, `!Cargo.toml`, `!Cargo.lock`, `!crates/`,
    `**/target`, `**/.env`, `**/.env.*`, `**/.secrets-denylist`, with one comment: the denylist holds real secret
    literals for the redaction gate and must never enter an image.
  - Red/green: the standalone context probe (Testing). Before the file exists it prints `LEAK target/probe`;
    after, `CLEAN`.
  - Commit: `build(container): build from an allowlisted context`.
- [ ] **Add the `Containerfile`.**
  - Files: `Containerfile` (new).
  - Stage `builder` (`FROM docker.io/library/rust:1-slim-trixie AS builder`): apt `pkg-config libssl-dev`;
    `WORKDIR /src`; `COPY . .`;
    `RUN cargo build --release --locked -p memory-engine-mcp -p memory-engine-cli`.
  - Stage runtime (`FROM docker.io/library/debian:trixie-slim`): apt `--no-install-recommends ca-certificates
    libssl3t64 sqlite3 curl`, lists removed; `groupadd --gid 10001 memory-engine` and `useradd --uid 10001 --gid 10001
    --no-log-init --create-home --home-dir /var/lib/memory-engine --shell /usr/sbin/nologin memory-engine`;
    `COPY --from=builder /src/target/release/memory-engine-mcp /src/target/release/memory-engine-cli /usr/local/bin/`;
    build-time smoke `RUN memory-engine-mcp --version && memory-engine-cli --version && sqlite3 --version && curl --version`
    (fails the build if a shared library is missing); `USER 10001:10001`; `WORKDIR /var/lib/memory-engine`;
    `ENTRYPOINT ["memory-engine-mcp"]`. One OCI label: `org.opencontainers.image.source` = the public repo URL.
    No `HEALTHCHECK` (no HTTP endpoint until ME-5), no `VOLUME`, no `EXPOSE` (the quadlet owns mounts and ports).
  - Red: `podman build .` fails (no Containerfile).
  - Green: Verification 3–6.
  - Commit: `build(container): add the multi-stage ME server image`.
- [ ] **Boy-scout: remove the nonexistent `async` feature from the install docs.** `crates/memory-engine/Cargo.toml`
  states there is no `async` feature (`default = ["backend-sqlite"]`), yet `README.md` `## Installation` shows
  `features = ["async"]` and `docs/getting-started/installation.md` claims `default = ["async"]`.
  - Files: `README.md`, `docs/getting-started/installation.md`. Replace with the fact that tokio is a mandatory
    dependency and the default feature is `backend-sqlite`. No executable behaviour.
  - Commit: `docs: drop the nonexistent async feature from the install guide`.
- [ ] **Document the image** once, as `## Server image` in `docs/getting-started/installation.md`: the build
  command `podman build -t memory-engine:<tag> .` from a checkout of the tag, that it builds with `--locked` from the
  committed lock, the UID/GID 10001, the entry point, and that deploy wiring lives outside this repository. README
  `## Installation` gains one line linking it. No host names.
  - Commit: `docs: describe building the server image`.

## Documentation

`docs/getting-started/installation.md` gains `## Server image`; README links it (item 7). The gate-matrix docs and
the qa-sweep policy text change in item 2. Stale `async` snippets are removed (item 6). No ADR change: spec D5 is
recorded in the epic; ADR-0020 does not cover packaging.

## Testing

No Rust code changes, so no new `cargo test` target; `gate_matrix_parity` covers the matrix edit. The image is
proven by executable probes rather than a committed script (no container CI exists, #989, and spec D5 forbids a new
CI dependency):

- **Lock:** `cargo build --workspace --locked`.
- **Context probe** (seconds, independent of the real build): in a scratch clone, plant `target/probe`,
  `.worktrees/probe/x`, `.secrets-denylist`, `.env`, `crates/memory-engine/.env`. A scratch
  `probe.Containerfile` outside the clone: `FROM docker.io/library/debian:trixie-slim`, `COPY . /ctx`,
  `RUN cd /ctx && for p in target/probe .worktrees .secrets-denylist .env crates/memory-engine/.env .git; do test ! -e "$p" || { echo "LEAK $p"; exit 1; }; done && test -f Cargo.lock && test -d crates && echo CLEAN`.
  Run `podman build --no-cache -f <scratch>/probe.Containerfile --ignorefile <clone>/.containerignore <clone>`; it
  prints `CLEAN`. The positive asserts catch an ignore file that is too broad.
- **Runtime:** the in-image `RUN … --version` smoke plus the `podman run` checks in Verification.
- Excluded: no image-content snapshot tests (they would assert Debian package versions, a dependency's incidental
  output).

## Verification

Run at the final clean HEAD, unpiped:

1. AGENTS.md block, including the new `cargo build --workspace --locked` line: `cargo fmt --all --check`;
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo build --workspace`;
   `cargo build --workspace --all-features`; `cargo test --workspace --all-features`; `cargo check -p memory-engine`;
   `cargo check -p memory-engine --no-default-features --features backend-sqlite`; both `cargo doc` lines with the
   `RUSTDOCFLAGS`; `cargo deny check`. `cargo +nightly fuzz build` is N/A: the facade is untouched. MSRV:
   `cargo +1.88 build --workspace --locked --tests --examples` and the same with `--all-features` (relevant: the
   lock now fixes the versions the MSRV job builds).
2. `git status --porcelain` is empty after step 1 (no cargo command rewrote the lock).
3. Clean checkout of a tag: `git clone --no-local <worktree> <scratch>/clone`, `git -C <scratch>/clone tag
   v0.0.0-image-probe <head>` (local to the scratch clone, never pushed), checkout the tag, then
   `podman build -t localhost/memory-engine:probe <scratch>/clone` succeeds.
4. `podman run --rm localhost/memory-engine:probe --version` and
   `podman run --rm --entrypoint memory-engine-cli localhost/memory-engine:probe --version` both print a version.
5. `podman run --rm --entrypoint sh localhost/memory-engine:probe -c 'id -u; id -g'` prints `10001` twice; the same
   with `test -s /etc/ssl/certs/ca-certificates.crt && curl --version >/dev/null && echo OK` prints `OK`.
6. Read-only backup path, inside the image: `memory-engine-cli --db /tmp/me.db add-fact --content probe
   --fact-type semantic --embedding '[0.1,0.2,0.3]'`, then `sqlite3 -readonly /tmp/me.db "VACUUM INTO '/tmp/b.db'"`,
   then `sqlite3 /tmp/b.db "PRAGMA integrity_check; SELECT count(*) FROM facts_fts;"` prints `ok` and `1`. This
   proves the system `sqlite3` reads the engine's FTS5/WAL schema written by the bundled SQLite.
7. The context probe in Testing prints `CLEAN`.

Out of scope, recorded: adding `--locked` to `.github/workflows/ci.yml` (manual-dispatch CI; the local matrix line
in item 2 is the gate); rustls instead of OpenSSL; build caching; SIGTERM handling as PID 1 (`memory-engine-mcp`
installs no signal handler, so `podman stop` kills it after the timeout and skips the snapshot flush; ME-5's
acceptance already requires SIGTERM handling).

## Resolution

Review: `docs/plans/2026-11-16-1016-me-container-image-lean-review.md` (one independent reviewer). Planning-review
severity: Default, so every finding is resolved.

1. Blocker, probe can never print `CLEAN`: adopted. The probe is now a standalone copy-only Containerfile with
   `--ignorefile`, testing the planted `target/probe` marker plus positive `Cargo.lock`/`crates` asserts (Testing).
2. High, item-3 red needs the Containerfile: resolved by finding 1's standalone probe; `.containerignore` is item 4
   and stands on its own.
3. High, lock policy text stale: adopted as item 2 (qa-sweep bullet rewritten; policy stated in Constraints and to
   be named in the PR body).
4. Medium, denylist premise and gaps: adopted. Allowlist with `**/` anchored secret patterns (item 4). The
   tests/benches/examples question is decided by the build itself, per Constraints, rather than by the reviewer's
   "likely".
5. Medium, `sqlite3` purpose untested: adopted as Verification 6 (real engine DB, read-only `VACUUM INTO`,
   `integrity_check`, FTS5 count).
6. Medium, host name in the public plan: adopted. "the deploy host"; D5 paraphrased.
7. Medium, stale lock caught too late: adopted, ideal option. `cargo build --workspace --locked` becomes the first
   matrix line in all three docs (item 2); Verification 2 checks nothing rewrote the lock.
8. Medium, nonexistent `async` feature in the docs being edited: adopted as boy-scout item 6 (own commit); image docs
   written once in `installation.md` with a README link (item 7).
9. Low, line numbers: corrected (`.gitignore:7`, `/target` at 56 vs 5); duplicate `.vscode/` and the empty block
   are dropped in item 3.
10. Low, `id` output: adopted, `id -u`/`id -g` (Verification 5).
11. Low, confirmations: no change.
12. Low, SIGTERM as PID 1: recorded under Out of scope, pointing at ME-5's existing acceptance line; no
    Containerfile change.
13. Note, who wrote the ignored lock: no effect; item 1 regenerates it.
