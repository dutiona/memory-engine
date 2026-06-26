//! `FilterSql` — a rendered SQL boolean fragment plus its bound parameters.
//!
//! The shared `search/{fts,vector}` cores accept this opaque fragment so a single
//! base query serves both the verbatim active-only path and the richer
//! `FactFilter` translation. Two builders feed it:
//!
//! - [`FilterSql::active`] (here) — the active-only fragment the verbatim
//!   `fts_search`/`vector_search` wrappers use; mirrors the historical
//!   `t_expired IS NULL (+ fact_type + scope)` SQL byte-for-byte in behavior.
//! - `convert::build_filter_sql` (backend) — the full `temporal`/`ids`/`pinned`/
//!   `metadata` translation (#684), owned by the backend's dialect layer.
//!
//! Keeping the *carrier* here (rather than in the backend's `convert`) lets the
//! shared `search/` cores depend on it without inverting the layering.

use rusqlite::ToSql;

use crate::error::Result;
use crate::search::serialize_scope_ids;
use crate::store::facts::fact_type_to_str;
use crate::types::FactType;

/// A SQL boolean expression (a `WHERE`-clause body) and its positional params.
///
/// `where_clause` is a complete boolean expression suitable to drop straight
/// after `WHERE`/`AND`. It is **never empty** — an unconstrained filter renders
/// the literal `1` (always-true), so callers can always interpolate it without a
/// special case. Every `?` placeholder in it binds, in order, from `params`.
pub struct FilterSql {
    /// Boolean expression with anonymous `?` placeholders (never empty).
    pub where_clause: String,
    /// Positional bind values — one per `?` in `where_clause`, in order.
    pub params: Vec<Box<dyn ToSql>>,
}

impl FilterSql {
    /// Build the **active-only** fragment: `t_expired IS NULL`, optionally
    /// AND-ed with a single `fact_type` and/or a `scope_ids` membership test.
    ///
    /// `prefix` is the column qualifier the host query needs (`"f."` for the
    /// FTS5 join, `""` for the bare-table vector scan). This reproduces exactly
    /// the predicate set the historical hand-written SQL honored, so the verbatim
    /// `fts_search`/`vector_search` wrappers stay behavior-identical.
    ///
    /// The `scope_ids` convention is preserved: `Some(empty)` serializes to an
    /// empty `json_each`, which **matches nothing** (distinct from `None`, which
    /// omits the clause entirely).
    ///
    /// # Errors
    /// Propagates [`serialize_scope_ids`] failure (unreachable for `&[i64]`).
    pub fn active(
        prefix: &str,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<Self> {
        let mut preds = vec![format!("{prefix}t_expired IS NULL")];
        let mut params: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(ft) = fact_type {
            preds.push(format!("{prefix}fact_type = ?"));
            params.push(Box::new(fact_type_to_str(ft).to_string()));
        }
        if let Some(json) = serialize_scope_ids(scope_ids)? {
            preds.push(format!(
                "{prefix}scope_id IN (SELECT value FROM json_each(?))"
            ));
            params.push(Box::new(json));
        }
        Ok(Self {
            where_clause: preds.join(" AND "),
            params,
        })
    }

    /// Borrow the params as `&dyn ToSql`, ready for `rusqlite::params_from_iter`.
    pub(crate) fn bind_refs(&self) -> impl Iterator<Item = &dyn ToSql> {
        self.params.iter().map(AsRef::as_ref)
    }
}
