//! The deferred-embedding drain's two steps (#1021): take the next item, then settle
//! the embedder's answer for it. The drain loop itself lives with its consumer.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{Connection, MAIN_DB};

use crate::error::{EmbedderError, MemoryError, Result, StorageError};
use crate::types::{
    EmbedQueueStatus, EmbeddingFailure, EmbeddingFingerprint, NextEmbedding, PendingEmbedding,
};

use super::MemoryEngine;

/// The per-item retry schedule: the delay before attempt `n` doubles from `base` and is
/// capped at `cap`, so retries go on however long the outage lasts.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use memory_engine::Backoff;
///
/// let backoff = Backoff::DEFAULT;
/// assert_eq!(backoff.delay(0), Duration::ZERO);
/// assert_eq!(backoff.delay(3), Duration::from_secs(4));
/// assert_eq!(backoff.delay(40), Duration::from_secs(60));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// The delay before the first retry.
    pub base: Duration,
    /// The longest delay.
    pub cap: Duration,
}

impl Backoff {
    /// 1 s doubling to 60 s: an outage of hours costs about one probe a minute.
    pub const DEFAULT: Self = Self {
        base: Duration::from_secs(1),
        cap: Duration::from_secs(60),
    };

    /// The delay before attempt `n`: none for `0`, else `min(cap, base·2^(n−1))`,
    /// saturating at `cap`.
    #[must_use]
    pub fn delay(self, n: u32) -> Duration {
        if n == 0 {
            return Duration::ZERO;
        }
        2_u32
            .checked_pow(n - 1)
            .and_then(|factor| self.base.checked_mul(factor))
            .map_or(self.cap, |delay| delay.min(self.cap))
    }
}

/// What [`MemoryEngine::settle_embedding`] did with an answer; the drain paces itself by
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The item has its vector (or already had one) and left the queue.
    Embedded,
    /// The embedder refused this input (400, 413, 422): the item failed, for good.
    Rejected,
    /// No usable answer for this item, or a vector the store refused: only it backs off.
    Retrying,
    /// No request reached the embedder: no item changed, the item stays due.
    Unreachable,
    /// The embedder rejected the key (401, 403): the queue is paused, no item changed.
    KeyRejected,
}

impl MemoryEngine {
    /// The item the embedding drain should take next (#1021): `Due` now, the time the
    /// earliest waiting item is `NotBefore`, or `Idle` when none waits.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on a read failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{MemoryEngine, NextEmbedding};
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let engine = MemoryEngine::builder(4).build()?;
    /// assert_eq!(engine.next_embedding().await?, NextEmbedding::Idle);
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn next_embedding(&self) -> Result<NextEmbedding> {
        self.storage.next_embedding(Utc::now()).await
    }

    /// Settle the embedder's `result` for `item`, a step of the embedding drain (#1021).
    ///
    /// | `result` | queue | returns |
    /// |---|---|---|
    /// | a vector | filled, never over another vector; a vector the store refuses (width, identity) backs the item off with that error | `Embedded` (`Retrying` if refused) |
    /// | `Embedder(Unreachable)` | no item changes | `Unreachable` |
    /// | `Embedder(KeyRejected)` | no item changes, the queue is paused | `KeyRejected` |
    /// | `Embedder(InputRejected)` | the item fails | `Rejected` |
    /// | any other error | only the item backs off, by `backoff.delay(attempts + 1)` | `Retrying` |
    ///
    /// An answered request clears the pause. A vector whose post-commit index update
    /// fails is stored, so it is `Embedded` (logged), never retried.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` on a read-only engine,
    /// `MemoryError::EmbeddingReopenRequired` on a fenced one, or a store error; the
    /// embedder's own failures are outcomes, not errors.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{
    ///     AddFactRequest, Backoff, EmbeddingFingerprint, FactType, MemoryEngine, NextEmbedding,
    ///     Settled,
    /// };
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let engine = MemoryEngine::builder(4).build()?;
    /// let id = engine
    ///     .add_fact_deferred(&AddFactRequest {
    ///         content: "embed me later".into(),
    ///         fact_type: FactType::Semantic,
    ///         source_event_id: None,
    ///         scope: None,
    ///         opts: None,
    ///     })
    ///     .await?;
    ///
    /// let NextEmbedding::Due(item) = engine.next_embedding().await? else {
    ///     panic!("the fact waits for its vector");
    /// };
    /// assert_eq!(item.fact_id, id);
    /// // A real drain asks its embedding provider here.
    /// let vector = Ok(vec![1.0, 0.0, 0.0, 0.0]);
    /// let fingerprint = EmbeddingFingerprint::new("model", "provider", 4);
    /// let settled = engine
    ///     .settle_embedding(&item, vector, &fingerprint, Backoff::DEFAULT)
    ///     .await?;
    /// assert_eq!(settled, Settled::Embedded);
    /// assert_eq!(engine.next_embedding().await?, NextEmbedding::Idle);
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn settle_embedding(
        &self,
        item: &PendingEmbedding,
        result: Result<Vec<f32>>,
        fingerprint: &EmbeddingFingerprint,
        backoff: Backoff,
    ) -> Result<Settled> {
        self.ensure_open()?;
        self.ensure_writable()?;
        let now = Utc::now();
        let error = match result {
            Ok(vector) => match self
                .storage
                .complete_embedding(item.fact_id, &vector, fingerprint, self.embed_dim, now)
                .await
            {
                Ok(()) => return Ok(Settled::Embedded),
                Err(MemoryError::IndexInconsistent { fact_id, detail }) => {
                    tracing::warn!(
                        fact_id,
                        %detail,
                        "embedded, but the vector index must be rebuilt"
                    );
                    return Ok(Settled::Embedded);
                }
                Err(
                    refused @ (MemoryError::EmbeddingDimension { .. }
                    | MemoryError::EmbeddingModelMismatch { .. }),
                ) => refused,
                Err(store) => return Err(store),
            },
            Err(error) => error,
        };
        let (failure, settled) = match &error {
            MemoryError::Embedder(EmbedderError::Unreachable(_)) => (
                EmbeddingFailure::Hold { paused: false },
                Settled::Unreachable,
            ),
            MemoryError::Embedder(EmbedderError::KeyRejected(_)) => (
                EmbeddingFailure::Hold { paused: true },
                Settled::KeyRejected,
            ),
            MemoryError::Embedder(EmbedderError::InputRejected(_)) => {
                (EmbeddingFailure::Fail, Settled::Rejected)
            }
            _ => {
                let delay = backoff.delay(item.attempts.saturating_add(1));
                let at = TimeDelta::from_std(delay)
                    .ok()
                    .and_then(|delay| now.checked_add_signed(delay))
                    .unwrap_or(DateTime::<Utc>::MAX_UTC);
                (EmbeddingFailure::Retry { at }, Settled::Retrying)
            }
        };
        self.storage
            .record_embedding_failure(item.fact_id, &error.to_string(), failure, now)
            .await?;
        Ok(settled)
    }
}

/// Report the embedding queue of the store at `db` without building an engine (#1021).
///
/// Opens one `SQLite` connection with `SQLITE_OPEN_READ_ONLY` and a busy timeout, checks
/// that it is read-only, and checks the schema version without migrating, so it runs
/// beside a live server and changes nothing; it takes no store lock (ADR-0020 §8).
///
/// # Errors
///
/// Returns `MemoryError::Migration` for a store whose schema is older or newer than this
/// build's (an older one names the migration it needs), `MemoryError::Internal` if the
/// connection is not read-only, or `MemoryError::Storage` if the store cannot be read.
///
/// # Examples
///
/// ```
/// use memory_engine::{AddFactRequest, FactType, MemoryEngine};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("memory.db");
/// let mut engine = MemoryEngine::builder(4).path(&db).build()?;
/// engine
///     .add_fact_deferred(&AddFactRequest {
///         content: "embed me later".into(),
///         fact_type: FactType::Semantic,
///         source_event_id: None,
///         scope: None,
///         opts: None,
///     })
///     .await?;
/// engine.close().await?;
///
/// let status = memory_engine::read_embed_queue_status(&db)?;
/// assert_eq!((status.waiting, status.failed), (1, 0));
/// assert_eq!(status.paused_since, None);
/// # Ok::<(), memory_engine::MemoryError>(())
/// # }).unwrap();
/// ```
pub fn read_embed_queue_status(db: &Path) -> Result<EmbedQueueStatus> {
    let conn = open_status_connection(db)?;
    crate::store::embed_queue::read_status(&conn)
}

fn open_status_connection(db: &Path) -> Result<Connection> {
    let conn = crate::store::schema::open_connection_read_only(&db.to_string_lossy())?;
    if !conn.is_readonly(MAIN_DB).map_err(StorageError::backend)? {
        return Err(MemoryError::Internal(
            "status needs a read-only connection".into(),
        ));
    }
    crate::store::schema::validate_schema_version(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use chrono::Utc;
    use rusqlite::OptionalExtension as _;

    use super::{Backoff, Settled};
    use crate::error::{EmbedderError, MemoryError, Result};
    use crate::store::embed_queue::{
        LAST_ERROR, LAST_ERROR_AT, LAST_SUCCESS_AT, PAUSE_ERROR, PAUSED_SINCE,
    };
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, EmbeddingFingerprint, FactType, NextEmbedding};
    use crate::{MemoryEngine, MemoryQuery};

    const DIM: usize = 4;
    const NORTH: [f32; DIM] = [1.0, 0.0, 0.0, 0.0];

    /// A provider whose every answer the test scripts.
    struct Scripted {
        model: &'static str,
        answer: fn(&str) -> Result<Vec<f32>>,
    }

    impl EmbeddingProvider for Scripted {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            (self.answer)(text)
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new(self.model, "scripted", DIM)
        }
    }

    const fn answers(answer: fn(&str) -> Result<Vec<f32>>) -> Scripted {
        Scripted {
            model: "scripted",
            answer,
        }
    }

    const NORTH_ANSWER: Scripted = answers(|_| Ok(NORTH.to_vec()));
    const KEY_401: Scripted = answers(|_| Err(EmbedderError::KeyRejected("401".into()).into()));
    const ANSWER_503: Scripted = answers(|_| Err(EmbedderError::Failed("503".into()).into()));

    struct Store {
        _dir: tempfile::TempDir,
        path: PathBuf,
        engine: MemoryEngine,
    }

    fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.db");
        let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
        Store {
            _dir: dir,
            path,
            engine,
        }
    }

    async fn deferred(engine: &MemoryEngine, content: &str) -> i64 {
        engine
            .add_fact_deferred(&AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            })
            .await
            .unwrap()
    }

    /// One drain step: take the due item, embed it with `provider`, settle it.
    async fn step(engine: &MemoryEngine, provider: &Scripted) -> (i64, Settled) {
        let NextEmbedding::Due(item) = engine.next_embedding().await.unwrap() else {
            panic!("an item is due");
        };
        let settled = engine
            .settle_embedding(
                &item,
                provider.embed(&item.content),
                &provider.fingerprint(),
                Backoff::DEFAULT,
            )
            .await
            .unwrap();
        (item.fact_id, settled)
    }

    /// `(attempts, next_attempt_at, last_error, failed_at)` of a queue row.
    type Row = (i64, String, Option<String>, Option<String>);

    fn queue_row(path: &Path, id: i64) -> Option<Row> {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT attempts, next_attempt_at, last_error, failed_at
                   FROM embed_queue WHERE fact_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .unwrap()
    }

    async fn config(engine: &MemoryEngine, key: &str) -> Option<String> {
        engine.get_config(key).await.unwrap()
    }

    #[tokio::test]
    async fn an_embedded_result_fills_the_vector_and_leaves_the_queue() {
        let s = store();
        let id = deferred(&s.engine, "the deploy key rotates weekly").await;

        assert_eq!(
            step(&s.engine, &NORTH_ANSWER).await,
            (id, Settled::Embedded)
        );
        assert_eq!(queue_row(&s.path, id), None);
        assert_eq!(
            s.engine.next_embedding().await.unwrap(),
            NextEmbedding::Idle
        );
        let found = s
            .engine
            .execute_query(&MemoryQuery::new().embedding(NORTH.to_vec()))
            .await
            .unwrap();
        assert_eq!(found.results[0].fact.id, id);
        assert_eq!(
            s.engine
                .storage()
                .load_embedding_fingerprint()
                .await
                .unwrap(),
            Some(NORTH_ANSWER.fingerprint()),
            "the first vector records the identity"
        );
        assert!(config(&s.engine, LAST_SUCCESS_AT).await.is_some());
    }

    #[tokio::test]
    async fn a_key_rejection_pauses_and_touches_no_item() {
        let s = store();
        let id = deferred(&s.engine, "a").await;
        let before = queue_row(&s.path, id);

        assert_eq!(step(&s.engine, &KEY_401).await, (id, Settled::KeyRejected));
        assert_eq!(queue_row(&s.path, id), before, "no item state change");
        let since = config(&s.engine, PAUSED_SINCE).await;
        assert!(since.is_some());
        assert_eq!(
            config(&s.engine, PAUSE_ERROR).await.as_deref(),
            Some("embedder: 401")
        );
        assert_eq!(
            config(&s.engine, LAST_ERROR).await.as_deref(),
            Some("embedder: 401")
        );
        assert!(config(&s.engine, LAST_ERROR_AT).await.is_some());

        let key_403 = answers(|_| Err(EmbedderError::KeyRejected("403".into()).into()));
        assert_eq!(step(&s.engine, &key_403).await, (id, Settled::KeyRejected));
        assert_eq!(
            config(&s.engine, PAUSED_SINCE).await,
            since,
            "paused since the first rejection"
        );
        assert_eq!(
            config(&s.engine, PAUSE_ERROR).await.as_deref(),
            Some("embedder: 403")
        );

        assert_eq!(
            step(&s.engine, &NORTH_ANSWER).await,
            (id, Settled::Embedded)
        );
        assert_eq!(config(&s.engine, PAUSED_SINCE).await, None);
        assert_eq!(config(&s.engine, PAUSE_ERROR).await, None);
    }

    #[tokio::test]
    async fn an_input_rejection_fails_only_that_item() {
        let s = store();
        let poison = deferred(&s.engine, "poison").await;
        let other = deferred(&s.engine, "fine").await;
        let rejected = answers(|_| Err(EmbedderError::InputRejected("400".into()).into()));

        assert_eq!(
            step(&s.engine, &rejected).await,
            (poison, Settled::Rejected)
        );
        let (attempts, _, last_error, failed_at) = queue_row(&s.path, poison).unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(last_error.as_deref(), Some("embedder: 400"));
        assert!(failed_at.is_some());
        assert_eq!(
            queue_row(&s.path, other).unwrap().0,
            0,
            "the other is untouched"
        );
        assert_eq!(
            step(&s.engine, &NORTH_ANSWER).await,
            (other, Settled::Embedded)
        );
        assert_eq!(
            s.engine.next_embedding().await.unwrap(),
            NextEmbedding::Idle,
            "a failed item is never taken again"
        );
    }

    #[tokio::test]
    async fn an_unreachable_embedder_touches_no_item() {
        let s = store();
        let id = deferred(&s.engine, "a").await;
        let before = queue_row(&s.path, id);
        let refused = answers(|_| Err(EmbedderError::Unreachable("refused".into()).into()));

        assert_eq!(step(&s.engine, &refused).await, (id, Settled::Unreachable));
        assert_eq!(queue_row(&s.path, id), before);
        assert_eq!(config(&s.engine, PAUSED_SINCE).await, None, "not a pause");
        assert_eq!(
            config(&s.engine, LAST_ERROR).await.as_deref(),
            Some("embedder: refused")
        );
        assert!(
            matches!(s.engine.next_embedding().await.unwrap(), NextEmbedding::Due(item) if item.fact_id == id),
            "the item stays due"
        );
    }

    #[tokio::test]
    async fn a_failed_answer_backs_off_only_that_item() {
        let s = store();
        let first = deferred(&s.engine, "a").await;
        let second = deferred(&s.engine, "b").await;

        let before = Utc::now();
        assert_eq!(
            step(&s.engine, &ANSWER_503).await,
            (first, Settled::Retrying)
        );
        let after = Utc::now();
        let (attempts, next_attempt_at, last_error, failed_at) = queue_row(&s.path, first).unwrap();
        assert_eq!((attempts, failed_at), (1, None));
        assert_eq!(last_error.as_deref(), Some("embedder: 503"));
        let at = chrono::DateTime::parse_from_rfc3339(&next_attempt_at)
            .unwrap()
            .with_timezone(&Utc);
        let one_second = chrono::TimeDelta::seconds(1);
        assert!(before + one_second <= at && at <= after + one_second);

        let not_an_embedder_error = answers(|_| Err(MemoryError::Internal("broken".into())));
        assert_eq!(
            step(&s.engine, &not_an_embedder_error).await,
            (second, Settled::Retrying),
            "another item is due first, and any other error backs off only its item"
        );
        assert_eq!(
            s.engine.next_embedding().await.unwrap(),
            NextEmbedding::NotBefore(at)
        );
    }

    #[tokio::test]
    async fn a_failed_answer_clears_the_pause() {
        let s = store();
        let id = deferred(&s.engine, "a").await;
        let rejected = answers(|_| Err(EmbedderError::InputRejected("422".into()).into()));
        deferred(&s.engine, "b").await;

        step(&s.engine, &KEY_401).await;
        assert_eq!(step(&s.engine, &ANSWER_503).await, (id, Settled::Retrying));
        assert_eq!(config(&s.engine, PAUSED_SINCE).await, None);
        assert_eq!(config(&s.engine, PAUSE_ERROR).await, None);

        step(&s.engine, &KEY_401).await;
        assert_eq!(step(&s.engine, &rejected).await.1, Settled::Rejected);
        assert_eq!(config(&s.engine, PAUSED_SINCE).await, None);
        assert_eq!(config(&s.engine, PAUSE_ERROR).await, None);
    }

    #[tokio::test]
    async fn a_vector_is_never_written_over_another() {
        let s = store();
        let id = deferred(&s.engine, "a").await;
        let NextEmbedding::Due(item) = s.engine.next_embedding().await.unwrap() else {
            panic!("an item is due");
        };
        let fp = NORTH_ANSWER.fingerprint();
        for vector in [NORTH.to_vec(), vec![0.0, 1.0, 0.0, 0.0]] {
            let settled = s
                .engine
                .settle_embedding(&item, Ok(vector), &fp, Backoff::DEFAULT)
                .await
                .unwrap();
            assert_eq!(settled, Settled::Embedded);
        }
        assert_eq!(s.engine.get_fact(id).await.unwrap().embedding, NORTH);
        assert_eq!(queue_row(&s.path, id), None);
    }

    #[tokio::test]
    async fn a_foreign_identity_is_refused_and_the_item_kept() {
        let s = store();
        deferred(&s.engine, "a").await;
        step(&s.engine, &NORTH_ANSWER).await;
        let id = deferred(&s.engine, "b").await;

        let foreign = Scripted {
            model: "another model",
            answer: |_| Ok(NORTH.to_vec()),
        };
        assert_eq!(step(&s.engine, &foreign).await, (id, Settled::Retrying));
        let (attempts, _, last_error, failed_at) = queue_row(&s.path, id).unwrap();
        assert_eq!((attempts, failed_at), (1, None));
        assert!(last_error.unwrap().contains("embedding model mismatch"));
        assert_eq!(s.engine.get_fact(id).await.unwrap().embedding, [0.0_f32; 0]);

        let NextEmbedding::NotBefore(_) = s.engine.next_embedding().await.unwrap() else {
            panic!("the item waits");
        };
        let item = crate::types::PendingEmbedding {
            fact_id: id,
            content: "b".into(),
            attempts: 1,
        };
        let narrow = s
            .engine
            .settle_embedding(
                &item,
                Ok(vec![1.0; DIM - 1]),
                &NORTH_ANSWER.fingerprint(),
                Backoff::DEFAULT,
            )
            .await
            .unwrap();
        assert_eq!(narrow, Settled::Retrying, "a vector of the wrong width too");
        let (attempts, _, last_error, _) = queue_row(&s.path, id).unwrap();
        assert_eq!(attempts, 2);
        assert!(last_error.unwrap().contains("invalid embedding dimension"));
    }

    #[tokio::test]
    async fn an_item_schedule_survives_a_reopen() {
        let Store {
            _dir: dir,
            path,
            mut engine,
        } = store();
        let id = deferred(&engine, "a").await;
        step(&engine, &ANSWER_503).await;
        let scheduled = engine.next_embedding().await.unwrap();
        assert!(matches!(scheduled, NextEmbedding::NotBefore(_)));
        let row = queue_row(&path, id);
        engine.close().await.unwrap();
        drop(engine);

        let reopened = MemoryEngine::builder(DIM).path(&path).build().unwrap();
        assert_eq!(reopened.next_embedding().await.unwrap(), scheduled);
        assert_eq!(queue_row(&path, id), row);
        drop(dir);
    }

    #[tokio::test]
    async fn the_status_connection_is_read_only() {
        let s = store();
        deferred(&s.engine, "a").await;
        let conn = super::open_status_connection(&s.path).unwrap();
        assert!(conn.is_readonly(rusqlite::MAIN_DB).unwrap());
        let err = conn
            .execute("DELETE FROM embed_queue", [])
            .expect_err("a write through the status connection");
        assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::ReadOnly));
        assert_eq!(super::read_embed_queue_status(&s.path).unwrap().waiting, 1);
    }

    #[test]
    fn backoff_doubles_from_base_to_cap() {
        let delays: Vec<u64> = (0..=8)
            .map(|n| Backoff::DEFAULT.delay(n).as_secs())
            .collect();
        assert_eq!(delays, [0, 1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(Backoff::DEFAULT.delay(u32::MAX), Duration::from_secs(60));
    }

    #[cfg(feature = "ann")]
    #[tokio::test]
    async fn a_filled_vector_joins_the_hnsw_index() {
        let dir = tempfile::tempdir().unwrap();
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("ann.db"))
            .search_config(crate::search::strategy::SearchConfig { ann_threshold: 0 })
            .build()
            .unwrap();
        let id = deferred(&engine, "a").await;
        step(&engine, &NORTH_ANSWER).await;

        let found = engine
            .execute_query(&MemoryQuery::new().embedding(NORTH.to_vec()))
            .await
            .unwrap();
        let ids: Vec<i64> = found.results.iter().map(|r| r.fact.id).collect();
        assert_eq!(ids, [id], "the HNSW search proposes it");
    }

    #[cfg(all(feature = "ann", feature = "test-util"))]
    #[tokio::test]
    async fn an_index_inconsistency_after_the_commit_is_embedded() {
        use std::sync::Arc;

        let mut engine = MemoryEngine::builder(DIM).build().unwrap();
        let backend = Arc::new(
            crate::storage::sqlite::SqliteBackend::from_pool(
                Arc::new(crate::pool::ConnectionPool::open_memory(DIM).unwrap()),
                Arc::new(crate::store::upcaster::UpcasterRegistry::new()),
            )
            .with_search_config(crate::search::strategy::SearchConfig { ann_threshold: 0 })
            .unwrap(),
        );
        engine.storage = backend.clone();
        let id = deferred(&engine, "a").await;
        backend.break_hnsw_index();

        assert_eq!(step(&engine, &NORTH_ANSWER).await, (id, Settled::Embedded));
        assert_eq!(
            engine.get_fact(id).await.unwrap().embedding,
            NORTH,
            "committed"
        );
        assert_eq!(
            engine.next_embedding().await.unwrap(),
            NextEmbedding::Idle,
            "never retried"
        );
    }
}
