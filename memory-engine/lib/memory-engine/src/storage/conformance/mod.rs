//! Cross-backend storage conformance battery (#632).
//!
//! Asserts the [`StorageBackend`](crate::storage::StorageBackend) trait CONTRACT
//! (behavior, not SQL) directly against `Arc<dyn StorageBackend>` /
//! `Arc<dyn ColdStorage>` — storage-PORT level, BELOW the engine facade.
//!
//! **Not** the engine-facade `tests/eval/conformance/` (which drives
//! [`MemoryEngine`](crate::MemoryEngine) via `eval_engine()`): that suite pins
//! engine behavior; this one pins the *port* contract so every backend proves the
//! same semantics. Do not conflate them.
//!
//! ## Adding a backend
//!
//! One [`ConformanceBackend`](factory::ConformanceBackend) impl in `factory.rs` +
//! one `mod` block below. Behaviors are written ONCE (generic over the factory) and
//! run against every backend; seed ONLY through the port (`fixtures.rs`), never via
//! the SQLite-private `FactStore` / `pool.write()`. #635 fills the `PgFactory`
//! `todo!()`s and deletes the `[#[ignore]]` token once `PgBackend` is a full
//! `StorageBackend` (#633 added only its `SchemaManager` + pool + migrations) —
//! **zero behavior edits**.
//!
//! ## Excluded (per-backend golden, NOT cross-backend)
//!
//! Lexical/vector score & order parity (tokenizers/rankers differ), HNSW dispatch,
//! `convert.rs` filter→SQL translation, and the `capabilities()` tier VALUES stay in
//! `src/storage/sqlite/*.rs`. This battery asserts only what the contract guarantees
//! identically across backends.

mod atomic;
mod consolidation;
mod event;
mod factory;
mod fixtures;
mod graph;
mod read_only;
mod schema;
mod search;
mod session;

#[cfg(feature = "archive")]
mod cold;

/// Emit one `#[tokio::test]` per `name => body`, recursively. The shared attribute
/// (e.g. `#[ignore]` for the inert PG arm) rides as a `[ … ]` token group so it can
/// be stamped onto every emitted test.
macro_rules! suite_items {
    ($factory:expr, [$(#[$attr:meta])*]) => {};
    ($factory:expr, [$(#[$attr:meta])*] $name:ident => $body:path $(, $($rest:tt)*)?) => {
        $(#[$attr])*
        #[tokio::test]
        async fn $name() { $body(&$factory).await; }
        suite_items!($factory, [$(#[$attr])*] $($($rest)*)?);
    };
}

/// The ONE canonical behavior registry, referenced by every backend's `mod` block.
/// Each `name => path` is one cross-backend contract test.
macro_rules! all_behaviors_into {
    ($factory:expr, [$(#[$attr:meta])*]) => {
        suite_items!($factory, [$(#[$attr])*]
            insert_get_round_trip => graph::insert_get_round_trip,
            write_rejected_reads_succeed => read_only::write_rejected_reads_succeed,
            insert_fact_atomic_rollback_on_dim_mismatch => atomic::insert_fact_atomic_rollback_on_dim_mismatch,
            insert_facts_batch_atomic_length_mismatch => atomic::insert_facts_batch_atomic_length_mismatch,
            insert_facts_batch_atomic_rollback => atomic::insert_facts_batch_atomic_rollback,
            resolve_conflict_atomic_rollback_leaves_old => atomic::resolve_conflict_atomic_rollback_leaves_old_active,
            prune_atomic_rollback => atomic::prune_atomic_rollback,
            apply_cycle_deltas_atomic_rollback => atomic::apply_cycle_deltas_atomic_rollback,
            as_of_returns_historical_excludes_expired => graph::as_of_returns_historical_excludes_expired,
            active_excludes_expired => graph::active_excludes_expired,
            include_expired_surfaces_soft_deleted => graph::include_expired_surfaces_soft_deleted,
            scope_ids_empty_slice_means_all => graph::scope_ids_empty_slice_means_all,
            scope_ids_empty_slice_means_none => graph::scope_ids_empty_slice_means_none,
            filter_scope_ids_some_empty_matches_nothing => graph::filter_scope_ids_some_empty_matches_nothing,
            filter_ids_some_empty_matches_nothing => graph::filter_ids_some_empty_matches_nothing,
            get_missing_yields_not_found => graph::get_missing_yields_not_found,
            metadata_key_recent_rejects_injection => graph::metadata_key_recent_rejects_injection,
            metadata_predicate_present_and_absent => graph::metadata_predicate_present_and_absent,
            pinned_filter_partitions => graph::pinned_filter_partitions,
            edge_insert_get_list_expire => graph::edge_insert_get_list_expire,
            scope_ensure_get_find => graph::scope_ensure_get_find,
            schema_version_and_migrate_idempotent => schema::schema_version_and_migrate_idempotent,
            validate_schema_version_ok_on_fresh => schema::validate_schema_version_ok_on_fresh,
            capabilities_self_consistent => schema::capabilities_self_consistent,
            config_round_trip => schema::config_round_trip,
            set_config_on_read_only_yields_read_only => schema::set_config_on_read_only_yields_read_only,
            fingerprint_record_then_returns_stored => schema::fingerprint_record_if_absent_records_then_returns_stored,
            fingerprint_dim_mismatch_is_embedding_dim => schema::fingerprint_dim_mismatch_is_embedding_dimension,
            fingerprint_model_mismatch_rejected => schema::fingerprint_model_mismatch_rejected,
            require_fingerprint_present_on_fresh_internal => schema::require_present_on_fresh_is_internal,
            event_insert_get_round_trip => event::insert_get_round_trip,
            event_get_missing_yields_not_found => event::get_missing_yields_not_found,
            event_list_count_parity => event::list_count_parity,
            event_for_each_order_and_cb_error => event::for_each_order_and_callback_error,
            event_upcasted_read_is_wired => event::upcasted_read_is_wired,
            event_count_outcome_signals => event::count_outcome_signals_and_batch,
            activity_insert_dedup_get_list_count => session::activity_insert_dedup_get_list_count,
            session_scope_empty_slice_means_none => session::list_recent_activities_by_scope_empty_means_none,
            update_activity_status => session::update_activity_status,
            checkpoint_upsert_get_by_session_scope_recent => session::checkpoint_upsert_get_by_session_scope_recent,
            summary_insert_list_get_delete_by_level => consolidation::summary_insert_list_get_delete_by_level,
            for_each_summary_parity_and_early_exit => consolidation::for_each_summary_parity_and_early_exit,
            lineage_insert_get_has_delete => consolidation::lineage_insert_get_has_delete_sources,
            search_malformed_query_yields_empty => search::malformed_query_yields_empty,
            search_vector_wrong_dim_is_embedding_dim => search::vector_wrong_dim_yields_embedding_dimension,
            search_lexical_count_expired_only_expired => search::lexical_count_expired_counts_only_expired,
            search_lexical_returns_matching_fact => search::lexical_returns_matching_fact,
            search_vector_returns_seeded_fact => search::vector_returns_seeded_fact,
        );
    };
}

/// The `ColdStorage` registry — a SEPARATE, `archive`-gated suite (its bodies
/// reference `Arc<dyn ColdStorage>`, so they cannot appear in the default-build suite).
#[cfg(feature = "archive")]
macro_rules! cold_behaviors_into {
    ($factory:expr, [$(#[$attr:meta])*]) => {
        suite_items!($factory, [$(#[$attr])*]
            manifest_insert_list_oldest_first => cold::manifest_insert_list_oldest_first,
            manifest_delete_existing_and_nonexistent => cold::manifest_delete_existing_and_nonexistent,
            manifest_round_trip_fields => cold::manifest_round_trip_fields,
            commit_archive_atomic_rollback => cold::commit_archive_atomic_rollback,
        );
    };
}

// One mod block per backend. Adding a backend = one block; #635 deletes the
// `[#[ignore = …]]` token to turn the postgres arm on (once `PgBackend` is a full
// `StorageBackend` — #633 added only its `SchemaManager` + pool + migrations).
mod sqlite {
    use super::*;
    all_behaviors_into!(factory::SqliteFactory, []);
}

#[cfg(feature = "backend-postgres")]
mod postgres {
    use super::*;
    all_behaviors_into!(factory::PgFactory, [#[ignore = "#635: PgBackend is not a full StorageBackend until #634 CRUD + #635 SearchIndex"]]);
}

#[cfg(feature = "archive")]
mod sqlite_cold {
    use super::*;
    cold_behaviors_into!(factory::SqliteFactory, []);
}

#[cfg(all(feature = "backend-postgres", feature = "archive"))]
mod postgres_cold {
    use super::*;
    cold_behaviors_into!(factory::PgFactory, [#[ignore = "#635: PgBackend is not a full StorageBackend until #634 CRUD + #635 SearchIndex"]]);
}
