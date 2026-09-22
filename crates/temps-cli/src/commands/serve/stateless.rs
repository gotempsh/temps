// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A replaceable control plane has one active owner per PostgreSQL database.
//! This connection must never come from a pool: returning an advisory-lock
//! connection to a pool does not release the lock.

use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use std::sync::Arc;
use std::time::Duration;

const OWNER_LOCK: i64 = 0x54454d5053435031;
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum StatelessStartupError {
    #[error("Stateless control-plane configuration: {detail}")]
    Configuration { detail: String },
    #[error("Another control plane owns this PostgreSQL database; stop that instance before replacing it")]
    AlreadyOwned,
    #[error("Stateless control-plane database operation '{operation}' failed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sqlx::Error,
    },
    #[error("Stateless control-plane database operation '{operation}' exceeded 5 seconds")]
    Timeout { operation: &'static str },
    #[error("Stateless control-plane state verification failed: {detail}")]
    State { detail: String },
    #[error("Stateless S3 startup probe failed during {operation}: {detail}")]
    Storage {
        operation: &'static str,
        detail: String,
    },
}

fn storage_configuration(
) -> Result<(String, temps_file_store::s3_config::S3StorageConfig), StatelessStartupError> {
    use temps_file_store::s3_config::{
        resolve_stateless_storage, resolve_static_storage_backend, StatelessStorage,
        StaticStorageBackend,
    };
    let config_error = |error: temps_file_store::s3_config::StaticStorageConfigError| {
        StatelessStartupError::Configuration {
            detail: error.to_string(),
        }
    };
    let StatelessStorage::Enabled { instance_id } =
        resolve_stateless_storage().map_err(config_error)?
    else {
        return Err(StatelessStartupError::Configuration {
            detail: "stateless storage was not enabled".into(),
        });
    };
    let StaticStorageBackend::S3(config) =
        resolve_static_storage_backend().map_err(config_error)?
    else {
        return Err(StatelessStartupError::Configuration {
            detail: "stateless storage must use S3".into(),
        });
    };
    Ok((instance_id, config))
}

pub fn storage_identity() -> Result<(String, String), StatelessStartupError> {
    let (instance_id, config) = storage_configuration()?;
    let identity = serde_json::to_string(&(
        config.bucket,
        config.region,
        config.endpoint,
        format!("instances/{instance_id}/"),
    ))
    .map_err(|error| StatelessStartupError::Configuration {
        detail: format!("cannot serialize storage identity: {error}"),
    })?;
    Ok((instance_id, identity))
}

pub async fn prepare_storage() -> Result<(), StatelessStartupError> {
    let (instance_id, config) = storage_configuration()?;
    let client = temps_file_store::s3_client::build_s3_client(&config);
    let key = format!("instances/{instance_id}/.health/{}", uuid::Uuid::new_v4());
    let payload = uuid::Uuid::new_v4().to_string();
    client
        .put_object()
        .bucket(&config.bucket)
        .key(&key)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            payload.clone().into_bytes(),
        ))
        .send()
        .await
        .map_err(|error| StatelessStartupError::Storage {
            operation: "write",
            detail: error.to_string(),
        })?;
    let read_result = async {
        let object = client
            .get_object()
            .bucket(&config.bucket)
            .key(&key)
            .range("bytes=0-63")
            .send()
            .await
            .map_err(|error| StatelessStartupError::Storage {
                operation: "read",
                detail: error.to_string(),
            })?;
        let bytes = tokio::time::timeout(Duration::from_secs(10), object.body.collect())
            .await
            .map_err(|_| StatelessStartupError::Storage {
                operation: "read body",
                detail: "10 second timeout".into(),
            })?
            .map_err(|error| StatelessStartupError::Storage {
                operation: "read body",
                detail: error.to_string(),
            })?;
        if bytes.into_bytes().as_ref() != payload.as_bytes() {
            return Err(StatelessStartupError::Storage {
                operation: "verify",
                detail: "stored probe differs from written data".into(),
            });
        }
        Ok(())
    }
    .await;
    let delete_result = client
        .delete_object()
        .bucket(&config.bucket)
        .key(&key)
        .send()
        .await
        .map_err(|error| StatelessStartupError::Storage {
            operation: "delete",
            detail: error.to_string(),
        });
    read_result?;
    delete_result?;
    Ok(())
}

/// Holds the dedicated session until process shutdown. A lost session makes
/// continued execution unsafe, so the watchdog terminates the whole process,
/// including detached plugin tasks. An orchestrator may then replace it.
pub struct ControlPlaneOwner {
    monitor: tokio::task::JoinHandle<()>,
}

impl ControlPlaneOwner {
    pub async fn acquire(database_url: &str) -> Result<Self, StatelessStartupError> {
        let mut connection =
            tokio::time::timeout(DATABASE_TIMEOUT, PgConnection::connect(database_url))
                .await
                .map_err(|_| StatelessStartupError::Timeout {
                    operation: "connect owner session",
                })?
                .map_err(|source| StatelessStartupError::Database {
                    operation: "connect owner session",
                    source,
                })?;
        let acquired: bool = tokio::time::timeout(
            DATABASE_TIMEOUT,
            sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(OWNER_LOCK)
                .fetch_one(&mut connection),
        )
        .await
        .map_err(|_| StatelessStartupError::Timeout {
            operation: "acquire owner lock",
        })?
        .map_err(|source| StatelessStartupError::Database {
            operation: "acquire owner lock",
            source,
        })?;
        if !acquired {
            return Err(StatelessStartupError::AlreadyOwned);
        }
        let monitor = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                match tokio::time::timeout(DATABASE_TIMEOUT, connection.ping()).await {
                    Ok(Ok(())) => {}
                    result => {
                        tracing::error!(error = ?result, "Control-plane ownership session lost; terminating to stop all mutations");
                        std::process::exit(1);
                    }
                }
            }
        });
        Ok(Self { monitor })
    }
}

impl Drop for ControlPlaneOwner {
    fn drop(&mut self) {
        self.monitor.abort();
    }
}

pub fn validate_profile(
    profile: super::ServeProfile,
    role: super::ServeRole,
) -> Result<(), StatelessStartupError> {
    if profile != super::ServeProfile::ControlPlane || role != super::ServeRole::Console {
        return Err(StatelessStartupError::Configuration {
            detail: "TEMPS_STATELESS=true requires --profile control-plane --role console; public application traffic must use workers".into(),
        });
    }
    Ok(())
}

pub fn validate_scratch_directory(data_dir: &std::path::Path) -> Result<(), StatelessStartupError> {
    for name in [
        "ai-applications",
        "plugins",
        "plugin-data",
        "source-bundles",
        "static-bundles",
    ] {
        let path = data_dir.join(name);
        let mut entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(StatelessStartupError::Configuration {
                    detail: format!(
                        "cannot inspect {} before stateless startup: {error}",
                        path.display()
                    ),
                })
            }
        };
        if entries.next().is_some() {
            return Err(StatelessStartupError::Configuration {
                detail: format!("{} contains persistent local feature data unsupported by stateless v1; export or relocate it before switching profiles", path.display()),
            });
        }
    }
    Ok(())
}

fn management_url(value: &str) -> Result<String, StatelessStartupError> {
    let parsed = url::Url::parse(value).map_err(|_| StatelessStartupError::Configuration {
        detail: "TEMPS_MANAGEMENT_URL must be an absolute HTTPS origin".into(),
    })?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (parsed.scheme() != "https" && !(loopback && parsed.scheme() == "http"))
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(StatelessStartupError::Configuration {
            detail: "TEMPS_MANAGEMENT_URL must be an HTTPS origin without credentials, path, query or fragment (HTTP is allowed only on loopback for local tests)".into(),
        });
    }
    Ok(parsed.origin().ascii_serialization())
}

pub async fn reject_local_mode_for_managed_database(
    db: &sea_orm::DatabaseConnection,
) -> Result<(), StatelessStartupError> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    if db.get_database_backend() != DatabaseBackend::Postgres {
        return Ok(());
    }
    let table = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT to_regclass('stateless_control_plane') IS NOT NULL AS present".to_owned(),
        ))
        .await
        .map_err(|error| StatelessStartupError::State {
            detail: format!("cannot inspect installation mode: {error}"),
        })?
        .ok_or_else(|| StatelessStartupError::State {
            detail: "database did not return installation-mode schema inspection".into(),
        })?;
    let table_present =
        table
            .try_get::<bool>("", "present")
            .map_err(|error| StatelessStartupError::State {
                detail: format!("cannot decode installation-mode schema inspection: {error}"),
            })?;
    if !table_present {
        return Ok(());
    }
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT id FROM stateless_control_plane WHERE id = 1".to_owned(),
        ))
        .await
        .map_err(|error| StatelessStartupError::State {
            detail: format!("cannot inspect installation binding: {error}"),
        })?;
    if row.is_some() {
        return Err(StatelessStartupError::Configuration {
            detail: "this database belongs to a stateless control plane; start with TEMPS_STATELESS=true and the original injected secrets and storage configuration".into(),
        });
    }
    Ok(())
}

/// Verify replacement credentials before migrations can change an existing database.
/// First adoption of an initialized OSS installation requires its original local
/// secrets as independent evidence; never let a new key certify itself.
pub async fn preflight_identity(
    db: &sea_orm::DatabaseConnection,
    config: &temps_config::ServerConfig,
    encryption: &temps_core::EncryptionService,
    identity: Option<(&str, &str)>,
) -> Result<(), StatelessStartupError> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    if identity.is_some() {
        let origin = std::env::var("TEMPS_MANAGEMENT_URL").map_err(|_| {
            StatelessStartupError::Configuration {
                detail: "TEMPS_MANAGEMENT_URL is required".into(),
            }
        })?;
        management_url(&origin)?;
    }
    let query = |sql: &str| Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
    let state_error = |error: sea_orm::DbErr| StatelessStartupError::State {
        detail: format!("cannot inspect installation before migration: {error}"),
    };
    let tables = db.query_one(query("SELECT to_regclass('stateless_control_plane') IS NOT NULL AS bound, to_regclass('users') IS NOT NULL AS initialized"))
        .await.map_err(state_error)?.ok_or_else(|| StatelessStartupError::State { detail: "database did not return schema inspection".into() })?;
    if tables.try_get::<bool>("", "bound").map_err(state_error)? {
        if let Some(row) = db.query_one(query("SELECT instance_id, management_url, storage_identity, secret_verifier FROM stateless_control_plane WHERE id = 1")).await.map_err(state_error)? {
            let Some((instance_id, storage_identity)) = identity else {
                return Err(StatelessStartupError::Configuration { detail: "this database requires TEMPS_STATELESS=true and its original injected configuration".into() });
            };
            let read = |name| row.try_get::<String>("", name).map_err(state_error);
            let supplied_url = std::env::var("TEMPS_MANAGEMENT_URL").map_err(|_| StatelessStartupError::Configuration { detail: "TEMPS_MANAGEMENT_URL is required".into() })?;
            if read("instance_id")? != instance_id || read("storage_identity")? != storage_identity || read("management_url")? != management_url(&supplied_url)? {
                return Err(StatelessStartupError::State { detail: "replacement identity or storage destination does not match this installation".into() });
            }
            let verifier = encryption.decrypt_string(&read("secret_verifier")?).map_err(|_| StatelessStartupError::State { detail: "injected encryption key does not match this installation".into() })?;
            if verifier != hex::encode(Sha256::digest(config.auth_secret.as_bytes())) {
                return Err(StatelessStartupError::State { detail: "injected auth secret does not match this installation".into() });
            }
            return Ok(());
        }
    }
    if identity.is_some()
        && tables
            .try_get::<bool>("", "initialized")
            .map_err(state_error)?
    {
        validate_adoption_secrets(
            &config.data_dir,
            &config.encryption_key,
            &config.auth_secret,
        )?;
    }
    Ok(())
}

fn validate_adoption_secrets(
    directory: &std::path::Path,
    key: &str,
    auth: &str,
) -> Result<(), StatelessStartupError> {
    let read = |name| {
        std::fs::read_to_string(directory.join(name)).map_err(|_| StatelessStartupError::State {
        detail: format!("first stateless adoption requires the original {name} file in the data directory; mount the original installation secrets for this one-time transition"),
    })
    };
    let original_key = read("encryption_key")?;
    let original_auth = read("auth_secret")?;
    let original = temps_core::EncryptionService::new(original_key.trim()).map_err(|_| {
        StatelessStartupError::State {
            detail: "original adoption encryption key is invalid".into(),
        }
    })?;
    let supplied =
        temps_core::EncryptionService::new(key).map_err(|_| StatelessStartupError::State {
            detail: "injected adoption encryption key is invalid".into(),
        })?;
    if original.derive_subkey("stateless-adoption") != supplied.derive_subkey("stateless-adoption")
        || original_auth.trim() != auth
    {
        return Err(StatelessStartupError::State { detail: "injected secrets differ from the original installation; adoption refused without changing the database".into() });
    }
    Ok(())
}

/// Bind a database to its stable installation identity and externally held
/// secrets. The verifier is encrypted, so neither secret is stored in clear.
/// A first adoption also validates the existing cluster CA before persisting
/// a verifier, preventing an incorrect key from blessing itself on upgrade.
pub async fn verify_identity(
    db: Arc<sea_orm::DatabaseConnection>,
    config: Arc<temps_config::ServerConfig>,
    encryption: &temps_core::EncryptionService,
    instance_id: &str,
    storage_identity: &str,
) -> Result<(), StatelessStartupError> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let supplied_url = std::env::var("TEMPS_MANAGEMENT_URL").map_err(|_| {
        StatelessStartupError::Configuration { detail: "TEMPS_MANAGEMENT_URL is required so callbacks and workers keep the same address after replacement".into() }
    })?;
    let url = management_url(&supplied_url)?;
    let config_service = temps_config::ConfigService::new(config.clone(), db.clone());
    let settings =
        config_service
            .get_settings()
            .await
            .map_err(|error| StatelessStartupError::State {
                detail: format!("cannot read installation settings: {error}"),
            })?;
    if let Some(existing) = settings.external_url.as_deref() {
        if management_url(existing)? != url {
            return Err(StatelessStartupError::State { detail: "TEMPS_MANAGEMENT_URL differs from the stored external URL; retain the existing origin when replacing a control plane".into() });
        }
    }
    if let Some(key) = settings.multi_node.cluster_ca_key_encrypted.as_deref() {
        encryption.decrypt(key).map_err(|_| StatelessStartupError::State {
            detail: "injected encryption key cannot decrypt the existing cluster CA; restore the original key".into(),
        })?;
    }
    let expected = hex::encode(Sha256::digest(config.auth_secret.as_bytes()));
    let row = db.query_one(Statement::from_string(DatabaseBackend::Postgres,
        "SELECT instance_id, management_url, storage_identity, secret_verifier FROM stateless_control_plane WHERE id = 1".to_string()))
        .await.map_err(|error| StatelessStartupError::State { detail: format!("cannot read persisted identity: {error}") })?;
    if let Some(row) = row {
        let read = |name| {
            row.try_get::<String>("", name)
                .map_err(|error| StatelessStartupError::State {
                    detail: format!("cannot read identity field {name}: {error}"),
                })
        };
        if read("instance_id")? != instance_id
            || read("management_url")? != url
            || read("storage_identity")? != storage_identity
        {
            return Err(StatelessStartupError::State { detail: "instance ID, management URL or S3 destination differs from this database's persisted identity; restore the original configuration".into() });
        }
        let verifier = encryption
            .decrypt_string(&read("secret_verifier")?)
            .map_err(|_| StatelessStartupError::State {
                detail: "injected encryption key does not match this installation".into(),
            })?;
        if verifier != expected {
            return Err(StatelessStartupError::State { detail: "injected auth secret does not match this installation; restore the original secret to preserve sessions".into() });
        }
    } else {
        let verifier =
            encryption
                .encrypt_string(&expected)
                .map_err(|_| StatelessStartupError::State {
                    detail: "cannot encrypt installation secret verifier".into(),
                })?;
        db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO stateless_control_plane (id, instance_id, management_url, storage_identity, secret_verifier) VALUES (1, $1, $2, $3, $4)",
            [instance_id.into(), url.clone().into(), storage_identity.into(), verifier.into()]))
            .await.map_err(|error| StatelessStartupError::State { detail: format!("cannot persist installation identity: {error}") })?;
    }
    if settings.external_url.is_none() {
        config_service
            .update_setting_field(|settings| settings.external_url = Some(url))
            .await
            .map_err(|error| StatelessStartupError::State {
                detail: format!("cannot persist management URL: {error}"),
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adoption_requires_matching_original_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let key = "ab".repeat(32);
        let auth = "original-auth-secret-that-is-at-least-32-bytes";
        assert!(validate_adoption_secrets(directory.path(), &key, auth).is_err());
        std::fs::write(directory.path().join("encryption_key"), &key).unwrap();
        std::fs::write(directory.path().join("auth_secret"), auth).unwrap();
        assert!(validate_adoption_secrets(directory.path(), &key, auth).is_ok());
        assert!(validate_adoption_secrets(directory.path(), &"cd".repeat(32), auth).is_err());
        assert!(
            validate_adoption_secrets(directory.path(), &key, "different-auth-secret").is_err()
        );
    }

    #[test]
    fn stateless_profile_cannot_run_local_workloads_or_ingress() {
        assert!(validate_profile(
            super::super::ServeProfile::ControlPlane,
            super::super::ServeRole::Console
        )
        .is_ok());
        assert!(validate_profile(
            super::super::ServeProfile::Full,
            super::super::ServeRole::Console
        )
        .is_err());
        assert!(validate_profile(
            super::super::ServeProfile::ControlPlane,
            super::super::ServeRole::All
        )
        .is_err());
    }

    #[test]
    fn management_origin_is_stable_and_cannot_embed_credentials() {
        assert_eq!(
            management_url("https://console.example.test/").unwrap(),
            "https://console.example.test"
        );
        assert!(management_url("http://127.0.0.1:59001").is_ok());
        for invalid in [
            "http://console.example.test",
            "https://user:secret@console.example.test",
            "https://console.example.test/callback",
            "https://console.example.test?token=x",
        ] {
            assert!(management_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn switching_modes_cannot_silently_abandon_local_workspaces() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_scratch_directory(directory.path()).is_ok());
        std::fs::create_dir(directory.path().join("ai-applications")).unwrap();
        std::fs::write(
            directory.path().join("ai-applications/source.txt"),
            b"keep me",
        )
        .unwrap();
        assert!(validate_scratch_directory(directory.path()).is_err());
    }

    #[tokio::test]
    async fn postgres_rejects_a_second_owner_and_allows_replacement() {
        let database = match temps_database::test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping owner integration test: Docker unavailable: {error}");
                return;
            }
            Err(error) => panic!("owner test database failed: {error}"),
        };
        let first = ControlPlaneOwner::acquire(&database.database_url)
            .await
            .unwrap();
        assert!(matches!(
            ControlPlaneOwner::acquire(&database.database_url).await,
            Err(StatelessStartupError::AlreadyOwned)
        ));
        drop(first);
        // Aborting the watchdog drops its owned session asynchronously.
        let replacement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ControlPlaneOwner::acquire(&database.database_url).await {
                    Ok(owner) => break owner,
                    Err(StatelessStartupError::AlreadyOwned) => {
                        tokio::time::sleep(Duration::from_millis(20)).await
                    }
                    Err(error) => panic!("replacement failed: {error}"),
                }
            }
        })
        .await
        .expect("owner session must be released after shutdown");
        drop(replacement);
    }
}
