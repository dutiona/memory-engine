# Phase 4a: Import/Export — JSON Event Log + SQLite Backup

**Issue:** #40
**Branch:** `feat/phase4a-import-export` (worktree)

---

## Context

Issue #39 (just completed) delivered the **export** side:
- `dump_json()` serializes `EngineSnapshot` (facts, edges, summaries, scopes, events, config) to JSON
- `dump_sqlite()` uses `VACUUM INTO` for atomic binary backup

Issue #40 completes the story with **import** (restore from backup) + **compression** (gzip/zstd for JSON).

---

## Design Decisions

**D1: Import = restore to fresh engine only.** No merge/additive. Merging state is a sync problem (deferred). Import targets an empty DB or a new file path.

**D2: Static constructors with config.** `MemoryEngine::restore_json(snapshot_path, config)`, `restore_json_memory(snapshot_path)`, `restore_sqlite(backup_path, config)`. File-backed variants accept `EngineConfig` to preserve pool size, search config, upcaster registry, and backup dir — matching `MemoryEngine::open()` semantics. The `embed_dim` in the config is validated against the snapshot's `embed_dim`. "Restore" naming distinguishes from future merge operations.

**D3: Raw SQL for ID preservation.** Import uses `INSERT INTO ... (id, ...) VALUES (...)` with explicit IDs in a dedicated `restore.rs` module. No new `insert_with_id` methods on stores — this is restore-only logic.

**D4: Compression via feature flags.** `compress-gzip` (dep: `flate2`) and `compress-zstd` (dep: `zstd`). Import auto-detects compression from magic bytes (`1f 8b` = gzip, `28 b5 2f fd` = zstd). Clear error if feature not enabled.

**D5: `DumpFormat` extension.** Add `JsonGzip(PathBuf)` and `JsonZstd(PathBuf)` variants. Add `#[non_exhaustive]` to `DumpFormat`.

**D6: SQLite restore = file copy + open.** `std::fs::copy`, then `MemoryEngine::open(config)`. Migration runs automatically. **Constraint:** `restore_sqlite` only accepts clean backups produced by `dump_sqlite()` (`VACUUM INTO`). Copying a live WAL-mode database is unsafe — the WAL sidecar may not be present. Document this constraint in the API doc.

**D7: Atomicity + cleanup.** Entire JSON import wrapped in a single SQLite transaction. On SQL failure → rollback → DB file remains but is empty (schema-only). On any error after DB creation (parse, validation, restore), **cleanup the orphan DB file** (`std::fs::remove_file`) before returning the error. This prevents subsequent retries from hitting a `Conflict("target path already exists")` error.

**D9: Root scope invariant.** `validate_snapshot` must check that the snapshot's scopes contain exactly one root node (`id=1, parent_id=None, label="root", depth=0`). `ScopeTree::load()` hardcodes root id=1 (`src/scope/tree.rs:37-39`). A snapshot without this invariant would break scope resolution.

**D10: Backward-compatible snapshot deserialization.** Add `#[serde(default)]` to `Event.origin_node_id` (default: `"local"`), `Event.sequence_id` (default: `0`), and `Event.created_at` (default: `None`) in `src/types.rs`. These fields were added in schema v4/v5 — older snapshots won't have them. Without serde defaults, `read_snapshot` fails on older JSON before `validate_snapshot` can run.

**D8: Config key handling.** Import writes all snapshot config keys EXCEPT `schema_version` and `storage_epoch` (left at values set by `init_schema`/`migrate`). The snapshot's `embed_dim` config is imported.

---

## API Surface

```rust
// src/inspect/types.rs
#[non_exhaustive]
pub enum DumpFormat {
    Json(PathBuf),
    JsonGzip(PathBuf),    // NEW
    JsonZstd(PathBuf),    // NEW
    Sqlite(PathBuf),
}

// src/engine.rs — new static constructors
impl MemoryEngine {
    /// Restore from JSON snapshot into a new file-backed engine.
    /// `config.path` must not exist. `config.embed_dim` validated against snapshot.
    pub fn restore_json(snapshot_path: &Path, config: &EngineConfig) -> Result<Self>;
    /// Restore from JSON snapshot into a new in-memory engine.
    pub fn restore_json_memory(snapshot_path: &Path) -> Result<Self>;
    /// Restore from a `dump_sqlite()` backup into a new file-backed engine.
    /// `config.path` must not exist. Only accepts clean VACUUM INTO backups.
    pub fn restore_sqlite(backup_path: &Path, config: &EngineConfig) -> Result<Self>;
}

// src/async_engine.rs — async mirrors (owned types for spawn_blocking)
impl AsyncMemoryEngine {
    pub async fn restore_json(snapshot_path: PathBuf, config: EngineConfig) -> Result<Self>;
    pub async fn restore_json_memory(snapshot_path: PathBuf) -> Result<Self>;
    pub async fn restore_sqlite(backup_path: PathBuf, config: EngineConfig) -> Result<Self>;
}
```

---

## Files

### New

| File | Purpose |
|---|---|
| `src/inspect/restore.rs` | Core import: `read_snapshot()`, `validate_snapshot()`, `restore_snapshot_into()`, compression detection |

### Modified

| File | Changes |
|---|---|
| `Cargo.toml` | Add `flate2` + `zstd` optional deps, `compress-gzip` + `compress-zstd` features |
| `src/inspect/types.rs` | `#[non_exhaustive]` on `DumpFormat`, add `JsonGzip`/`JsonZstd` variants |
| `src/inspect/dump.rs` | Factor out writer helper, add compressed export paths |
| `src/inspect/mod.rs` | Add `pub mod restore;` |
| `src/engine.rs` | Add 3 `restore_*` static constructors, update `dump_state()` for new variants |
| `src/async_engine.rs` | Async mirrors of restore methods |
| `src/types.rs` | Add `#[serde(default)]` to `Event` fields for backward-compatible deserialization (D10) |

---

## Tasks (Dependency Order)

### Task 1: `Cargo.toml` — compression dependencies

- [ ] Add `flate2 = { version = "1", optional = true }` and `zstd = { version = "0.13", optional = true }`
- [ ] Add features: `compress-gzip = ["dep:flate2"]`, `compress-zstd = ["dep:zstd"]`

### Task 2: Serde defaults for backward-compatible snapshot deserialization (D10)

**File:** `src/types.rs`

- [ ] Add `#[serde(default = "default_origin_node_id")]` to `Event.origin_node_id` (default: `"local".to_string()`)
- [ ] Add `#[serde(default)]` to `Event.sequence_id` (default: `0`)
- [ ] `Event.created_at` already `Option` — ensure it deserializes as `None` from missing field (should work with `Option`)
- [ ] Test: deserialize JSON event without `origin_node_id`/`sequence_id` fields → gets defaults

### Task 3: `DumpFormat` extension

**File:** `src/inspect/types.rs`

- [ ] Add `#[non_exhaustive]` to `DumpFormat`
- [ ] Add `JsonGzip(PathBuf)` and `JsonZstd(PathBuf)` variants
- [ ] Audit all `match` on `DumpFormat` (engine.rs `dump_state`, tests) — add arms

### Task 4: Compressed JSON export

**Files:** `src/inspect/dump.rs`, `src/engine.rs`

- [ ] Factor out `fn write_snapshot(writer: impl Write, snapshot: &EngineSnapshot) -> Result<()>`
- [ ] `dump_json` calls the helper with `BufWriter<File>`
- [ ] Add `dump_json_gzip(conn, embed_dim, path)` behind `#[cfg(feature = "compress-gzip")]`
- [ ] Add `dump_json_zstd(conn, embed_dim, path)` behind `#[cfg(feature = "compress-zstd")]`
- [ ] Update `dump_state()` to dispatch new variants (return `NotImplemented` if feature disabled)
- [ ] Tests: magic byte verification, round-trip with manual decompression

### Task 5: `restore.rs` — core import logic (TDD)

**File:** `src/inspect/restore.rs`

**4a: Compression detection + snapshot reading**
- [ ] `detect_compression(path) -> Result<Compression>` — magic byte check
- [ ] `read_snapshot(path) -> Result<EngineSnapshot>` — auto-detect, decompress, deserialize
- [ ] Tests: plain JSON, gzip JSON, zstd JSON, corrupt file

**4b: Validation**
- [ ] `validate_snapshot(snapshot, current_schema_version, storage_epoch) -> Result<()>`
  - schema_version > current → `Migration` error
  - storage_epoch mismatch → `UnsupportedEpoch` error
  - embed_dim == 0 → `Internal` error
  - Root scope invariant (D9): exactly one scope with `id=1, parent_id=None, label="root"` → `Internal` error if missing
- [ ] Tests: each validation case including missing/malformed root scope

**4c: `restore_snapshot_into(conn, snapshot) -> Result<()>`**

FK insertion order within a single transaction:
1. Delete auto-inserted root scope (`DELETE FROM scopes WHERE id = 1`)
2. Insert scopes (sorted by depth, then id → respects parent FK)
3. Insert events (depends on scope_id)
4. Insert facts (depends on scope_id, source_event_id)
5. Insert edges (depends on source_fact_id, target_fact_id)
6. Insert summaries (depends on scope_id)
7. Import config keys (except `schema_version`, `storage_epoch`)
8. Reset `sqlite_sequence` for each table to max(id)
9. Commit

All inserts use explicit IDs. Embeddings converted via `serialize_embedding()`.

- [ ] Tests:
  - Empty snapshot → success (just root scope + config)
  - Full round-trip: create engine → add data → dump → restore → verify equality
  - Version mismatch → rejected
  - Non-empty target DB → `Conflict` error
  - Autoincrement reset: new inserts after restore get IDs > max imported

### Task 6: `MemoryEngine` restore methods

**File:** `src/engine.rs`

- [ ] `restore_json(snapshot_path, config)`:
  1. Check `config.path` does not exist → `Conflict`
  2. `read_snapshot(snapshot_path)`
  3. Validate `config.embed_dim` matches `snapshot.embed_dim` → `EmbeddingDimension` error
  4. `ConnectionPool::open(&config.path, config.embed_dim, config.read_pool_size, config.backup_dir)` — creates DB
  5. `restore_snapshot_into(&conn, &snapshot)` in write lock
  6. On error after DB creation: `std::fs::remove_file(&config.path)` cleanup (D7)
  7. `init_from_pool(pool, config.embed_dim, config.search_config, config.upcaster_registry, None)` — preserves full config
- [ ] `restore_json_memory(snapshot_path)`:
  Same but `ConnectionPool::open_memory(snapshot.embed_dim)`, no cleanup needed
- [ ] `restore_sqlite(backup_path, config)`:
  1. Check `config.path` does not exist → `Conflict`, `backup_path` exists → `NotFound`
  2. `std::fs::copy(backup_path, &config.path)` — only safe for `dump_sqlite()` output (no WAL)
  3. Probe copied DB: validate `embed_dim` matches `config.embed_dim`
  4. On mismatch: cleanup copied file, return `EmbeddingDimension` error
  5. `MemoryEngine::open(config)` — uses full config (pool size, search config, upcasters)
- [ ] Tests: round-trip for each method, error cases, cleanup on failure

### Task 7: Async mirrors

**File:** `src/async_engine.rs`

- [ ] 3 async methods wrapping sync via `spawn_blocking` with owned `PathBuf`
- [ ] Return `AsyncMemoryEngine::new(sync_engine)`

### Task 8: Integration test

**File:** `tests/restore_roundtrip.rs`

- [ ] Create engine with facts + edges + summaries in multiple scopes
- [ ] Dump JSON → restore to file engine → verify all data + engine is functional
- [ ] Dump JSON → restore to memory engine → verify
- [ ] Dump SQLite → restore → verify
- [ ] Compressed round-trips (feature-gated)
- [ ] Post-restore: add new fact, verify ID doesn't collide with imported IDs

### Task 9: Documentation (What / Why / How)

**Files to update:**

**`docs/usage/inspection.md`** — Expand "Dump State" section into "Import/Export" section:

- [ ] **What:** Rename section to "Import/Export" (or add "Restore" subsection). Document the 3 restore methods (`restore_json`, `restore_json_memory`, `restore_sqlite`) alongside existing `dump_state`. Document new `DumpFormat::JsonGzip`/`JsonZstd` variants. Document the `EngineConfig` parameter on file-backed restore methods.
- [ ] **Why:** Long-running agents need backup/restore for disaster recovery, migration between machines, and offline analysis. The event-sourced architecture (ADR-0001) makes full-state export/import natural — the `EngineSnapshot` captures the complete materialized view. Compressed exports reduce storage costs for archival (referenced in Phase 4 design: "10 agents × 10 years" cold storage scenario). Research context: link to context adaptation research (ACE, AWM — see `docs/design/plans/2026-03-09-future-phases-design.md § Phase 4`) which emphasizes that memory systems must be _portable_ and _inspectable_.
- [ ] **How:** Usage examples for each method — JSON round-trip, compressed round-trip, SQLite restore. Document constraints: restore targets fresh engine only (no merge), `restore_sqlite` only accepts `dump_sqlite()` output (clean VACUUM INTO, no WAL sidecars). Show `EngineConfig` usage for preserving pool/search/upcaster settings.
- [ ] Add format comparison table update: add gzip/zstd rows with compression ratio estimates

**`docs/reference/api.md`** — Lifecycle table:

- [ ] Add `restore_json`, `restore_json_memory`, `restore_sqlite` to the Lifecycle method table
- [ ] Document `DumpFormat` variants including `JsonGzip`/`JsonZstd`
- [ ] Note: `#[non_exhaustive]` on `DumpFormat` — downstream match must use wildcard

**`docs/ROADMAP.md`**:

- [ ] Update Phase 4a import/export status to Done/In Progress
- [ ] Reference the implementation plan in `docs/design/plans/`

**`docs/design/plans/`**:

- [ ] Move the final plan (this file) to `docs/design/plans/2026-03-19-phase4a-import-export.md` as the canonical design record

**`docs/design/research-basis.md`** (if applicable):

- [ ] No changes needed — import/export is an operational concern, not research-driven. The research link is through observability (already referenced in inspection.md).

---

## Key Implementation Details

**Insertion SQL for facts (example):**
```sql
INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created,
  t_expired, t_valid, t_invalid, source_event_id, importance, access_count,
  last_accessed, metadata, scope_id, is_pinned, importance_score)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
```

**Autoincrement reset:**
```sql
UPDATE sqlite_sequence SET seq = ?1 WHERE name = ?2
```
Where `?1` = `max(id)` from imported data for each table.

**Compression detection:**
```rust
fn detect_compression(path: &Path) -> Result<Compression> {
    let mut buf = [0u8; 4];
    File::open(path)?.read_exact(&mut buf)?;  // use read() not read_exact for small files
    match buf {
        [0x1f, 0x8b, ..] => Ok(Compression::Gzip),
        [0x28, 0xb5, 0x2f, 0xfd] => Ok(Compression::Zstd),
        _ => Ok(Compression::None),
    }
}
```

**Config skip list:** `["schema_version", "storage_epoch"]` — these are managed by `init_schema`/`migrate`, not imported from snapshot.

---

## Cross-Model Review Changelog

**Round 1** — Codex (gpt-5.4) + Gemini (2.5-pro) persistent tmux sessions.

| # | Source | Sev | Finding | Resolution |
|---|--------|-----|---------|------------|
| C1 | Codex | BLOCKER | Restore constructors drop `EngineConfig` semantics (pool size, search config, upcasters) | **Fixed:** D2 updated — restore methods now accept `EngineConfig`, pass through to `ConnectionPool::open` and `init_from_pool` |
| C2 | Codex | HIGH | Orphan DB file on import failure blocks retry | **Fixed:** D7 updated — cleanup DB file on error before returning |
| C3 | Codex | HIGH | `std::fs::copy` unsafe for live WAL databases | **Fixed:** D6 updated — constrain to `dump_sqlite()` output only, document |
| C4 | Codex | MEDIUM | Older snapshots lack serde defaults for newer `Event` fields | **Fixed:** New D10 + Task 2 — add `#[serde(default)]` to `origin_node_id`, `sequence_id` |
| C5 | Codex | MEDIUM | Root scope invariant not validated | **Fixed:** New D9 — validate root scope `id=1, parent_id=None` in snapshot |
| C6 | Codex | LOW | No CHANGELOG file exists | **Fixed:** Removed CHANGELOG task, docs-only update |
| G1-G7 | Gemini | various | Surface-level findings already addressed by plan (D1, D2, D3, D7) | No changes needed — plan already covered these |

---

## Verification

```bash
# Build with all features
cargo build --all-features

# Run all tests including feature-gated
cargo test --all-features

# Run specific restore tests
cargo test restore --all-features

# Clippy
cargo clippy --all-features -- -D warnings

# Verify compressed export produces correct magic bytes
cargo test dump_json_gzip --features compress-gzip
cargo test dump_json_zstd --features compress-zstd
```
