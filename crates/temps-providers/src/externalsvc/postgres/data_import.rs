// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL support for importing data from an external server.
//!
//! The copy is `pg_dump | psql`, run with the service's own image so the
//! client tools match the server they write into. It is atomic: the dump is
//! wrapped in `BEGIN` … `COMMIT` and the `COMMIT` is only emitted after
//! `pg_dump` succeeded, so a source that fails half-way (or a helper that is
//! killed) closes the session with the transaction still open and PostgreSQL
//! rolls it back. `psql --single-transaction` would not do: it commits on
//! end of input, partial input included.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::Connection;

use super::{PostgresConfig, PostgresService};
use crate::data_import::source::{
    parse_source_url, percent_encode_userinfo, scrub_secrets, SourceUrlRules,
};
use crate::data_import::{
    bounded_step, DataImportEngine, DataImportError, DataImportSpec, ImportSource,
    TargetInspection, TargetPreparation, TransferEnv, TransferPlan, TransferTarget,
};
use crate::externalsvc::ServiceConfig;

/// Databases every PostgreSQL server has, never an import target.
const RESERVED_DATABASES: [&str; 3] = ["postgres", "template0", "template1"];

/// Bound on each control-plane SQL step (inspect, drop, create, measure).
const ADMIN_SQL_TIMEOUT: Duration = Duration::from_secs(60);

const SOURCE_RULES: SourceUrlRules<'static> = SourceUrlRules {
    schemes: &["postgres", "postgresql"],
    default_port: 5432,
    // libpq honours host, hostaddr, port, dbname, passfile, sslrootcert, …
    // in the query string. Any of them would let a URL whose hosts passed the
    // SSRF guard connect elsewhere or read files inside the helper.
    allowed_options: &[
        "sslmode",
        "connect_timeout",
        "application_name",
        "target_session_attrs",
    ],
    options_case_insensitive: false,
    max_hosts: 8,
    default_database: None,
};

const SSL_MODES: [&str; 6] = [
    "disable",
    "allow",
    "prefer",
    "require",
    "verify-ca",
    "verify-full",
];

const TARGET_SESSION_ATTRS: [&str; 6] = [
    "any",
    "read-write",
    "read-only",
    "primary",
    "standby",
    "prefer-standby",
];

/// User relations outside the system schemas, excluding objects an extension
/// owns (PostGIS' `spatial_ref_sys`, for instance), which do not make a
/// database "hold data".
const COUNT_USER_RELATIONS_SQL: &str = "SELECT count(*) FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
       AND n.nspname NOT LIKE 'pg\\_toast%' \
       AND n.nspname NOT LIKE 'pg\\_temp\\_%' \
       AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
       AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d \
                       WHERE d.classid = 'pg_catalog.pg_class'::regclass \
                         AND d.objid = c.oid AND d.deptype = 'e')";

/// `BEGIN`, the dump, and `COMMIT` only if the dump succeeded.
const PRODUCER: &str = "printf 'BEGIN;\\n' && \
     pg_dump --no-owner --no-privileges --no-publications --no-subscriptions --no-password \
     --dbname=\"$TEMPS_IMPORT_SOURCE\" && \
     printf 'COMMIT;\\n'";

/// Stops at the first error; output other than errors is discarded.
const CONSUMER: &str = "psql --no-psqlrc --quiet --no-password --set ON_ERROR_STOP=1 \
     --dbname=\"$TEMPS_IMPORT_TARGET\" >/dev/null";

impl PostgresService {
    fn import_config(&self, config: &ServiceConfig) -> Result<PostgresConfig, DataImportError> {
        self.get_postgres_config(config.clone()).map_err(|e| {
            DataImportError::target(
                &config.name,
                "read the PostgreSQL configuration",
                e.to_string(),
            )
        })
    }

    /// Control-plane connection to `database`, the way provisioning connects.
    async fn import_connect(
        &self,
        service: &str,
        pg: &PostgresConfig,
        database: &str,
    ) -> Result<sqlx::PgConnection, DataImportError> {
        let url = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode=disable",
            percent_encode_userinfo(&pg.username),
            percent_encode_userinfo(&pg.password),
            pg.host,
            pg.port,
            database
        );
        let operation = format!("connect to database '{database}'");
        tokio::time::timeout(ADMIN_SQL_TIMEOUT, sqlx::PgConnection::connect(&url))
            .await
            .map_err(|_| {
                DataImportError::target(
                    service,
                    &operation,
                    format!("timed out after {}s", ADMIN_SQL_TIMEOUT.as_secs()),
                )
            })?
            .map_err(|e| {
                DataImportError::target(
                    service,
                    &operation,
                    scrub_secrets(&e.to_string(), &[url.clone(), pg.password.clone()]),
                )
            })
    }

    /// Body of `inspect_target`, bounded there.
    async fn import_inspect(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError> {
        let service = config.name.as_str();
        let pg = self.import_config(config)?;
        let mut admin = self.import_connect(service, &pg, "postgres").await?;
        let size: Option<i64> = sqlx::query_scalar(
            "SELECT pg_database_size(datname) FROM pg_catalog.pg_database WHERE datname = $1",
        )
        .bind(database)
        .fetch_optional(&mut admin)
        .await
        .map_err(|e| sql_error(service, "check whether the database exists", &pg, e))?;
        let _ = admin.close().await;

        let Some(size) = size else {
            return Ok(TargetInspection {
                exists: false,
                object_count: 0,
                size_bytes: None,
            });
        };
        let mut conn = self.import_connect(service, &pg, database).await?;
        let object_count: i64 = sqlx::query_scalar(COUNT_USER_RELATIONS_SQL)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| sql_error(service, &format!("count tables in '{database}'"), &pg, e))?;
        let _ = conn.close().await;
        Ok(TargetInspection {
            exists: true,
            object_count,
            size_bytes: Some(size),
        })
    }

    /// Body of `prepare_target`, bounded there.
    async fn import_prepare(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
    ) -> Result<(), DataImportError> {
        // Re-validated here: the name is interpolated into DDL below.
        self.validate_target_database(database)?;
        let service = config.name.as_str();
        if preparation == TargetPreparation::UseExisting {
            return Ok(());
        }
        if preparation == TargetPreparation::Recreate {
            let pg = self.import_config(config)?;
            let mut admin = self.import_connect(service, &pg, "postgres").await?;
            let version: i32 =
                sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
                    .fetch_one(&mut admin)
                    .await
                    .map_err(|e| sql_error(service, "read the server version", &pg, e))?;
            if version < 130_000 {
                sqlx::query(
                    "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
                     WHERE datname = $1 AND pid <> pg_backend_pid()",
                )
                .bind(database)
                .execute(&mut admin)
                .await
                .map_err(|e| {
                    sql_error(
                        service,
                        &format!("disconnect sessions from '{database}'"),
                        &pg,
                        e,
                    )
                })?;
            }
            // WITH (FORCE) terminates the sessions still attached: replacing
            // a database the application is connected to is what replace means.
            let drop = if version >= 130_000 {
                format!("DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)")
            } else {
                format!("DROP DATABASE IF EXISTS \"{database}\"")
            };
            sqlx::query(&drop)
                .execute(&mut admin)
                .await
                .map_err(|e| sql_error(service, &format!("drop database '{database}'"), &pg, e))?;
            let _ = admin.close().await;
        }
        // Same path as project provisioning: created by, and owned by, the
        // service user.
        self.create_database(config.clone(), database)
            .await
            .map_err(|e| {
                DataImportError::target(
                    service,
                    format!("create database '{database}'"),
                    e.to_string(),
                )
            })
    }
}

fn sql_error(
    service: &str,
    operation: &str,
    pg: &PostgresConfig,
    e: sqlx::Error,
) -> DataImportError {
    DataImportError::target(
        service,
        operation,
        scrub_secrets(&e.to_string(), std::slice::from_ref(&pg.password)),
    )
}

/// Reject option values libpq would misread or that make no sense here.
fn validate_source_options(source: &ImportSource) -> Result<(), DataImportError> {
    for (name, value) in source.options() {
        let valid = match name.as_str() {
            "sslmode" => SSL_MODES.contains(&value.as_str()),
            "target_session_attrs" => TARGET_SESSION_ATTRS.contains(&value.as_str()),
            "connect_timeout" => !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()),
            _ => value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
        };
        if !valid {
            return Err(DataImportError::invalid_source(format!(
                "option '{name}' has an unsupported value '{value}'"
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl DataImportEngine for PostgresService {
    fn import_spec(&self) -> DataImportSpec {
        DataImportSpec {
            engine_label: "PostgreSQL".to_string(),
            source_schemes: SOURCE_RULES.schemes.iter().map(|s| s.to_string()).collect(),
            source_url_example: "postgres://user:password@db.example.com:5432/app?sslmode=require"
                .to_string(),
            allowed_source_options: SOURCE_RULES
                .allowed_options
                .iter()
                .map(|s| s.to_string())
                .collect(),
            atomic: true,
            object_noun: "table".to_string(),
            max_target_length: 63,
        }
    }

    fn parse_source(&self, raw: &str) -> Result<ImportSource, DataImportError> {
        let source = parse_source_url(raw, &SOURCE_RULES)?;
        validate_source_options(&source)?;
        Ok(source)
    }

    fn validate_target_database(&self, database: &str) -> Result<(), DataImportError> {
        Self::validate_database_name(database)
            .map_err(|e| DataImportError::invalid_target_database(database, e.to_string()))?;
        if RESERVED_DATABASES.contains(&database) {
            return Err(DataImportError::invalid_target_database(
                database,
                "it is a PostgreSQL system database",
            ));
        }
        Ok(())
    }

    async fn inspect_target(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError> {
        bounded_step(
            &config.name,
            &format!("inspect database '{database}'"),
            self.import_inspect(config, database),
        )
        .await
    }

    async fn prepare_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
    ) -> Result<(), DataImportError> {
        bounded_step(
            &config.name,
            &format!("prepare database '{database}'"),
            self.import_prepare(config, database, preparation),
        )
        .await
    }

    fn target_container(&self, config: &ServiceConfig) -> Result<String, DataImportError> {
        Ok(self.get_live_container_name(&self.import_config(config)?))
    }

    async fn transfer_plan(
        &self,
        config: &ServiceConfig,
        source: &ImportSource,
        target: &TransferTarget<'_>,
    ) -> Result<TransferPlan, DataImportError> {
        let pg = self.import_config(config)?;
        let target_url = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode=disable",
            percent_encode_userinfo(&pg.username),
            percent_encode_userinfo(&pg.password),
            target.host,
            target.port,
            target.database
        );
        Ok(TransferPlan {
            image: pg.docker_image,
            producer: PRODUCER.to_string(),
            consumer: CONSUMER.to_string(),
            env: vec![
                TransferEnv::secret("TEMPS_IMPORT_SOURCE", source.raw()),
                TransferEnv::secret("TEMPS_IMPORT_TARGET", target_url),
                TransferEnv::secret("TEMPS_IMPORT_TARGET_PASSWORD", pg.password),
            ],
        })
    }

    fn failure_hint(&self, output: &str) -> Option<String> {
        postgres_failure_hint(output)
    }
}

fn postgres_failure_hint(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let hint = if lower.contains("server version mismatch") {
        "the source runs a newer PostgreSQL major version than this service, and pg_dump cannot \
         read servers newer than itself; upgrade this service first, or import into a service \
         running the source's major version or newer"
    } else if lower.contains("password authentication failed") {
        "the source rejected the user name or password"
    } else if lower.contains("no pg_hba.conf entry") {
        "the source does not accept connections from this server; allow this server's public \
         address in the source's pg_hba.conf or firewall (and use sslmode=require if it only \
         accepts TLS)"
    } else if lower.contains("does not support ssl") {
        "the source does not support TLS; use sslmode=prefer or sslmode=disable"
    } else if lower.contains("certificate verify failed") || lower.contains("certificate") {
        "the source's TLS certificate could not be verified; use sslmode=require to encrypt \
         without verifying it"
    } else if lower.contains("connection refused")
        || lower.contains("timeout expired")
        || lower.contains("could not connect")
        || lower.contains("network is unreachable")
    {
        "the source could not be reached from this server; check its address, port and firewall"
    } else if lower.contains("permission denied for") {
        "the source user cannot read every object; connect as the database owner or grant it \
         read access to every table, sequence and schema"
    } else if lower.contains("is not available")
        || lower.contains("could not open extension control file")
    {
        "the source uses a PostgreSQL extension this service's image does not include"
    } else if lower.contains("already exists") {
        "an object in the dump already exists in the target; enable replace to import into a \
         fresh database"
    } else {
        return None;
    };
    Some(hint.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_import::runner::compose_script;

    fn service() -> PostgresService {
        let docker = std::sync::Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("docker client value"),
        );
        PostgresService::new("orders".to_string(), docker)
    }

    fn config() -> ServiceConfig {
        ServiceConfig {
            name: "orders".to_string(),
            service_type: crate::externalsvc::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5433",
                "database": "postgres",
                "username": "app",
                "password": "p@ss:w/rd",
                "docker_image": "gotempsh/postgres-walg:18-bookworm",
            }),
        }
    }

    #[test]
    fn accepts_both_schemes_and_allowed_options() {
        let engine = service();
        for url in [
            "postgres://u:p@db.example.com/app",
            "postgresql://u:p@db.example.com:6543/app?sslmode=verify-full&connect_timeout=10",
            "postgres://u:p@a.example.com,b.example.com/app?target_session_attrs=read-write",
        ] {
            engine.parse_source(url).expect(url);
        }
    }

    #[test]
    fn refuses_bad_option_values() {
        let engine = service();
        for url in [
            "postgres://u:p@db.example.com/app?sslmode=sometimes",
            "postgres://u:p@db.example.com/app?connect_timeout=ten",
            "postgres://u:p@db.example.com/app?application_name=a%20b",
        ] {
            let error = engine.parse_source(url).expect_err(url);
            assert!(
                matches!(error, DataImportError::InvalidSource { .. }),
                "{url}"
            );
        }
    }

    #[test]
    fn target_name_follows_provisioning_rules_and_refuses_system_databases() {
        let engine = service();
        engine
            .validate_target_database("shop_production")
            .expect("valid");
        for bad in ["Shop", "1shop", "shop-prod", "", "postgres", "template1"] {
            let error = engine.validate_target_database(bad).expect_err(bad);
            assert!(
                matches!(error, DataImportError::InvalidTargetDatabase { .. }),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn plan_uses_the_service_image_and_keeps_secrets_in_env() {
        let engine = service();
        let source = engine
            .parse_source("postgres://reader:s3cret@db.example.com/shop")
            .expect("source");
        let plan = engine
            .transfer_plan(
                &config(),
                &source,
                &TransferTarget {
                    host: "postgres-orders",
                    port: "5432",
                    database: "shop_production",
                },
            )
            .await
            .expect("plan");
        assert_eq!(plan.image, "gotempsh/postgres-walg:18-bookworm");
        let target = plan
            .env
            .iter()
            .find(|e| e.name == "TEMPS_IMPORT_TARGET")
            .expect("target env");
        assert!(target.secret);
        assert_eq!(
            target.value,
            "postgres://app:p%40ss%3Aw%2Frd@postgres-orders:5432/shop_production?sslmode=disable"
        );
        let script = compose_script(&plan.producer, &plan.consumer);
        for secret in ["s3cret", "p@ss:w/rd", "p%40ss"] {
            assert!(
                !script.contains(secret),
                "secret {secret} leaked into the command"
            );
        }
    }

    #[test]
    fn commit_is_only_emitted_after_a_successful_dump() {
        // The atomicity guarantee rests on this ordering.
        let begin = PRODUCER.find("BEGIN").expect("begin");
        let dump = PRODUCER.find("pg_dump").expect("dump");
        let commit = PRODUCER.find("COMMIT").expect("commit");
        assert!(begin < dump && dump < commit);
        assert!(PRODUCER.contains("--no-owner"));
        assert!(!CONSUMER.contains("--single-transaction"));
        assert!(CONSUMER.contains("ON_ERROR_STOP=1"));
    }

    #[test]
    fn hints_cover_the_common_failures() {
        let cases = [
            (
                "pg_dump: error: aborting because of server version mismatch",
                "newer PostgreSQL",
            ),
            (
                "FATAL:  password authentication failed for user \"x\"",
                "user name or password",
            ),
            ("FATAL:  no pg_hba.conf entry for host", "pg_hba.conf"),
            (
                "connection to server failed: Connection refused",
                "could not be reached",
            ),
            ("ERROR:  relation \"users\" already exists", "replace"),
        ];
        for (output, expected) in cases {
            let hint = postgres_failure_hint(output).expect(output);
            assert!(hint.contains(expected), "{output} -> {hint}");
        }
        assert_eq!(postgres_failure_hint("something else entirely"), None);
    }

    /// Docker end-to-end: a PostgreSQL 16 source with data is imported into
    /// a PostgreSQL 18 target through the real runner, re-imported with
    /// replace (no duplicates), and a source that refuses the login leaves
    /// the freshly prepared target empty — the BEGIN without COMMIT rolled
    /// back.
    #[tokio::test]
    async fn imports_a_pg16_source_into_a_pg18_target_atomically() {
        use futures::FutureExt;
        let Some(mut docker) = TestDocker::connect().await else {
            println!("Docker not available, skipping");
            return;
        };
        let result = std::panic::AssertUnwindSafe(postgres_scenario(&mut docker))
            .catch_unwind()
            .await;
        docker.finish(result).await;
    }

    use crate::data_import::runner::{run_helper, HelperOutcome, HelperRequest};
    use crate::data_import::test_support::{leftover_helpers, TestDocker};
    use crate::data_import::{plan_target_preparation, TargetPreparation};

    const TARGET_IMAGE: &str = "gotempsh/postgres-walg:18-bookworm";
    const TARGET_PASSWORD: &str = "t@rget:pa/ss";

    async fn count_items(docker: &TestDocker, target: &str) -> String {
        let (ok, output) = docker
            .sh(
                target,
                "psql -h 127.0.0.1 -U app -d shop_production -tAc 'SELECT count(*) FROM items'",
                vec![format!("PGPASSWORD={TARGET_PASSWORD}")],
            )
            .await;
        assert!(ok, "count failed: {output}");
        output.trim().to_string()
    }

    async fn transfer(
        docker: &TestDocker,
        engine: &PostgresService,
        config: &ServiceConfig,
        target: &str,
        source_url: &str,
        run_id: i32,
    ) -> HelperOutcome {
        let source = engine.parse_source(source_url).expect("source");
        let plan = engine
            .transfer_plan(
                config,
                &source,
                &TransferTarget {
                    host: target,
                    port: "5432",
                    database: "shop_production",
                },
            )
            .await
            .expect("plan");
        let mut secrets = source.secrets();
        secrets.extend(
            plan.env
                .iter()
                .filter(|e| e.secret)
                .map(|e| e.value.clone()),
        );
        let name = format!("temps-data-import-test-{run_id}");
        let outcome = run_helper(
            &docker.docker,
            &HelperRequest {
                run_id,
                service_id: 1,
                container_name: &name,
                plan: &plan,
                network: &docker.network,
                extra_hosts: vec![],
                timeout: Duration::from_secs(300),
                secrets: &secrets,
            },
        )
        .await
        .expect("helper ran");
        assert_eq!(leftover_helpers(&docker.docker, run_id).await, 0);
        outcome
    }

    async fn postgres_scenario(docker: &mut TestDocker) {
        let source = docker
            .run(
                "pg-source",
                "postgres:16-bookworm",
                vec!["POSTGRES_PASSWORD=source-pass".to_string()],
                None,
            )
            .await;
        let target = docker
            .run(
                "pg-target",
                TARGET_IMAGE,
                vec![
                    "POSTGRES_USER=app".to_string(),
                    format!("POSTGRES_PASSWORD={TARGET_PASSWORD}"),
                    "POSTGRES_DB=postgres".to_string(),
                ],
                Some("5432/tcp"),
            )
            .await;
        let ready = "pg_isready -h 127.0.0.1 -q";
        docker
            .wait_for(&source.name, ready, vec![], Duration::from_secs(90))
            .await;
        docker
            .wait_for(&target.name, ready, vec![], Duration::from_secs(90))
            .await;
        let (seeded, output) = docker
            .sh(
                &source.name,
                "psql -h 127.0.0.1 -U postgres -c 'CREATE DATABASE shop' && \
                 psql -h 127.0.0.1 -U postgres -d shop -v ON_ERROR_STOP=1 -c \
                 \"CREATE TABLE items (id serial PRIMARY KEY, name text NOT NULL); \
                   INSERT INTO items (name) SELECT 'item-' || g FROM generate_series(1, 250) g;\"",
                vec!["PGPASSWORD=source-pass".to_string()],
            )
            .await;
        assert!(seeded, "seeding the source failed: {output}");

        let host_port = target.host_port.expect("published port");
        let config = ServiceConfig {
            name: "e2e".to_string(),
            service_type: crate::externalsvc::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "127.0.0.1",
                "port": host_port.to_string(),
                "database": "postgres",
                "username": "app",
                "password": TARGET_PASSWORD,
                "docker_image": TARGET_IMAGE,
                "container_name": target.name,
            }),
        };
        let engine = PostgresService::new(
            "e2e".to_string(),
            std::sync::Arc::new(docker.docker.clone()),
        );
        assert_eq!(
            engine.target_container(&config).expect("container"),
            target.name
        );
        let good_source = format!("postgres://postgres:source-pass@{}:5432/shop", source.name);

        // 1. Fresh import into a database that does not exist yet.
        let inspection = engine
            .inspect_target(&config, "shop_production")
            .await
            .expect("inspect");
        assert!(!inspection.exists);
        let preparation = plan_target_preparation(1, "shop_production", inspection, false, "table")
            .expect("plan");
        assert_eq!(preparation, TargetPreparation::Create);
        engine
            .prepare_target(&config, "shop_production", preparation)
            .await
            .expect("prepare");
        let run_base = (std::process::id() as i32 % 10_000) * 10;
        let outcome = transfer(
            docker,
            &engine,
            &config,
            &target.name,
            &good_source,
            run_base + 1,
        )
        .await;
        assert!(
            matches!(outcome, HelperOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        assert_eq!(count_items(docker, &target.name).await, "250");
        let (ok, owner) = docker
            .sh(
                &target.name,
                "psql -h 127.0.0.1 -U app -d shop_production -tAc \
                 \"SELECT tableowner FROM pg_tables WHERE tablename = 'items'\"",
                vec![format!("PGPASSWORD={TARGET_PASSWORD}")],
            )
            .await;
        assert!(ok, "{owner}");
        assert_eq!(
            owner.trim(),
            "app",
            "imported objects belong to the service user"
        );

        // 2. The database now holds data: refused without replace…
        let inspection = engine
            .inspect_target(&config, "shop_production")
            .await
            .expect("inspect");
        assert!(inspection.exists);
        assert_eq!(inspection.object_count, 1);
        assert!(inspection.size_bytes.unwrap_or(0) > 0);
        assert!(plan_target_preparation(1, "shop_production", inspection, false, "table").is_err());

        // …and replaced, not duplicated, with it.
        engine
            .prepare_target(&config, "shop_production", TargetPreparation::Recreate)
            .await
            .expect("recreate");
        let outcome = transfer(
            docker,
            &engine,
            &config,
            &target.name,
            &good_source,
            run_base + 2,
        )
        .await;
        assert!(
            matches!(outcome, HelperOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        assert_eq!(count_items(docker, &target.name).await, "250");

        // 3. A source that refuses the login fails on the source side, its
        //    password never shows, and nothing is committed.
        engine
            .prepare_target(&config, "shop_production", TargetPreparation::Recreate)
            .await
            .expect("recreate");
        let bad_source = format!("postgres://postgres:wrong-pass@{}:5432/shop", source.name);
        let outcome = transfer(
            docker,
            &engine,
            &config,
            &target.name,
            &bad_source,
            run_base + 3,
        )
        .await;
        let HelperOutcome::SourceFailed { output } = outcome else {
            panic!("expected a source failure, got {outcome:?}");
        };
        assert!(!output.contains("wrong-pass"), "{output}");
        assert!(!output.contains(TARGET_PASSWORD), "{output}");
        assert!(
            engine
                .failure_hint(&output)
                .is_some_and(|hint| hint.contains("user name or password")),
            "{output}"
        );
        let inspection = engine
            .inspect_target(&config, "shop_production")
            .await
            .expect("inspect");
        assert_eq!(
            inspection.object_count, 0,
            "a failed import must leave no tables"
        );
    }
}
