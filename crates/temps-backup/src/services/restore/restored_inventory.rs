// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a PostgreSQL restore brought back, compared with what the projects
//! linked to the service read.
//!
//! Right after a restore completes, the databases and login roles of the
//! target are recorded on the run (`restore_runs.restored_inventory`), before
//! any deployment creates the empty databases its links expect. The service
//! page reads [`RestoredDatabasesReport`]: each restored database with the
//! linked projects that read it, and each linked database the backup does
//! not have, with a restored database it could read instead.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use temps_entities::restore_runs;
use temps_providers::{
    is_valid_custom_database_name, DatabaseProvisioningMode, ExternalServiceManager,
    LinkedDatabase, PostgresInventory, ServiceLinkedDatabases,
};
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use super::RestoreError;

/// How a restored database relates to the service and its links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoredDatabaseKind {
    /// A linked project reads it.
    Linked,
    /// No linked project reads it: restored data no app sees.
    Unused,
    /// The database in the service's own configuration.
    Service,
    /// PostgreSQL's own `postgres` database.
    System,
    /// A sandbox's database.
    Sandbox,
}

/// A linked project (or one of its environments) that reads a database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoredDatabaseUse {
    pub project_id: i32,
    pub project_name: String,
    /// `None` when every environment of the project reads the database.
    pub environment_name: Option<String>,
}

/// One database the restore brought back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoredDatabase {
    #[schema(example = "shop_prod")]
    pub name: String,
    pub kind: RestoredDatabaseKind,
    /// The linked projects you can see that read it.
    pub used_by: Vec<RestoredDatabaseUse>,
    /// A project you cannot see also reads it.
    pub used_by_other_projects: bool,
    /// The name is one a link can be pointed at (`[a-z_][a-z0-9_]{0,62}`).
    pub usable_by_link: bool,
}

/// A login role the restore brought back. Names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoredRole {
    pub name: String,
    pub superuser: bool,
}

/// A database a linked project reads that the backup does not have, while
/// the backup has databases no linked project reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MissingLinkedDatabase {
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    /// `None` when every environment of the project reads the database.
    pub environment_name: Option<String>,
    /// The database the project's apps receive.
    #[schema(example = "shop_production")]
    pub expected_database: String,
    /// How the link picks its database: `project_environment`, `project`
    /// or `custom`.
    pub database_provisioning_mode: DatabaseProvisioningMode,
    /// Environments of the project that would all read one database once
    /// the link is pointed at a restored one.
    pub environment_names: Vec<String>,
    /// The unused restored database that most likely holds this project's
    /// data, when one stands out.
    pub suggested_database: Option<String>,
}

/// What the latest restore of a PostgreSQL service brought back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoredDatabasesReport {
    pub restore_run_id: i32,
    #[schema(value_type = String, format = DateTime)]
    pub restored_at: DateTime<Utc>,
    /// Sorted by name.
    pub databases: Vec<RestoredDatabase>,
    /// Sorted by name.
    pub roles: Vec<RestoredRole>,
    /// A list was longer than Temps keeps and was cut.
    pub truncated: bool,
    /// Linked databases the backup does not have. Empty when every restored
    /// database is read by a link: then nothing restored is left unused.
    pub missing: Vec<MissingLinkedDatabase>,
}

/// `GET /external-services/{id}/restored-databases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoredDatabasesResponse {
    /// `None` when the service's latest completed restore recorded nothing
    /// (no restore yet, another engine, or the list could not be read).
    pub report: Option<RestoredDatabasesReport>,
}

/// The service a completed run restored into.
fn run_target(run: &restore_runs::Model) -> i32 {
    run.target_service_id.unwrap_or(run.source_service_id)
}

/// Read the databases and roles of the service a completed run restored into
/// and record them on the run. Best effort: a failure is logged and leaves
/// the column `NULL`; the restore itself already succeeded.
pub(super) async fn record_restored_inventory(
    db: &DatabaseConnection,
    mgr: &ExternalServiceManager,
    run_id: i32,
) {
    let run = match restore_runs::Entity::find_by_id(run_id).one(db).await {
        Ok(Some(run)) if run.status == "completed" => run,
        Ok(_) => return,
        Err(error) => {
            warn!(run_id, %error, "Could not load the completed restore run to list what it restored");
            return;
        }
    };
    let target = run_target(&run);
    let service = match mgr.get_service(target).await {
        Ok(service) => service,
        Err(error) => {
            warn!(run_id, service_id = target, %error, "Could not load the restored service to list its databases");
            return;
        }
    };
    if service.service_type != "postgres" || service.topology == "cluster" {
        debug!(run_id, service_id = target, service_type = %service.service_type, "No database list is recorded for this restore");
        return;
    }
    let inventory = match mgr.read_postgres_inventory(target).await {
        Ok(inventory) => inventory,
        Err(error) => {
            warn!(run_id, service_id = target, %error, "Could not list the databases and users the restore brought back");
            return;
        }
    };
    let value = match serde_json::to_value(&inventory) {
        Ok(value) => value,
        Err(error) => {
            warn!(run_id, %error, "Could not encode the restored database list");
            return;
        }
    };
    let update = restore_runs::Entity::update_many()
        .col_expr(
            restore_runs::Column::RestoredInventory,
            sea_orm::sea_query::Expr::value(value),
        )
        .filter(restore_runs::Column::Id.eq(run_id))
        .exec(db)
        .await;
    match update {
        Ok(_) => info!(
            run_id,
            service_id = target,
            databases = inventory.databases.len(),
            roles = inventory.roles.len(),
            "Recorded the databases and users the restore brought back"
        ),
        Err(error) => warn!(run_id, %error, "Could not record the restored database list"),
    }
}

/// The latest completed restore into `service_id`, when it recorded what it
/// brought back. A run with no target id restored into its source only when
/// it was in place (no new-service name): a new-service run whose target was
/// deleted since has its id cleared too, and its databases are not this
/// service's.
pub(super) async fn latest_restored_inventory(
    db: &DatabaseConnection,
    service_id: i32,
) -> Result<Option<(restore_runs::Model, PostgresInventory)>, RestoreError> {
    let latest = restore_runs::Entity::find()
        .filter(restore_runs::Column::Status.eq("completed"))
        .filter(
            Condition::any()
                .add(restore_runs::Column::TargetServiceId.eq(service_id))
                .add(
                    Condition::all()
                        .add(restore_runs::Column::TargetServiceId.is_null())
                        .add(restore_runs::Column::TargetServiceName.is_null())
                        .add(restore_runs::Column::SourceServiceId.eq(service_id)),
                ),
        )
        .order_by_desc(restore_runs::Column::FinishedAt)
        .order_by_desc(restore_runs::Column::Id)
        .one(db)
        .await?;
    let Some(run) = latest else {
        return Ok(None);
    };
    let Some(value) = run.restored_inventory.clone() else {
        return Ok(None);
    };
    match serde_json::from_value::<PostgresInventory>(value) {
        Ok(inventory) => Ok(Some((run, inventory))),
        Err(error) => {
            warn!(run_id = run.id, %error, "The recorded restored database list could not be read");
            Ok(None)
        }
    }
}

fn database_use(link: &LinkedDatabase) -> RestoredDatabaseUse {
    RestoredDatabaseUse {
        project_id: link.project_id,
        project_name: link.project_name.clone(),
        environment_name: link.environment.as_ref().map(|e| e.name.clone()),
    }
}

/// The unused database most likely to hold `slug`'s data: the only usable
/// unused one, or the only one named after the project.
fn suggestion(project_slug: &str, unused: &[&str]) -> Option<String> {
    let usable: Vec<&str> = unused
        .iter()
        .copied()
        .filter(|name| is_valid_custom_database_name(name))
        .collect();
    let prefix = temps_providers::externalsvc::postgres::PostgresService::normalize_database_name(
        project_slug,
    );
    let named: Vec<&str> = usable
        .iter()
        .copied()
        .filter(|name| *name == prefix || name.starts_with(&format!("{prefix}_")))
        .collect();
    match (named.as_slice(), usable.as_slice()) {
        ([only], _) => Some(only.to_string()),
        ([], [only]) => Some(only.to_string()),
        _ => None,
    }
}

/// Compare what was restored with what the links read. `visible` limits the
/// projects named in the report (`None`: every project); databases read only
/// by hidden projects still count as used, so they are never suggested.
pub(super) fn build_report(
    run: &restore_runs::Model,
    inventory: PostgresInventory,
    linked: &ServiceLinkedDatabases,
    visible: Option<&BTreeSet<i32>>,
) -> RestoredDatabasesReport {
    let shown = |project_id: i32| visible.is_none_or(|ids| ids.contains(&project_id));
    let mut uses: BTreeMap<&str, Vec<&LinkedDatabase>> = BTreeMap::new();
    for link in &linked.links {
        uses.entry(link.database.as_str()).or_default().push(link);
    }

    let kind_of = |name: &str| {
        if uses.contains_key(name) {
            RestoredDatabaseKind::Linked
        } else if linked.configured_database.as_deref() == Some(name) {
            RestoredDatabaseKind::Service
        } else if name == "postgres" {
            RestoredDatabaseKind::System
        } else if temps_providers::sandbox_engines::is_reserved_sandbox_database_name(name) {
            RestoredDatabaseKind::Sandbox
        } else {
            RestoredDatabaseKind::Unused
        }
    };

    let databases: Vec<RestoredDatabase> = inventory
        .databases
        .iter()
        .map(|name| {
            let readers = uses.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
            RestoredDatabase {
                name: name.clone(),
                kind: kind_of(name),
                used_by: readers
                    .iter()
                    .filter(|link| shown(link.project_id))
                    .map(|link| database_use(link))
                    .collect(),
                used_by_other_projects: readers.iter().any(|link| !shown(link.project_id)),
                usable_by_link: is_valid_custom_database_name(name),
            }
        })
        .collect();

    let restored: BTreeSet<&str> = inventory.databases.iter().map(String::as_str).collect();
    let unused: Vec<&str> = databases
        .iter()
        .filter(|database| database.kind == RestoredDatabaseKind::Unused)
        .map(|database| database.name.as_str())
        .collect();

    // A linked database missing from the backup only matters while restored
    // data sits unused: otherwise the project is simply new on this server.
    // A cut database list cannot tell; a cut role list does not matter here.
    let mut missing = Vec::new();
    if !unused.is_empty() && !inventory.databases_truncated {
        let mut environment_names: BTreeMap<i32, Vec<String>> = BTreeMap::new();
        for link in &linked.links {
            let names = environment_names.entry(link.project_id).or_default();
            if let Some(environment) = &link.environment {
                names.push(environment.name.clone());
            }
        }
        for link in linked
            .links
            .iter()
            .filter(|link| shown(link.project_id))
            .filter(|link| !restored.contains(link.database.as_str()))
        {
            let environment_names = environment_names
                .get(&link.project_id)
                .cloned()
                .unwrap_or_default();
            missing.push(MissingLinkedDatabase {
                project_id: link.project_id,
                project_name: link.project_name.clone(),
                project_slug: link.project_slug.clone(),
                environment_name: link.environment.as_ref().map(|e| e.name.clone()),
                expected_database: link.database.clone(),
                database_provisioning_mode: link.mode,
                environment_names,
                suggested_database: suggestion(&link.project_slug, &unused),
            });
        }
    }

    let truncated = inventory.truncated();
    RestoredDatabasesReport {
        restore_run_id: run.id,
        restored_at: run.finished_at.unwrap_or(run.updated_at),
        databases,
        roles: inventory
            .roles
            .into_iter()
            .map(|role| RestoredRole {
                name: role.name,
                superuser: role.superuser,
            })
            .collect(),
        truncated,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_providers::{InventoryRole, LinkedEnvironment};

    fn run() -> restore_runs::Model {
        restore_runs::Model {
            id: 21,
            source_backup_id: Some(3),
            source_service_id: 5,
            target_service_id: None,
            target_service_name: None,
            mode: "in_place".into(),
            status: "completed".into(),
            phase: "completed".into(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log".into(),
            error_message: None,
            attempt: 1,
            started_at: None,
            finished_at: Some(Utc::now()),
            created_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            cancel_requested_at: None,
            cancel_requested_by: None,
            restored_login_encrypted: None,
            restored_inventory: None,
        }
    }

    fn inventory(databases: &[&str]) -> PostgresInventory {
        PostgresInventory {
            databases: databases.iter().map(|d| d.to_string()).collect(),
            roles: vec![InventoryRole {
                name: "origin-admin".into(),
                superuser: true,
            }],
            databases_truncated: false,
            roles_truncated: false,
        }
    }

    fn per_environment(project_id: i32, slug: &str, envs: &[&str]) -> Vec<LinkedDatabase> {
        envs.iter()
            .enumerate()
            .map(|(i, env)| LinkedDatabase {
                project_id,
                project_name: format!("{slug} app"),
                project_slug: slug.to_string(),
                environment: Some(LinkedEnvironment {
                    id: project_id * 10 + i as i32,
                    name: env.to_string(),
                    slug: env.to_string(),
                }),
                mode: DatabaseProvisioningMode::ProjectEnvironment,
                database: format!("{slug}_{env}"),
            })
            .collect()
    }

    fn linked(links: Vec<LinkedDatabase>) -> ServiceLinkedDatabases {
        ServiceLinkedDatabases {
            service_id: 5,
            configured_database: Some("app".into()),
            links,
        }
    }

    #[test]
    fn a_renamed_database_is_reported_with_the_restored_one_suggested() {
        let report = build_report(
            &run(),
            inventory(&["app", "postgres", "shop_prod"]),
            &linked(per_environment(1, "shop", &["production"])),
            None,
        );
        assert_eq!(report.missing.len(), 1, "{report:?}");
        let missing = &report.missing[0];
        assert_eq!(missing.expected_database, "shop_production");
        assert_eq!(missing.suggested_database.as_deref(), Some("shop_prod"));
        assert_eq!(missing.environment_names, vec!["production"]);
        let kinds: Vec<_> = report
            .databases
            .iter()
            .map(|d| (d.name.as_str(), d.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("app", RestoredDatabaseKind::Service),
                ("postgres", RestoredDatabaseKind::System),
                ("shop_prod", RestoredDatabaseKind::Unused),
            ]
        );
        assert_eq!(report.roles[0].name, "origin-admin");
    }

    #[test]
    fn matching_names_report_nothing_missing_and_show_who_reads_each() {
        let report = build_report(
            &run(),
            inventory(&["postgres", "shop_production"]),
            &linked(per_environment(1, "shop", &["production"])),
            None,
        );
        assert!(report.missing.is_empty(), "{report:?}");
        let shop = &report.databases[1];
        assert_eq!(shop.kind, RestoredDatabaseKind::Linked);
        assert_eq!(
            shop.used_by[0].environment_name.as_deref(),
            Some("production")
        );
    }

    #[test]
    fn a_new_project_is_not_reported_when_nothing_restored_is_unused() {
        // `blog` was linked after the restore: its database is not in the
        // backup, but no restored data is waiting for it either.
        let mut links = per_environment(1, "shop", &["production"]);
        links.extend(per_environment(2, "blog", &["production"]));
        let report = build_report(
            &run(),
            inventory(&["postgres", "shop_production"]),
            &linked(links),
            None,
        );
        assert!(report.missing.is_empty(), "{report:?}");
    }

    #[test]
    fn several_candidates_suggest_only_the_one_named_after_the_project() {
        let mut links = per_environment(1, "shop", &["production", "staging"]);
        links.extend(per_environment(2, "blog", &["production"]));
        let report = build_report(
            &run(),
            inventory(&["blog_live", "shop_prod", "legacy"]),
            &linked(links),
            None,
        );
        let suggested: Vec<_> = report
            .missing
            .iter()
            .map(|m| {
                (
                    m.expected_database.as_str(),
                    m.suggested_database.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            suggested,
            vec![
                ("shop_production", Some("shop_prod")),
                ("shop_staging", Some("shop_prod")),
                ("blog_production", Some("blog_live")),
            ]
        );
        assert_eq!(
            report.missing[0].environment_names,
            vec!["production", "staging"]
        );
    }

    #[test]
    fn an_ambiguous_or_unusable_candidate_is_not_suggested() {
        let report = build_report(
            &run(),
            inventory(&["Shop-Prod", "archive_a", "archive_b"]),
            &linked(per_environment(1, "web", &["production"])),
            None,
        );
        assert_eq!(report.missing[0].suggested_database, None);
        let uppercase = &report.databases[0];
        assert_eq!(uppercase.name, "Shop-Prod");
        assert!(!uppercase.usable_by_link);
    }

    #[test]
    fn hidden_projects_are_not_named_and_their_databases_are_never_suggested() {
        let mut links = per_environment(1, "shop", &["production"]);
        links.extend(per_environment(2, "secret", &["production"]));
        let visible = BTreeSet::from([1]);
        let report = build_report(
            &run(),
            inventory(&["secret_production", "shop_prod"]),
            &linked(links),
            Some(&visible),
        );
        assert!(
            report.missing.iter().all(|m| m.project_id == 1),
            "{report:?}"
        );
        let secret = &report.databases[0];
        assert_eq!(secret.kind, RestoredDatabaseKind::Linked);
        assert!(secret.used_by.is_empty());
        assert!(secret.used_by_other_projects);
        assert_eq!(
            report.missing[0].suggested_database.as_deref(),
            Some("shop_prod")
        );
    }

    #[test]
    fn a_cut_database_list_reports_nothing_missing() {
        let mut cut = inventory(&["shop_prod"]);
        cut.databases_truncated = true;
        let report = build_report(
            &run(),
            cut,
            &linked(per_environment(1, "shop", &["production"])),
            None,
        );
        assert!(report.truncated);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn a_cut_role_list_still_reports_what_is_missing() {
        let mut cut = inventory(&["shop_prod"]);
        cut.roles_truncated = true;
        let report = build_report(
            &run(),
            cut,
            &linked(per_environment(1, "shop", &["production"])),
            None,
        );
        assert!(report.truncated, "the role list is marked as cut");
        assert_eq!(report.missing.len(), 1, "{report:?}");
        assert_eq!(
            report.missing[0].suggested_database.as_deref(),
            Some("shop_prod")
        );
    }

    #[test]
    fn the_target_of_an_in_place_run_is_its_source_service() {
        let mut in_place = run();
        assert_eq!(run_target(&in_place), 5);
        in_place.target_service_id = Some(9);
        assert_eq!(run_target(&in_place), 9);
    }

    #[tokio::test]
    async fn the_latest_completed_run_is_read_back_or_nothing_when_it_recorded_none() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let mut recorded = run();
        recorded.restored_inventory =
            Some(serde_json::to_value(inventory(&["shop_prod"])).expect("json"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![recorded]])
            .append_query_results([vec![run()]])
            .append_query_results([Vec::<restore_runs::Model>::new()])
            .into_connection();

        let (found, listed) = latest_restored_inventory(&db, 5)
            .await
            .expect("query")
            .expect("recorded");
        assert_eq!(found.id, 21);
        assert_eq!(listed.databases, vec!["shop_prod"]);
        assert!(latest_restored_inventory(&db, 5)
            .await
            .expect("query")
            .is_none());
        assert!(latest_restored_inventory(&db, 5)
            .await
            .expect("query")
            .is_none());

        let log = db.into_transaction_log();
        let sql = log[0].statements()[0].to_string();
        assert!(sql.contains("\"status\" = 'completed'"), "{sql}");
        assert!(sql.contains("\"target_service_id\" IS NULL"), "{sql}");
        assert!(
            sql.contains("\"target_service_name\" IS NULL"),
            "only an in-place run falls back to its source: {sql}"
        );
        assert!(
            sql.contains("ORDER BY \"restore_runs\".\"finished_at\" DESC"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn a_run_that_is_not_completed_records_nothing() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        let mut failed = run();
        failed.status = "failed".into();
        // Only the run lookup has a result: reading the service or writing
        // the column would fail the mock.
        let db = std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![failed]])
                .into_connection(),
        );
        let mgr = ExternalServiceManager::new_with_handle(
            db.clone(),
            std::sync::Arc::new(temps_core::EncryptionService::new_from_password(
                "inventory",
            )),
            std::sync::Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "inventory test",
            )),
            true,
            std::sync::Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        );
        record_restored_inventory(db.as_ref(), &mgr, 21).await;
        drop(mgr);
        let db = std::sync::Arc::try_unwrap(db).unwrap_or_else(|_| panic!("database still owned"));
        assert_eq!(db.into_transaction_log().len(), 1, "only the run was read");
    }
}
