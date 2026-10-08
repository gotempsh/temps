// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MariaDB (and MySQL-source) support for importing data from an external
//! server.
//!
//! The copy is `mariadb-dump | mariadb`, run with the service's own image so
//! the client matches the server it writes into. It is applied as root —
//! the only account allowed to create functions while binary logging (on for
//! point-in-time recovery) is enabled — and every `DEFINER` in the dump is
//! rewritten to the service user, so routines, views and triggers end up
//! owned by the account deployments use instead of a user that only exists
//! on the source.
//!
//! Not atomic: MariaDB commits DDL implicitly, so a failed import can leave
//! part of the data behind. The spec says so and the console warns.

use std::time::Duration;

use async_trait::async_trait;

use super::{MariaDbConfig, MariaDbService};
use crate::data_import::source::{parse_source_url, scrub_secrets, SourceUrlRules};
use crate::data_import::{
    DataImportEngine, DataImportError, DataImportSpec, ImportSource, TargetInspection,
    TargetPreparation, TransferEnv, TransferPlan, TransferTarget,
};
use crate::externalsvc::ServiceConfig;

/// Schemas every MariaDB server has, never an import target.
const RESERVED_DATABASES: [&str; 4] = ["mysql", "information_schema", "performance_schema", "sys"];

/// Bound on each control-plane SQL step.
const ADMIN_SQL_TIMEOUT: Duration = Duration::from_secs(60);

const SOURCE_RULES: SourceUrlRules<'static> = SourceUrlRules {
    schemes: &["mysql", "mariadb"],
    default_port: 3306,
    allowed_options: &["ssl-mode"],
    options_case_insensitive: true,
    max_hosts: 1,
    default_database: None,
};

/// Accepted `ssl-mode` values (MySQL spelling) and the client flags each
/// maps to. Without the option the client's own default applies.
const SSL_MODES: [(&str, &str); 5] = [
    ("DISABLED", "--skip-ssl"),
    ("PREFERRED", ""),
    ("REQUIRED", "--ssl --skip-ssl-verify-server-cert"),
    ("VERIFY_CA", "--ssl --ssl-verify-server-cert"),
    ("VERIFY_IDENTITY", "--ssl --ssl-verify-server-cert"),
];

/// Dumps the source database. `$TEMPS_IMPORT_SOURCE_TLS` is deliberately
/// unquoted: it holds zero or more fixed flags from [`SSL_MODES`].
const PRODUCER: &str = "DUMP=$(command -v mariadb-dump || command -v mysqldump) || \
     { echo 'temps-import: neither mariadb-dump nor mysqldump is in the image' >&2; exit 127; }; \
     MYSQL_PWD=\"$TEMPS_IMPORT_SOURCE_PASSWORD\" MARIADB_PWD=\"$TEMPS_IMPORT_SOURCE_PASSWORD\" \
     \"$DUMP\" --host=\"$TEMPS_IMPORT_SOURCE_HOST\" --port=\"$TEMPS_IMPORT_SOURCE_PORT\" \
     --user=\"$TEMPS_IMPORT_SOURCE_USER\" $TEMPS_IMPORT_SOURCE_TLS \
     --single-transaction --quick --routines --triggers --hex-blob --no-tablespaces \
     --default-character-set=utf8mb4 \"$TEMPS_IMPORT_SOURCE_DATABASE\"";

/// Rewrites every DEFINER to the service user, then applies the dump as root.
const CONSUMER: &str = "CLIENT=$(command -v mariadb || command -v mysql) || \
     { echo 'temps-import: neither mariadb nor mysql is in the image' >&2; exit 127; }; \
     sed -E -e \"$TEMPS_IMPORT_DEFINER_SED\" | \
     MYSQL_PWD=\"$TEMPS_IMPORT_TARGET_PASSWORD\" MARIADB_PWD=\"$TEMPS_IMPORT_TARGET_PASSWORD\" \
     \"$CLIENT\" --host=\"$TEMPS_IMPORT_TARGET_HOST\" --port=\"$TEMPS_IMPORT_TARGET_PORT\" \
     --user=root --skip-ssl --default-character-set=utf8mb4 \"$TEMPS_IMPORT_TARGET_DATABASE\"";

/// The `sed -E` program that rewrites definers to `user`@`%`.
///
/// Only definition statements are touched: the version-comment form
/// `mariadb-dump` writes for views, triggers and events
/// (`/*!50013 DEFINER=…`, `/*!50017 DEFINER=…`, `/*!50117 DEFINER=…`) and
/// `CREATE DEFINER=…` at the start of a routine. Data lines (`INSERT …`)
/// are skipped entirely, so a row value that happens to contain
/// `DEFINER=` is imported byte for byte. `user` has passed
/// `validate_identifier` (`[A-Za-z0-9_]`), so it cannot break the program.
fn definer_sed_program(user: &str) -> String {
    format!(
        "/^INSERT /!{{s#/\\*!([0-9]{{5}}) DEFINER=`[^`]*`@`[^`]*`#/*!\\1 DEFINER=`{user}`@`%`#g;\
         s#^CREATE DEFINER=`[^`]*`@`[^`]*`#CREATE DEFINER=`{user}`@`%`#;}}"
    )
}

impl MariaDbService {
    fn import_config(&self, config: &ServiceConfig) -> Result<MariaDbConfig, DataImportError> {
        self.get_mariadb_config(config.clone()).map_err(|e| {
            DataImportError::target(
                &config.name,
                "read the MariaDB configuration",
                e.to_string(),
            )
        })
    }

    /// Run one query as root inside the service container and return its
    /// tab-separated output.
    async fn import_query(
        &self,
        service: &str,
        maria: &MariaDbConfig,
        operation: &str,
        sql: &str,
    ) -> Result<String, DataImportError> {
        let container = self.get_live_container_name(maria);
        self.run_container_command(
            &container,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "if command -v mariadb >/dev/null 2>&1; then \
                     mariadb -uroot -N -B -e \"$TEMPS_MARIADB_SQL\"; \
                 else \
                     mysql -uroot -N -B -e \"$TEMPS_MARIADB_SQL\"; \
                 fi"
                .to_string(),
            ],
            Some(vec![
                format!("MYSQL_PWD={}", maria.root_password),
                format!("MARIADB_PWD={}", maria.root_password),
                format!("TEMPS_MARIADB_SQL={}", sql),
            ]),
            ADMIN_SQL_TIMEOUT,
        )
        .await
        .map_err(|e| {
            DataImportError::target(
                service,
                operation,
                scrub_secrets(&e.to_string(), std::slice::from_ref(&maria.root_password)),
            )
        })
    }
}

/// The `exists, tables, bytes` row of the inspection query, ignoring any
/// warning lines the client prints around it.
fn parse_inspection(output: &str) -> Option<TargetInspection> {
    output.lines().rev().find_map(|line| {
        let mut fields = line.trim().split('\t');
        let exists: i64 = fields.next()?.parse().ok()?;
        let tables: i64 = fields.next()?.parse().ok()?;
        let bytes: i64 = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(TargetInspection {
            exists: exists > 0,
            object_count: tables,
            size_bytes: (exists > 0).then_some(bytes),
        })
    })
}

fn tls_flags(source: &ImportSource) -> Result<&'static str, DataImportError> {
    match source.option("ssl-mode") {
        None => Ok(""),
        Some(value) => SSL_MODES
            .iter()
            .find(|(mode, _)| mode.eq_ignore_ascii_case(value))
            .map(|(_, flags)| *flags)
            .ok_or_else(|| {
                DataImportError::invalid_source(format!(
                    "ssl-mode '{}' is not supported; use one of {}",
                    value,
                    SSL_MODES
                        .iter()
                        .map(|(m, _)| *m)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }),
    }
}

#[async_trait]
impl DataImportEngine for MariaDbService {
    fn import_spec(&self) -> DataImportSpec {
        DataImportSpec {
            engine_label: "MariaDB / MySQL".to_string(),
            source_schemes: SOURCE_RULES.schemes.iter().map(|s| s.to_string()).collect(),
            source_url_example: "mysql://user:password@db.example.com:3306/app?ssl-mode=REQUIRED"
                .to_string(),
            allowed_source_options: SOURCE_RULES
                .allowed_options
                .iter()
                .map(|s| s.to_string())
                .collect(),
            atomic: false,
            object_noun: "table".to_string(),
            max_target_length: 63,
        }
    }

    fn parse_source(&self, raw: &str) -> Result<ImportSource, DataImportError> {
        let source = parse_source_url(raw, &SOURCE_RULES)?;
        if source.username().is_none() {
            return Err(DataImportError::invalid_source(
                "the source connection string must include a user name (mysql://user:password@…)",
            ));
        }
        tls_flags(&source)?;
        Ok(source)
    }

    fn validate_target_database(&self, database: &str) -> Result<(), DataImportError> {
        Self::validate_identifier("database", database)
            .map_err(|e| DataImportError::invalid_target_database(database, e.to_string()))?;
        if RESERVED_DATABASES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(database))
        {
            return Err(DataImportError::invalid_target_database(
                database,
                "it is a MariaDB system schema",
            ));
        }
        Ok(())
    }

    async fn inspect_target(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError> {
        // Interpolated into the query below.
        self.validate_target_database(database)?;
        let maria = self.import_config(config)?;
        let sql = format!(
            "SELECT \
               (SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{database}'), \
               (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{database}'), \
               (SELECT COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) \
                  FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{database}')"
        );
        let operation = format!("inspect database '{database}'");
        let output = self
            .import_query(&config.name, &maria, &operation, &sql)
            .await?;
        parse_inspection(&output).ok_or_else(|| {
            DataImportError::target(
                &config.name,
                operation,
                format!(
                    "unexpected output from the inspection query: {}",
                    output.trim()
                ),
            )
        })
    }

    async fn prepare_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
    ) -> Result<(), DataImportError> {
        self.validate_target_database(database)?;
        let service = config.name.as_str();
        match preparation {
            TargetPreparation::UseExisting => return Ok(()),
            TargetPreparation::Recreate => {
                self.drop_database(config.clone(), database)
                    .await
                    .map_err(|e| {
                        DataImportError::target(
                            service,
                            format!("drop database '{database}'"),
                            e.to_string(),
                        )
                    })?;
            }
            TargetPreparation::Create => {}
        }
        // Same path as project provisioning: the database plus the service
        // user's grant on it.
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

    fn target_container(&self, config: &ServiceConfig) -> Result<String, DataImportError> {
        Ok(self.get_live_container_name(&self.import_config(config)?))
    }

    async fn transfer_plan(
        &self,
        config: &ServiceConfig,
        source: &ImportSource,
        target: &TransferTarget<'_>,
    ) -> Result<TransferPlan, DataImportError> {
        let maria = self.import_config(config)?;
        let endpoint = source.endpoints().first().ok_or_else(|| {
            DataImportError::invalid_source("the source connection string names no host")
        })?;
        Ok(TransferPlan {
            image: maria.docker_image.clone(),
            producer: PRODUCER.to_string(),
            consumer: CONSUMER.to_string(),
            env: vec![
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_HOST", endpoint.host.clone()),
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_PORT", endpoint.port.to_string()),
                TransferEnv::plain(
                    "TEMPS_IMPORT_SOURCE_USER",
                    source.username().unwrap_or_default(),
                ),
                TransferEnv::secret(
                    "TEMPS_IMPORT_SOURCE_PASSWORD",
                    source.password().unwrap_or_default(),
                ),
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_TLS", tls_flags(source)?),
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_DATABASE", source.database()),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_HOST", target.host),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_PORT", target.port),
                TransferEnv::secret("TEMPS_IMPORT_TARGET_PASSWORD", maria.root_password),
                TransferEnv::plain(
                    "TEMPS_IMPORT_DEFINER_SED",
                    definer_sed_program(&maria.username),
                ),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_DATABASE", target.database),
            ],
        })
    }

    fn failure_hint(&self, output: &str) -> Option<String> {
        mariadb_failure_hint(output)
    }
}

fn mariadb_failure_hint(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let hint = if lower.contains("access denied for user") {
        "the source rejected the user name or password, or the user may not connect from this \
         server's address"
    } else if lower.contains("ssl is required") || lower.contains("tls/ssl error") {
        "TLS could not be negotiated with the source; add ?ssl-mode=DISABLED if it does not \
         support TLS, or ?ssl-mode=REQUIRED to encrypt without verifying its certificate"
    } else if lower.contains("certificate") {
        "the source's TLS certificate could not be verified; use ?ssl-mode=REQUIRED to encrypt \
         without verifying it"
    } else if lower.contains("can't connect") || lower.contains("lost connection") {
        "the source could not be reached from this server; check its address, port and firewall"
    } else if lower.contains("unknown collation") {
        "the dump uses a collation this MariaDB version does not know (MySQL 8's \
         utf8mb4_0900_* collations are a common case); convert those tables to \
         utf8mb4_unicode_ci on the source, or run this service on a newer MariaDB"
    } else if lower.contains("max_allowed_packet") {
        "a row in the dump is larger than the server's max_allowed_packet"
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
    use crate::data_import::plan_target_preparation;
    use crate::data_import::runner::{compose_script, run_helper, HelperOutcome, HelperRequest};
    use crate::data_import::test_support::{leftover_helpers, TestDocker};

    fn service() -> MariaDbService {
        let docker = std::sync::Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("docker client value"),
        );
        MariaDbService::new("orders".to_string(), docker)
    }

    #[test]
    fn source_needs_a_user_and_accepts_known_ssl_modes() {
        let engine = service();
        engine
            .parse_source("mysql://reader:pw@db.example.com/shop?ssl-mode=required")
            .expect("valid");
        engine
            .parse_source("mariadb://reader@db.example.com:3307/shop")
            .expect("password is optional");
        for url in [
            "mysql://db.example.com/shop",
            "mysql://u:p@db.example.com/shop?ssl-mode=sometimes",
            "mysql://u:p@a.example.com,b.example.com/shop",
            "mysql://u:p@db.example.com/shop?ssl-ca=/etc/passwd",
        ] {
            let error = engine.parse_source(url).expect_err(url);
            assert!(
                matches!(error, DataImportError::InvalidSource { .. }),
                "{url}"
            );
        }
    }

    #[test]
    fn target_name_must_be_an_identifier_and_not_a_system_schema() {
        let engine = service();
        engine
            .validate_target_database("shop_production")
            .expect("valid");
        for bad in [
            "shop-prod",
            "1shop",
            "",
            "mysql",
            "SYS",
            "information_schema",
        ] {
            assert!(engine.validate_target_database(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn inspection_output_is_parsed_past_client_warnings() {
        let output = "Warning: something deprecated\n1\t4\t81920\n";
        assert_eq!(
            parse_inspection(output),
            Some(TargetInspection {
                exists: true,
                object_count: 4,
                size_bytes: Some(81920),
            })
        );
        assert_eq!(
            parse_inspection("0\t0\t0"),
            Some(TargetInspection {
                exists: false,
                object_count: 0,
                size_bytes: None,
            })
        );
        assert_eq!(parse_inspection("ERROR 1045"), None);
    }

    #[test]
    fn plan_keeps_passwords_out_of_the_command() {
        let script = compose_script(PRODUCER, CONSUMER);
        assert!(script.contains("$TEMPS_IMPORT_SOURCE_PASSWORD"));
        assert!(script.contains("--single-transaction"));
        assert!(!script.contains("--password"));
    }

    #[test]
    fn definer_rewrite_skips_data_lines() {
        let program = definer_sed_program("app");
        assert!(program.starts_with("/^INSERT /!{"), "{program}");
        assert!(program.contains("DEFINER=`app`@`%`"), "{program}");
        assert!(
            !program.contains("s/DEFINER="),
            "no unanchored global rewrite: {program}"
        );
    }

    #[test]
    fn hints_cover_the_common_failures() {
        let cases = [
            ("ERROR 1045 (28000): Access denied for user 'x'@'1.2.3.4'", "user name or password"),
            ("ERROR 2026 (HY000): TLS/SSL error: SSL is required, but the server does not support it", "ssl-mode=DISABLED"),
            ("ERROR 1273 (HY000) at line 25: Unknown collation: 'utf8mb4_0900_ai_ci'", "utf8mb4_unicode_ci"),
            ("ERROR 2002 (HY000): Can't connect to server on 'db'", "could not be reached"),
        ];
        for (output, expected) in cases {
            let hint = mariadb_failure_hint(output).expect(output);
            assert!(hint.contains(expected), "{output} -> {hint}");
        }
        assert_eq!(mariadb_failure_hint("fine"), None);
    }

    const ROOT_PASSWORD: &str = "root-pa$$word";
    const APP_PASSWORD: &str = "app-pa$$word";

    /// Docker end-to-end: a MariaDB source with a table and a trigger defined
    /// by a source-only user is imported into a MariaDB target; the trigger's
    /// definer becomes the service user, and a re-import with replace does
    /// not duplicate rows.
    #[tokio::test]
    async fn imports_a_mariadb_source_and_rewrites_definers() {
        use futures::FutureExt;
        let Some(mut docker) = TestDocker::connect().await else {
            println!("Docker not available, skipping");
            return;
        };
        let result = std::panic::AssertUnwindSafe(mariadb_scenario(&mut docker))
            .catch_unwind()
            .await;
        docker.finish(result).await;
    }

    async fn target_sql(docker: &TestDocker, target: &str, sql: &str) -> String {
        let (ok, output) = docker
            .sh(
                target,
                "mariadb -uroot -N -B -e \"$SQL\"",
                vec![format!("MYSQL_PWD={ROOT_PASSWORD}"), format!("SQL={sql}")],
            )
            .await;
        assert!(ok, "{sql}: {output}");
        output.trim().to_string()
    }

    async fn mariadb_scenario(docker: &mut TestDocker) {
        if !docker.ensure_images(&["mariadb:11.4"]).await {
            return;
        }
        let image_id = docker
            .docker
            .inspect_image("mariadb:11.4")
            .await
            .ok()
            .and_then(|image| image.id);
        let Some(image_id) = image_id else {
            println!("mariadb:11.4 has no image id, skipping");
            return;
        };
        let source = docker
            .run(
                "maria-source",
                "mariadb:11.4",
                vec![
                    "MARIADB_ROOT_PASSWORD=source-root".to_string(),
                    "MARIADB_DATABASE=shop".to_string(),
                    "MARIADB_USER=reader".to_string(),
                    "MARIADB_PASSWORD=reader-pass".to_string(),
                ],
                None,
            )
            .await;
        let target = docker
            .run(
                "maria-target",
                &image_id,
                vec![format!("MARIADB_ROOT_PASSWORD={ROOT_PASSWORD}")],
                None,
            )
            .await;
        let ready = "mariadb-admin ping -h 127.0.0.1 -uroot --silent";
        docker
            .wait_for(
                &source.name,
                ready,
                vec!["MYSQL_PWD=source-root".to_string()],
                Duration::from_secs(120),
            )
            .await;
        docker
            .wait_for(
                &target.name,
                ready,
                vec![format!("MYSQL_PWD={ROOT_PASSWORD}")],
                Duration::from_secs(120),
            )
            .await;
        let (seeded, output) = docker
            .sh(
                &source.name,
                "mariadb -uroot shop -e \"$SQL\"",
                vec![
                    "MYSQL_PWD=source-root".to_string(),
                    "SQL=CREATE USER 'legacy_owner'@'%' IDENTIFIED BY 'x-legacy-1'; \
                     GRANT ALL ON shop.* TO 'legacy_owner'@'%'; \
                     CREATE TABLE items (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(64), name_len INT); \
                     CREATE DEFINER='legacy_owner'@'%' TRIGGER items_len BEFORE INSERT ON items \
                       FOR EACH ROW SET NEW.name_len = CHAR_LENGTH(NEW.name); \
                     INSERT INTO items (name) SELECT CONCAT('item-', seq) FROM seq_1_to_120; \
                     CREATE TABLE notes (id INT PRIMARY KEY, body TEXT); \
                     INSERT INTO notes VALUES (1, 'kept DEFINER=`legacy_owner`@`%` as written'); \
                     CREATE DEFINER='legacy_owner'@'%' PROCEDURE count_items() SELECT COUNT(*) FROM items;"
                        .to_string(),
                ],
            )
            .await;
        assert!(seeded, "seeding failed: {output}");

        let config = ServiceConfig {
            name: "e2e".to_string(),
            service_type: crate::externalsvc::ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({
                "host": "127.0.0.1",
                "port": "3306",
                "database": "app",
                "username": "app",
                "password": APP_PASSWORD,
                "root_password": ROOT_PASSWORD,
                "docker_image": image_id,
                "container_name": target.name,
            }),
        };
        let engine = MariaDbService::new(
            "e2e".to_string(),
            std::sync::Arc::new(docker.docker.clone()),
        );
        let source_url = format!(
            "mysql://reader:reader-pass@{}:3306/shop?ssl-mode=DISABLED",
            source.name
        );
        let run_base = (std::process::id() as i32 % 10_000) * 10 + 5;

        for (attempt, replace) in [(0, false), (1, true)] {
            let inspection = engine
                .inspect_target(&config, "shop_production")
                .await
                .expect("inspect");
            let preparation =
                plan_target_preparation(1, "shop_production", inspection, replace, "table")
                    .expect("plan");
            engine
                .prepare_target(&config, "shop_production", preparation)
                .await
                .expect("prepare");
            let parsed = engine.parse_source(&source_url).expect("source");
            let plan = engine
                .transfer_plan(
                    &config,
                    &parsed,
                    &TransferTarget {
                        host: &target.name,
                        port: "3306",
                        database: "shop_production",
                    },
                )
                .await
                .expect("plan");
            let mut secrets = parsed.secrets();
            secrets.extend(
                plan.env
                    .iter()
                    .filter(|e| e.secret)
                    .map(|e| e.value.clone()),
            );
            let run_id = run_base + attempt;
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
            assert!(
                matches!(outcome, HelperOutcome::Succeeded { .. }),
                "attempt {attempt}: {outcome:?}"
            );
            assert_eq!(leftover_helpers(&docker.docker, run_id).await, 0);
            assert_eq!(
                target_sql(
                    docker,
                    &target.name,
                    "SELECT COUNT(*) FROM shop_production.items"
                )
                .await,
                "120",
                "attempt {attempt}"
            );
        }

        assert_eq!(
            target_sql(
                docker,
                &target.name,
                "SELECT DEFINER FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = 'shop_production'"
            )
            .await,
            "app@%"
        );
        assert_eq!(
            target_sql(
                docker,
                &target.name,
                "SELECT DEFINER FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = 'shop_production'"
            )
            .await,
            "app@%",
            "routines are re-owned too"
        );
        assert_eq!(
            target_sql(
                docker,
                &target.name,
                "SELECT body FROM shop_production.notes WHERE id = 1"
            )
            .await,
            "kept DEFINER=`legacy_owner`@`%` as written",
            "row values must never be rewritten"
        );
        let inspection = engine
            .inspect_target(&config, "shop_production")
            .await
            .expect("inspect");
        assert_eq!(inspection.object_count, 2);
        assert!(plan_target_preparation(1, "shop_production", inspection, false, "table").is_err());
    }
}
