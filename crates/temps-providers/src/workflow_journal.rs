// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded managed PostgreSQL journal access. Credentials never cross this boundary.
use crate::services::ExternalServiceManager;
use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::{sync::Arc, time::Duration};
use temps_core::workflow_journal::{
    ManagedWorkflowJournal, WorkflowJournalError as Error, WorkflowJournalScope as Scope,
};
use temps_entities::project_services;
use tokio::sync::Semaphore;

const MAX_RECORD: usize = 8 * 1024 * 1024;
const TABLE: &str = "CREATE TABLE IF NOT EXISTS temps_workflow_journal_v1 (project_id INTEGER NOT NULL, run_id TEXT NOT NULL, revision TEXT NOT NULL, record TEXT NOT NULL CHECK(octet_length(record)<=8388608), PRIMARY KEY(project_id,run_id,revision))";

pub struct PostgresWorkflowJournal {
    db: Arc<DatabaseConnection>,
    manager: Arc<ExternalServiceManager>,
    slots: Semaphore,
}
impl PostgresWorkflowJournal {
    pub fn new(db: Arc<DatabaseConnection>, manager: Arc<ExternalServiceManager>) -> Self {
        Self {
            db,
            manager,
            slots: Semaphore::new(4),
        }
    }
    async fn connect(&self, scope: Scope, linked: bool) -> Result<tokio_postgres::Client, Error> {
        let fail = |reason| Error {
            service_id: scope.service_id,
            operation: "connect",
            reason,
        };
        if linked
            && project_services::Entity::find()
                .filter(project_services::Column::ProjectId.eq(scope.project_id))
                .filter(project_services::Column::ServiceId.eq(scope.service_id))
                .one(self.db.as_ref())
                .await
                .map_err(|_| fail("cannot check project service link"))?
                .is_none()
        {
            return Err(fail("service is not linked to the project"));
        }
        let service = self
            .manager
            .get_service(scope.service_id)
            .await
            .map_err(|_| fail("service is unavailable"))?;
        if service.service_type != "postgres" {
            return Err(fail("service must be managed PostgreSQL"));
        }
        let config = self
            .manager
            .get_service_config(scope.service_id)
            .await
            .map_err(|_| fail("credentials are unavailable"))?;
        let config: temps_providers_postgres::postgres::PostgresInputConfig =
            serde_json::from_value(config.parameters)
                .map_err(|_| fail("invalid PostgreSQL configuration"))?;
        let (host, port) = match self
            .manager
            .get_cluster_primary_address(scope.service_id)
            .await
            .map_err(|_| fail("cannot resolve primary"))?
        {
            Some(endpoint) => endpoint,
            None => {
                if !matches!(config.host.as_str(), "localhost" | "127.0.0.1" | "::1") {
                    return Err(fail("standalone host must be loopback"));
                }
                let host = self
                    .manager
                    .worker_data_address(&service)
                    .await
                    .map_err(|_| fail("cannot resolve worker"))?
                    .unwrap_or(config.host);
                let port = config
                    .port
                    .as_deref()
                    .and_then(|v| v.parse::<u16>().ok())
                    .filter(|v| *v > 0)
                    .ok_or_else(|| fail("missing managed port"))?;
                (host, port)
            }
        };
        let password = config
            .password
            .as_deref()
            .ok_or_else(|| fail("missing managed credentials"))?;
        let client = temps_providers_postgres::query::connect_with_private_tls_ladder(
            &host,
            port,
            &config.username,
            password,
            &config.database,
        )
        .await
        .map_err(|_| fail("PostgreSQL connection failed"))?;
        client
            .batch_execute(TABLE)
            .await
            .map_err(|_| fail("cannot initialize journal table"))?;
        Ok(client)
    }
    async fn bounded<T>(
        &self,
        service_id: i32,
        operation: &'static str,
        future: impl std::future::Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        tokio::time::timeout(Duration::from_secs(15), async {
            let _slot = self.slots.acquire().await.map_err(|_| Error {
                service_id,
                operation,
                reason: "journal service is stopping",
            })?;
            future.await
        })
        .await
        .map_err(|_| Error {
            service_id,
            operation,
            reason: "15-second deadline exceeded",
        })?
    }
}
#[async_trait]
impl ManagedWorkflowJournal for PostgresWorkflowJournal {
    async fn check(&self, service_id: i32, project_id: i32) -> Result<(), Error> {
        self.bounded(service_id, "check", async {
            self.connect(
                Scope {
                    service_id,
                    project_id,
                    run_id: uuid::Uuid::nil(),
                },
                true,
            )
            .await?;
            Ok(())
        })
        .await
    }
    async fn put_first(
        &self,
        scope: Scope,
        key: &str,
        previous: Option<&str>,
        record: &str,
    ) -> Result<String, Error> {
        let fail = |reason| Error {
            service_id: scope.service_id,
            operation: "write entry",
            reason,
        };
        if key.len() > 200 || record.len() > MAX_RECORD {
            return Err(fail("entry exceeds the storage bound"));
        }
        self.bounded(scope.service_id,"write entry",async {
            let client=self.connect(scope,true).await?;
            // The no-op conflict update returns the first committed value even
            // when two control-plane processes race to recover a transaction.
            let row=client.query_one("INSERT INTO temps_workflow_journal_v1(project_id,run_id,revision,record) VALUES($1,$2,$3,$4) ON CONFLICT(project_id,run_id,revision) DO UPDATE SET revision=EXCLUDED.revision RETURNING record",&[&scope.project_id,&scope.run_id.to_string(),&key,&record]).await.map_err(|_| fail("PostgreSQL write failed"))?;
            let stored = row.try_get(0).map_err(|_| fail("invalid stored entry"))?;
            client.execute("DELETE FROM temps_workflow_journal_v1 WHERE project_id=$1 AND run_id=$2 AND split_part(revision,':',1)=split_part($3,':',1) AND revision<>$3 AND ($4::text IS NULL OR revision<>$4)",&[&scope.project_id,&scope.run_id.to_string(),&key,&previous]).await.map_err(|_|fail("cannot reclaim superseded revisions"))?;
            Ok(stored)
        }).await
    }
    async fn read(&self, scope: Scope, keys: &[String]) -> Result<Vec<String>, Error> {
        let fail = |reason| Error {
            service_id: scope.service_id,
            operation: "read entries",
            reason,
        };
        if keys.len() > 100 {
            return Err(fail("page exceeds 100 entries"));
        }
        self.bounded(scope.service_id,"read entries",async {
            let client=self.connect(scope,true).await?;
            let size:i64=client.query_one("SELECT COALESCE(sum(octet_length(record)),0)::BIGINT FROM temps_workflow_journal_v1 WHERE project_id=$1 AND run_id=$2 AND revision=ANY($3)",&[&scope.project_id,&scope.run_id.to_string(),&keys]).await.map_err(|_|fail("cannot measure journal page"))?.try_get(0).map_err(|_|fail("invalid journal page size"))?;
            if size>16*1024*1024 { return Err(fail("journal page exceeds 16 MiB")); }
            let rows=client.query("SELECT revision,record FROM temps_workflow_journal_v1 WHERE project_id=$1 AND run_id=$2 AND revision=ANY($3)",&[&scope.project_id,&scope.run_id.to_string(),&keys]).await.map_err(|_| fail("PostgreSQL read failed"))?;
            let mut entries=std::collections::HashMap::<String,String>::new();
            for row in rows { entries.insert(row.try_get(0).map_err(|_| fail("invalid revision"))?,row.try_get(1).map_err(|_| fail("invalid record"))?); }
            keys.iter().map(|key| entries.remove(key).ok_or_else(|| fail("journal entry is missing"))).collect()
        }).await
    }
    async fn delete_run(&self, scope: Scope) -> Result<(), Error> {
        self.bounded(scope.service_id, "delete run", async {
            let client = self.connect(scope, false).await?;
            client
                .execute(
                    "DELETE FROM temps_workflow_journal_v1 WHERE project_id=$1 AND run_id=$2",
                    &[&scope.project_id, &scope.run_id.to_string()],
                )
                .await
                .map_err(|_| Error {
                    service_id: scope.service_id,
                    operation: "delete run",
                    reason: "PostgreSQL delete failed",
                })?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use temps_core::{DockerHandle, EncryptionService};
    #[tokio::test]
    async fn managed_postgres_journal_preserves_first_write_scopes_and_reclaims_revisions() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Docker unavailable; skipping managed journal transport test");
            return;
        }
        let db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let conn = db.connection_arc();
        let pg: tokio_postgres::Config = db.database_url.parse().unwrap();
        let tokio_postgres::config::Host::Tcp(host) = &pg.get_hosts()[0] else {
            panic!("TCP test database")
        };
        let config = serde_json::json!({"host":host,"port":pg.get_ports()[0].to_string(),"username":pg.get_user().unwrap(),"password":String::from_utf8(pg.get_password().unwrap().to_vec()).unwrap(),"database":pg.get_dbname().unwrap()});
        let encryption =
            Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
        let encrypted = encryption.encrypt_string(&config.to_string()).unwrap();
        let service_id:i32=conn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO external_services(name,service_type,status,config,created_at,updated_at) VALUES('journal-transport','postgres','running',$1,now(),now()) RETURNING id",vec![encrypted.into()])).await.unwrap().unwrap().try_get("","id").unwrap();
        let project_id:i32=conn.query_one(Statement::from_string(DbBackend::Postgres,"INSERT INTO projects(name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES('Journal transport','','','.','main','dockerfile','journal-transport',now(),now()) RETURNING id".to_string())).await.unwrap().unwrap().try_get("","id").unwrap();
        let manager = Arc::new(ExternalServiceManager::new_with_handle(
            conn.clone(),
            encryption,
            Arc::new(DockerHandle::disabled(
                temps_core::PROFILE_CONTROL_PLANE,
                "journal test needs no containers",
            )),
            false,
            Arc::new(crate::DnsRegistry::new(conn.clone())),
        ));
        let journal = PostgresWorkflowJournal::new(conn.clone(), manager);
        assert!(journal
            .check(service_id, project_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("not linked"));
        conn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO project_services(project_id,service_id,created_at,updated_at) VALUES($1,$2,now(),now())",
            vec![project_id.into(), service_id.into()],
        ))
        .await
        .unwrap();
        journal.check(service_id, project_id).await.unwrap();
        let scope = Scope {
            service_id,
            project_id,
            run_id: uuid::Uuid::new_v4(),
        };
        assert_eq!(
            journal
                .put_first(scope, "0:1:1:pending", None, "first sealed record")
                .await
                .unwrap(),
            "first sealed record"
        );
        assert_eq!(
            journal
                .put_first(scope, "0:1:1:pending", None, "different sealed record")
                .await
                .unwrap(),
            "first sealed record"
        );
        journal
            .put_first(
                scope,
                "0:2:2:pending",
                Some("0:1:1:pending"),
                "second attempt",
            )
            .await
            .unwrap();
        journal
            .put_first(scope, "0:3:3:completed", Some("0:2:2:pending"), "completed")
            .await
            .unwrap();
        assert!(journal
            .read(scope, &["0:1:1:pending".into()])
            .await
            .is_err());
        assert_eq!(
            journal
                .read(scope, &["0:3:3:completed".into(), "0:2:2:pending".into()])
                .await
                .unwrap(),
            vec!["completed", "second attempt"]
        );
        assert!(journal
            .read(
                Scope {
                    run_id: uuid::Uuid::new_v4(),
                    ..scope
                },
                &["0:3:3:completed".into()]
            )
            .await
            .is_err());
        assert!(journal
            .read(
                Scope {
                    project_id: project_id + 1,
                    ..scope
                },
                &["0:3:3:completed".into()]
            )
            .await
            .is_err());
        assert!(journal
            .put_first(scope, "1:1:1:completed", None, &"x".repeat(MAX_RECORD + 1))
            .await
            .is_err());
        conn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM project_services WHERE project_id=$1 AND service_id=$2",
            vec![project_id.into(), service_id.into()],
        ))
        .await
        .unwrap();
        assert!(journal
            .read(scope, &["0:3:3:completed".into()])
            .await
            .is_err());
        journal.delete_run(scope).await.unwrap();
        journal.delete_run(scope).await.unwrap();
    }
}
