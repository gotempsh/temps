// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service parameter schemas, served without a Docker daemon.

use super::*;
use crate::externalsvc::{
    garage::GarageService, mariadb::MariaDbService, mongodb::MongodbService,
    postgres::PostgresService, redis::RedisService, rustfs::RustfsService, s3::S3Service,
    ManagedS3BackendKind, ManagedS3BackendSelection, ManagedS3Error, ServiceType,
};
use std::collections::HashMap;

/// The parameter schema for a service type, without touching Docker.
///
/// Every engine's `get_parameter_schema` is pure `schemars` metadata derived
/// from its input-config type: it never talks to a daemon. Routing schema
/// lookups through `create_service_instance` (which needs a `bollard::Docker`
/// only so it can construct the engine struct) made a control-plane process —
/// which deliberately has no daemon — answer a plain metadata question with a
/// 500. This dispatch is the Docker-free path those callers use instead.
///
/// The arms mirror `ExternalServiceManager::create_service_instance` exactly,
/// including the KV/Blob aliases, so the published schema can never disagree
/// with the engine that will actually be provisioned.
#[allow(deprecated)]
pub fn parameter_schema_for_service_type(service_type: ServiceType) -> Option<serde_json::Value> {
    match service_type {
        ServiceType::Mariadb => MariaDbService::parameter_schema(),
        ServiceType::Mongodb => MongodbService::parameter_schema(),
        ServiceType::Postgres => PostgresService::parameter_schema(),
        // Temps KV is Redis-backed; the name differs, the parameters do not.
        ServiceType::Redis | ServiceType::Kv => RedisService::parameter_schema(),
        // S3 and Blob are RustFS-backed by default.
        ServiceType::S3 | ServiceType::Blob | ServiceType::Rustfs => {
            RustfsService::parameter_schema()
        }
        ServiceType::Minio => S3Service::parameter_schema(),
    }
}

/// The parameter schema for an **existing** service, honouring the managed-S3
/// backend recorded in its parameters.
///
/// Detail responses must describe the engine the service actually runs: an S3
/// service created on the legacy MinIO backend has a different parameter set
/// from a RustFS one. This mirrors
/// `ExternalServiceManager::create_service_instance_for_parameter_value`'s
/// backend selection, minus the Docker client it only needed in order to build
/// an engine it then asked a static question.
#[allow(deprecated)]
pub fn parameter_schema_for_parameters(
    service_type: ServiceType,
    parameters: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ExternalServiceError> {
    if !matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
        return Ok(parameter_schema_for_service_type(service_type));
    }

    let backend_selection =
        ManagedS3BackendSelection::from_parameters(parameters).map_err(|e| {
            ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason: e.to_string(),
            }
        })?;
    match backend_selection.backend {
        ManagedS3BackendKind::Rustfs => Ok(parameter_schema_for_service_type(service_type)),
        ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => {
            Ok(S3Service::parameter_schema())
        }
        ManagedS3BackendKind::Minio => Err(ExternalServiceError::ParameterValidationFailed {
            service_id: 0,
            reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services"
                .to_string(),
        }),
        ManagedS3BackendKind::Garage => Ok(GarageService::parameter_schema()),
    }
}

/// Add the canonical create-form defaults to a service parameter schema.
///
/// Both the console and AI chat read this schema. Keeping the suggested name
/// and materialized parameter defaults here prevents chat from inventing a
/// second set of defaults (notably a bare `redis` name that can collide with
/// an existing `redis-*` managed container).
pub(super) fn service_creation_schema(
    service_type: ServiceType,
    schema: serde_json::Value,
) -> serde_json::Value {
    use rand::{distr::Alphanumeric, RngExt};

    // Match the console's lowercase alpha-numeric four-character suffix.
    let suffix: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .map(char::from)
        .filter(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        .take(4)
        .collect();
    service_creation_schema_with_suffix(service_type, schema, &suffix)
}

pub(super) fn service_creation_schema_with_suffix(
    service_type: ServiceType,
    mut schema: serde_json::Value,
    suffix: &str,
) -> serde_json::Value {
    let parameter_defaults = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .filter_map(|(name, property)| {
                    property
                        .get("default")
                        .cloned()
                        .map(|value| (name.clone(), value))
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()
        })
        .unwrap_or_default();

    if let Some(schema_object) = schema.as_object_mut() {
        schema_object.insert(
            "x-temps-creation-defaults".to_string(),
            serde_json::json!({
                "name": format!("{}-{}", service_type, suffix),
                "parameters": parameter_defaults,
                "topology": "standalone",
                "node_id": null,
            }),
        );
    }
    schema
}

impl ExternalServiceManager {
    pub async fn get_service_type_schema(
        &self,
        service_type: ServiceType,
    ) -> Result<Option<serde_json::Value>, ExternalServiceError> {
        self.get_service_type_schema_for_backend(service_type, None)
            .await
    }

    /// Creation schema for `service_type`, for a specific managed-S3 `backend`
    /// when one is named (`rustfs` when it is not). Backends other than the
    /// default configure different engine parameters, so the console asks for
    /// the schema of the engine the operator picked. `backend` is ignored for
    /// service types that have no backends.
    pub async fn get_service_type_schema_for_backend(
        &self,
        service_type: ServiceType,
        backend: Option<&str>,
    ) -> Result<Option<serde_json::Value>, ExternalServiceError> {
        // Pure metadata: no engine instance, no Docker client. A control
        // plane must answer this identically to a full node.
        let schema = match (service_type, backend.map(str::trim)) {
            (ServiceType::S3 | ServiceType::Blob, Some(backend)) if !backend.is_empty() => {
                let selection = ManagedS3BackendSelection::from_parameters(
                    &serde_json::json!({ "backend": backend }),
                )
                .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: e.to_string(),
                })?;
                if !selection.backend.is_creatable() {
                    return Err(ExternalServiceError::ParameterValidationFailed {
                        service_id: 0,
                        reason: ManagedS3Error::DeprecatedBackend.to_string(),
                    });
                }
                parameter_schema_for_parameters(
                    service_type,
                    &serde_json::json!({ "backend": selection.backend.as_str() }),
                )?
            }
            _ => parameter_schema_for_service_type(service_type),
        };
        let selection: HashMap<String, serde_json::Value> = backend
            .map(str::trim)
            .filter(|backend| !backend.is_empty())
            .map(|backend| {
                HashMap::from([(
                    "backend".to_string(),
                    serde_json::Value::String(backend.to_string()),
                )])
            })
            .unwrap_or_default();
        let strategy_key = Self::update_strategy_key(&service_type.to_string(), &selection);
        Ok(schema.map(|mut schema| {
            align_editable_with_update_rules(&mut schema, &strategy_key);
            service_creation_schema(service_type, schema)
        }))
    }

    /// Docker-free parameter-schema lookup for an existing service's stored
    /// parameters. Wraps [`parameter_schema_for_parameters`] with the
    /// `HashMap` -> `serde_json::Value` conversion both detail paths need.
    pub(super) fn parameter_schema_for(
        service_type: ServiceType,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ExternalServiceError> {
        let parameter_value =
            serde_json::to_value(parameters).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to inspect managed S3 backend parameters: {}", e),
            })?;
        let strategy_key = Self::update_strategy_key(&service_type.to_string(), parameters);
        Ok(
            parameter_schema_for_parameters(service_type, &parameter_value)?.map(|mut schema| {
                align_editable_with_update_rules(&mut schema, &strategy_key);
                schema
            }),
        )
    }
}

/// Set each property's `x-editable` to whether `PUT /external-services/{id}`
/// accepts a change to it, as decided by the engine's update strategy.
///
/// The engines annotate their schemas by hand, and those annotations drifted
/// from the rules updates enforce (a PostgreSQL `password` was advertised as
/// editable and then rejected). The strategy is what the update path
/// enforces, so it is the one source of truth for both.
pub(super) fn align_editable_with_update_rules(schema: &mut serde_json::Value, strategy_key: &str) {
    let Some(strategy) = crate::parameter_strategies::get_strategy(strategy_key) else {
        return;
    };
    let updateable = strategy.updateable_keys();
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for (name, property) in properties.iter_mut() {
        if let Some(property) = property.as_object_mut() {
            property.insert(
                "x-editable".to_string(),
                serde_json::Value::Bool(updateable.contains(&name.as_str())),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::{
        mariadb::MariaDbService, mongodb::MongodbService, postgres::PostgresService,
        redis::RedisService, rustfs::RustfsService, s3::S3Service, ServiceType,
    };
    use crate::services::test_support::*;

    #[test]
    fn service_creation_schema_exposes_console_defaults_to_all_clients() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer", "default": 6379 },
                "docker_image": {
                    "type": "string",
                    "default": "gotempsh/redis-walg:8-bookworm"
                },
                "password": { "type": "string" }
            }
        });

        let enriched = service_creation_schema_with_suffix(ServiceType::Redis, schema, "a1b2");
        let defaults = &enriched["x-temps-creation-defaults"];

        assert_eq!(defaults["name"], "redis-a1b2");
        assert_eq!(defaults["topology"], "standalone");
        assert!(defaults["node_id"].is_null());
        assert_eq!(defaults["parameters"]["port"], 6379);
        assert_eq!(
            defaults["parameters"]["docker_image"],
            "gotempsh/redis-walg:8-bookworm"
        );
        assert!(defaults["parameters"].get("password").is_none());
    }

    #[tokio::test]
    async fn creation_schema_follows_the_requested_s3_backend() {
        let manager = mock_service_manager(vec![]);
        let default = manager
            .get_service_type_schema_for_backend(ServiceType::S3, None)
            .await
            .unwrap()
            .unwrap();
        let rustfs = manager
            .get_service_type_schema_for_backend(ServiceType::S3, Some("rustfs"))
            .await
            .unwrap()
            .unwrap();
        let garage = manager
            .get_service_type_schema_for_backend(ServiceType::S3, Some(" Garage "))
            .await
            .unwrap()
            .unwrap();

        assert!(default["properties"].get("console_port").is_some());
        assert_eq!(default["properties"], rustfs["properties"]);
        assert!(garage["properties"].get("console_port").is_none());
        assert!(garage["properties"].get("admin_port").is_some());
        assert!(
            garage["x-temps-creation-defaults"]["parameters"]["docker_image"]
                .as_str()
                .is_some_and(|image| image.starts_with("dxflrs/garage:"))
        );

        for backend in ["minio", "ceph"] {
            let err = manager
                .get_service_type_schema_for_backend(ServiceType::S3, Some(backend))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
                "{backend}: {err:?}"
            );
        }

        // Backends mean nothing to other service types.
        let redis = manager
            .get_service_type_schema_for_backend(ServiceType::Redis, Some("garage"))
            .await
            .unwrap()
            .unwrap();
        assert!(redis["properties"].get("admin_port").is_none());
    }

    /// The regression this work fixes: asking for a service type's parameter
    /// schema is a pure metadata question — every engine answers it from
    /// `schemars` and never touches a daemon — yet it used to be routed
    /// through `create_service_instance`, which needs one. On a control plane
    /// that produced a 500 ("Failed to get parameter schema: This process has
    /// no local Docker daemon") for a request that cannot fail.
    #[tokio::test]
    async fn parameter_schema_is_served_without_a_docker_daemon() {
        let manager = control_plane_manager();

        for service_type in [
            ServiceType::Postgres,
            ServiceType::Mariadb,
            ServiceType::Mongodb,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Kv,
            ServiceType::Blob,
            ServiceType::Rustfs,
        ] {
            let schema = manager
                .get_service_type_schema(service_type)
                .await
                .unwrap_or_else(|e| panic!("{service_type} schema must not need Docker: {e}"))
                .unwrap_or_else(|| panic!("{service_type} must publish a schema"));

            assert_eq!(
                schema.get("type").and_then(|t| t.as_str()),
                Some("object"),
                "{service_type} schema must be a JSON Schema object: {schema}"
            );
            assert!(
                schema.get("x-temps-creation-defaults").is_some(),
                "{service_type} schema must keep the creation defaults: {schema}"
            );
        }
    }

    /// A client that trusts `x-editable` must never send a change the update
    /// then rejects: every published flag matches what the engine's update
    /// strategy accepts (a PostgreSQL `password` used to be advertised as
    /// editable and then refused as read-only).
    #[tokio::test]
    async fn published_editable_flags_match_what_an_update_accepts() {
        let manager = control_plane_manager();
        let cases = [
            (ServiceType::Postgres, None, "postgres"),
            (ServiceType::Mariadb, None, "mariadb"),
            (ServiceType::Mongodb, None, "mongodb"),
            (ServiceType::Redis, None, "redis"),
            (ServiceType::Kv, None, "kv"),
            (ServiceType::S3, None, "s3"),
            (ServiceType::S3, Some("garage"), "garage"),
            (ServiceType::Blob, None, "blob"),
            (ServiceType::Rustfs, None, "rustfs"),
        ];
        for (service_type, backend, strategy_key) in cases {
            let schema = manager
                .get_service_type_schema_for_backend(service_type, backend)
                .await
                .unwrap_or_else(|e| panic!("{service_type} schema: {e}"))
                .unwrap_or_else(|| panic!("{service_type} must publish a schema"));
            let strategy = crate::parameter_strategies::get_strategy(strategy_key)
                .unwrap_or_else(|| panic!("{strategy_key} has an update strategy"));
            let properties = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{service_type} schema has properties"));
            let updateable = strategy.updateable_keys();
            for (name, property) in properties {
                let editable = property["x-editable"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("{service_type}.{name} has x-editable"));
                assert_eq!(
                    editable,
                    updateable.contains(&name.as_str()),
                    "{service_type} ({strategy_key}).{name}: x-editable must match the update rules"
                );
                if !editable {
                    // A read-only field is refused on update whatever its
                    // value, so advertising it as editable would be a lie.
                    let change = HashMap::from([(name.clone(), serde_json::json!("changed"))]);
                    assert!(
                        strategy.validate_for_update(&change).is_err(),
                        "{service_type} ({strategy_key}).{name} is read-only but an update accepts it"
                    );
                }
            }
        }

        let postgres = manager
            .get_service_type_schema(ServiceType::Postgres)
            .await
            .unwrap()
            .unwrap();
        for read_only in ["password", "database", "username", "host"] {
            assert_eq!(
                postgres["properties"][read_only]["x-editable"], false,
                "postgres.{read_only}"
            );
        }
        assert_eq!(postgres["properties"]["port"]["x-editable"], true);
    }

    /// The static dispatch must agree with the engine a provisioning request
    /// would actually build, or the console publishes a form that the create
    /// validator then rejects.
    #[test]
    fn static_schema_matches_the_engine_schema_for_every_service_type() {
        #[allow(deprecated)]
        let cases = [
            (ServiceType::Postgres, PostgresService::parameter_schema()),
            (ServiceType::Mariadb, MariaDbService::parameter_schema()),
            (ServiceType::Mongodb, MongodbService::parameter_schema()),
            (ServiceType::Redis, RedisService::parameter_schema()),
            (ServiceType::Kv, RedisService::parameter_schema()),
            (ServiceType::S3, RustfsService::parameter_schema()),
            (ServiceType::Blob, RustfsService::parameter_schema()),
            (ServiceType::Rustfs, RustfsService::parameter_schema()),
            (ServiceType::Minio, S3Service::parameter_schema()),
        ];

        for (service_type, expected) in cases {
            assert_eq!(
                parameter_schema_for_service_type(service_type),
                expected,
                "static dispatch disagrees with the engine for {service_type}"
            );
        }
    }

    /// An existing managed-S3 service on the legacy MinIO backend must keep
    /// describing MinIO's parameters, without a daemon.
    #[test]
    fn parameters_aware_schema_honours_the_managed_s3_backend() {
        let minio = serde_json::json!({ "backend": "minio" });
        #[allow(deprecated)]
        let expected = S3Service::parameter_schema();
        assert_eq!(
            parameter_schema_for_parameters(ServiceType::S3, &minio)
                .expect("minio is a valid S3 backend"),
            expected
        );

        let default = serde_json::json!({});
        assert_eq!(
            parameter_schema_for_parameters(ServiceType::S3, &default)
                .expect("the default backend is valid"),
            RustfsService::parameter_schema()
        );
    }
}
