// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MongoDB support for importing data from an external server.
//!
//! The copy is `mongodump --archive | mongorestore --archive`, renaming the
//! source database to the target one on the way (`--nsFrom`/`--nsTo`), run
//! with the same MongoDB tools image the engine's restore sidecar uses.
//!
//! Not atomic: a failed import can leave some collections behind. The spec
//! says so and the console warns.
//!
//! `mongodb+srv://` sources are refused: their hosts come from a DNS SRV
//! lookup made by the tool itself, so they could be neither checked by the
//! SSRF guard nor pinned to the checked address.

use std::time::Duration;

use async_trait::async_trait;
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::ClientOptions;
use mongodb::Client as MongoClient;

use super::{MongodbRuntimeConfig, MongodbService, MONGO_SIDECAR_IMAGE};
use crate::data_import::source::{
    parse_source_url, percent_encode_userinfo, scrub_secrets, SourceUrlRules,
};
use crate::data_import::{
    DataImportEngine, DataImportError, DataImportSpec, ImportSource, TargetInspection,
    TargetPreparation, TransferEnv, TransferPlan, TransferTarget,
};
use crate::externalsvc::ServiceConfig;

/// Databases every MongoDB server has, never an import target.
const RESERVED_DATABASES: [&str; 3] = ["admin", "local", "config"];

/// Collection project provisioning creates so an empty database exists.
/// It does not make a database "hold data".
const INIT_COLLECTION: &str = "_temps_init";

/// Bound on each control-plane step.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest database name MongoDB accepts.
const MAX_DATABASE_LEN: usize = 63;

const SOURCE_RULES: SourceUrlRules<'static> = SourceUrlRules {
    schemes: &["mongodb"],
    default_port: 27017,
    // Options that read local files (tlsCAFile, tlsCertificateKeyFile) or
    // carry credentials (authMechanismProperties) are deliberately absent.
    allowed_options: &[
        "authSource",
        "authMechanism",
        "tls",
        "ssl",
        "tlsAllowInvalidCertificates",
        "tlsAllowInvalidHostnames",
        "tlsInsecure",
        "replicaSet",
        "directConnection",
        "readPreference",
        "appName",
        "connectTimeoutMS",
        "serverSelectionTimeoutMS",
        "socketTimeoutMS",
        "retryReads",
        "compressors",
    ],
    options_case_insensitive: true,
    max_hosts: 16,
    default_database: None,
};

const AUTH_MECHANISMS: [&str; 3] = ["DEFAULT", "SCRAM-SHA-1", "SCRAM-SHA-256"];

const PRODUCER: &str = "mongodump --uri=\"$TEMPS_IMPORT_SOURCE\" \
     --db=\"$TEMPS_IMPORT_SOURCE_DATABASE\" --archive";

const CONSUMER: &str = "mongorestore --uri=\"$TEMPS_IMPORT_TARGET\" --archive --stopOnError \
     --nsFrom=\"$TEMPS_IMPORT_SOURCE_DATABASE.*\" --nsTo=\"$TEMPS_IMPORT_TARGET_DATABASE.*\"";

impl MongodbService {
    fn import_config(
        &self,
        config: &ServiceConfig,
    ) -> Result<MongodbRuntimeConfig, DataImportError> {
        self.get_mongodb_config(config.clone()).map_err(|e| {
            DataImportError::target(
                &config.name,
                "read the MongoDB configuration",
                e.to_string(),
            )
        })
    }

    /// Control-plane client for the service, built from `config` (a freshly
    /// constructed engine holds no config of its own).
    async fn import_client(
        &self,
        service: &str,
        mongo: &MongodbRuntimeConfig,
    ) -> Result<MongoClient, DataImportError> {
        let uri = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin&directConnection=true\
             &serverSelectionTimeoutMS=10000&connectTimeoutMS=10000",
            percent_encode_userinfo(&mongo.username),
            percent_encode_userinfo(&mongo.password),
            mongo.host,
            mongo.port
        );
        let options = ClientOptions::parse(&uri).await.map_err(|e| {
            DataImportError::target(
                service,
                "build the MongoDB client",
                scrub_secrets(&e.to_string(), &[uri.clone(), mongo.password.clone()]),
            )
        })?;
        MongoClient::with_options(options).map_err(|e| {
            DataImportError::target(service, "build the MongoDB client", e.to_string())
        })
    }
}

/// Run a driver call bounded by [`ADMIN_TIMEOUT`], mapping both failure modes.
async fn bounded<T, F>(
    service: &str,
    operation: &str,
    secret: &str,
    call: F,
) -> Result<T, DataImportError>
where
    F: std::future::IntoFuture<Output = mongodb::error::Result<T>>,
{
    tokio::time::timeout(ADMIN_TIMEOUT, call.into_future())
        .await
        .map_err(|_| {
            DataImportError::target(
                service,
                operation,
                format!("timed out after {}s", ADMIN_TIMEOUT.as_secs()),
            )
        })?
        .map_err(|e| {
            DataImportError::target(
                service,
                operation,
                scrub_secrets(&e.to_string(), &[secret.to_string()]),
            )
        })
}

fn number(document: &Document, key: &str) -> Option<i64> {
    match document.get(key)? {
        Bson::Int32(value) => Some(i64::from(*value)),
        Bson::Int64(value) => Some(*value),
        Bson::Double(value) => Some(*value as i64),
        _ => None,
    }
}

/// Collections that count as data.
fn data_collections(names: Vec<String>) -> i64 {
    names
        .iter()
        .filter(|name| name.as_str() != INIT_COLLECTION && !name.starts_with("system."))
        .count() as i64
}

fn validate_source_options(source: &ImportSource) -> Result<(), DataImportError> {
    for (name, value) in source.options() {
        let valid = match name.as_str() {
            "authMechanism" => AUTH_MECHANISMS
                .iter()
                .any(|mechanism| mechanism.eq_ignore_ascii_case(value)),
            _ => {
                !value.is_empty()
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ','))
            }
        };
        if !valid {
            return Err(DataImportError::invalid_source(format!(
                "option '{name}' has an unsupported value '{value}'"
            )));
        }
    }
    if source.database().contains('.') {
        return Err(DataImportError::invalid_source(
            "MongoDB database names cannot contain '.'",
        ));
    }
    Ok(())
}

#[async_trait]
impl DataImportEngine for MongodbService {
    fn import_spec(&self) -> DataImportSpec {
        DataImportSpec {
            engine_label: "MongoDB".to_string(),
            source_schemes: SOURCE_RULES.schemes.iter().map(|s| s.to_string()).collect(),
            source_url_example:
                "mongodb://user:password@db.example.com:27017/app?authSource=admin&tls=true"
                    .to_string(),
            allowed_source_options: SOURCE_RULES
                .allowed_options
                .iter()
                .map(|s| s.to_string())
                .collect(),
            atomic: false,
            object_noun: "collection".to_string(),
        }
    }

    fn parse_source(&self, raw: &str) -> Result<ImportSource, DataImportError> {
        if raw
            .trim()
            .to_ascii_lowercase()
            .starts_with("mongodb+srv://")
        {
            return Err(DataImportError::invalid_source(
                "mongodb+srv:// connection strings are not supported: the hosts they resolve to \
                 cannot be checked before connecting. Use your provider's standard connection \
                 string, which lists the hosts (mongodb://host1:27017,host2:27017/app?\
                 replicaSet=…&tls=true&authSource=admin)",
            ));
        }
        let source = parse_source_url(raw, &SOURCE_RULES)?;
        validate_source_options(&source)?;
        Ok(source)
    }

    fn validate_target_database(&self, database: &str) -> Result<(), DataImportError> {
        if database.is_empty() || database.len() > MAX_DATABASE_LEN {
            return Err(DataImportError::invalid_target_database(
                database,
                format!("must be 1 to {MAX_DATABASE_LEN} characters long"),
            ));
        }
        if !database
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(DataImportError::invalid_target_database(
                database,
                "may only contain letters, digits, '_' and '-'",
            ));
        }
        if RESERVED_DATABASES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(database))
        {
            return Err(DataImportError::invalid_target_database(
                database,
                "it is a MongoDB system database",
            ));
        }
        Ok(())
    }

    async fn inspect_target(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError> {
        let service = config.name.as_str();
        let mongo = self.import_config(config)?;
        let client = self.import_client(service, &mongo).await?;
        let names = bounded(
            service,
            "list databases",
            &mongo.password,
            client.list_database_names(),
        )
        .await?;
        if !names.iter().any(|name| name == database) {
            return Ok(TargetInspection {
                exists: false,
                object_count: 0,
                size_bytes: None,
            });
        }
        let db = client.database(database);
        let collections = bounded(
            service,
            &format!("list collections of '{database}'"),
            &mongo.password,
            db.list_collection_names(),
        )
        .await?;
        let stats = bounded(
            service,
            &format!("measure '{database}'"),
            &mongo.password,
            db.run_command(doc! { "dbStats": 1 }),
        )
        .await?;
        let size_bytes = match (number(&stats, "dataSize"), number(&stats, "indexSize")) {
            (Some(data), Some(index)) => Some(data + index),
            (Some(data), None) => Some(data),
            _ => None,
        };
        Ok(TargetInspection {
            exists: true,
            object_count: data_collections(collections),
            size_bytes,
        })
    }

    async fn prepare_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
    ) -> Result<(), DataImportError> {
        self.validate_target_database(database)?;
        if preparation == TargetPreparation::UseExisting {
            return Ok(());
        }
        let service = config.name.as_str();
        let mongo = self.import_config(config)?;
        let client = self.import_client(service, &mongo).await?;
        let db = client.database(database);
        if preparation == TargetPreparation::Recreate {
            bounded(
                service,
                &format!("drop database '{database}'"),
                &mongo.password,
                db.drop(),
            )
            .await?;
        }
        // Same marker collection project provisioning creates.
        bounded(
            service,
            &format!("create database '{database}'"),
            &mongo.password,
            db.create_collection(INIT_COLLECTION),
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
        let mongo = self.import_config(config)?;
        let target_url = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin&directConnection=true",
            percent_encode_userinfo(&mongo.username),
            percent_encode_userinfo(&mongo.password),
            target.host,
            target.port
        );
        Ok(TransferPlan {
            image: MONGO_SIDECAR_IMAGE.to_string(),
            producer: PRODUCER.to_string(),
            consumer: CONSUMER.to_string(),
            env: vec![
                TransferEnv::secret("TEMPS_IMPORT_SOURCE", source.raw()),
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_DATABASE", source.database()),
                TransferEnv::secret("TEMPS_IMPORT_TARGET", target_url),
                TransferEnv::secret("TEMPS_IMPORT_TARGET_PASSWORD", mongo.password),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_DATABASE", target.database),
            ],
        })
    }

    fn failure_hint(&self, output: &str) -> Option<String> {
        mongodb_failure_hint(output)
    }
}

fn mongodb_failure_hint(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let hint = if lower.contains("authentication failed") {
        "the source rejected the user name or password; check them and authSource (usually \
         admin for users created there)"
    } else if lower.contains("not authorized on") || lower.contains("unauthorized") {
        "the source user cannot read every collection; give it the read role on the database"
    } else if lower.contains("x509") || lower.contains("certificate") {
        "the source's TLS certificate could not be verified; add tlsAllowInvalidCertificates=true \
         to encrypt without verifying it"
    } else if lower.contains("server selection")
        || lower.contains("no reachable servers")
        || lower.contains("connection refused")
        || lower.contains("i/o timeout")
    {
        "the source could not be reached from this server; check its addresses, port, firewall \
         and whether it requires tls=true"
    } else if lower.contains("e11000") || lower.contains("duplicate key") {
        "documents in the dump already exist in the target; enable replace to import into a \
         fresh database"
    } else {
        return None;
    };
    Some(hint.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_import::runner::{run_helper, HelperOutcome, HelperRequest};
    use crate::data_import::test_support::{leftover_helpers, TestDocker};
    use crate::data_import::{plan_target_preparation, TargetPreparation};

    fn service() -> MongodbService {
        let docker = std::sync::Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("docker client value"),
        );
        MongodbService::new("orders".to_string(), docker)
    }

    #[test]
    fn accepts_replica_set_urls_and_refuses_srv() {
        let engine = service();
        let source = engine
            .parse_source(
                "mongodb://u:p@a.example.com:27017,b.example.com:27017/shop?replicaSet=rs0&tls=true&authsource=admin",
            )
            .expect("valid");
        assert_eq!(source.endpoints().len(), 2);
        assert_eq!(source.option("authSource"), Some("admin"));

        let error = engine
            .parse_source("mongodb+srv://u:p@cluster0.example.com/shop")
            .expect_err("srv");
        assert!(error.to_string().contains("mongodb+srv"), "{error}");
    }

    #[test]
    fn refuses_file_reading_options_bad_mechanisms_and_dotted_databases() {
        let engine = service();
        for url in [
            "mongodb://u:p@h.example.com/shop?tlsCAFile=/etc/passwd",
            "mongodb://u:p@h.example.com/shop?authMechanism=MONGODB-X509",
            "mongodb://u:p@h.example.com/shop.v2",
            "mongodb://u:p@h.example.com/",
        ] {
            assert!(
                matches!(
                    engine.parse_source(url),
                    Err(DataImportError::InvalidSource { .. })
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn target_name_rules() {
        let engine = service();
        engine
            .validate_target_database("shop_production")
            .expect("valid");
        for bad in ["", "admin", "Local", "a.b", "a b", &"x".repeat(64)] {
            assert!(engine.validate_target_database(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn marker_and_system_collections_do_not_count_as_data() {
        let names = vec![
            "_temps_init".to_string(),
            "system.views".to_string(),
            "orders".to_string(),
        ];
        assert_eq!(data_collections(names), 1);
    }

    #[test]
    fn hints_cover_the_common_failures() {
        for (output, expected) in [
            ("Failed: error connecting to db server: connection() error occurred during connection handshake: auth error: sasl conversation error: unable to authenticate using mechanism \"SCRAM-SHA-256\": (AuthenticationFailed) Authentication failed.", "user name or password"),
            ("server selection error: server selection timeout", "could not be reached"),
            ("E11000 duplicate key error collection", "replace"),
        ] {
            let hint = mongodb_failure_hint(output).expect(output);
            assert!(hint.contains(expected), "{output} -> {hint}");
        }
    }

    const ROOT_PASSWORD: &str = "root-p@ss";

    /// Docker end-to-end: a MongoDB source database is imported under a
    /// different name, then re-imported with replace without duplicates.
    #[tokio::test]
    async fn imports_a_mongodb_database_under_a_new_name() {
        use futures::FutureExt;
        let Some(mut docker) = TestDocker::connect().await else {
            println!("Docker not available, skipping");
            return;
        };
        let result = std::panic::AssertUnwindSafe(mongodb_scenario(&mut docker))
            .catch_unwind()
            .await;
        docker.finish(result).await;
    }

    async fn mongodb_scenario(docker: &mut TestDocker) {
        let source = docker
            .run(
                "mongo-source",
                MONGO_SIDECAR_IMAGE,
                vec![
                    "MONGO_INITDB_ROOT_USERNAME=root".to_string(),
                    "MONGO_INITDB_ROOT_PASSWORD=source-pass".to_string(),
                ],
                None,
            )
            .await;
        let target = docker
            .run(
                "mongo-target",
                MONGO_SIDECAR_IMAGE,
                vec![
                    "MONGO_INITDB_ROOT_USERNAME=root".to_string(),
                    format!("MONGO_INITDB_ROOT_PASSWORD={ROOT_PASSWORD}"),
                ],
                Some("27017/tcp"),
            )
            .await;
        let ping = "mongosh --quiet -u root -p \"$PW\" --authenticationDatabase admin \
                    --eval 'db.runCommand({ping: 1}).ok' | grep -q 1";
        docker
            .wait_for(
                &source.name,
                ping,
                vec!["PW=source-pass".to_string()],
                Duration::from_secs(120),
            )
            .await;
        docker
            .wait_for(
                &target.name,
                ping,
                vec![format!("PW={ROOT_PASSWORD}")],
                Duration::from_secs(120),
            )
            .await;
        let (seeded, output) = docker
            .sh(
                &source.name,
                "mongosh --quiet -u root -p source-pass --authenticationDatabase admin --eval \
                 'db.getSiblingDB(\"shop\").items.insertMany(Array.from({length: 75}, (_, i) => ({n: i})))'",
                vec![],
            )
            .await;
        assert!(seeded, "seeding failed: {output}");

        let config = ServiceConfig {
            name: "e2e".to_string(),
            service_type: crate::externalsvc::ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({
                "host": "127.0.0.1",
                "port": target.host_port.expect("published port").to_string(),
                "database": "admin",
                "username": "root",
                "password": ROOT_PASSWORD,
                "docker_image": MONGO_SIDECAR_IMAGE,
                "container_name": target.name,
            }),
        };
        let engine = MongodbService::new(
            "e2e".to_string(),
            std::sync::Arc::new(docker.docker.clone()),
        );
        let source_url = format!(
            "mongodb://root:source-pass@{}:27017/shop?authSource=admin",
            source.name
        );
        let run_base = (std::process::id() as i32 % 10_000) * 10 + 8;

        for (attempt, replace) in [(0, false), (1, true)] {
            let inspection = engine
                .inspect_target(&config, "shop_production")
                .await
                .expect("inspect");
            let preparation =
                plan_target_preparation(1, "shop_production", inspection, replace, "collection")
                    .expect("plan");
            if attempt == 1 {
                assert_eq!(preparation, TargetPreparation::Recreate);
            }
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
                        port: "27017",
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

            let (ok, count) = docker
                .sh(
                    &target.name,
                    "mongosh --quiet -u root -p \"$PW\" --authenticationDatabase admin --eval \
                     'db.getSiblingDB(\"shop_production\").items.countDocuments()'",
                    vec![format!("PW={ROOT_PASSWORD}")],
                )
                .await;
            assert!(ok, "{count}");
            assert_eq!(count.trim(), "75", "attempt {attempt}");
        }
        let inspection = engine
            .inspect_target(&config, "shop_production")
            .await
            .expect("inspect");
        assert_eq!(inspection.object_count, 1);
    }
}
