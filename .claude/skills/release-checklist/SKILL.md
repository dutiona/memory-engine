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

**Conventions used below**

```bash
OPT=~/.local/opt/memory-engine
DB="$OPT/data/memory.db"              # the live DB the harness uses (env MEMORY_ENGINE_DB)
BACKUPS="$OPT/data/backups"
REPO=~/dev/memory-engine              # the source checkout (build here, never consume target/)
TS=$(date -u +%Y%m%dT%H%M%SZ)
```

## Step 1 — Build the optimized release (never debug)

```bash
cd "$REPO"
cargo build --release -p memory-engine-cli -p memory-engine-mcp
```

Produces `target/release/memory-engine-cli` and `target/release/memory-engine-mcp`. These are
**staged**, not yet promoted. **STOP** on any build error.

## Step 2 — Quality gate — GATE (all must pass; blocks promote)

Run in order; **STOP on the first failure** — a failure here means **do not release**:

```bash
cargo test --workspace                # includes the schema-migration test(s)
cargo +nightly fmt --check            # memory-engine is nightly-fmt canonical
cargo clippy --workspace --all-targets
```

Version-bump check: confirm the crate version in `Cargo.toml` was bumped for this release (SemVer:
patch = fixes, minor = features, major = breaking). Any gate failure → STOP; fix on a feature branch
→ PR → restart from Step 1.

## Step 3 — Back up the live DB (before any mutation)

```bash
mkdir -p "$BACKUPS"
[ -f "$DB" ] && cp -- "$DB" "$BACKUPS/memory.$TS.pre-release.db" && echo "backed up → $BACKUPS/memory.$TS.pre-release.db"
```

memory-engine also self-backs-up via `VACUUM INTO` when the writable engine opens for a migration,
but the gate takes its **own** copy first so restore-on-fail never depends on the very operation
that just failed. If `$DB` does not exist yet (first release), there is nothing to back up — skip.

## Step 4 — Migrate + verify — IRREVERSIBLE (requires explicit "go")

> **STOP. Ask the user to confirm "go" before migrating the live DB.**

```bash
# Dry-run first: report what would change without mutating.
target/release/memory-engine-cli --db "$DB" migrate --check; echo "check exit=$?"
# On "go": apply (memory-engine takes its own VACUUM-INTO backup, then migrates transactionally).
target/release/memory-engine-cli --db "$DB" migrate
# Verify: schema reports the binary's CURRENT_SCHEMA_VERSION (exit 0 = matched;
# non-zero = mismatch/newer). `--format` is a GLOBAL flag (before the subcommand).
target/release/memory-engine-cli --db "$DB" schema; echo "schema exit=$?"
```

**On migration OR verify failure → restore the backup and ABORT (do not promote):**

```bash
cp -- "$BACKUPS/memory.$TS.pre-release.db" "$DB"   # restore the pre-release copy
# leave the OLD binaries in bin/ in place — they serve the restored (old, intact) DB.
```

Only a `schema` exit code of `0` (live `schema_version == CURRENT_SCHEMA_VERSION`) clears this gate.

## Step 5 — Promote the binaries atomically — IRREVERSIBLE (requires explicit "go")

> **STOP. Ask the user to confirm "go" before promoting.** The DB is already migrated (Step 4), so
> the new binaries will open a compatible DB.

```bash
mkdir -p "$OPT/bin"
for b in memory-engine-cli memory-engine-mcp; do
  cp -- "target/release/$b" "$OPT/bin/.$b.$TS.tmp"   # stage beside the target
  mv -f -- "$OPT/bin/.$b.$TS.tmp" "$OPT/bin/$b"       # atomic rename into place
done
# Write the RELEASE manifest (provenance for "what is installed").
cat > "$OPT/RELEASE" <<MANIFEST
version: $(grep -m1 '^version' "$REPO/Cargo.toml" | cut -d'"' -f2)
git_sha: $(git -C "$REPO" rev-parse HEAD)
schema_version: $(target/release/memory-engine-cli --db "$DB" --format json schema 2>/dev/null | grep -oE '"schema_version"[: ]+[0-9]+' | grep -oE '[0-9]+' | head -1)
released_at: $TS
MANIFEST
# Smoke the promoted binary against the live DB.
"$OPT/bin/memory-engine-cli" --db "$DB" stats; echo "smoke exit=$?"
```

A non-zero smoke exit after promote is a **release failure** — investigate before declaring done.

## Done

Report: built version, git sha, schema_version, backup path, and the smoke result. Keep the old
`~/.local/share/memory-engine` DB (if any) until the new `~/.local/opt` install is verified in live
use. The `RELEASE` manifest at `$OPT/RELEASE` records what is installed.

## Failure-mode summary

| Failure | Action |
|---|---|
| Build (Step 1) / quality gate (Step 2) | STOP — do not release; fix → PR → restart |
| Migration or verify (Step 4) | restore backup → ABORT; old binaries keep serving the old DB |
| Promote smoke (Step 5) | release failure — the DB is migrated but the new binary is unhealthy; investigate |
