//! `FactFilter` → `FilterSql` translation for the `SQLite` search path.
//!
//! The [`SearchIndex`](me_storage::SearchIndex) trait takes a rich
//! [`FactFilter`]; this renders **all** of its dimensions into a parametrized SQL
//! boolean fragment ([`FilterSql`]) that the shared `search/{fts,vector}` cores
//! drop after `WHERE`/`AND` (#684). Each predicate transcribes the *exact* `SQLite`
//! SQL shape the hand-written store queries already use, so the lexical/vector path
//! stays parity-faithful to the row store:
//!
//! | Dimension              | SQL (oracle in `store/facts.rs`)                          |
//! |------------------------|-----------------------------------------------------------|
//! | `Active`               | `t_expired IS NULL`                                       |
//! | `IncludeExpired`       | *(no system-time predicate)*                              |
//! | `AsOf(t)`              | `t_expired IS NULL AND (t_valid IS NULL OR t_valid <= ?) AND (t_invalid IS NULL OR t_invalid > ?)` |
//! | `ValidDue(t)`          | `t_expired IS NULL AND t_valid IS NOT NULL AND t_valid <= ? AND (t_invalid IS NULL OR t_invalid > ?)` |
//! | `fact_type`            | `fact_type = ?`                                           |
//! | `scope_ids` / `ids`    | `<col> IN (SELECT value FROM json_each(?))`              |
//! | `pinned`               | `is_pinned = ?` (`1`/`0`)                                 |
//! | `KeyAbsent(k)`         | `json_type(metadata, ?) IS NULL`                         |
//! | `KeyPresent(k)`        | `json_extract(metadata, ?) IS NOT NULL`                  |
//! | `KeyEquals(k, v)`      | `json_extract(metadata, ?) = ?`                          |
//!
//! Timestamps bind as RFC3339 strings — the on-disk encoding (`facts.rs` writes
//! `to_rfc3339()`), so lexicographic `<=`/`>` ordering is correct. The JSON path
//! (`$.k`) binds as a **parameter**, not interpolated, closing the injection seam
//! the store left open for its trusted-literal callers.

use rusqlite::ToSql;
use serde_json::Value;

use crate::search::{FilterSql, serialize_scope_ids};
use crate::store::facts::fact_type_to_str;
use me_storage::{FactFilter, MetadataPredicate, TemporalFilter};
use me_types::error::{MemoryError, Result};

/// Translate a [`FactFilter`] into a [`FilterSql`] fragment for the search cores.
///
/// `prefix` qualifies every column reference (`"f."` for the FTS5 join, `""` for
/// the bare-table vector scan). The rendered `where_clause` is never empty (an
/// unconstrained filter — e.g. `IncludeExpired` with no other dimension — renders
/// the literal `1`).
///
/// # Errors
/// - [`MemoryError::Serialization`] if `scope_ids`/`ids` fail to serialize
///   (unreachable for `&[i64]`).
/// - [`MemoryError::Internal`] if a `KeyEquals` carries a null, a non-scalar JSON
///   value (array/object), or an unrepresentable number — `json_extract` equality
///   is defined only for non-null scalars (`= NULL` never matches).
pub(super) fn build_filter_sql(filter: &FactFilter, prefix: &str) -> Result<FilterSql> {
    let mut preds: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    push_temporal(filter.temporal, prefix, &mut preds, &mut params);

    if let Some(ft) = filter.fact_type {
        preds.push(format!("{prefix}fact_type = ?"));
        params.push(Box::new(fact_type_to_str(&ft).to_string()));
    }
    if let Some(ref scope_ids) = filter.scope_ids {
        push_id_membership(
            &format!("{prefix}scope_id"),
            scope_ids,
            &mut preds,
            &mut params,
        )?;
    }
    if let Some(ref ids) = filter.ids {
        push_id_membership(&format!("{prefix}id"), ids, &mut preds, &mut params)?;
    }
    if let Some(pinned) = filter.pinned {
        preds.push(format!("{prefix}is_pinned = ?"));
        params.push(Box::new(i64::from(pinned)));
    }
    for predicate in &filter.metadata {
        push_metadata(predicate, prefix, &mut preds, &mut params)?;
    }

    let where_clause = if preds.is_empty() {
        "1".to_string()
    } else {
        preds.join(" AND ")
    };
    Ok(FilterSql {
        where_clause,
        params,
    })
}

/// Append the bi-temporal visibility predicate (and its params) for `temporal`.
///
/// Transcribes the four shapes documented on [`TemporalFilter`]; `IncludeExpired`
/// contributes no predicate (all rows visible). `AsOf`/`ValidDue` bind their
/// instant twice (once per `t_valid`/`t_invalid` comparison) as an RFC3339 string.
fn push_temporal(
    temporal: TemporalFilter,
    prefix: &str,
    preds: &mut Vec<String>,
    params: &mut Vec<Box<dyn ToSql>>,
) {
    match temporal {
        TemporalFilter::Active => preds.push(format!("{prefix}t_expired IS NULL")),
        TemporalFilter::IncludeExpired => {}
        TemporalFilter::AsOf(t) => {
            // System-time guard included to match the store oracle `list_active_at`
            // (`facts.rs`): AsOf is "active AND valid at t", so soft-deleted rows
            // never surface — only `IncludeExpired` does.
            preds.push(format!(
                "{prefix}t_expired IS NULL \
                 AND ({prefix}t_valid IS NULL OR {prefix}t_valid <= ?) \
                 AND ({prefix}t_invalid IS NULL OR {prefix}t_invalid > ?)"
            ));
            params.push(Box::new(t.to_rfc3339()));
            params.push(Box::new(t.to_rfc3339()));
        }
        TemporalFilter::ValidDue(t) => {
            // Same system-time guard, matching the store oracle `list_due`.
            preds.push(format!(
                "{prefix}t_expired IS NULL \
                 AND {prefix}t_valid IS NOT NULL AND {prefix}t_valid <= ? \
                 AND ({prefix}t_invalid IS NULL OR {prefix}t_invalid > ?)"
            ));
            params.push(Box::new(t.to_rfc3339()));
            params.push(Box::new(t.to_rfc3339()));
        }
    }
}

/// Append a `<col> IN (SELECT value FROM json_each(?))` membership predicate.
///
/// Preserves the `Some(empty)` = matches-nothing contract: an empty slice
/// serializes to `[]`, yielding an empty `json_each` that excludes every row.
fn push_id_membership(
    col: &str,
    ids: &[i64],
    preds: &mut Vec<String>,
    params: &mut Vec<Box<dyn ToSql>>,
) -> Result<()> {
    // `serialize_scope_ids(Some(_))` always yields `Some(json)`, so this `if let`
    // never drops the clause — it just reuses the shared `&[i64] -> json` helper.
    if let Some(json) = serialize_scope_ids(Some(ids))? {
        preds.push(format!("{col} IN (SELECT value FROM json_each(?))"));
        params.push(Box::new(json));
    }
    Ok(())
}

/// Append a metadata predicate, transcribing the store's JSON1 idioms.
///
/// `json_type(...) IS NULL` for **absence** (an absent key is `NULL`; a
/// present-`null` value has type `"null"`), `json_extract(...) IS NOT NULL` for
/// **presence** (collapses absent and present-`null` — the deliberate asymmetry
/// documented in `facts.rs`). The `$.k` path binds as a parameter.
fn push_metadata(
    predicate: &MetadataPredicate,
    prefix: &str,
    preds: &mut Vec<String>,
    params: &mut Vec<Box<dyn ToSql>>,
) -> Result<()> {
    match predicate {
        MetadataPredicate::KeyAbsent(key) => {
            preds.push(format!("json_type({prefix}metadata, ?) IS NULL"));
            params.push(Box::new(json_path(key)));
        }
        MetadataPredicate::KeyPresent(key) => {
            preds.push(format!("json_extract({prefix}metadata, ?) IS NOT NULL"));
            params.push(Box::new(json_path(key)));
        }
        // Present-and-explicitly-null: `json_extract(..) = NULL` is never true, so
        // null equality is `json_type(..) = 'null'` (the 'null' type is returned
        // only for a present JSON null, not for an absent key). The literal is
        // inline (not a bind) — it is a fixed SQL string, not caller input.
        MetadataPredicate::KeyEquals(key, Value::Null) => {
            preds.push(format!("json_type({prefix}metadata, ?) = 'null'"));
            params.push(Box::new(json_path(key)));
        }
        MetadataPredicate::KeyEquals(key, value) => {
            preds.push(format!("json_extract({prefix}metadata, ?) = ?"));
            params.push(Box::new(json_path(key)));
            params.push(value_to_sql(value)?);
        }
    }
    Ok(())
}

/// Render a metadata key as a JSON path expression for the path argument of
/// `json_extract`/`json_type`.
///
/// The key is **double-quoted and escaped** (`$."key"`) so any valid JSON object
/// key works — an unquoted `$.user-id` is a `SQLite` JSON-path syntax error.
/// Quoting is identity-preserving for simple keys, so it stays parity-faithful to
/// the store's unquoted `'$.dream_cycle'` literals.
fn json_path(key: &str) -> String {
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    format!("$.\"{escaped}\"")
}

/// Bind a non-null scalar `serde_json::Value` for comparison against a
/// `json_extract` result, matching how `SQLite` surfaces extracted JSON scalars
/// (text / integer / real; booleans as `1`/`0`).
///
/// `Null`, composite values (array/object), and non-finite numbers are rejected:
/// `= NULL` is never true in SQL, and equality against an extracted JSON scalar is
/// undefined for composites — so failing loud beats silently never matching.
fn value_to_sql(value: &Value) -> Result<Box<dyn ToSql>> {
    match value {
        Value::String(s) => Ok(Box::new(s.clone())),
        Value::Bool(b) => Ok(Box::new(i64::from(*b))),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Ok(Box::new(i)),
            (None, Some(f)) => Ok(Box::new(f)),
            (None, None) => Err(MemoryError::Internal(format!(
                "metadata KeyEquals: unrepresentable JSON number {n}"
            ))),
        },
        // `json_extract(...) = NULL` is never true in SQL, so a bound NULL would
        // silently match nothing. Reject and direct callers to KeyAbsent/KeyPresent,
        // which express null-ness correctly via `json_type`/`json_extract IS NULL`.
        Value::Null => Err(MemoryError::Internal(
            "metadata KeyEquals does not support a null value (`= NULL` never matches in SQL); \
             use KeyAbsent or KeyPresent to test for null/absence"
                .into(),
        )),
        Value::Array(_) | Value::Object(_) => Err(MemoryError::Internal(
            "metadata KeyEquals supports scalar JSON values only (string/number/bool); \
             got a composite value"
                .into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained_active_filter_renders_expiry_only() {
        let sql = build_filter_sql(&FactFilter::new(), "f.").unwrap();
        assert_eq!(sql.where_clause, "f.t_expired IS NULL");
        assert!(sql.params.is_empty());
    }

    #[test]
    fn include_expired_with_no_other_dimension_renders_always_true() {
        let f = FactFilter::new().temporal(TemporalFilter::IncludeExpired);
        let sql = build_filter_sql(&f, "f.").unwrap();
        assert_eq!(sql.where_clause, "1");
        assert!(sql.params.is_empty());
    }

    #[test]
    fn asof_binds_instant_twice() {
        let t = chrono::Utc::now();
        let f = FactFilter::new().temporal(TemporalFilter::AsOf(t));
        let sql = build_filter_sql(&f, "").unwrap();
        assert!(sql.where_clause.contains("t_valid <= ?"));
        assert!(sql.where_clause.contains("t_invalid > ?"));
        assert_eq!(sql.params.len(), 2, "AsOf binds its instant twice");
    }

    #[test]
    fn every_dimension_contributes_exactly_one_clause_and_its_params() {
        let f = FactFilter::new()
            .fact_type(me_types::types::FactType::Semantic)
            .scope_ids(vec![1, 2])
            .ids(vec![10])
            .pinned(true)
            .with_metadata(MetadataPredicate::KeyAbsent("dream_cycle".into()));
        let sql = build_filter_sql(&f, "f.").unwrap();
        // Active expiry + fact_type + scope + ids + pinned + metadata = 6 clauses.
        assert_eq!(sql.where_clause.matches(" AND ").count(), 5);
        assert!(sql.where_clause.contains("f.fact_type = ?"));
        assert!(
            sql.where_clause
                .contains("f.id IN (SELECT value FROM json_each(?))")
        );
        assert!(sql.where_clause.contains("f.is_pinned = ?"));
        assert!(
            sql.where_clause
                .contains("json_type(f.metadata, ?) IS NULL")
        );
        // params: fact_type, scope_json, ids_json, pinned, metadata-path = 5.
        assert_eq!(sql.params.len(), 5);
    }

    #[test]
    fn key_equals_rejects_composite_value() {
        let f = FactFilter::new().with_metadata(MetadataPredicate::KeyEquals(
            "k".into(),
            serde_json::json!([1, 2, 3]),
        ));
        assert!(matches!(
            build_filter_sql(&f, ""),
            Err(MemoryError::Internal(_))
        ));
    }

    #[test]
    fn key_equals_null_renders_json_type_null_predicate() {
        // `= NULL` is never true in SQL; present-null equality is `json_type(..) = 'null'`.
        let f = FactFilter::new().with_metadata(MetadataPredicate::KeyEquals(
            "k".into(),
            serde_json::Value::Null,
        ));
        let sql = build_filter_sql(&f, "").unwrap();
        assert!(
            sql.where_clause.contains("json_type(metadata, ?) = 'null'"),
            "got: {}",
            sql.where_clause
        );
        // Only the path param — the 'null' literal is inline, not bound.
        assert_eq!(sql.params.len(), 1);
    }

    #[test]
    fn json_path_quotes_and_escapes_special_keys() {
        assert_eq!(json_path("dream_cycle"), "$.\"dream_cycle\"");
        assert_eq!(json_path("user-id"), "$.\"user-id\"");
        // Embedded quote/backslash must be escaped, not break out of the path.
        assert_eq!(json_path("a\"b"), "$.\"a\\\"b\"");
        assert_eq!(json_path("a\\b"), "$.\"a\\\\b\"");
    }

    #[test]
    fn empty_scope_ids_round_trips_to_empty_json_each() {
        // Some(empty) must render the membership clause (matches nothing), NOT omit it.
        let f = FactFilter::new().scope_ids(Vec::<i64>::new());
        let sql = build_filter_sql(&f, "").unwrap();
        assert!(
            sql.where_clause
                .contains("scope_id IN (SELECT value FROM json_each(?))")
        );
        assert_eq!(sql.params.len(), 1);
    }
}
