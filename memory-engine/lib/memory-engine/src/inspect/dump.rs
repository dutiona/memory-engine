use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;
use serde::Serializer as _;
use serde::ser::SerializeMap as _;

use crate::error::{ConflictError, MemoryError, Result};
use crate::inspect::types::{EmbeddingSpaceSnapshot, FactVectorSnapshot};
use crate::store::UpcasterRegistry;
use crate::store::edges::EdgeStore;
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::lineage::LineageStore;
use crate::store::schema::{get_config, list_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;

/// Build a sibling temporary path for atomic write-then-rename.
///
/// # Errors
///
/// Returns [`MemoryError::Internal`] if `path` has no final component — i.e.
/// `Path::file_name()` is `None`, which happens for the root `/`, `..`, and `.`
/// (a trailing slash is normalized away, so `/var/data/` still reports `data`).
/// Falling back to an empty file name in that case would silently produce a
/// `.tmp` sibling in the *parent* directory rather than next to the intended
/// target, breaking the atomic write-then-rename invariant.
fn tmp_path(path: &Path) -> Result<std::path::PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        MemoryError::Internal(format!("dump path has no file name: {}", path.display()))
    })?;
    let mut name = name.to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

/// Create (or truncate) the dump's temporary write target, refusing to follow a
/// symlink at the leaf.
///
/// The JSON dumps write to a sibling `<path>.tmp` and then atomically
/// `rename` it onto `path`. `rename` does not follow a symlink at the
/// destination (it replaces the link), but the create of the `.tmp` leaf would,
/// so an attacker who pre-plants `<path>.tmp` as a symlink could redirect the
/// write outside the intended directory. On Unix we open with `O_NOFOLLOW`, so
/// such a leaf fails the open *atomically* — closing the check-then-use (TOCTOU)
/// window that an out-of-band `symlink_metadata` probe would leave open
/// (CWE-59 / CWE-367; part of the #296 / #354 / #414 hardening). The
/// caller-facing path guard in `memory-engine-mcp` confines the destination to
/// the temp directory; this is the in-engine backstop at the open site.
///
/// On non-Unix targets `O_NOFOLLOW` is unavailable, so this degrades to a plain
/// create and the mcp-level `symlink_metadata` rejection remains the guard.
fn create_dump_tmp(tmp: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NOFOLLOW`: the open fails with `ELOOP` if the final path component
        // is a symlink. The flag value is arch-specific (e.g. `0x20000` on
        // x86_64 but `0x8000` on aarch64), so we take it from `libc`, which the
        // toolchain resolves correctly per target. A hand-maintained constant
        // hardcoded the x86_64 value and silently disabled the guard on ARM64
        // Linux (CWE-59 / CWE-367; part of the #296 / #354 / #414 hardening).
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(tmp)
    }
    #[cfg(not(unix))]
    {
        File::create(tmp)
    }
}

/// Mint a fresh, empty, server-named sibling temp file as the `VACUUM INTO`
/// target, never following a symlink at the leaf.
///
/// `VACUUM INTO` follows a symlink at its destination leaf, so writing it
/// directly at the caller-supplied `path` leaves a TOCTOU window (CWE-59 /
/// CWE-367): the mcp-level guard lstat-rejects a symlink leaf, but an attacker
/// with write access to the directory can race a symlink into place *after* the
/// check and *before* the `VACUUM INTO`. We close that by VACUUM-ing into an
/// **unpredictably-named** sibling created with `O_NOFOLLOW | O_EXCL`
/// (`create_new`) — the open fails atomically if the name is a symlink or
/// already exists — and then atomically `rename` it onto `path`. `rename`
/// replaces a destination symlink rather than following it, so the final move is
/// safe; this mirrors the symlink-safe JSON write-then-rename path.
///
/// `VACUUM INTO` accepts an *empty* target file (`SQLite` requirement: the
/// `INTO` file must not previously exist or must be empty), so the empty file
/// minted here is a valid `VACUUM` destination.
///
/// On non-Unix targets `O_NOFOLLOW` is unavailable, so this degrades to a plain
/// `create_new` (still `O_EXCL` + an unpredictable name); the mcp-level
/// `symlink_metadata` rejection remains the guard there.
fn mint_sqlite_vacuum_tmp(dir: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(".dump-")
        .suffix(".db.tmp")
        .make_in(dir, |p| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(p)
            }
            #[cfg(not(unix))]
            {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(p)
            }
        })
}

/// Stream engine state as JSON to `writer`, one entity at a time.
///
/// Produces the same JSON format as [`super::types::EngineSnapshot`] but never
/// holds more than one entity in memory per collection.  Peak memory drops from
/// O(total entities) to O(1), making this suitable for databases with 100K+
/// facts.
fn stream_snapshot<W: Write>(conn: &Connection, embed_dim: usize, writer: &mut W) -> Result<()> {
    let registry = UpcasterRegistry::new(); // raw events, no upcasting

    let schema_version: u32 = get_config(conn, "schema_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let storage_epoch: u16 = get_config(conn, "storage_epoch")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // Embedding-space registry (#622): the identity left the `config` table for its own
    // table, so dump it explicitly or a restore would come up with no identity. Always
    // small (one active row today), so materialize it directly.
    let spaces: Vec<EmbeddingSpaceSnapshot> = crate::store::embedding_spaces::list_spaces(conn)?
        .into_iter()
        .map(|s| EmbeddingSpaceSnapshot {
            name: s.name,
            status: s.status.as_sql().to_string(),
            fingerprint: s.fingerprint,
        })
        .collect();

    // Config is always small — materialize directly.
    let config = list_config(conn)?;

    // Serialize the whole snapshot object through serde's `SerializeMap` rather
    // than hand-assembling JSON braces and field names. Field names and ordering
    // now track [`EngineSnapshot`]'s serde representation by construction: each
    // `serialize_entry` key below mirrors a field there, so a rename/reorder of
    // the struct that drifts from this writer is caught here, not only by the
    // `streaming_output_matches_snapshot_schema` round-trip test.
    //
    // The collections are serialized row-by-row via [`SeqStreamer`] adapters, so
    // peak memory stays O(1) per collection — the same streaming guarantee the
    // previous `write!`/`stream_for_each` code provided.
    //
    // INVARIANT: the three leading scalar entries (`schema_version`,
    // `storage_epoch`, `embed_dim`) MUST stay first, in this order, before the
    // streamed collections. `memory-engine-cli`'s `import` peeks `embed_dim` from
    // the head of the snapshot under a small byte cap (`peek_embed_dim_from_reader`)
    // to auto-detect the dimension; moving `embed_dim` after `facts`/`edges`/…
    // would push it past that cap and silently break auto-detection on large
    // snapshots. A `SerializeMap` refactor must preserve this entry ORDER, not just
    // the names.
    let mut ser = serde_json::Serializer::new(&mut *writer);
    let mut map = ser.serialize_map(None)?;

    map.serialize_entry("schema_version", &schema_version)?;
    map.serialize_entry("storage_epoch", &storage_epoch)?;
    map.serialize_entry("embed_dim", &embed_dim)?;

    map.serialize_entry(
        "facts",
        &SeqStreamer::new(|cb| FactStore::new(conn, embed_dim).for_each(cb)),
    )?;
    map.serialize_entry(
        "edges",
        &SeqStreamer::new(|cb| EdgeStore::new(conn).for_each(cb)),
    )?;
    map.serialize_entry(
        "summaries",
        &SeqStreamer::new(|cb| SummaryStore::new(conn, embed_dim).for_each(cb)),
    )?;
    map.serialize_entry(
        "scopes",
        &SeqStreamer::new(|cb| ScopeStore::new(conn).for_each(cb)),
    )?;
    map.serialize_entry(
        "events",
        &SeqStreamer::new(|cb| EventStore::new(conn, &registry).for_each(cb)),
    )?;
    map.serialize_entry(
        "lineage",
        &SeqStreamer::new(|cb| LineageStore::new(conn).for_each(cb)),
    )?;

    map.serialize_entry("embedding_spaces", &spaces)?;

    // fact_vectors (#623): the non-active spaces' per-fact vectors (a populating
    // space mid-reconstruction, or a deprecated space retained for rollback).
    // Streamed row-by-row — a deprecated space holds one vector per fact, so this
    // can be O(N). The active vectors are already in `facts[].embedding`.
    map.serialize_entry(
        "fact_vectors",
        &SeqStreamer::new(|mut cb| {
            crate::store::fact_vectors::for_each(conn, embed_dim, |fact_id, space_id, embedding| {
                cb(FactVectorSnapshot {
                    fact_id,
                    space_id,
                    embedding,
                })
            })
        }),
    )?;

    map.serialize_entry("config", &config)?;

    map.end()?;

    writer.flush()?;
    Ok(())
}

/// A [`Serialize`] adapter that streams a JSON array out of a `for_each`-style
/// callback without materializing the whole collection.
///
/// `iterate` is invoked once during serialization; it must call the closure it
/// is handed exactly once per entity. Each entity is serialized into the
/// sequence immediately and then dropped, so only one `T` is live at a time —
/// preserving the O(1)-per-collection peak-memory guarantee of the snapshot
/// dump.
///
/// # Error bridging
///
/// The store callbacks fail with [`MemoryError`], whereas serde's sequence API
/// fails with the serializer's own `S::Error`. The two are reconciled inside
/// [`Serialize::serialize`]: a serde element error is stashed and re-raised as
/// the true cause, and a store/DB error is funneled through
/// [`serde::ser::Error::custom`].
struct SeqStreamer<T, F> {
    iterate: std::cell::RefCell<Option<F>>,
    _item: std::marker::PhantomData<fn(T)>,
}

impl<T, F> SeqStreamer<T, F>
where
    F: FnOnce(Box<dyn FnMut(T) -> Result<()> + '_>) -> Result<()>,
{
    fn new(iterate: F) -> Self {
        Self {
            iterate: std::cell::RefCell::new(Some(iterate)),
            _item: std::marker::PhantomData,
        }
    }
}

impl<T, F> Serialize for SeqStreamer<T, F>
where
    T: Serialize,
    F: FnOnce(Box<dyn FnMut(T) -> Result<()> + '_>) -> Result<()>,
{
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::Error as _;
        use serde::ser::SerializeSeq as _;

        // `serialize` takes `&self`, but the `FnOnce` consumer must be moved out;
        // serde calls `serialize` exactly once per value, so the `Option` is
        // always `Some` here.
        let iterate = self
            .iterate
            .borrow_mut()
            .take()
            .expect("SeqStreamer::serialize is called exactly once by serde");

        let mut seq = serializer.serialize_seq(None)?;

        // Stash any serde element error so it can be re-raised as the real cause
        // after `iterate` is aborted via the sentinel `MemoryError` below.
        let mut ser_err: Option<S::Error> = None;
        let iter_result = iterate(Box::new(|item: T| {
            if let Err(e) = seq.serialize_element(&item) {
                ser_err = Some(e);
                // Abort `for_each` early; the value is a placeholder — the real
                // error is the captured `ser_err`.
                return Err(MemoryError::Internal(
                    "snapshot element serialization failed".to_string(),
                ));
            }
            Ok(())
        }));

        if let Some(e) = ser_err {
            // A serde serialization error: surface the true serializer error.
            return Err(e);
        }
        // Otherwise a genuine store/DB error (or success). Funnel a store error
        // through serde's error type.
        iter_result.map_err(S::Error::custom)?;

        seq.end()
    }
}

/// Dump engine state to a JSON file.
///
/// Streams entities one-by-one via [`stream_snapshot`], keeping peak memory
/// constant regardless of database size.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
pub fn dump_json(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path)?;
    let file = create_dump_tmp(&tmp)?;
    let mut buf = BufWriter::new(file);
    match stream_snapshot(conn, embed_dim, &mut buf) {
        Ok(()) => {
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Dump engine state to a gzip-compressed JSON file.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
#[cfg(feature = "compress-gzip")]
pub fn dump_json_gzip(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path)?;
    let file = create_dump_tmp(&tmp)?;
    let mut encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
    match stream_snapshot(conn, embed_dim, &mut encoder) {
        Ok(()) => {
            encoder.finish()?;
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Dump engine state to a zstd-compressed JSON file.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem or zstd I/O failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
#[cfg(feature = "compress-zstd")]
pub fn dump_json_zstd(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path)?;
    let file = create_dump_tmp(&tmp)?;
    let mut encoder = zstd::Encoder::new(BufWriter::new(file), zstd::DEFAULT_COMPRESSION_LEVEL)?;
    match stream_snapshot(conn, embed_dim, &mut encoder) {
        Ok(()) => {
            encoder.finish()?;
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Create an atomic `SQLite` backup via `VACUUM INTO`.
///
/// Works for both file-backed and in-memory databases (`SQLite` 3.27+).
///
/// # Symlink safety
///
/// `path` is a caller-controlled destination, but the symlink-leaf race that
/// `VACUUM INTO` would otherwise open is closed: rather than VACUUM-ing straight
/// at `path` (which follows a symlink at the leaf), we VACUUM into an
/// unpredictably-named sibling temp file created with `O_NOFOLLOW | O_EXCL`
/// (see [`mint_sqlite_vacuum_tmp`]) and then atomically `rename` it onto `path`.
/// `rename` *replaces* a destination symlink rather than following it, so an
/// attacker who swaps `path` to a symlink between the guards and the move cannot
/// redirect the write outside the directory (CWE-59 / CWE-367; #296 / #354 /
/// #414 hardening — symmetric with the JSON write-then-rename path).
///
/// Residual: this does not confine *where* `path` itself points (a caller can
/// still name a destination outside any sandbox); the mcp-level
/// `validate_dump_path` guard handles that containment. The live-DB and
/// directory checks below remain best-effort probes that turn the common
/// *mistakes* into clear typed errors.
///
/// # Errors
///
/// Returns [`MemoryError::Conflict`] with
/// [`ConflictError::DumpTargetIsLiveDatabase`] if `path` resolves to the live
/// database file (refusing to overwrite the source) or
/// [`ConflictError::DumpTargetIsDirectory`] if `path` is an existing directory;
/// [`MemoryError::Io`] on filesystem failures; or [`MemoryError::Internal`] if
/// the database path cannot be read, `path` contains a null byte, or
/// `VACUUM INTO` fails.
pub fn dump_sqlite(conn: &Connection, path: &Path) -> Result<()> {
    // Detect whether this is a file-backed database.
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MemoryError::Internal(format!("cannot read database path: {e}")))?;

    let is_file_backed = !db_path.is_empty() && db_path != ":memory:";

    // Guard: refuse to dump onto the live database file (or a symlink resolving to it).
    if is_file_backed {
        let source = std::fs::canonicalize(&db_path)?;
        if let Ok(target) = std::fs::canonicalize(path)
            && target == source
        {
            return Err(MemoryError::Conflict(
                ConflictError::DumpTargetIsLiveDatabase,
            ));
        }
    }

    // Best-effort *mistake* guard (not a security boundary): reject an existing
    // directory at `path` with a clear typed error instead of letting the final
    // `rename` fail opaquely. `is_dir()` follows symlinks, so this also refuses a
    // symlink that resolves to a directory. A regular file or absent leaf is the
    // normal case and is replaced atomically by the rename below.
    match std::fs::symlink_metadata(path) {
        Ok(_) if path.is_dir() => {
            return Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory));
        }
        Ok(_) => {} // regular file / file-symlink: replaced by rename
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // absent: nothing to do
        Err(e) => return Err(MemoryError::Io(e)),
    }

    // The rename target dir is `path`'s parent; the temp must be a sibling there
    // so the final `rename` is intra-directory (atomic, no cross-device copy).
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));

    // VACUUM INTO a fresh, symlink-safe, server-named sibling temp, then
    // atomically rename it onto `path`. This closes the symlink-leaf TOCTOU
    // (`VACUUM INTO` follows a destination symlink; `rename` replaces it).
    let tmp = mint_sqlite_vacuum_tmp(dir).map_err(MemoryError::Io)?;
    let tmp_path = tmp.path().to_path_buf();

    let escaped = tmp_path.to_string_lossy().replace('\'', "''");
    if escaped.contains('\0') {
        return Err(MemoryError::Internal(
            "dump temp path contains null byte".to_string(),
        ));
    }
    let sql = format!("VACUUM INTO '{escaped}'");
    conn.execute_batch(&sql)
        .map_err(|e| MemoryError::Internal(format!("VACUUM INTO failed: {e}")))?;

    // Atomic publish: `persist` renames the temp onto `path`, replacing any
    // existing file (or symlink leaf) without following it. On failure the
    // `NamedTempFile` drop removes the temp, leaving `path` untouched.
    tmp.persist(path).map_err(|e| MemoryError::Io(e.error))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::inspect::types::{DumpFormat, EngineSnapshot};
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    #[tokio::test]
    async fn json_dump_roundtrip() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "test fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        // Deserialize and verify
        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();
        assert_eq!(snapshot.facts.len(), 1);
        assert_eq!(snapshot.facts[0].content, "test fact");
        assert_eq!(snapshot.embed_dim, DIM);
        assert!(!snapshot.scopes.is_empty()); // root scope
    }

    /// Verify that streaming produces valid JSON that round-trips through
    /// `EngineSnapshot` deserialization — the format contract with restore.
    #[tokio::test]
    async fn streaming_output_matches_snapshot_schema() {
        use crate::store::schema::{init_schema, migrate, open_memory};
        use crate::store::serialize_embedding;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        // Insert test data via raw SQL to avoid MemoryEngine dependencies.
        // Timestamps must be RFC3339 — SQLite's datetime('now') produces a
        // format that the row mappers cannot parse.
        let emb = serialize_embedding(&[0.1, 0.2, 0.3, 0.4]);
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type,
                    t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
             VALUES ('alpha', 'h1', ?1, 'semantic', ?2, ?2, '{}', 1, 0, 0.0)",
            rusqlite::params![emb, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type,
                    t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
             VALUES ('beta', 'h2', ?1, 'episodic', ?2, ?2, '{}', 1, 0, 0.0)",
            rusqlite::params![emb, now],
        )
        .unwrap();

        // Stream to an in-memory buffer.
        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        // Must deserialize cleanly into EngineSnapshot.
        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("streaming output must be valid EngineSnapshot");

        assert_eq!(snapshot.facts.len(), 2);
        assert_eq!(snapshot.edges.len(), 0);
        assert!(!snapshot.scopes.is_empty());
        assert_eq!(snapshot.embed_dim, DIM);
        assert!(snapshot.schema_version > 0);
    }

    /// Verify empty database produces a valid (empty) snapshot.
    #[tokio::test]
    async fn streaming_empty_database() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("empty streaming output must be valid");

        assert_eq!(snapshot.facts.len(), 0);
        assert_eq!(snapshot.edges.len(), 0);
        assert_eq!(snapshot.summaries.len(), 0);
        assert!(!snapshot.scopes.is_empty()); // root scope
    }

    /// Stress test: 10K facts streamed to buffer. Verifies correctness at scale.
    #[tokio::test]
    async fn streaming_10k_facts_roundtrips() {
        use crate::store::schema::{init_schema, migrate, open_memory};
        use crate::store::serialize_embedding;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let emb = serialize_embedding(&[0.1, 0.2, 0.3, 0.4]);
        let now = chrono::Utc::now().to_rfc3339();

        // Batch-insert 10K facts via raw SQL for speed.
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..10_000 {
            conn.execute(
                "INSERT INTO facts (content, content_hash, embedding, fact_type,
                        t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
                 VALUES (?1, ?2, ?3, 'semantic', ?4, ?4, '{}', 1, 0, 0.0)",
                rusqlite::params![format!("fact-{i}"), format!("h-{i}"), emb, now],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("10K streaming output must be valid");
        assert_eq!(snapshot.facts.len(), 10_000);
        assert_eq!(snapshot.embed_dim, DIM);
    }

    #[tokio::test]
    async fn sqlite_dump_from_in_memory() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "in-memory fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dump.db");
        engine
            .dump_state(&DumpFormat::Sqlite(db_path.clone()))
            .await
            .unwrap();

        // Verify the dump is a valid SQLite database with our data.
        let dump_conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = dump_conn
            .query_row("SELECT count(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[cfg(feature = "compress-gzip")]
    #[tokio::test]
    async fn gzip_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "gzip test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.gz");
        engine
            .dump_state(&DumpFormat::JsonGzip(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 2, "file too small");
        assert_eq!(bytes[0], 0x1f, "gzip magic byte 0");
        assert_eq!(bytes[1], 0x8b, "gzip magic byte 1");
    }

    #[cfg(feature = "compress-zstd")]
    #[tokio::test]
    async fn zstd_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "zstd test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.zst");
        engine
            .dump_state(&DumpFormat::JsonZstd(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 4, "file too small");
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");
    }

    #[tokio::test]
    async fn snapshot_empty_engine_dump() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_slice(&buf).unwrap();

        insta::assert_yaml_snapshot!(snapshot, {
            ".config" => insta::sorted_redaction(),
        });
    }

    #[tokio::test]
    async fn snapshot_populated_engine_dump() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "snapshot fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();

        insta::assert_yaml_snapshot!(snapshot, {
            ".facts[].t_created" => "[timestamp]",
            ".facts[].last_accessed" => "[timestamp]",
            ".facts[].embedding" => "[embedding]",
            ".facts[].content_hash" => "[hash]",
            ".events" => "[]",
            ".config" => "{}",
        });
    }

    /// L6 hardening: a mistaken or hostile dump target that is a directory must
    /// be refused with a clear typed error, not the opaque `remove_file`-on-a-
    /// directory I/O failure the bare `path.exists()` check produced before.
    #[tokio::test]
    async fn dump_sqlite_refuses_directory_target() {
        use crate::store::schema::{init_schema, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = dump_sqlite(&conn, dir.path());

        assert!(
            matches!(
                result,
                Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory))
            ),
            "dumping onto a directory must be refused with DumpTargetIsDirectory, got {result:?}"
        );
    }

    /// `tmp_path` must build a `.tmp` sibling next to a normal target path.
    #[test]
    fn tmp_path_builds_sibling_for_normal_path() {
        let p = Path::new("/var/data/dump.json");
        let tmp = tmp_path(p).unwrap();
        assert_eq!(tmp, Path::new("/var/data/dump.json.tmp"));
    }

    /// `tmp_path` must reject paths with no final component instead of silently
    /// degrading to a `.tmp` file in the parent directory (which would break the
    /// atomic write-then-rename invariant). The paths that yield
    /// `Path::file_name() == None` are the root `/`, `..`, and `.` (Rust
    /// normalizes a trailing slash away, so `/var/data/` still reports `data`).
    #[test]
    fn tmp_path_rejects_paths_without_file_name() {
        for raw in ["/", "..", "."] {
            let result = tmp_path(Path::new(raw));
            assert!(
                matches!(result, Err(MemoryError::Internal(_))),
                "path {raw:?} with no file name must be rejected, got {result:?}"
            );
        }
    }

    /// Issue #352 invariant: `embed_dim` must appear as a leading scalar in the
    /// streamed header, before the large streamed collections, so the CLI's
    /// byte-capped `peek_embed_dim_from_reader` can find it. After the move to a
    /// `serialize_map`-based writer, assert both that the first three keys are the
    /// expected scalars in order AND that `embed_dim` sits within a tiny byte cap.
    #[tokio::test]
    async fn streaming_header_keeps_embed_dim_leading() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();

        let sv = text
            .find("\"schema_version\"")
            .expect("schema_version present");
        let se = text
            .find("\"storage_epoch\"")
            .expect("storage_epoch present");
        let ed = text.find("\"embed_dim\"").expect("embed_dim present");
        let facts = text.find("\"facts\"").expect("facts present");

        assert!(
            sv < se && se < ed && ed < facts,
            "header field order must be schema_version < storage_epoch < embed_dim < facts; \
             got sv={sv} se={se} ed={ed} facts={facts} in: {head}",
            head = &text[..text.len().min(120)]
        );

        // The object must literally begin with `schema_version`.
        assert!(
            text.starts_with(r#"{"schema_version":"#),
            "snapshot must begin with schema_version, got: {head}",
            head = &text[..text.len().min(80)]
        );

        // Defense-in-depth against the CLI byte cap: `embed_dim` must sit well
        // within the first 64 bytes for an empty DB (its real-world position is
        // fixed regardless of collection size since it precedes them).
        assert!(
            ed < 64,
            "embed_dim must stay near the head (byte {ed}); the CLI peek caps its scan"
        );
    }

    /// A symlink resolving to a directory must also be refused — `is_dir()`
    /// follows the link, so the guard is not bypassed by indirection.
    #[cfg(unix)]
    #[tokio::test]
    async fn dump_sqlite_refuses_symlink_to_directory() {
        use crate::store::schema::{init_schema, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real_dir");
        std::fs::create_dir(&real_dir).unwrap();
        let link = dir.path().join("link_to_dir");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let result = dump_sqlite(&conn, &link);
        assert!(
            matches!(
                result,
                Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory))
            ),
            "a symlink to a directory must be refused, got {result:?}"
        );
    }

    /// `O_NOFOLLOW` backstop (#296 / #354 / #414): if the sibling `<path>.tmp`
    /// write target is a pre-planted symlink, `dump_json` must fail at the open
    /// rather than follow the link and clobber its target. This closes the
    /// in-engine TOCTOU window beneath the mcp-level path guard.
    #[cfg(unix)]
    #[tokio::test]
    async fn dump_json_refuses_symlinked_tmp_leaf() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dump.json");

        // Pre-plant the `.tmp` write target as a symlink pointing at a file we
        // must not be tricked into overwriting.
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"do not clobber").unwrap();
        let tmp = tmp_path(&target).unwrap();
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();

        let result = dump_json(&conn, DIM, &target);
        assert!(
            result.is_err(),
            "dump_json must refuse to follow a symlinked .tmp leaf, got {result:?}"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"do not clobber",
            "the symlink target must NOT have been overwritten"
        );
    }

    /// `SQLite`-path symlink safety (#296 / #354 / #414): if the final dump
    /// target is a pre-planted symlink to a file we must not clobber,
    /// `dump_sqlite` must not follow it. It VACUUM-s into a fresh
    /// `O_NOFOLLOW | O_EXCL` sibling temp and `rename`s onto the target —
    /// `rename` replaces the symlink (operating on the link, not its referent),
    /// so the victim stays intact and the target path becomes a real `SQLite`
    /// file. This closes the `VACUUM INTO` symlink-leaf TOCTOU that the mcp
    /// lstat-reject alone left racy.
    #[cfg(unix)]
    #[tokio::test]
    async fn dump_sqlite_replaces_symlinked_target_without_following() {
        use crate::store::schema::{init_schema, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"do not clobber").unwrap();

        // The dump target is a symlink pointing at the victim.
        let target = dir.path().join("dump.db");
        std::os::unix::fs::symlink(&victim, &target).unwrap();

        dump_sqlite(&conn, &target).expect("dump must succeed by replacing the symlink");

        // The victim referent is untouched: the write did not follow the link.
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"do not clobber",
            "the symlink target must NOT have been overwritten"
        );
        // The target is now a real (non-symlink) SQLite file containing the dump.
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "dump target must be a regular file after the rename, not a symlink"
        );
        let dump_conn = rusqlite::Connection::open_with_flags(
            &target,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("dumped target must be a valid SQLite database");
        let n: i64 = dump_conn
            .query_row("SELECT count(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "fresh in-memory db dumps an empty facts table");
    }
}
