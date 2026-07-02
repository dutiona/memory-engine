//! `impl SchemaManager for PgBackend` (#633).
//!
//! The lifecycle/identity/config core is implemented; inspection (`statistics`,
//! `dump_state`) and the #623 background-reconstruction methods are
//! [`MemoryError::NotImplemented`] stubs — data-manipulation-heavy surfaces that land
//! with the PG data layer (#634/#635). The v14 schema still *creates*
//! `embedding_spaces` + `fact_vectors`, so the stubbed methods' tables exist; only the
//! method bodies are deferred (fillable without touching the struct or trait wiring).
//!
//! The embedding-fingerprint methods reimplement the `store::embedding_meta` /
//! `store::embedding_spaces` logic in PG SQL (R11): those free functions take a
//! `&rusqlite::Connection` and return `MemoryError::Storage` (a `#[from]
//! rusqlite::Error` variant Postgres cannot construct), so PG re-expresses the SQL and
//! maps driver errors via [`pg_err`], while preserving the *exact* semantic variants
//! (`EmbeddingDimension`, `EmbeddingModelMismatch`, `Internal`).

use async_trait::async_trait;
use tokio_postgres::Client;

use crate::error::{MemoryError, Result};
use crate::storage::capabilities::{BackendCapabilities, LexicalRanker};
use crate::storage::schema::SchemaManager;
use crate::store::embedding_spaces::EmbeddingSpace;
use crate::types::{EmbeddingFingerprint, PromoteOutcome};

use super::{PgBackend, migrations, pg_err};

#[async_trait]
impl SchemaManager for PgBackend {
    async fn migrate(&self) -> Result<()> {
        self.run_migrations().await
    }

    async fn schema_version(&self) -> Result<u32> {
        self.with_client(|client| async move {
            let raw = migrations::get_config(&client, "schema_version")
                .await?
                .unwrap_or_else(|| migrations::CURRENT_PG_SCHEMA_VERSION.to_string());
            raw.parse::<u32>().map_err(|_| {
                MemoryError::Internal(format!("invalid schema_version in config: {raw}"))
            })
        })
        .await
    }

    async fn validate_schema_version(&self) -> Result<()> {
        let embed_dim = self.embed_dim;
        self.with_client(move |client| async move {
            migrations::validate_schema_version(&client, embed_dim).await
        })
        .await
    }

    fn capabilities(&self) -> BackendCapabilities {
        // The stock managed-Postgres tier: `ts_rank_cd` lexical (no corpus IDF) and
        // server-side vector search via pgvector. NOTE: `server_side_vector` is a
        // *forward-declaration* of the target tier — #633 ships the `vector(N)` columns,
        // but the HNSW index + `SearchIndex` impl that exercise them land in #635, and a
        // dynamic BM25-extension probe that could upgrade `lexical_ranker` is also #635.
        BackendCapabilities {
            lexical_ranker: LexicalRanker::TsRankCd,
            server_side_vector: true,
            true_idf: false,
        }
    }

    async fn load_embedding_fingerprint(&self) -> Result<Option<EmbeddingFingerprint>> {
        self.with_client(|client| async move { load_fingerprint(&client).await })
            .await
    }

    async fn store_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()> {
        self.read_only_guard()?;
        let fp = fp.clone();
        self.with_client(|client| async move { store_fingerprint(&client, &fp).await })
            .await
    }

    async fn record_embedding_fingerprint_if_absent(
        &self,
        candidate: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<EmbeddingFingerprint> {
        self.read_only_guard()?;
        let candidate = candidate.clone();
        self.with_client(|client| async move {
            // Mirrors `store::embedding_meta::record_if_absent`: write-once, the #614
            // mismatch check on the present-branch, and the dimension guard on the
            // absent-branch. A concurrent first-write race is bounded structurally by the
            // single-active partial unique index (a losing INSERT errors, never corrupts).
            if let Some(stored) = load_fingerprint(&client).await? {
                ensure_match(&stored, &candidate)?;
                return Ok(stored);
            }
            if candidate.dim != expected_dim {
                return Err(MemoryError::EmbeddingDimension {
                    expected: expected_dim,
                    actual: candidate.dim,
                });
            }
            store_fingerprint(&client, &candidate).await?;
            Ok(candidate)
        })
        .await
    }

    async fn require_embedding_fingerprint_present(&self) -> Result<()> {
        let present = self
            .with_client(|client| async move { Ok(load_fingerprint(&client).await?.is_some()) })
            .await?;
        if present {
            Ok(())
        } else {
            Err(MemoryError::Internal(
                "cannot write a pre-computed embedding to a store with no embedding identity; \
                 write a fact first"
                    .into(),
            ))
        }
    }

    async fn check_embedding_compatible(&self, candidate: &EmbeddingFingerprint) -> Result<()> {
        let candidate = candidate.clone();
        self.with_client(|client| async move {
            load_fingerprint(&client)
                .await?
                .map_or_else(|| Ok(()), |stored| ensure_match(&stored, &candidate))
        })
        .await
    }

    async fn get_config(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.with_client(|client| async move { migrations::get_config(&client, &key).await })
            .await
    }

    async fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.read_only_guard()?;
        let key = key.to_string();
        let value = value.to_string();
        self.with_client(
            |client| async move { migrations::set_config(&client, &key, &value).await },
        )
        .await
    }

    async fn write_engine_snapshot(
        &self,
        _graph: crate::types::snapshot::GraphSnapshot,
        _scope_tree: crate::types::snapshot::ScopeTreeSnapshot,
    ) -> Result<bool> {
        // PgBackend has no sidecar snapshot mechanism — the trait doc names this exact
        // case as the `Ok(false)` ("no durable snapshot location") return.
        Ok(false)
    }

    async fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        Err(MemoryError::NotImplemented(
            "PgBackend statistics is not implemented in #633 (inspection lands with the PG data \
             layer — see #759 (PgBackend inspection), under epic #628)"
                .into(),
        ))
    }

    async fn dump_state(
        &self,
        _embed_dim: usize,
        _format: crate::inspect::DumpFormat,
    ) -> Result<()> {
        Err(MemoryError::NotImplemented(
            "PgBackend dump_state is not implemented in #633 (inspection lands with the PG data \
             layer — see #759 (PgBackend inspection), under epic #628)"
                .into(),
        ))
    }

    #[cfg(feature = "test-util")]
    async fn raw_exec(&self, sql: &str) -> Result<()> {
        self.read_only_guard()?;
        let sql = sql.to_string();
        self.with_client(|client| async move { client.batch_execute(&sql).await.map_err(pg_err) })
            .await
    }

    async fn begin_populating_space(
        &self,
        _name: &str,
        _fingerprint: &EmbeddingFingerprint,
    ) -> Result<()> {
        Err(reconstruction_unimplemented("begin_populating_space"))
    }

    async fn next_backfill_window(
        &self,
        _space: &str,
        _after_id: i64,
        _limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        Err(reconstruction_unimplemented("next_backfill_window"))
    }

    async fn write_backfill_batch(
        &self,
        _space: &str,
        _rows: Vec<(i64, Vec<f32>)>,
    ) -> Result<usize> {
        Err(reconstruction_unimplemented("write_backfill_batch"))
    }

    async fn count_unbackfilled(&self, _space: &str) -> Result<usize> {
        Err(reconstruction_unimplemented("count_unbackfilled"))
    }

    async fn promote_space(&self, _populating: &str) -> Result<PromoteOutcome> {
        Err(reconstruction_unimplemented("promote_space"))
    }

    async fn deprecate_space(&self, _name: &str) -> Result<()> {
        Err(reconstruction_unimplemented("deprecate_space"))
    }
}

/// The #623 background-reconstruction methods are deferred to the PG data layer
/// (#635, on the pgvector substrate). The v14 schema already created
/// `embedding_spaces` + `fact_vectors`, so only the method bodies are deferred.
fn reconstruction_unimplemented(method: &str) -> MemoryError {
    MemoryError::NotImplemented(format!(
        "PgBackend::{method} (#623 background-reconstruction mechanism) is not implemented in \
         #633 — it lands with pgvector data CRUD (see #760, PgBackend reconstruction, \
         under epic #628)"
    ))
}

/// Load the single `active` embedding-space identity (the PG analogue of
/// `store::embedding_meta::load` → `embedding_spaces::find_active`). At most one active
/// row exists (the partial unique index), so `query_opt` is exact.
async fn load_fingerprint(client: &Client) -> Result<Option<EmbeddingFingerprint>> {
    let row = client
        .query_opt(
            "SELECT model, provider, dim, matryoshka_base_dim, element_type \
             FROM embedding_spaces WHERE status = 'active'",
            &[],
        )
        .await
        .map_err(pg_err)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let model: String = row.get(0);
    let provider: String = row.get(1);
    let dim_raw: i64 = row.get(2);
    let base_raw: Option<i64> = row.get(3);
    let element_type: String = row.get(4);
    let dim = usize::try_from(dim_raw)
        .map_err(|_| MemoryError::Internal("embedding_spaces.dim is negative/oversized".into()))?;
    let matryoshka_base_dim = base_raw.map(usize::try_from).transpose().map_err(|_| {
        MemoryError::Internal("embedding_spaces.matryoshka_base_dim is negative/oversized".into())
    })?;
    Ok(Some(EmbeddingFingerprint {
        model,
        provider,
        dim,
        matryoshka_base_dim,
        element_type,
    }))
}

/// Persist the active identity (the PG analogue of `embedding_spaces::upsert_active_fingerprint`):
/// re-stamp the active row in place; if none exists, seed the canonical `default` active row.
async fn store_fingerprint(client: &Client, fp: &EmbeddingFingerprint) -> Result<()> {
    let dim = i64::try_from(fp.dim)
        .map_err(|_| MemoryError::Internal("embedding fingerprint dim overflows i64".into()))?;
    let base = fp
        .matryoshka_base_dim
        .map(i64::try_from)
        .transpose()
        .map_err(|_| {
            MemoryError::Internal("embedding fingerprint matryoshka_base_dim overflows i64".into())
        })?;
    let updated = client
        .execute(
            "UPDATE embedding_spaces \
             SET model = $1, provider = $2, dim = $3, matryoshka_base_dim = $4, element_type = $5 \
             WHERE status = 'active'",
            &[&fp.model, &fp.provider, &dim, &base, &fp.element_type],
        )
        .await
        .map_err(pg_err)?;
    if updated == 0 {
        client
            .execute(
                "INSERT INTO embedding_spaces \
                 (name, model, provider, dim, matryoshka_base_dim, element_type, status) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'active')",
                &[
                    &EmbeddingSpace::DEFAULT_NAME,
                    &fp.model,
                    &fp.provider,
                    &dim,
                    &base,
                    &fp.element_type,
                ],
            )
            .await
            .map_err(pg_err)?;
    }
    Ok(())
}

/// Reject a candidate that differs from the stored identity (#614) — field-by-field
/// equality, identical to `store::embedding_meta::ensure_match`.
fn ensure_match(stored: &EmbeddingFingerprint, candidate: &EmbeddingFingerprint) -> Result<()> {
    if stored == candidate {
        return Ok(());
    }
    Err(MemoryError::EmbeddingModelMismatch {
        expected: Box::new(stored.clone()),
        actual: Box::new(candidate.clone()),
    })
}
