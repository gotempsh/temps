//! Fast row-count helpers for large append-only hypertables.
//!
//! Listing endpoints paginate with a `total` so the UI can render
//! "page X of N". The straightforward way to get that total is
//! `SELECT COUNT(*)`, but on a TimescaleDB hypertable with tens of
//! millions of rows an *unfiltered* `COUNT(*)` scans every chunk and can
//! take many seconds — even though the user only wanted the first 20 rows.
//!
//! TimescaleDB ships `approximate_row_count(regclass)`, which reads the
//! per-chunk planner statistics (`reltuples`) instead of scanning rows. It
//! is effectively O(number of chunks) and returns in microseconds
//! regardless of table size.
//!
//! ## When the estimate is correct
//!
//! `approximate_row_count` reports the row count of the **whole table** — it
//! cannot apply a `WHERE` clause. It is therefore only valid for the
//! *unfiltered* total. For any filtered query the caller must fall back to an
//! exact count (which is cheap anyway, because the filter is selective and
//! indexed).
//!
//! Use [`count_for_pagination`] rather than calling [`approximate_row_count`]
//! directly: it encodes the "approximate only when unfiltered" rule and
//! degrades gracefully on a plain-Postgres database where the TimescaleDB
//! function does not exist (CI, tests, non-Timescale deployments).

use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, Statement};

/// A row count obtained for pagination, tagged with how it was derived so
/// callers (and tests) can reason about its exactness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountKind {
    /// `COUNT(*)` — exact. Used for filtered queries.
    Exact,
    /// `approximate_row_count()` — planner estimate. Used for the unfiltered
    /// whole-table total on a hypertable.
    Approximate,
}

#[derive(FromQueryResult)]
struct CountRow {
    // `approximate_row_count` returns bigint; it can be negative or NULL on a
    // table that has never been analyzed, so we read it as a nullable i64 and
    // clamp below.
    count: Option<i64>,
}

/// Returns true if `name` is a bare, safe SQL identifier (letters, digits,
/// underscore; not starting with a digit). The table name is interpolated
/// into SQL via `regclass`, so we reject anything that could break out even
/// though every current caller passes a hardcoded literal.
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63 // Postgres identifier limit
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Fast, approximate row count for a TimescaleDB hypertable (whole table, no
/// filter). Reads planner statistics; does not scan rows.
///
/// The result is clamped to `0` (the planner can return a small negative
/// estimate or `NULL` on a freshly-created table with no statistics yet).
///
/// Errors if `table` is not a safe identifier, or if the database does not
/// have the `approximate_row_count` function (e.g. plain Postgres). Callers
/// that must work on both Timescale and plain Postgres should use
/// [`count_for_pagination`], which handles the fallback.
pub async fn approximate_row_count<C: ConnectionTrait>(db: &C, table: &str) -> Result<u64, DbErr> {
    if !is_safe_identifier(table) {
        return Err(DbErr::Custom(format!(
            "approximate_row_count: refusing unsafe table identifier {table:?}"
        )));
    }

    // `$1::regclass` resolves the table name safely as a bind parameter; no
    // string interpolation of the identifier into the SQL text itself.
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT approximate_row_count($1::regclass) AS count",
        [table.into()],
    );

    let row = CountRow::find_by_statement(stmt)
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::Custom(format!(
                "approximate_row_count returned no row for table {table:?}"
            ))
        })?;

    Ok(row.count.unwrap_or(0).max(0) as u64)
}

/// Count rows for a paginated listing, choosing the cheapest correct method.
///
/// * `has_filters == false` → tries [`approximate_row_count`] (instant on a
///   hypertable). If the function is unavailable (plain Postgres), falls back
///   to `exact()`.
/// * `has_filters == true` → always calls `exact()`, because the approximate
///   count cannot honor a `WHERE` clause.
///
/// `exact` is a closure (rather than running the count here) so the caller
/// can reuse the Sea-ORM `Paginator` it already built with all filters
/// applied: `|| async { paginator.num_items().await }`.
///
/// Returns the count plus a [`CountKind`] describing how it was derived.
pub async fn count_for_pagination<C, Fut, F>(
    db: &C,
    table: &str,
    has_filters: bool,
    exact: F,
) -> Result<(u64, CountKind), DbErr>
where
    C: ConnectionTrait,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<u64, DbErr>>,
{
    if has_filters {
        return Ok((exact().await?, CountKind::Exact));
    }

    match approximate_row_count(db, table).await {
        Ok(count) => Ok((count, CountKind::Approximate)),
        Err(e) => {
            // Plain Postgres (no TimescaleDB) won't have the function; fall
            // back to an exact count so listings still work in CI/tests.
            tracing::debug!(
                table,
                error = %e,
                "approximate_row_count unavailable; falling back to exact COUNT(*)"
            );
            Ok((exact().await?, CountKind::Exact))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_table_names() {
        assert!(is_safe_identifier("proxy_logs"));
        assert!(is_safe_identifier("otel_spans"));
        assert!(is_safe_identifier("_private"));
        assert!(is_safe_identifier("t1"));
    }

    #[test]
    fn rejects_injection_and_malformed_names() {
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("1table")); // leading digit
        assert!(!is_safe_identifier("proxy logs")); // space
        assert!(!is_safe_identifier("proxy_logs; DROP TABLE users")); // injection
        assert!(!is_safe_identifier("public.proxy_logs")); // schema-qualified
        assert!(!is_safe_identifier("\"proxy_logs\"")); // quotes
        assert!(!is_safe_identifier(&"x".repeat(64))); // over 63 chars
    }

    #[tokio::test]
    async fn unsafe_identifier_errors_before_querying() {
        // No DB needed: the guard rejects before any statement runs. We use a
        // mock connection that would panic if queried.
        use sea_orm::{DatabaseBackend, MockDatabase};
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let err = approximate_row_count(&db, "bad; DROP")
            .await
            .expect_err("must reject unsafe identifier");
        assert!(matches!(err, DbErr::Custom(_)));
    }

    #[tokio::test]
    async fn approximate_clamps_negative_and_null() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        // Planner can report -1 / NULL on an un-analyzed table.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![maplit_count(Some(-1))],
                vec![maplit_count(None)],
                vec![maplit_count(Some(42))],
            ])
            .into_connection();

        assert_eq!(approximate_row_count(&db, "proxy_logs").await.unwrap(), 0);
        assert_eq!(approximate_row_count(&db, "proxy_logs").await.unwrap(), 0);
        assert_eq!(approximate_row_count(&db, "proxy_logs").await.unwrap(), 42);
    }

    #[tokio::test]
    async fn filtered_always_uses_exact() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        // Mock DB with no query results queued: if the code tried the
        // approximate path it would error on the empty queue. It must not.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let (total, kind) = count_for_pagination(&db, "proxy_logs", true, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(total, 7);
        assert_eq!(kind, CountKind::Exact);
    }

    #[tokio::test]
    async fn unfiltered_falls_back_to_exact_when_function_missing() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        // Empty result queue → the approximate query returns "no row" → Err →
        // fallback to the exact closure.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let (total, kind) = count_for_pagination(&db, "proxy_logs", false, || async { Ok(99) })
            .await
            .unwrap();
        assert_eq!(total, 99);
        assert_eq!(kind, CountKind::Exact);
    }

    // Helper to build a mock count row.
    fn maplit_count(v: Option<i64>) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut m = std::collections::BTreeMap::new();
        m.insert("count".to_string(), sea_orm::Value::BigInt(v));
        m
    }
}
