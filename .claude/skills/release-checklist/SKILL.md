---
name: release-checklist
description: "Use when releasing memory-engine to its durable home — promoting a built release into ~/.local/opt/memory-engine/{bin,data} that the harness consumes. Runs an execute-with-gates procedure: build optimized cli+mcp, run the quality gate (test/fmt/clippy/migration), back up the live DB, migrate + verify schema_version, then atomically promote the binaries and write a RELEASE manifest — STOP on any failure, explicit go before each irreversible step (DB migrate, binary promote). Triggers on /release-checklist, 'release memory-engine', 'promote memory-engine to ~/.local/opt', 'cut a memory-engine release'."
---

# /release-checklist — memory-engine durable release

Promote a built `memory-engine` release into its **durable home**
`~/.local/opt/memory-engine/{bin,data}` — the only artifacts the harness consumes. The harness
must **never** run a `target/` dev/debug build; this skill is what moves a verified release into
`bin/`, with **database and code migrated together** and the migration ordered **before** the new
binaries are served.

This is an **execute-with-gates** procedure. Run each step in order; **STOP and report on the first
failure**; require an explicit user **"go"** before every IRREVERSIBLE step (the DB **migrate** in
Step 4, the binary **promote** in Step 5). CI does not gate this repo — **this checklist is the
gate.** Do not shortcut it.

> **Ordering is load-bearing.** Migrate the live DB **before** promoting the new binaries, so the
> harness never starts a new binary against an un-migrated DB. If the migration fails, restore the
> backup and **abort** — the old binaries in `bin/` keep serving the old (intact) DB.

## Preconditions — release offline

Run this checklist with **no live writer holding the DB**: stop the harness and any running
`memory-engine-mcp` first. This is load-bearing, not advisory:

- The Step-3 `cp` backup of a WAL-mode DB is only consistent when no writer is mid-transaction.
- `migrate` opens the engine **writable**; a second writer contends for the write lock.
- Between Step 4 (migrate) and Step 5 (promote) the on-disk DB is at the **new** schema while the
  **old** binaries in `bin/` would reject it — keep that window offline and brief.

Confirm nothing holds the DB before starting:

```bash
DB=~/.local/opt/memory-engine/data/memory.db
{ command -v fuser >/dev/null && fuser "$DB" 2>/dev/null && { echo "STOP: a process still has the DB open"; exit 1; }; } || echo "DB is free (or absent — first release)"
```

## Step 0 — Establish the release environment (run once; every later step sources it)

Each step runs in its **own** shell — and the gated STOPs mean the steps cannot share one shell — so
shell variables do **not** persist between them. A re-evaluated `$TS` would also drift between backup
and restore. Freeze the config (one stable `$TS`) into a sourceable env file now; every later step
begins by sourcing it.

```bash
OPT=~/.local/opt/memory-engine
REPO=$(git -C ~/dev/memory-engine rev-parse --show-toplevel 2>/dev/null || echo ~/dev/memory-engine)
TS=$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$OPT/data/backups"
cat > "$OPT/.release-env" <<EOF
OPT=$OPT
REPO=$REPO
DB=$OPT/data/memory.db
BACKUPS=$OPT/data/backups
TS=$TS
BACKUP=$OPT/data/backups/memory.$TS.pre-release.db
EOF
cat "$OPT/.release-env"
```

## Step 1 — Build the optimized release (never debug)

```bash
source ~/.local/opt/memory-engine/.release-env
cd "$REPO"
cargo build --release -p memory-engine-cli -p memory-engine-mcp
```

Produces `$REPO/target/release/memory-engine-cli` and `…/memory-engine-mcp`. These are **staged**,
not yet promoted. **STOP** on any build error.

## Step 2 — Quality gate — GATE (all must pass; blocks promote)

Run in order; **STOP on the first failure** — a failure here means **do not release**:

```bash
source ~/.local/opt/memory-engine/.release-env
cd "$REPO"
git diff-index --quiet HEAD -- || { echo "STOP: uncommitted changes — commit/stash so the manifest git_sha matches what is built"; exit 1; }
cargo test --workspace                # includes the schema-migration test(s)
cargo +nightly fmt --check            # memory-engine is nightly-fmt canonical
cargo clippy --workspace --all-targets
```

Version-bump check: confirm the crate version in `Cargo.toml` was bumped for this release (SemVer:
patch = fixes, minor = features, major = breaking). Any gate failure → STOP; fix on a feature branch
→ PR → restart from Step 1.

## Step 3 — Back up the live DB (before any mutation)

```bash
source ~/.local/opt/memory-engine/.release-env
if [ -f "$DB" ]; then
  cp -- "$DB" "$BACKUP" && echo "backed up → $BACKUP"
else
  echo "first release: no live DB at $DB — nothing to back up (the MCP server creates it on first run)"
fi
```

`memory-engine` also self-backs-up via `VACUUM INTO` when the writable engine opens for a migration
(the WAL-safe primary). The gate takes its **own** independent `cp` copy first so restore-on-fail
never depends on the very operation that just failed. With the harness stopped (Preconditions) the DB
is quiescent, so the plain `cp` is consistent.

## Step 4 — Migrate + verify — IRREVERSIBLE (requires explicit "go")

> **STOP. Ask the user to confirm "go" before migrating the live DB.**

```bash
source ~/.local/opt/memory-engine/.release-env
cd "$REPO"
if [ -f "$DB" ]; then
  # Dry-run report. `migrate --check` exits non-zero when migrations are PENDING — that is a STATUS
  # signal, NOT a gate failure — so show it but never abort on its exit.
  target/release/memory-engine-cli --db "$DB" migrate --check || true
  # On "go": apply. memory-engine takes its own VACUUM-INTO backup, then migrates transactionally;
  # a genuine failure rolls back and exits non-zero here.
  target/release/memory-engine-cli --db "$DB" migrate || NEED_RESTORE=1
  # GATE: schema_version must equal the binary's CURRENT_SCHEMA_VERSION (exit 0). `--format` is a
  # GLOBAL flag (before the subcommand).
  if [ -z "$NEED_RESTORE" ]; then
    target/release/memory-engine-cli --db "$DB" schema || NEED_RESTORE=1
  fi
  if [ -n "$NEED_RESTORE" ]; then
    echo "migrate/verify FAILED — restoring the pre-release backup and ABORTING (do not promote)"
    # Drop stale WAL sidecars first, or SQLite replays the failed migration's frames onto the
    # restored file → silent corruption. Restore via a temp file so a short write can't truncate $DB.
    cp -- "$BACKUP" "$DB.restoring"
    rm -f -- "$DB-wal" "$DB-shm" "$DB-journal"
    mv -f -- "$DB.restoring" "$DB"
    echo "restored from $BACKUP — the old binaries in bin/ keep serving this DB"
    exit 1
  fi
  echo "migrated + verified"
else
  echo "first release: no DB to migrate — the promoted MCP server initializes it at CURRENT_SCHEMA_VERSION on first run"
fi
```

Only a clean `schema` exit `0` (live `schema_version == CURRENT_SCHEMA_VERSION`) clears this gate.

## Step 5 — Promote the binaries atomically — IRREVERSIBLE (requires explicit "go")

> **STOP. Ask the user to confirm "go" before promoting.** The DB is already migrated (Step 4), so
> the new binaries will open a compatible DB.

```bash
source ~/.local/opt/memory-engine/.release-env
cd "$REPO"
mkdir -p "$OPT/bin"
# Stage BOTH binaries beside their targets first; flip only once both copies succeed, so a mid-copy
# failure (disk full, perms) can never leave a split-version install (new cli + old mcp).
for b in memory-engine-cli memory-engine-mcp; do
  cp -- "target/release/$b" "$OPT/bin/.$b.$TS.tmp" || { echo "STAGE FAILED: $b"; rm -f "$OPT/bin/".*.$TS.tmp; exit 1; }
done
for b in memory-engine-cli memory-engine-mcp; do
  mv -f -- "$OPT/bin/.$b.$TS.tmp" "$OPT/bin/$b"   # atomic rename into place
done
# schema_version for the manifest: read it from the live DB via `--format plain` (exact key,
# no JSON grepping). On a first release with no DB yet, record it as pending.
if [ -f "$DB" ]; then
  SCHEMA_VER=$("$OPT/bin/memory-engine-cli" --db "$DB" --format plain schema 2>/dev/null | grep '^schema_version=' | cut -d= -f2)
else
  SCHEMA_VER="pending (initialized on first MCP run)"
fi
cat > "$OPT/RELEASE" <<MANIFEST
version: $(grep -m1 '^version' "$REPO/Cargo.toml" | cut -d'"' -f2)
git_sha: $(git -C "$REPO" rev-parse HEAD)
schema_version: ${SCHEMA_VER:-unknown}
released_at: $TS
MANIFEST
cat "$OPT/RELEASE"
# Smoke the PROMOTED binary. With a live DB → `stats` (proves it opens the migrated DB). On a first
# release with no DB yet → `--version` (DB-free liveness; the CLI cannot open a missing DB).
if [ -f "$DB" ]; then
  "$OPT/bin/memory-engine-cli" --db "$DB" stats >/dev/null || { echo "SMOKE FAILED"; exit 1; }
else
  "$OPT/bin/memory-engine-cli" --version >/dev/null || { echo "SMOKE FAILED"; exit 1; }
fi
echo "promoted OK"
```

A non-zero smoke exit after promote is a **release failure** — investigate before declaring done.

## Done

Report: built version, git sha, schema_version, backup path, and the smoke result. Keep the old
`~/.local/share/memory-engine` DB (if any) until the new `~/.local/opt` install is verified in live
use. The `RELEASE` manifest at `$OPT/RELEASE` records what is installed.

## Failure-mode summary

| Failure                                | Action                                                                             |
| -------------------------------------- | ---------------------------------------------------------------------------------- |
| DB still open (Preconditions)          | STOP — stop the harness/mcp; a hot backup or a locked migrate is unsafe            |
| Build (Step 1) / quality gate (Step 2) | STOP — do not release; fix → PR → restart                                          |
| Migration or verify (Step 4)           | sidecar-aware restore from `$BACKUP` → ABORT; old binaries keep serving the old DB |
| Promote smoke (Step 5)                 | release failure — the DB is migrated but the new binary is unhealthy; investigate  |
