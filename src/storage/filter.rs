//! The closed `SearchIndex` filter — replaces the inline JSON1 filtering the
//! `SQLite` store writes by hand (`json_each` / `json_type` / `json_extract`).
//!
//! **Closed by design** (no general JSON-path / predicate language — YAGNI). Each
//! backend translates it to its dialect: `SQLite` to JSON1, Postgres to `jsonb`
//! operators (`?`, `->>`, `@>`). `None` on an optional field means "no constraint
//! on this dimension".

use chrono::{DateTime, Utc};

use crate::types::FactType;

/// A closed, declarative filter over the fact table, consumed by the
/// `SearchIndex` retrieval methods.
///
/// `metadata` is an AND-list (every predicate must hold; empty = no metadata
/// constraint). This is deliberately the *search* predicate set — list/scan knobs
/// (`min_importance`, `limit`, ordering) stay explicit params on `FactGraph`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FactFilter {
    /// Restrict to a single fact type; `None` = any type.
    pub fact_type: Option<FactType>,
    /// Restrict to facts in these scope ids; `None` = any scope.
    /// (`SQLite`: `scope_id IN (SELECT value FROM json_each(?))`.)
    pub scope_ids: Option<Vec<i64>>,
    /// Restrict to these fact ids; `None` = no id constraint.
    /// (`SQLite`: `id IN (json_each(?))`.)
    pub ids: Option<Vec<i64>>,
    /// Bi-temporal visibility constraint. Defaults to [`TemporalFilter::Active`].
    pub temporal: TemporalFilter,
    /// `Some(true)` = pinned only, `Some(false)` = unpinned only, `None` = either.
    pub pinned: Option<bool>,
    /// AND-list of metadata predicates; empty = no metadata constraint.
    pub metadata: Vec<MetadataPredicate>,
}

impl FactFilter {
    /// An empty filter (`Default`): no constraints, [`TemporalFilter::Active`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict to a single fact type.
    #[must_use]
    pub fn fact_type(mut self, ft: FactType) -> Self {
        self.fact_type = Some(ft);
        self
    }

    /// Restrict to a set of scope ids.
    #[must_use]
    pub fn scope_ids(mut self, ids: impl Into<Vec<i64>>) -> Self {
        self.scope_ids = Some(ids.into());
        self
    }

    /// Restrict to a set of fact ids.
    #[must_use]
    pub fn ids(mut self, ids: impl Into<Vec<i64>>) -> Self {
        self.ids = Some(ids.into());
        self
    }

    /// Set the bi-temporal visibility constraint.
    #[must_use]
    pub fn temporal(mut self, t: TemporalFilter) -> Self {
        self.temporal = t;
        self
    }

    /// Restrict by pinned status.
    #[must_use]
    pub fn pinned(mut self, p: bool) -> Self {
        self.pinned = Some(p);
        self
    }

    /// Append a metadata predicate (AND-combined with any others).
    #[must_use]
    pub fn with_metadata(mut self, p: MetadataPredicate) -> Self {
        self.metadata.push(p);
        self
    }
}

/// Bi-temporal visibility constraint for [`FactFilter`].
///
/// Grounds the four `t_expired`/`t_valid`/`t_invalid` query shapes the store uses
/// today. The SQL each maps to (`SQLite` dialect; Postgres uses native
/// `timestamptz` comparisons) is documented per variant so a backend author can
/// verify parity against the existing `facts.rs` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TemporalFilter {
    /// System-time live rows: `t_expired IS NULL`. The common case — default.
    #[default]
    Active,
    /// Valid at an instant: `(t_valid IS NULL OR t_valid <= t) AND
    /// (t_invalid IS NULL OR t_invalid > t)`.
    AsOf(DateTime<Utc>),
    /// "Due now": `t_valid IS NOT NULL AND t_valid <= now AND
    /// (t_invalid IS NULL OR t_invalid > now)`.
    ValidDue(DateTime<Utc>),
    /// No system-time filter — include expired (soft-deleted) rows.
    IncludeExpired,
}

/// A single metadata predicate for [`FactFilter`] — a **closed** set matching
/// exactly the JSON1 shapes the store uses today (YAGNI on a general language).
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataPredicate {
    /// Key absent: `json_type(metadata, '$.k') IS NULL` (the dream-cycle "not yet
    /// marked" probe).
    KeyAbsent(String),
    /// Key present: `json_extract(metadata, '$.k') IS NOT NULL`.
    KeyPresent(String),
    /// Key equals a JSON value: `json_extract(metadata, '$.k') = ?`.
    KeyEquals(String, serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FactType;

    #[test]
    fn default_filter_is_unconstrained_and_active() {
        let f = FactFilter::default();
        assert!(f.fact_type.is_none() && f.scope_ids.is_none() && f.ids.is_none());
        assert_eq!(f.temporal, TemporalFilter::Active);
        assert!(f.pinned.is_none() && f.metadata.is_empty());
    }

    #[test]
    fn builder_chains_compose() {
        let f = FactFilter::new()
            .fact_type(FactType::Semantic)
            .scope_ids(vec![1, 2])
            .ids(vec![10])
            .temporal(TemporalFilter::IncludeExpired)
            .pinned(true)
            .with_metadata(MetadataPredicate::KeyAbsent("dream_cycle".into()));
        assert_eq!(f.fact_type, Some(FactType::Semantic));
        assert_eq!(f.scope_ids.as_deref(), Some(&[1, 2][..]));
        assert_eq!(f.ids.as_deref(), Some(&[10][..]));
        assert_eq!(f.temporal, TemporalFilter::IncludeExpired);
        assert_eq!(f.pinned, Some(true));
        assert_eq!(f.metadata.len(), 1);
    }

    #[test]
    fn temporal_default_is_active_and_variants_carry_instant() {
        assert_eq!(TemporalFilter::default(), TemporalFilter::Active);
        let t = chrono::Utc::now();
        assert!(matches!(TemporalFilter::AsOf(t), TemporalFilter::AsOf(_)));
        assert!(matches!(
            TemporalFilter::ValidDue(t),
            TemporalFilter::ValidDue(_)
        ));
    }

    #[test]
    fn metadata_predicate_equality_over_all_variants() {
        assert_eq!(
            MetadataPredicate::KeyAbsent("k".into()),
            MetadataPredicate::KeyAbsent("k".into())
        );
        assert_eq!(
            MetadataPredicate::KeyPresent("k".into()),
            MetadataPredicate::KeyPresent("k".into())
        );
        let p = MetadataPredicate::KeyEquals("k".into(), serde_json::json!(42));
        assert!(
            matches!(&p, MetadataPredicate::KeyEquals(k, v) if k == "k" && *v == serde_json::json!(42))
        );
    }
}
