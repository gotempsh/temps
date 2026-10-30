// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The read-time half of the per-project datacenter-traffic choice.
//!
//! Ingest flags hosting-provider traffic as excluded when the project's
//! `project_analytics_settings.exclude_datacenter_traffic` is on (the default,
//! also used when the project has no settings row). Some read views go further
//! and drop hosting-provider IPs themselves, which also catches rows recorded
//! before the ASN database was available. Those views must honour the same
//! choice: a project that opted out wants that traffic counted everywhere,
//! not only in the views that trust the ingest flag.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

/// SQL predicate for a query scoped to one project: true when the row should
/// be counted, i.e. its IP is not a hosting provider or the project does not
/// exclude datacenter traffic.
///
/// `geo_alias` is the `ip_geolocations` alias joined to the row, and
/// `project_param` the bind placeholder holding the query's project id. Both
/// are compile-time constants, never user input.
///
/// The placeholder is what keeps this cheap: the settings lookup does not
/// depend on the row, so PostgreSQL evaluates it once (an InitPlan) and the
/// query keeps its parallel plan. Do not pass a column here; for queries that
/// span projects use [`hosting_traffic_counted_unless_opted_out`].
pub fn hosting_traffic_counted(geo_alias: &'static str, project_param: &'static str) -> String {
    format!(
        "(COALESCE({geo_alias}.is_hosting_provider, false) = false \
         OR NOT COALESCE((SELECT pas.exclude_datacenter_traffic \
         FROM project_analytics_settings pas WHERE pas.project_id = {project_param}), true))"
    )
}

/// SQL predicate for a query spanning projects: true when the row's IP is not
/// a hosting provider, or its project is in `opt_outs_param`, an `int[]` bind
/// holding [`projects_counting_datacenter_traffic`].
///
/// A per-row settings lookup (a correlated subquery or a join) would change
/// the row estimate enough for PostgreSQL to give up the parallel plan, which
/// measured 1.6x slower on a 2M-row Speed facet. A bound array is planned
/// with its actual contents: when no project opted out, the default, the
/// extra condition folds away and the plan is the one without it.
pub fn hosting_traffic_counted_unless_opted_out(
    geo_alias: &'static str,
    project_column: &'static str,
    opt_outs_param: &str,
) -> String {
    format!(
        "(COALESCE({geo_alias}.is_hosting_provider, false) = false \
         OR {project_column} = ANY({opt_outs_param}::int[]))"
    )
}

/// Projects that turned datacenter exclusion off, for
/// [`hosting_traffic_counted_unless_opted_out`]. A primary-key table with at
/// most one row per project, so this stays small.
pub async fn projects_counting_datacenter_traffic<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<i32>, DbErr> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT project_id FROM project_analytics_settings \
             WHERE NOT exclude_datacenter_traffic ORDER BY project_id",
        ))
        .await?;
    rows.iter()
        .map(|row| row.try_get::<i32>("", "project_id"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_checks_the_hosting_flag_and_the_project_choice() {
        let sql = hosting_traffic_counted("ig", "$1");
        assert!(sql.contains("COALESCE(ig.is_hosting_provider, false) = false"));
        assert!(sql.contains("pas.project_id = $1"));
        // A project without a settings row excludes, matching ingest.
        assert!(sql.contains("), true))"));
    }

    #[test]
    fn cross_project_predicate_reads_the_bound_opt_outs_not_the_table() {
        let sql = hosting_traffic_counted_unless_opted_out("g", "e.project_id", "$10");
        assert!(sql.contains("COALESCE(g.is_hosting_provider, false) = false"));
        assert!(sql.contains("e.project_id = ANY($10::int[])"));
        assert!(
            !sql.contains("project_analytics_settings"),
            "a per-row settings lookup costs the parallel plan: {sql}"
        );
    }
}
