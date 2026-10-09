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

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::{info, warn};

use super::{RedisConfig, RedisService};
use crate::data_import::source::{parse_source_url, scrub_secrets, SourceUrlRules};
use crate::data_import::{
    DataImportEngine, DataImportError, DataImportSpec, ImportSource, TargetInspection,
    TargetPreparation, TransferEnv, TransferPlan, TransferTarget, TARGET_STEP_TIMEOUT,
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

/// Release the logical database of resource `ARGV[2]` for import run
/// `ARGV[1]`: only while the claim (`KEYS[1]`) is `"{run}:{db}"` for the db
/// the mapping (`KEYS[2]`) points to and the db's owner key (`ARGV[3]` +
/// db) is still the resource. Flushes the db and removes mapping, owner and
/// claim, all in one atomic step. Returns 1 when it released.
const RELEASE_SCRIPT: &str = "\
    local db = redis.call('GET', KEYS[2]) \
    if not db then return 0 end \
    if redis.call('GET', KEYS[1]) ~= ARGV[1] .. ':' .. db then return 0 end \
    local owner = ARGV[3] .. db \
    if redis.call('GET', owner) ~= ARGV[2] then return 0 end \
    redis.call('DEL', KEYS[1], KEYS[2], owner) \
    redis.call('SELECT', tonumber(db)) \
    redis.call('FLUSHDB') \
    redis.call('SELECT', 0) \
    return 1";

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

    /// Allocate (or find) the logical database of `resource`, exactly as
    /// provisioning does; `import_run` is set when the run holds a pending
    /// claim on it (see [`RedisService::allocate_database_for`]).
    ///
    /// The allocation claims a DB (`SETNX` on its owner key) and then records
    /// the resource's mapping in separate commands; dropping it between the
    /// two would leave a claimed DB that no resource maps to, which neither
    /// another allocation nor `drop_database` would ever release. So it runs
    /// to completion in its own task — on a dedicated engine instance holding
    /// the same configuration — and only the import's wait for it is
    /// bounded, by `wait`.
    ///
    /// If the import stops waiting and the allocation later completes, the
    /// task gives the database back itself while the run's claim is still
    /// there: the import has already failed and nobody else uses it. A
    /// handshake on `state` makes sure exactly one side — the waiter or the
    /// task — handles a completion that races the timeout.
    async fn import_allocate(
        &self,
        service: &str,
        redis: &RedisConfig,
        resource: &str,
        import_run: Option<i32>,
        wait: Duration,
    ) -> Result<u8, DataImportError> {
        const RUNNING: u8 = 0;
        const DONE: u8 = 1;
        const ABANDONED: u8 = 2;

        let operation = format!("allocate a logical database for '{resource}'");
        let worker = RedisService::new(self.name.clone(), self.docker.clone());
        *worker.config.write().await = Some(redis.clone());
        let state = Arc::new(AtomicU8::new(RUNNING));
        let task_state = state.clone();
        let owned_resource = resource.to_string();
        let mut task = tokio::spawn(async move {
            let allocated = worker
                .allocate_database_for(&owned_resource, import_run)
                .await;
            let abandoned = task_state
                .compare_exchange(RUNNING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_err();
            if let (true, Ok(db_number), Some(run_id)) = (abandoned, &allocated, import_run) {
                match tokio::time::timeout(
                    ADMIN_TIMEOUT,
                    worker.release_claimed(&owned_resource, run_id),
                )
                .await
                {
                    Ok(Ok(true)) => info!(
                        run_id,
                        resource = %owned_resource,
                        db_number,
                        "Released the Redis DB an abandoned import allocation created"
                    ),
                    Ok(Ok(false)) => {}
                    Ok(Err(e)) => warn!(
                        run_id,
                        resource = %owned_resource,
                        error = %e,
                        "Could not release the Redis DB an abandoned import allocation created"
                    ),
                    Err(_) => warn!(
                        run_id,
                        resource = %owned_resource,
                        "Releasing the Redis DB an abandoned import allocation created timed out"
                    ),
                }
            }
            allocated
        });
        let joined = match tokio::time::timeout(wait, &mut task).await {
            Ok(joined) => joined,
            Err(_) => {
                if state
                    .compare_exchange(RUNNING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let after = if import_run.is_some() {
                        "the allocation keeps running and gives the database back if it \
                         completes, so run the import again once Redis responds"
                    } else {
                        "the allocation keeps running and completes on its own, so run the \
                         import again once Redis responds"
                    };
                    return Err(DataImportError::target(
                        service,
                        &operation,
                        format!("Redis did not answer within {}s; {after}", wait.as_secs()),
                    ));
                }
                // The allocation finished just as the wait ran out: its
                // result is ready, and it is the import's to use.
                task.await
            }
        };
        match joined {
            Ok(Ok(db_number)) => Ok(db_number),
            Ok(Err(e)) => Err(DataImportError::target(
                service,
                &operation,
                scrub_secrets(&e.to_string(), std::slice::from_ref(&redis.password)),
            )),
            Err(join_error) => Err(DataImportError::target(
                service,
                &operation,
                format!("the allocation task stopped unexpectedly: {join_error}"),
            )),
        }
    }

    /// Allocate a new logical database for `resource` on behalf of import
    /// run `run_id`. The claim is set to `"{run_id}:pending"` *before*
    /// allocating; the allocation confirms it atomically with the mapping it
    /// creates. Provisioning (or another import) adopting the name at any
    /// point removes it, so the run never holds a confirmed claim on a
    /// database something else uses.
    async fn create_claimed(
        &self,
        service: &str,
        redis: &RedisConfig,
        resource: &str,
        run_id: i32,
    ) -> Result<(), DataImportError> {
        let claim_key = RedisService::import_claim_key(resource);
        let pending = format!("{run_id}:pending");
        let mut conn = self.import_connection(service, redis).await?;
        bounded(service, &format!("claim '{resource}'"), redis, async {
            redis::cmd("SELECT")
                .arg(0)
                .query_async::<()>(&mut conn)
                .await?;
            conn.set::<_, _, ()>(&claim_key, &pending).await
        })
        .await?;
        self.import_allocate(service, redis, resource, Some(run_id), TARGET_STEP_TIMEOUT)
            .await?;
        Ok(())
    }

    /// Run [`RELEASE_SCRIPT`] for `resource` and import run `run_id`.
    /// Returns whether it released anything.
    async fn release_claimed(&self, resource: &str, run_id: i32) -> anyhow::Result<bool> {
        let mut conn = self.get_connection().await?;
        redis::cmd("SELECT")
            .arg(0)
            .query_async::<()>(&mut conn)
            .await?;
        let released: i64 = redis::Script::new(RELEASE_SCRIPT)
            .key(RedisService::import_claim_key(resource))
            .key(RedisService::resource_mapping_key(resource))
            .arg(run_id)
            .arg(resource)
            .arg(RedisService::DATABASE_OWNER_KEY_PREFIX)
            .invoke_async(&mut conn)
            .await?;
        Ok(released == 1)
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

    /// Besides allocating the logical database, this keeps the claim that
    /// lets [`Self::release_created_target`] give it back after a failure
    /// consistent: only a run that created the mapping holds a claim, and
    /// every other use of the name removes it.
    async fn prepare_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
        run_id: i32,
    ) -> Result<(), DataImportError> {
        self.validate_target_database(database)?;
        let service = config.name.as_str();
        let redis = self.import_hydrate(config).await?;
        if preparation == TargetPreparation::UseExisting {
            // Adopt it like provisioning does: an earlier failed run whose
            // release is still pending can no longer release it.
            let mut conn = self.import_connection(service, &redis).await?;
            let adopted = bounded(service, &format!("adopt '{database}'"), &redis, async {
                redis::cmd("SELECT")
                    .arg(0)
                    .query_async::<()>(&mut conn)
                    .await?;
                RedisService::adopt_resource(&mut conn, database).await
            })
            .await?;
            if adopted.is_none() {
                return Err(DataImportError::target(
                    service,
                    format!("use the logical database of '{database}'"),
                    "it was released after this import was planned; run the import again"
                        .to_string(),
                ));
            }
            return Ok(());
        }
        if preparation == TargetPreparation::Create {
            return self.create_claimed(service, &redis, database, run_id).await;
        }
        let db_number = self
            .import_allocate(service, &redis, database, None, TARGET_STEP_TIMEOUT)
            .await?;
        {
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

    /// One atomic script: release only while this run's claim still names
    /// the database the resource maps to and the database is still owned by
    /// the resource. Atomicity is what makes a late release harmless — one
    /// that runs after the import stopped waiting for it (and a retry or a
    /// deployment adopted the name) finds the claim gone and does nothing.
    async fn release_created_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        run_id: i32,
    ) -> Result<bool, DataImportError> {
        self.validate_target_database(database)?;
        let service = config.name.as_str();
        let redis = self.import_hydrate(config).await?;
        let operation = format!("release the logical database of '{database}'");
        match tokio::time::timeout(ADMIN_TIMEOUT, self.release_claimed(database, run_id)).await {
            Ok(Ok(released)) => Ok(released),
            Ok(Err(e)) => Err(DataImportError::target(
                service,
                &operation,
                scrub_secrets(&e.to_string(), std::slice::from_ref(&redis.password)),
            )),
            Err(_) => Err(DataImportError::target(
                service,
                &operation,
                format!("timed out after {}s", ADMIN_TIMEOUT.as_secs()),
            )),
        }
    }

    fn releases_created_target(&self) -> bool {
        true
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
        if !docker.ensure_images(&["redis:7.4"]).await {
            return;
        }
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
                .prepare_target(&config, "storefront_production", preparation, 1)
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

        // A failed import that created its target gives the logical DB back:
        // flushed, unmapped and unclaimed, so the pool shared with
        // provisioning does not shrink with every failed attempt.
        assert!(engine.releases_created_target());
        let inspection = engine
            .inspect_target(&config, "abandoned_import")
            .await
            .expect("inspect");
        assert_eq!(
            plan_target_preparation(1, "abandoned_import", inspection, false, "key").expect("plan"),
            TargetPreparation::Create
        );
        engine
            .prepare_target(&config, "abandoned_import", TargetPreparation::Create, 1)
            .await
            .expect("prepare");
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 GET _temps:redis_db_mapping:abandoned_import"
            )
            .await,
            "2"
        );
        target_cli(docker, &target.name, "-n 2 SET partial 1").await;
        assert!(engine
            .release_created_target(&config, "abandoned_import", 1)
            .await
            .expect("release"));
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 EXISTS _temps:redis_db_mapping:abandoned_import"
            )
            .await,
            "0"
        );
        assert_eq!(
            target_cli(docker, &target.name, "-n 0 EXISTS _temps:redis_db_owner:2").await,
            "0"
        );
        assert_eq!(target_cli(docker, &target.name, "-n 2 DBSIZE").await, "0");
        assert!(
            !engine
                .inspect_target(&config, "abandoned_import")
                .await
                .expect("inspect")
                .exists
        );
        // The imported database next to it is untouched.
        assert_eq!(
            target_cli(docker, &target.name, "-n 1 DBSIZE").await,
            "5006"
        );
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 GET _temps:redis_db_mapping:storefront_production"
            )
            .await,
            "1"
        );
        // The freed number is handed out again.
        engine
            .prepare_target(&config, "next_import", TargetPreparation::Create, 1)
            .await
            .expect("prepare");
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 GET _temps:redis_db_mapping:next_import"
            )
            .await,
            "2"
        );

        // A release that arrives late (after the import stopped waiting for
        // it) must not touch a database something else adopted meanwhile.
        // `next_import` was created by run 1 and holds its claim.
        let mapping = |name: &str| format!("-n 0 GET _temps:redis_db_mapping:{name}");

        // 1. A retry of the import adopts the name (UseExisting): the old
        //    run's release finds its claim gone.
        engine
            .prepare_target(&config, "next_import", TargetPreparation::UseExisting, 2)
            .await
            .expect("retry adopts");
        target_cli(docker, &target.name, "-n 2 SET retry-data 1").await;
        assert!(!engine
            .release_created_target(&config, "next_import", 1)
            .await
            .expect("late release"));
        assert_eq!(
            target_cli(docker, &target.name, &mapping("next_import")).await,
            "2"
        );
        assert_eq!(
            target_cli(docker, &target.name, "-n 2 GET retry-data").await,
            "1"
        );

        // 2. Provisioning adopts a name a run created.
        engine
            .prepare_target(&config, "deployed_meanwhile", TargetPreparation::Create, 3)
            .await
            .expect("prepare");
        let adopted = engine
            .allocate_database("deployed_meanwhile")
            .await
            .expect("provisioning allocation");
        target_cli(
            docker,
            &target.name,
            &format!("-n {adopted} SET app-data 1"),
        )
        .await;
        assert!(!engine
            .release_created_target(&config, "deployed_meanwhile", 3)
            .await
            .expect("release"));
        assert_eq!(
            target_cli(docker, &target.name, &format!("-n {adopted} GET app-data")).await,
            "1"
        );

        // 3. A name provisioning mapped before the run allocated it: the run
        //    reused the mapping, so it never holds a claim.
        let provisioned = engine
            .allocate_database("provisioned_first")
            .await
            .expect("provisioning allocation");
        engine
            .prepare_target(&config, "provisioned_first", TargetPreparation::Create, 4)
            .await
            .expect("prepare");
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 GET _temps:redis_import_claim:provisioned_first"
            )
            .await,
            "4:pending",
            "a reused mapping never confirms the claim"
        );
        assert!(!engine
            .release_created_target(&config, "provisioned_first", 4)
            .await
            .expect("release"));
        assert_eq!(
            target_cli(docker, &target.name, &mapping("provisioned_first")).await,
            provisioned.to_string()
        );

        // 4. Another run's id never releases; the run's own does.
        engine
            .prepare_target(&config, "someone_elses", TargetPreparation::Create, 5)
            .await
            .expect("prepare");
        assert!(!engine
            .release_created_target(&config, "someone_elses", 6)
            .await
            .expect("release"));
        assert!(engine
            .release_created_target(&config, "someone_elses", 5)
            .await
            .expect("release"));

        // 5. A retry planned (UseExisting) before the old run's release went
        //    through fails cleanly instead of writing into an unmapped
        //    database.
        engine
            .prepare_target(
                &config,
                "released_under_retry",
                TargetPreparation::Create,
                7,
            )
            .await
            .expect("prepare");
        assert!(engine
            .release_created_target(&config, "released_under_retry", 7)
            .await
            .expect("release"));
        let refused = engine
            .prepare_target(
                &config,
                "released_under_retry",
                TargetPreparation::UseExisting,
                8,
            )
            .await
            .expect_err("nothing to adopt");
        assert!(
            refused.to_string().contains("run the import again"),
            "{refused}"
        );

        // 6. An allocation the import stopped waiting for (Redis stalled)
        //    gives its database back once it completes. FLUSHDB only runs
        //    inside the release script, so its call count proves the release
        //    ran rather than the allocation never happening.
        let flush_calls = |stats: String| -> u64 {
            stats
                .lines()
                .find_map(|line| line.strip_prefix("cmdstat_flushdb:calls="))
                .and_then(|rest| rest.split(',').next())
                .and_then(|calls| calls.parse().ok())
                .unwrap_or(0)
        };
        let flushes_before =
            flush_calls(target_cli(docker, &target.name, "INFO commandstats").await);
        target_cli(
            docker,
            &target.name,
            "-n 0 SET _temps:redis_import_claim:abandoned_wait 9:pending",
        )
        .await;
        let redis_config = engine.import_hydrate(&config).await.expect("hydrate");
        docker
            .docker
            .pause_container(&target.name)
            .await
            .expect("pause target");
        let gave_up = engine
            .import_allocate(
                "e2e",
                &redis_config,
                "abandoned_wait",
                Some(9),
                Duration::from_secs(1),
            )
            .await;
        docker
            .docker
            .unpause_container(&target.name)
            .await
            .expect("unpause target");
        let error = gave_up.expect_err("the wait runs out while Redis is paused");
        assert!(
            error.to_string().contains("gives the database back"),
            "{error}"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let flushes = flush_calls(target_cli(docker, &target.name, "INFO commandstats").await);
            let mapped = target_cli(docker, &target.name, &mapping("abandoned_wait")).await;
            if flushes > flushes_before && mapped.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "abandoned allocation was not released (flushes {flushes_before} -> {flushes}, mapping {mapped:?})"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert_eq!(
            target_cli(
                docker,
                &target.name,
                "-n 0 EXISTS _temps:redis_import_claim:abandoned_wait"
            )
            .await,
            "0"
        );
    }
}
