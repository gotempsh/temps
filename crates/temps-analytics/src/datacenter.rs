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

/// SQL predicate that is true when the row should be counted: its IP is not a
/// hosting provider, or its project does not exclude datacenter traffic.
///
/// `geo_alias` is the `ip_geolocations` alias joined to the row, and
/// `project_expr` the SQL expression holding the row's project id (a column or
/// a bind placeholder). Both are compile-time constants, never user input.
///
/// The settings lookup only runs for hosting-provider rows, and is a
/// primary-key probe when it does.
pub fn hosting_traffic_counted(geo_alias: &'static str, project_expr: &'static str) -> String {
    format!(
        "(COALESCE({geo_alias}.is_hosting_provider, false) = false \
         OR NOT COALESCE((SELECT pas.exclude_datacenter_traffic \
         FROM project_analytics_settings pas WHERE pas.project_id = {project_expr}), true))"
    )
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
}
