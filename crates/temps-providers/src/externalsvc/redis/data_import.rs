// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Redis support for importing data from an external server.
//!
//! A Redis service hands each project environment one logical database
//! (DB 1–15), allocated under a resource name and recorded in DB 0 (see
//! `RedisService::allocate_database`). An import targets such a resource
//! name — the same `<project>_<environment>` name provisioning uses — so the
//! imported keys land exactly where a deployment linked to that environment
//! reads them. "Replace" flushes that one logical database, never the others.
//!
//! The copy is `SCAN` + `DUMP`/`PTTL` on the source and `RESTORE` on the
//! target, run by `data_import_resp.py` in a stock Python image. Replication
//! (`SYNC`) and RDB files are avoided on purpose: hosted Redis offerings
//! commonly disable replication commands but allow `DUMP`. Values travel as
//! opaque serialized payloads, so every data type and its TTL is preserved.
//!
//! Not atomic: a failed import can leave part of the keys behind.

use std::time::Duration;

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

use super::{RedisConfig, RedisService};
use crate::data_import::source::{parse_source_url, scrub_secrets, SourceUrlRules};
use crate::data_import::{
    bounded_step, DataImportEngine, DataImportError, DataImportSpec, ImportSource,
    TargetInspection, TargetPreparation, TransferEnv, TransferPlan, TransferTarget,
};
use crate::externalsvc::ServiceConfig;

/// The copy program, run in [`HELPER_IMAGE`].
const RESP_SCRIPT: &str = include_str!("data_import_resp.py");

/// Official Python image; the script uses the standard library only.
const HELPER_IMAGE: &str = "python:3.13-slim";

/// Bound on each control-plane step.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest target resource name accepted.
const MAX_TARGET_LEN: usize = 128;

const SOURCE_RULES: SourceUrlRules<'static> = SourceUrlRules {
    schemes: &["redis", "rediss"],
    default_port: 6379,
    allowed_options: &["ssl_cert_reqs"],
    options_case_insensitive: false,
    // Cluster and Sentinel deployments are out of scope: one endpoint.
    max_hosts: 1,
    default_database: Some("0"),
};

const PRODUCER: &str = "python3 -I -c \"$TEMPS_IMPORT_REDIS_SCRIPT\" produce";
const CONSUMER: &str = "python3 -I -c \"$TEMPS_IMPORT_REDIS_SCRIPT\" consume";

impl RedisService {
    /// Load `config` into this instance. A freshly built engine holds no
    /// config, and the allocation helpers read it from the instance.
    async fn import_hydrate(&self, config: &ServiceConfig) -> Result<RedisConfig, DataImportError> {
        let redis = self.get_redis_config(config.clone()).map_err(|e| {
            DataImportError::target(&config.name, "read the Redis configuration", e.to_string())
        })?;
        *self.config.write().await = Some(redis.clone());
        Ok(redis)
    }

    async fn import_connection(
        &self,
        service: &str,
        redis: &RedisConfig,
    ) -> Result<ConnectionManager, DataImportError> {
        self.get_connection().await.map_err(|e| {
            DataImportError::target(
                service,
                "connect to Redis",
                scrub_secrets(&e.to_string(), std::slice::from_ref(&redis.password)),
            )
        })
    }

    /// Logical database allocated to `resource`, if any.
    async fn import_lookup(
        &self,
        service: &str,
        redis: &RedisConfig,
        conn: &mut ConnectionManager,
        resource: &str,
    ) -> Result<Option<u8>, DataImportError> {
        let operation = format!("look up the logical database of '{resource}'");
        bounded(service, &operation, redis, async {
            redis::cmd("SELECT").arg(0).query_async::<()>(conn).await?;
            let db_number: Option<u8> = conn.get(Self::resource_mapping_key(resource)).await?;
            Ok(db_number)
        })
        .await
    }
}

/// Run a Redis call bounded by [`ADMIN_TIMEOUT`], mapping both failure modes.
async fn bounded<T, F>(
    service: &str,
    operation: &str,
    redis: &RedisConfig,
    call: F,
) -> Result<T, DataImportError>
where
    F: std::future::Future<Output = redis::RedisResult<T>>,
{
    tokio::time::timeout(ADMIN_TIMEOUT, call)
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
                scrub_secrets(&e.to_string(), std::slice::from_ref(&redis.password)),
            )
        })
}

fn validate_source(source: &ImportSource) -> Result<(), DataImportError> {
    if !source.database().chars().all(|c| c.is_ascii_digit()) || source.database().len() > 4 {
        return Err(DataImportError::invalid_source(format!(
            "the path names the logical database to copy and must be a number (e.g. /0), got \
             '{}'",
            source.database()
        )));
    }
    if let Some(value) = source.option("ssl_cert_reqs") {
        if source.scheme() != "rediss" {
            return Err(DataImportError::invalid_source(
                "ssl_cert_reqs only applies to rediss:// connection strings",
            ));
        }
        if !matches!(value, "required" | "none") {
            return Err(DataImportError::invalid_source(format!(
                "ssl_cert_reqs '{value}' is not supported; use required or none"
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl DataImportEngine for RedisService {
    fn import_spec(&self) -> DataImportSpec {
        DataImportSpec {
            engine_label: "Redis".to_string(),
            source_schemes: SOURCE_RULES.schemes.iter().map(|s| s.to_string()).collect(),
            source_url_example: "rediss://default:password@cache.example.com:6379/0".to_string(),
            allowed_source_options: SOURCE_RULES
                .allowed_options
                .iter()
                .map(|s| s.to_string())
                .collect(),
            atomic: false,
            object_noun: "key".to_string(),
            max_target_length: MAX_TARGET_LEN as u32,
        }
    }

    fn parse_source(&self, raw: &str) -> Result<ImportSource, DataImportError> {
        let source = parse_source_url(raw, &SOURCE_RULES)?;
        validate_source(&source)?;
        Ok(source)
    }

    fn validate_target_database(&self, database: &str) -> Result<(), DataImportError> {
        if database.is_empty() || database.len() > MAX_TARGET_LEN {
            return Err(DataImportError::invalid_target_database(
                database,
                format!("must be 1 to {MAX_TARGET_LEN} characters long"),
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
        if database.starts_with("_temps") {
            return Err(DataImportError::invalid_target_database(
                database,
                "names starting with _temps are reserved for Temps' own metadata",
            ));
        }
        Ok(())
    }

    async fn inspect_target(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError> {
        self.validate_target_database(database)?;
        let service = config.name.as_str();
        let redis = self.import_hydrate(config).await?;
        let mut conn = self.import_connection(service, &redis).await?;
        let Some(db_number) = self
            .import_lookup(service, &redis, &mut conn, database)
            .await?
        else {
            return Ok(TargetInspection {
                exists: false,
                object_count: 0,
                size_bytes: None,
            });
        };
        let operation = format!("count the keys of '{database}' (DB {db_number})");
        let keys = bounded(service, &operation, &redis, async {
            redis::cmd("SELECT")
                .arg(db_number)
                .query_async::<()>(&mut conn)
                .await?;
            redis::cmd("DBSIZE").query_async::<i64>(&mut conn).await
        })
        .await?;
        Ok(TargetInspection {
            exists: true,
            object_count: keys,
            size_bytes: None,
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
        let redis = self.import_hydrate(config).await?;
        // Same allocation as provisioning: reuses the resource's DB when it
        // has one, otherwise claims a free one.
        let operation = format!("allocate a logical database for '{database}'");
        let db_number = bounded_step(service, &operation, async {
            self.allocate_database(database).await.map_err(|e| {
                DataImportError::target(
                    service,
                    &operation,
                    scrub_secrets(&e.to_string(), std::slice::from_ref(&redis.password)),
                )
            })
        })
        .await?;
        if preparation == TargetPreparation::Recreate {
            let mut conn = self.import_connection(service, &redis).await?;
            let operation = format!("flush '{database}' (DB {db_number})");
            bounded(service, &operation, &redis, async {
                redis::cmd("SELECT")
                    .arg(db_number)
                    .query_async::<()>(&mut conn)
                    .await?;
                redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await
            })
            .await?;
        }
        Ok(())
    }

    fn target_container(&self, config: &ServiceConfig) -> Result<String, DataImportError> {
        let redis = self.get_redis_config(config.clone()).map_err(|e| {
            DataImportError::target(&config.name, "read the Redis configuration", e.to_string())
        })?;
        Ok(self.get_live_container_name(&redis))
    }

    async fn transfer_plan(
        &self,
        config: &ServiceConfig,
        source: &ImportSource,
        target: &TransferTarget<'_>,
    ) -> Result<TransferPlan, DataImportError> {
        let service = config.name.as_str();
        let redis = self.import_hydrate(config).await?;
        let mut conn = self.import_connection(service, &redis).await?;
        let db_number = self
            .import_lookup(service, &redis, &mut conn, target.database)
            .await?
            .ok_or_else(|| {
                DataImportError::target(
                    service,
                    format!("find the logical database of '{}'", target.database),
                    "it was not allocated; the target was not prepared".to_string(),
                )
            })?;
        let endpoint = source.endpoints().first().ok_or_else(|| {
            DataImportError::invalid_source("the source connection string names no host")
        })?;
        let tls = source.scheme() == "rediss";
        let verify = source.option("ssl_cert_reqs") != Some("none");
        Ok(TransferPlan {
            image: HELPER_IMAGE.to_string(),
            producer: PRODUCER.to_string(),
            consumer: CONSUMER.to_string(),
            env: vec![
                TransferEnv::plain("TEMPS_IMPORT_REDIS_SCRIPT", RESP_SCRIPT),
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
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_TLS", if tls { "1" } else { "0" }),
                TransferEnv::plain(
                    "TEMPS_IMPORT_SOURCE_TLS_VERIFY",
                    if verify { "1" } else { "0" },
                ),
                TransferEnv::plain("TEMPS_IMPORT_SOURCE_DATABASE", source.database()),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_HOST", target.host),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_PORT", target.port),
                TransferEnv::secret("TEMPS_IMPORT_TARGET_PASSWORD", redis.password),
                TransferEnv::plain("TEMPS_IMPORT_TARGET_DATABASE", db_number.to_string()),
            ],
        })
    }

    fn failure_hint(&self, output: &str) -> Option<String> {
        redis_failure_hint(output)
    }
}

fn redis_failure_hint(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let hint = if lower.contains("wrongpass")
        || lower.contains("invalid password")
        || lower.contains("noauth")
        || lower.contains("invalid username-password")
    {
        "the source rejected the user name or password"
    } else if lower.contains("payload version or checksum are wrong")
        || lower.contains("bad data format")
    {
        "the source runs a newer Redis than this service and its serialized values cannot be \
         loaded here; upgrade this service to the source's Redis version or newer"
    } else if lower.contains("moved ")
        || lower.contains("clusterdown")
        || lower.contains("crossslot")
    {
        "the source is a Redis Cluster, which cannot be imported; import from a standalone \
         endpoint"
    } else if lower.contains("unknown command") && lower.contains("dump") {
        "the source does not allow the DUMP command, which the import needs to read values"
    } else if lower.contains("certificate verify failed") {
        "the source's TLS certificate could not be verified; add ?ssl_cert_reqs=none to encrypt \
         without verifying it"
    } else if lower.contains("tls handshake") || lower.contains("wrong version number") {
        "TLS could not be negotiated with the source; use rediss:// only if the source expects \
         TLS, redis:// otherwise"
    } else if lower.contains("could not connect") || lower.contains("timed out") {
        "the source could not be reached from this server; check its address, port and firewall"
    } else if lower.contains("db index is out of range") {
        "the source has no logical database with that number"
    } else {
        return None;
    };
    Some(hint.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_import::runner::{compose_script, run_helper, HelperOutcome, HelperRequest};
    use crate::data_import::test_support::{leftover_helpers, TestDocker};
    use crate::data_import::{plan_target_preparation, TargetPreparation};

    fn service() -> RedisService {
        let docker = std::sync::Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("docker client value"),
        );
        RedisService::new("cache".to_string(), docker)
    }

    #[test]
    fn source_database_defaults_to_zero_and_must_be_numeric() {
        let engine = service();
        assert_eq!(
            engine
                .parse_source("redis://:pw@cache.example.com:6380")
                .expect("no path")
                .database(),
            "0"
        );
        engine
            .parse_source("rediss://default:pw@cache.example.com/4?ssl_cert_reqs=none")
            .expect("tls with db");
        for url in [
            "redis://:pw@cache.example.com/sessions",
            "redis://:pw@cache.example.com/0?ssl_cert_reqs=none",
            "rediss://:pw@cache.example.com/0?ssl_cert_reqs=maybe",
            "redis://:pw@a.example.com,b.example.com/0",
            "redis+sentinel://cache.example.com/0",
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
    fn target_is_a_resource_name_and_metadata_names_are_reserved() {
        let engine = service();
        engine
            .validate_target_database("storefront_production")
            .expect("valid");
        for bad in ["", "a b", "_temps:redis_db_owner:1", "_temps_meta", "x/y"] {
            assert!(engine.validate_target_database(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn script_is_embedded_and_secrets_stay_out_of_commands() {
        assert!(RESP_SCRIPT.contains("def produce()"));
        assert!(RESP_SCRIPT.contains("def consume()"));
        let script = compose_script(PRODUCER, CONSUMER);
        assert!(script.contains("$TEMPS_IMPORT_REDIS_SCRIPT"));
        assert!(!script.contains("PASSWORD="));
    }

    #[test]
    fn hints_cover_the_common_failures() {
        for (output, expected) in [
            ("temps-import: source: AUTH failed: WRONGPASS invalid username-password pair", "user name or password"),
            ("temps-import: target: restoring key b'x' failed: ERR DUMP payload version or checksum are wrong", "newer Redis"),
            ("temps-import: source: reading key b'k' failed: MOVED 3999 10.0.0.2:6379", "Redis Cluster"),
            ("temps-import: source: could not connect to cache.example.com:6379: timed out", "could not be reached"),
        ] {
            let hint = redis_failure_hint(output).expect(output);
            assert!(hint.contains(expected), "{output} -> {hint}");
        }
        assert_eq!(redis_failure_hint("fine"), None);
    }

    const TARGET_PASSWORD: &str = "t@rget-pass";

    /// Docker end-to-end: keys of every type, with TTLs and binary values, in
    /// source DB 3 are imported under a resource name; the target allocates a
    /// logical DB for it the way provisioning does, other DBs are untouched,
    /// and a re-import with replace does not duplicate or keep stale keys.
    #[tokio::test]
    async fn imports_a_redis_logical_database_under_a_resource_name() {
        use futures::FutureExt;
        let Some(mut docker) = TestDocker::connect().await else {
            println!("Docker not available, skipping");
            return;
        };
        let result = std::panic::AssertUnwindSafe(redis_scenario(&mut docker))
            .catch_unwind()
            .await;
        docker.finish(result).await;
    }

    async fn target_cli(docker: &TestDocker, target: &str, args: &str) -> String {
        let (ok, output) = docker
            .sh(
                target,
                &format!("redis-cli --no-auth-warning -a \"$PW\" {args}"),
                vec![format!("PW={TARGET_PASSWORD}")],
            )
            .await;
        assert!(ok, "{args}: {output}");
        output.trim().to_string()
    }

    async fn redis_scenario(docker: &mut TestDocker) {
        let source = docker.run("redis-source", "redis:7.4", vec![], None).await;
        let target = docker
            .run("redis-target", "redis:7.4", vec![], Some("6379/tcp"))
            .await;
        for (container, password) in [
            (&source.name, "source-pass"),
            (&target.name, TARGET_PASSWORD),
        ] {
            docker
                .wait_for(
                    container,
                    "redis-cli PING | grep -q PONG",
                    vec![],
                    Duration::from_secs(60),
                )
                .await;
            let (ok, output) = docker
                .sh(
                    container,
                    &format!("redis-cli CONFIG SET requirepass '{password}'"),
                    vec![],
                )
                .await;
            assert!(ok, "{output}");
        }
        let (seeded, output) = docker
            .sh(
                &source.name,
                "C='redis-cli --no-auth-warning -a source-pass -n 3'; \
                 $C SET plain hello >/dev/null && $C SET expiring v EX 3600 >/dev/null && \
                 $C HSET h a 1 b 2 >/dev/null && $C RPUSH l x y z >/dev/null && \
                 $C ZADD z 1 a 2 b >/dev/null && $C XADD s '*' f v >/dev/null && \
                 $C EVAL \"for i=1,5000 do redis.call('SET','k:'..i,i) end return 1\" 0 >/dev/null && \
                 redis-cli --no-auth-warning -a source-pass -n 0 SET elsewhere x >/dev/null",
                vec![],
            )
            .await;
        assert!(seeded, "seeding failed: {output}");

        let config = ServiceConfig {
            name: "e2e".to_string(),
            service_type: crate::externalsvc::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": target.host_port.expect("published port").to_string(),
                "password": TARGET_PASSWORD,
                "docker_image": "redis:7.4",
                "container_name": target.name,
            }),
        };
        let engine = RedisService::new(
            "e2e".to_string(),
            std::sync::Arc::new(docker.docker.clone()),
        );
        let source_url = format!("redis://:source-pass@{}:6379/3", source.name);
        let run_base = (std::process::id() as i32 % 10_000) * 10 + 9_000_000;

        for (attempt, replace) in [(0, false), (1, true)] {
            let inspection = engine
                .inspect_target(&config, "storefront_production")
                .await
                .expect("inspect");
            let preparation =
                plan_target_preparation(1, "storefront_production", inspection, replace, "key")
                    .expect("plan");
            if attempt == 1 {
                assert_eq!(preparation, TargetPreparation::Recreate);
                // A stale key must not survive the replace.
                target_cli(docker, &target.name, "-n 1 SET stale 1").await;
            }
            engine
                .prepare_target(&config, "storefront_production", preparation)
                .await
                .expect("prepare");
            let parsed = engine.parse_source(&source_url).expect("source");
            let plan = engine
                .transfer_plan(
                    &config,
                    &parsed,
                    &TransferTarget {
                        host: &target.name,
                        port: "6379",
                        database: "storefront_production",
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

            // The first allocation on an empty server is DB 1.
            assert_eq!(
                target_cli(docker, &target.name, "-n 1 DBSIZE").await,
                "5006"
            );
            assert_eq!(
                target_cli(docker, &target.name, "-n 1 EXISTS stale").await,
                "0"
            );
        }

        let ttl: i64 = target_cli(docker, &target.name, "-n 1 TTL expiring")
            .await
            .parse()
            .expect("ttl");
        assert!(ttl > 3500 && ttl <= 3600, "TTL kept: {ttl}");
        assert_eq!(
            target_cli(docker, &target.name, "-n 1 TYPE s").await,
            "stream"
        );
        assert_eq!(target_cli(docker, &target.name, "-n 1 HGET h b").await, "2");
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 GET _temps:redis_db_mapping:storefront_production"
            )
            .await,
            "1",
            "allocation recorded the way provisioning records it"
        );
        assert_eq!(
            target_cli(docker, &target.name, "-n 0 EXISTS elsewhere").await,
            "0"
        );
        let inspection = engine
            .inspect_target(&config, "storefront_production")
            .await
            .expect("inspect");
        assert_eq!(inspection.object_count, 5006);

        // A source that refuses the password fails on the source side.
        let bad = engine
            .parse_source(&format!("redis://:wrong-pass@{}:6379/3", source.name))
            .expect("source");
        let plan = engine
            .transfer_plan(
                &config,
                &bad,
                &TransferTarget {
                    host: &target.name,
                    port: "6379",
                    database: "storefront_production",
                },
            )
            .await
            .expect("plan");
        let mut secrets = bad.secrets();
        secrets.extend(
            plan.env
                .iter()
                .filter(|e| e.secret)
                .map(|e| e.value.clone()),
        );
        let name = format!("temps-data-import-test-{}", run_base + 2);
        let outcome = run_helper(
            &docker.docker,
            &HelperRequest {
                run_id: run_base + 2,
                service_id: 1,
                container_name: &name,
                plan: &plan,
                network: &docker.network,
                extra_hosts: vec![],
                timeout: Duration::from_secs(120),
                secrets: &secrets,
            },
        )
        .await
        .expect("helper ran");
        let HelperOutcome::SourceFailed { output } = outcome else {
            panic!("expected a source failure, got {outcome:?}");
        };
        assert!(!output.contains("wrong-pass"), "{output}");
        assert!(
            engine
                .failure_hint(&output)
                .is_some_and(|hint| hint.contains("user name or password")),
            "{output}"
        );
    }
}
