# Schema Evolution Policy

This document defines the versioning, migration, and backwards-compatibility rules for memory-engine's SQLite schema. It covers four version axes, the migration framework, event envelope versioning, and testing requirements.

## Version Axes

memory-engine tracks four independent version numbers:

| Axis             | Location       | Purpose                          | Example |
| ---------------- | -------------- | -------------------------------- | ------- |
| `schema_version` | `config` table | DDL version of the SQLite schema | `6`     |
| `storage_epoch`  | `config` table | Coarse compatibility gate        | `1`     |
| Crate semver     | `Cargo.toml`   | Rust API compatibility           | `0.1.0` |
| MCP API version  | MCP server     | Wire protocol compatibility      | `1.0`   |

**`schema_version`** is a monotonic integer incremented by each migration. It tracks which DDL changes have been applied. All migrations within the same epoch are forward-compatible.

**`storage_epoch`** is a coarse-grained gate. All schema versions within the same epoch can be migrated forward. Bumping the epoch signals a breaking architectural change (e.g., dropping support for old migrations). The library rejects databases from future epochs with `MemoryError::UnsupportedEpoch`.

## Forward-Only Migration Policy

Migrations are forward-only. There is no rollback code.

**Rationale:** For an embedded library where consumers control the upgrade timing, forward-only simplifies the migration framework and avoids the complexity of reversible DDL changes (which SQLite makes especially difficult since `ALTER TABLE` is limited). Pre-migration backup provides the safety net.

### Pre-Migration Backup

Before running migrations, a WAL-safe backup can be created via `VACUUM INTO`. This produces an atomic, consistent copy regardless of WAL state (no sidecar `-wal` or `-shm` files).

- **Opt-in:** Pass `backup_dir` in `EngineConfig` to enable. `None` skips backup.
- **File-backed only:** In-memory databases cannot be backed up (returns error).
- **Naming:** `<db_name>.v<current_version>.bak` in the specified directory.

To restore from backup, replace the database file with the backup file.

### Backwards-Compatibility Window

All schema versions within the current epoch are supported by the migration chain. The library:

1. Reads `storage_epoch` from the config table (defaults to 1 for pre-epoch databases).
2. Rejects future epochs with `UnsupportedEpoch`.
3. Rejects future schema versions with a clear "consider upgrading" error.
4. Runs the migration chain from the stored version to `CURRENT_SCHEMA_VERSION`.

## Migration Framework

Migrations are defined as `(MigrationFn, bool)` tuples in the `MIGRATIONS` array:

```rust
const MIGRATIONS: &[(MigrationFn, bool)] = &[
    (migrate_v1_to_v2, false),   // Add scopes + scope_id
    (migrate_v2_to_v3, true),    // Table rebuild for FK constraints
    (migrate_v3_to_v4, false),   // Add is_pinned, importance_score, envelope
    (migrate_v4_to_v5, false),   // Add event_revision
    (migrate_v5_to_v6, false),   // Add depth_shaping / query diagnostics
];
```

The boolean flag indicates whether `PRAGMA foreign_keys` must be disabled during the migration (required for table-rebuild migrations that `DROP` and recreate tables).

Each migration runs inside a transaction. On failure, the transaction rolls back and the version is NOT bumped.

### How to Add a Migration

1. Write a `migrate_vN_to_vM(conn: &Connection) -> Result<()>` function with frozen DDL (do not reference live DDL constants).
2. Append `(migrate_vN_to_vM, needs_fk_disable)` to `MIGRATIONS`.
3. Bump `CURRENT_SCHEMA_VERSION` to `M`.
4. Update `TABLES_DDL` (and other live DDL constants) to reflect the new schema.
5. Add a frozen DDL snapshot (`TABLES_VM_DDL`, `init_schema_vM()`) in the test module.
6. Update version assertions in existing tests (e.g., `"N"` to `"M"`).
7. Add specific migration tests: `migrate_vN_to_vM_adds_...`, `fresh_db_creates_vM_schema`.
8. Run `cargo test` — the insta snapshot test will fail with a pending snapshot. Review and accept the new snapshot.
9. Verify all property-based migration tests still pass (they exercise the full chain).

### Config-Only Migrations (no DDL delta)

Some migrations change only `config` rows, not table/index/trigger DDL — e.g. **v11→v12**
(#613, ADR 0015), which replaces the bare `embed_dim` config key with the richer
`embedding_meta` identity tuple (`DELETE FROM config WHERE key = 'embed_dim'`; the tuple is
written lazily on the first embedding write, so no backfill). For these:

- Steps 4–5 (live DDL constants, frozen DDL snapshot) are **N/A** — the table shape is
  unchanged, so `TABLES_VM_DDL` would be byte-identical to the prior version.
- **The DDL snapshot does NOT guard a config-only migration.** `deterministic_schema_dump`
  projects only `sqlite_master` (DDL); config rows are invisible to it, and the DDL is
  identical across the bump. The dedicated migration test (e.g.
  `migrate_v11_to_v12_drops_embed_dim`) is therefore the **load-bearing guard** — it must
  inject the legacy config row before migrating so the change is exercised non-vacuously
  ("snapshot green ≠ migration correct").

### Data-Folding DDL Migrations (DDL delta **and** a data move)

**v12→v13** (#622, ADR 0015) is the counterpart: it adds the `embedding_spaces` registry
table (a real DDL delta the snapshots **do** guard) _and_ folds the single `embedding_meta`
config value into one `active` row, then drops the legacy key. For these:

- Steps 4–5 apply: the table lives in the fresh-init `TABLES_DDL` **and** a frozen snapshot
  inside `migrate_v12_to_v13`; a `fresh_vs_migrated_*_converge` test (normalized
  `sqlite_master.sql`, not a raw string compare) asserts the two copies agree.
- The DDL snapshot (`schema_ddl_snapshot_is_stable`, the insta `schema_v*` snapshots,
  `all_nine_indexes_created`) guards the table/index shape, but **not the data move** — a
  green snapshot says nothing about whether a row was lifted. A dedicated round-trip test
  (`migrate_v12_to_v13_roundtrips_fingerprint`) injects the legacy value, migrates, and
  asserts the reconstructed identity is bit-identical and the legacy key is gone, plus
  fresh-store (no fabricated row) and corrupt-value (rolls back, version stays 12) cases.
- **Identity relocation has a blast radius.** Moving the identity out of `config` broke every
  consumer that read/wrote it via raw SQL outside the `embedding_meta` facade — dump/restore,
  the CLI/MCP dim probes, and test seeders. Those were all updated in the same change. When a
  migration moves data between tables/keys, audit _all_ raw readers/writers of the old
  location, not just the facade.

## Event Envelope Versioning

Events in the append-only log carry a per-event-type revision via the `event_revision` column.

### How It Works

- **`Event` struct** (read side): Has `event_revision: u16` with `#[serde(default = 1)]`.
- **`NewEvent` struct** (write side): Does NOT have `event_revision`. The store stamps it internally from the `UpcasterRegistry`.
- **On insert:** `EventStore::insert()` writes `registry.latest_revision(event_type)`.
- **On read:** `get()`/`list()` return raw stored data (audit-log semantics). `get_upcasted()`/`list_upcasted()` apply the upcaster chain.

### Raw vs Upcasted Reads

| Method                  | Applies upcasters | Use case                     |
| ----------------------- | ----------------- | ---------------------------- |
| `get(id)`               | No                | Audit log, debugging, replay |
| `list(filter)`          | No                | Audit log, batch export      |
| `get_upcasted(id)`      | Yes               | Application logic            |
| `list_upcasted(filter)` | Yes               | Application logic            |

Raw reads preserve the exact payload that was stored. This is critical for event-sourced systems where the event log is the source of truth.

### UpcasterRegistry

The registry maps `(event_type, from_revision)` to a transformation function:

```rust
let mut registry = UpcasterRegistry::new();
registry.register("Interaction", 1, |mut payload| {
    // Transform v1 payload to v2 format
    payload["schema_version"] = json!("v2");
    Ok(payload)
});
```

Chains are applied sequentially. If an event is at revision 1 and the latest is 3, upcasters `(1->2)` then `(2->3)` run in order. A gap in the chain (e.g., `2->3` registered but `1->2` missing) returns `MemoryError::Migration`.

### How to Add an Upcaster

1. Identify the event type and the payload change.
2. Register the upcaster in the `UpcasterRegistry` before constructing `EngineConfig`:
   ```rust
   let mut registry = UpcasterRegistry::new();
   registry.register("EventType", current_revision, |payload| {
       // Transform payload
       Ok(transformed)
   });
   config.upcaster_registry = registry;
   ```
3. Add tests in `events.rs`: insert at old revision (via empty registry), read with new registry, verify transformation.
4. The next schema migration should update the `event_revision` DEFAULT if the new revision should be the baseline for fresh databases.

### Replay Contract

Upcasting must produce **semantically equivalent** results — the upcasted payload should be interpretable by application logic the same way as a natively-written payload at the target revision. Exact byte-level equality is not required (field ordering may differ), but the semantic content must be identical.

## Testing Requirements

### Schema Snapshot (insta)

The `schema_v10_snapshot` test captures the complete DDL of a fresh database via a deterministic projection (sorted by type+name, whitespace-normalized). This snapshot:

- Breaks when any DDL constant changes (catches unintentional schema drift).
- Is version-controlled alongside the code.
- Must be reviewed and accepted after any schema change.

### Property-Based Tests (proptest)

Three property-based tests verify migration invariants across random inputs:

| Test                                      | Property                                                       |
| ----------------------------------------- | -------------------------------------------------------------- |
| `migration_preserves_event_count`         | Event count is identical before and after full migration chain |
| `migration_preserves_fact_content_hashes` | Content hashes survive the full migration chain unchanged      |
| `migration_v1_to_v5_fk_integrity`         | `PRAGMA foreign_key_check` is clean after migration            |

### Frozen DDL Snapshots

Each major schema version has a complete, standalone DDL snapshot in the test module (`init_schema_v1`, `init_schema_v2`, `init_schema_v4`, `init_schema_v5`). These depend on NO live DDL constants, preventing fixture drift when tables evolve.

This allows migration tests to start from any historical version and verify the migration chain produces the correct result.
