//! Live `PostgreSQL` harness for #633 — exercises `PgBackend`'s pool + fresh v14
//! migration chain + `SchemaManager` against a real Postgres (the `pgvector/pgvector`
//! image, for `CREATE EXTENSION vector`).
//!
//! **Every test is `#[ignore]` by default** so `cargo test --all-features` stays GREEN
//! on a machine with no Docker daemon (mirrors the conformance arm's inert `#[ignore]`
//! and the #621 gated-TEI-smoke-test precedent). Run the live suite with:
//!
//! ```text
//! cargo test -p memory-engine --features backend-postgres -- --ignored
//! ```
//!
//! The `migrated_backend` helper (a `pgvector/pgvector:pg17` container + a migrated
//! `PgBackend`) is the PG-live entry point #634/#635 reuse — and the thing #635 will
//! promote to fill `PgFactory::make()` when it flips the conformance arm.

use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;

use crate::error::MemoryError;
use crate::storage::schema::SchemaManager;
use crate::types::EmbeddingFingerprint;

use super::{PgBackend, pg_err};

/// The harness embedding dimension — matches the conformance `fixtures::DIM`.
const DIM: usize = 4;

/// Start a `pgvector/pgvector:pg17` container and return it + its connection URL. The
/// container handle MUST be kept in scope for the test's lifetime (drop = teardown).
async fn start_pg() -> (ContainerAsync<Postgres>, String) {
    let container = Postgres::default()
        .with_name("pgvector/pgvector")
        .with_tag("pg17")
        .start()
        .await
        .expect("start pgvector container (is the Docker daemon running?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("map container port 5432");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    (container, url)
}

/// A migrated `PgBackend` at [`DIM`] + its container + URL.
async fn migrated_backend() -> (ContainerAsync<Postgres>, String, PgBackend) {
    let (container, url) = start_pg().await;
    let backend = PgBackend::connect(&url, DIM)
        .await
        .expect("connect + migrate to HEAD");
    (container, url, backend)
}

// --- introspection helpers (via the private `with_client` seam) ---

async fn col_strings(be: &PgBackend, sql: &'static str) -> Vec<String> {
    be.with_client(|c| async move {
        let rows = c.query(sql, &[]).await.map_err(pg_err)?;
        Ok(rows
            .iter()
            .map(|r| r.get::<_, String>(0))
            .collect::<Vec<_>>())
    })
    .await
    .expect("column-list query")
}

async fn scalar_string(be: &PgBackend, sql: &'static str) -> Option<String> {
    be.with_client(|c| async move {
        Ok(c.query_opt(sql, &[])
            .await
            .map_err(pg_err)?
            .map(|r| r.get::<_, String>(0)))
    })
    .await
    .expect("scalar-string query")
}

async fn scalar_bool(be: &PgBackend, sql: &'static str) -> bool {
    be.with_client(|c| async move {
        Ok(c.query_one(sql, &[])
            .await
            .map_err(pg_err)?
            .get::<_, bool>(0))
    })
    .await
    .expect("scalar-bool query")
}

/// `(column_name, data_type, is_nullable)` for a table, in column order.
async fn columns(be: &PgBackend, table: &'static str) -> Vec<(String, String, String)> {
    be.with_client(move |c| async move {
        let rows = c
            .query(
                "SELECT column_name, data_type, is_nullable FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
                &[&table],
            )
            .await
            .map_err(pg_err)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                )
            })
            .collect::<Vec<_>>())
    })
    .await
    .expect("columns query")
}

fn type_of<'a>(cols: &'a [(String, String, String)], name: &str) -> Option<&'a str> {
    cols.iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, t, _)| t.as_str())
}

/// Insert a minimal fact via raw SQL (`FactGraph` is #634) and return its id.
async fn insert_fact(be: &PgBackend, content: &str) -> i64 {
    let content = content.to_string();
    be.with_client(move |c| async move {
        let row = c
            .query_one(
                "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed) \
                 VALUES ($1, 'hash', '[0,0,0,0]', 'episodic', now(), now()) RETURNING id",
                &[&content],
            )
            .await
            .map_err(pg_err)?;
        Ok(row.get::<_, i64>(0))
    })
    .await
    .expect("insert fact")
}

// =========================================================================
// Phase 2 — migration chain + the schema-parity keystone (R2, R5, R6)
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn migrate_creates_the_twelve_v14_tables_and_no_fts_vtable() {
    let (_c, _url, be) = migrated_backend().await;
    let tables = col_strings(
        &be,
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .await;
    let expected = [
        "activities",
        "archive_manifest",
        "config",
        "edges",
        "embedding_spaces",
        "events",
        "fact_vectors",
        "facts",
        "lineage",
        "scopes",
        "session_checkpoints",
        "summaries",
    ];
    assert_eq!(
        tables, expected,
        "the 12 v14 logical tables must exist, exactly"
    );
    assert!(
        !tables.iter().any(|t| t == "facts_fts"),
        "there is NO FTS5 virtual table on PG — FTS is a generated tsvector column"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn facts_columns_match_the_v14_logical_shape() {
    let (_c, _url, be) = migrated_backend().await;
    let cols = columns(&be, "facts").await;
    // Native PG types for the load-bearing columns (the #1-trap surface).
    assert_eq!(type_of(&cols, "id"), Some("bigint"), "identity id");
    assert_eq!(type_of(&cols, "content"), Some("text"));
    assert_eq!(
        type_of(&cols, "embedding"),
        Some("USER-DEFINED"),
        "pgvector vector(N)"
    );
    assert_eq!(type_of(&cols, "fact_type"), Some("text"));
    assert_eq!(
        type_of(&cols, "t_created"),
        Some("timestamp with time zone"),
        "timestamptz, not TEXT"
    );
    assert_eq!(type_of(&cols, "importance"), Some("double precision"));
    assert_eq!(
        type_of(&cols, "is_pinned"),
        Some("boolean"),
        "boolean, not INTEGER 0/1"
    );
    assert_eq!(type_of(&cols, "metadata"), Some("jsonb"), "jsonb, not TEXT");
    assert_eq!(
        type_of(&cols, "content_tsv"),
        Some("tsvector"),
        "the generated FTS column"
    );

    // The generated column is GENERATED ALWAYS (replaces the 3 FTS5 sync triggers).
    let is_generated = scalar_string(
        &be,
        "SELECT is_generated FROM information_schema.columns \
         WHERE table_name = 'facts' AND column_name = 'content_tsv'",
    )
    .await;
    assert_eq!(is_generated.as_deref(), Some("ALWAYS"));

    // facts.id is an IDENTITY column (the #209 never-reused-id invariant's PG form).
    let is_identity = scalar_string(
        &be,
        "SELECT is_identity FROM information_schema.columns \
         WHERE table_name = 'facts' AND column_name = 'id'",
    )
    .await;
    assert_eq!(is_identity.as_deref(), Some("YES"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
#[allow(
    clippy::too_many_lines,
    reason = "the body is dominated by the exhaustive 12-table v14 column manifest (data, not logic)"
)]
async fn every_table_has_its_full_v14_column_set() {
    // The full v14 column manifest. A future DDL edit that drops, renames, or adds a
    // column to ANY of the 12 tables fails here — the parity keystone's regression guard.
    // (`facts_columns_match_the_v14_logical_shape` above checks column *types*, but only
    // for `facts`; this checks the full column *set* for every table.)
    const MANIFEST: &[(&str, &[&str])] = &[
        ("scopes", &["id", "parent_id", "label", "depth"]),
        (
            "events",
            &[
                "id",
                "timestamp",
                "event_type",
                "payload",
                "source",
                "session_id",
                "scope_id",
                "origin_node_id",
                "sequence_id",
                "created_at",
                "event_revision",
            ],
        ),
        (
            "facts",
            &[
                "id",
                "content",
                "content_hash",
                "embedding",
                "fact_type",
                "t_created",
                "t_expired",
                "t_valid",
                "t_invalid",
                "source_event_id",
                "importance",
                "access_count",
                "last_accessed",
                "metadata",
                "scope_id",
                "is_pinned",
                "importance_score",
                "surfaced_at",
                "content_tsv",
            ],
        ),
        (
            "edges",
            &[
                "id",
                "source_fact_id",
                "target_fact_id",
                "relation_type",
                "weight",
                "t_created",
                "t_expired",
                "scope_id",
            ],
        ),
        (
            "summaries",
            &[
                "id",
                "content",
                "embedding",
                "level",
                "source_fact_ids",
                "created_at",
                "scope_id",
            ],
        ),
        ("config", &["key", "value"]),
        (
            "archive_manifest",
            &[
                "id",
                "pak_path",
                "created_at",
                "fact_count",
                "edge_count",
                "fact_id_min",
                "fact_id_max",
                "t_created_min",
                "t_created_max",
                "size_bytes",
                "blake3_hash",
            ],
        ),
        (
            "lineage",
            &[
                "lineage_id",
                "wisdom_fact_id",
                "source_fact_ids",
                "provenance",
            ],
        ),
        (
            "activities",
            &[
                "id",
                "session_id",
                "tool_name",
                "args_hash",
                "args",
                "result_summary",
                "outcome_class",
                "status",
                "occurrence_count",
                "first_seen",
                "last_seen",
                "scope_id",
                "promoted_fact_id",
            ],
        ),
        (
            "session_checkpoints",
            &[
                "session_id",
                "scope_path",
                "summary",
                "last_activity_id",
                "checkpoint_at",
                "metadata",
            ],
        ),
        (
            "embedding_spaces",
            &[
                "name",
                "model",
                "provider",
                "dim",
                "matryoshka_base_dim",
                "element_type",
                "status",
                "created_at",
            ],
        ),
        ("fact_vectors", &["fact_id", "space_id", "embedding"]),
    ];
    let (_c, _url, be) = migrated_backend().await;
    for &(table, expected) in MANIFEST {
        let mut got: Vec<String> = columns(&be, table)
            .await
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        got.sort();
        let mut want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
        want.sort();
        assert_eq!(got, want, "column set mismatch for table `{table}`");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn vector_extension_present_and_dim_is_parameterized() {
    let (_c, _url, be) = migrated_backend().await;
    // R5: the pgvector extension is installed.
    assert!(
        scalar_bool(
            &be,
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector')",
        )
        .await,
        "the `vector` extension must be installed"
    );
    // R6: the embedding column's typmod reflects DIM (here 4).
    let fmt = scalar_string(
        &be,
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute \
         WHERE attrelid = 'facts'::regclass AND attname = 'embedding'",
    )
    .await;
    assert_eq!(fmt.as_deref(), Some("vector(4)"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn single_active_embedding_space_partial_unique_index() {
    let (_c, _url, be) = migrated_backend().await;
    // The predicate lives only in pg_catalog (no information_schema view); PG normalizes
    // it to `(status = 'active'::text)`.
    let predicate = scalar_string(
        &be,
        "SELECT pg_get_expr(indpred, indrelid) FROM pg_index \
         WHERE indexrelid = 'idx_embedding_spaces_one_active'::regclass",
    )
    .await
    .expect("the single-active partial index must exist");
    assert!(
        predicate.contains("status = 'active'"),
        "expected a partial index on status='active', got: {predicate}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn fact_vectors_composite_pk_and_cascade_fks() {
    let (_c, _url, be) = migrated_backend().await;
    // Composite PK (fact_id, space_id), in order.
    let pk_cols = col_strings(
        &be,
        "SELECT a.attname FROM pg_index i \
         JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) \
         WHERE i.indrelid = 'fact_vectors'::regclass AND i.indisprimary ORDER BY a.attnum",
    )
    .await;
    assert_eq!(pk_cols, ["fact_id", "space_id"]);
    // Both FKs are ON DELETE CASCADE (confdeltype 'c').
    let cascades = col_strings(
        &be,
        "SELECT confdeltype::text FROM pg_constraint \
         WHERE conrelid = 'fact_vectors'::regclass AND contype = 'f'",
    )
    .await;
    assert_eq!(cascades.len(), 2, "two FKs on fact_vectors");
    assert!(
        cascades.iter().all(|d| d == "c"),
        "both fact_vectors FKs must be ON DELETE CASCADE, got {cascades:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn content_tsv_has_a_gin_index() {
    let (_c, _url, be) = migrated_backend().await;
    let indexdef = scalar_string(
        &be,
        "SELECT indexdef FROM pg_indexes WHERE indexname = 'idx_facts_content_tsv'",
    )
    .await
    .expect("the content_tsv GIN index must exist");
    assert!(
        indexdef.to_lowercase().contains("using gin"),
        "expected a GIN index, got: {indexdef}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn facts_id_identity_never_reuses_ids() {
    // The PG analogue of the SQLite `facts_id_is_autoincrement` guard (#209 cursor).
    let (_c, _url, be) = migrated_backend().await;
    let id1 = insert_fact(&be, "first").await;
    let id2 = insert_fact(&be, "second").await;
    assert!(id2 > id1);
    be.raw_exec(&format!("DELETE FROM facts WHERE id = {id2}"))
        .await
        .expect("delete second");
    let id3 = insert_fact(&be, "third").await;
    assert!(
        id3 > id2,
        "GENERATED ALWAYS AS IDENTITY must not reuse ids: id3={id3} <= id2={id2}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn fk_cascade_deletes_fact_vectors() {
    let (_c, _url, be) = migrated_backend().await;
    let fid = insert_fact(&be, "cascade me").await;
    be.raw_exec(
        "INSERT INTO embedding_spaces (name, model, provider, dim, element_type, status) \
         VALUES ('default', 'm', 'tei', 4, 'float32', 'active')",
    )
    .await
    .expect("seed active space");
    be.raw_exec(&format!(
        "INSERT INTO fact_vectors (fact_id, space_id, embedding) VALUES ({fid}, 'default', '[1,2,3,4]')"
    ))
    .await
    .expect("seed fact_vector");
    be.raw_exec(&format!("DELETE FROM facts WHERE id = {fid}"))
        .await
        .expect("delete fact");
    let remaining = scalar_bool(&be, "SELECT EXISTS (SELECT 1 FROM fact_vectors)").await;
    assert!(
        !remaining,
        "deleting the fact must cascade-delete its fact_vectors row"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn migrate_is_idempotent_at_head() {
    let (_c, _url, be) = migrated_backend().await;
    // Connect already migrated; an explicit re-migrate is a no-op.
    be.migrate().await.expect("re-migrate is idempotent");
    assert_eq!(be.schema_version().await.expect("version"), 1);
    be.validate_schema_version()
        .await
        .expect("validate ok at head");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn rejects_out_of_range_embed_dim() {
    let (_c, url) = start_pg().await;
    // Not `.expect_err` — the Ok variant (`PgBackend`) is intentionally not `Debug`.
    match PgBackend::connect(&url, 0).await {
        Err(MemoryError::Migration(_)) => {}
        Err(other) => panic!("expected a Migration error for a 0 dimension, got {other:?}"),
        Ok(_) => panic!("embed_dim 0 must be rejected"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn reopen_at_a_different_dim_is_rejected() {
    // A store migrated at DIM (=4) has vector(4) columns; reopening at a different dim
    // must be rejected, not silently mis-deserialize vectors (review: gemini + codex).
    let (_c, url, _be) = migrated_backend().await;
    match PgBackend::connect(&url, 8).await {
        Err(MemoryError::EmbeddingDimension {
            expected: 4,
            actual: 8,
        }) => {}
        Err(other) => panic!("expected EmbeddingDimension on RW reopen, got {other:?}"),
        Ok(_) => panic!("reopen (RW) at dim 8 must be rejected"),
    }
    match PgBackend::connect_read_only(&url, 8).await {
        Err(MemoryError::EmbeddingDimension {
            expected: 4,
            actual: 8,
        }) => {}
        Err(other) => panic!("expected EmbeddingDimension on RO reopen, got {other:?}"),
        Ok(_) => panic!("reopen (read-only) at dim 8 must be rejected"),
    }
}

// =========================================================================
// Phase 3 — SchemaManager core: capabilities, config, fingerprint, read-only
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn capabilities_is_the_stock_pg_tier() {
    let (_c, _url, be) = migrated_backend().await;
    let caps = be.capabilities();
    assert_eq!(
        caps.lexical_ranker,
        crate::storage::capabilities::LexicalRanker::TsRankCd
    );
    assert!(caps.server_side_vector);
    assert!(!caps.true_idf);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn config_round_trip() {
    let (_c, _url, be) = migrated_backend().await;
    assert_eq!(be.get_config("missing").await.expect("get"), None);
    be.set_config("k", "v1").await.expect("set");
    assert_eq!(
        be.get_config("k").await.expect("get"),
        Some("v1".to_string())
    );
    be.set_config("k", "v2").await.expect("upsert");
    assert_eq!(
        be.get_config("k").await.expect("get"),
        Some("v2".to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn fingerprint_record_load_and_mismatch_semantics() {
    let (_c, _url, be) = migrated_backend().await;
    assert_eq!(
        be.load_embedding_fingerprint().await.expect("load fresh"),
        None
    );
    // require_present on a fresh store → Internal.
    assert!(matches!(
        be.require_embedding_fingerprint_present().await,
        Err(MemoryError::Internal(_))
    ));

    let fp = EmbeddingFingerprint::new("model-a", "tei", DIM);
    let recorded = be
        .record_embedding_fingerprint_if_absent(&fp, DIM)
        .await
        .expect("record");
    assert_eq!(recorded, fp);
    assert_eq!(
        be.load_embedding_fingerprint().await.expect("load"),
        Some(fp.clone())
    );
    be.require_embedding_fingerprint_present()
        .await
        .expect("present now");

    // Re-record the SAME identity → returns stored (idempotent).
    assert_eq!(
        be.record_embedding_fingerprint_if_absent(&fp, DIM)
            .await
            .expect("re-record"),
        fp
    );
    // A DIFFERENT model at the same dim → EmbeddingModelMismatch (#614).
    let other = EmbeddingFingerprint::new("model-b", "ollama", DIM);
    assert!(matches!(
        be.record_embedding_fingerprint_if_absent(&other, DIM).await,
        Err(MemoryError::EmbeddingModelMismatch { .. })
    ));
    // check_embedding_compatible: matching ok, differing rejected.
    be.check_embedding_compatible(&fp).await.expect("match ok");
    assert!(matches!(
        be.check_embedding_compatible(&other).await,
        Err(MemoryError::EmbeddingModelMismatch { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn fingerprint_dim_guard_on_first_write() {
    let (_c, _url, be) = migrated_backend().await;
    // candidate.dim != expected_dim on a fresh store → EmbeddingDimension, nothing stored.
    let wrong = EmbeddingFingerprint::new("model-a", "tei", 8);
    assert!(matches!(
        be.record_embedding_fingerprint_if_absent(&wrong, DIM).await,
        Err(MemoryError::EmbeddingDimension {
            expected: DIM,
            actual: 8
        })
    ));
    assert_eq!(
        be.load_embedding_fingerprint().await.expect("load"),
        None,
        "nothing persisted on a dim mismatch"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn read_only_backend_rejects_writes_typed_but_reads_succeed() {
    let (_c, url, _rw) = migrated_backend().await; // migrate via the RW handle first
    let ro = PgBackend::connect_read_only(&url, DIM)
        .await
        .expect("open read-only");
    // The Rust-level guard yields the typed ReadOnly variant (R-C).
    assert!(matches!(
        ro.set_config("k", "v").await,
        Err(MemoryError::ReadOnly)
    ));
    assert!(matches!(
        ro.raw_exec("SELECT 1").await,
        Err(MemoryError::ReadOnly)
    ));
    assert!(matches!(ro.migrate().await, Err(MemoryError::ReadOnly)));
    // Reads still work.
    assert_eq!(ro.schema_version().await.expect("read version"), 1);
}

// =========================================================================
// Phase 4 — the stubbed surface returns NotImplemented (deliberate deferrals)
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Postgres testcontainer; run with --ignored"]
async fn deferred_methods_return_not_implemented() {
    let (_c, _url, be) = migrated_backend().await;
    let fp = EmbeddingFingerprint::new("m", "tei", DIM);
    assert!(matches!(
        be.statistics().await,
        Err(MemoryError::NotImplemented(_))
    ));
    assert!(matches!(
        be.begin_populating_space("shadow", &fp).await,
        Err(MemoryError::NotImplemented(_))
    ));
    assert!(matches!(
        be.count_unbackfilled("shadow").await,
        Err(MemoryError::NotImplemented(_))
    ));
    assert!(matches!(
        be.promote_space("shadow").await,
        Err(MemoryError::NotImplemented(_))
    ));
    assert!(matches!(
        be.deprecate_space("shadow").await,
        Err(MemoryError::NotImplemented(_))
    ));
}
