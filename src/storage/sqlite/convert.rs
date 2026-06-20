//! `FactFilter` → verbatim-search-param projection.
//!
//! The [`SearchIndex`](crate::storage::SearchIndex) trait takes a rich
//! [`FactFilter`], but the verbatim `search/{fts,vector}.rs` SQL honors only
//! `fact_type` + `scope_ids` and hard-codes `t_expired IS NULL` (Active). This
//! projects the filter onto what that SQL accepts and **errors loud** if a
//! dimension the SQL cannot honor is set — rather than silently dropping a
//! predicate and returning the wrong rows. Extending the SQL to honor the richer
//! dimensions would be *new behavior* `#630` must not introduce (it is tracked as
//! a separate follow-up).

use crate::error::{MemoryError, Result};
use crate::storage::{FactFilter, TemporalFilter};
use crate::types::FactType;

/// Project a [`FactFilter`] onto the `(fact_type, scope_ids)` the verbatim FTS5 /
/// brute-force vector SQL accepts.
///
/// The `scope_ids` convention round-trips faithfully: `None` = no constraint,
/// `Some(empty)` = matches nothing — passed straight to `serialize_scope_ids`,
/// which the SQL turns into an empty `json_each` (excludes every row).
///
/// # Errors
/// [`MemoryError::Internal`] if `temporal != Active`, or `ids` / `pinned` /
/// `metadata` are set — dimensions the verbatim search SQL does not honor. The
/// engine query path never sets these on a search filter.
pub(super) fn search_params(filter: &FactFilter) -> Result<(Option<FactType>, Option<Vec<i64>>)> {
    if filter.temporal != TemporalFilter::Active
        || filter.ids.is_some()
        || filter.pinned.is_some()
        || !filter.metadata.is_empty()
    {
        return Err(MemoryError::Internal(
            "SqliteBackend search path received a FactFilter dimension the verbatim \
             FTS5/vector SQL does not honor (temporal != Active, ids, pinned, or metadata); \
             the engine query path does not set these on a search filter"
                .into(),
        ));
    }
    Ok((filter.fact_type, filter.scope_ids.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MetadataPredicate;

    #[test]
    fn passes_supported_dimensions() {
        let f = FactFilter::new()
            .fact_type(FactType::Semantic)
            .scope_ids(vec![1, 2]);
        let (ft, scopes) = search_params(&f).unwrap();
        assert_eq!(ft, Some(FactType::Semantic));
        assert_eq!(scopes, Some(vec![1, 2]));
    }

    #[test]
    fn empty_scope_ids_round_trip_preserved() {
        // Some(empty) must stay Some(empty) (matches nothing), NOT normalized to None.
        let f = FactFilter::new().scope_ids(Vec::<i64>::new());
        let (_, scopes) = search_params(&f).unwrap();
        assert_eq!(scopes, Some(vec![]));
    }

    #[test]
    fn rejects_unsupported_dimensions() {
        use chrono::Utc;
        for f in [
            FactFilter::new().temporal(TemporalFilter::IncludeExpired),
            FactFilter::new().temporal(TemporalFilter::AsOf(Utc::now())),
            FactFilter::new().ids(vec![1]),
            FactFilter::new().pinned(true),
            FactFilter::new().with_metadata(MetadataPredicate::KeyPresent("k".into())),
        ] {
            assert!(
                matches!(search_params(&f), Err(MemoryError::Internal(_))),
                "expected Internal for {f:?}"
            );
        }
    }
}
