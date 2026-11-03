// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The route table edge CDN nodes poll: which domains to handle, whether each
//! serves static files, and (for edge nodes only) their TLS certificates,
//! encrypted to the polling node's key.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_config::AppSettings;
use temps_core::PublicHostnameStrategy;
use temps_entities::{
    custom_routes, deployments, domains, environment_domains, environments, nodes,
    project_custom_domains, settings,
};
use thiserror::Error;
use tracing::{error, warn};
use utoipa::ToSchema;

/// A single route entry for edge CDN nodes.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeRouteEntry {
    pub domain: String,
    pub is_static: bool,
    /// Whether this domain uses wildcard matching (e.g. `*.localho.st`).
    #[serde(default)]
    pub is_wildcard: bool,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

/// An encrypted TLS certificate bundle for an edge node.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeCertBundle {
    pub domain: String,
    /// Base64-encoded AES-256-GCM ciphertext of (cert_pem + "\n" + key_pem)
    pub ciphertext: String,
    /// Base64-encoded 12-byte nonce
    pub nonce: String,
    /// SHA-256 hex fingerprint of the certificate PEM (for change detection)
    pub fingerprint: String,
}

/// Encrypted certificate payload in the edge routes response.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EdgeCertificates {
    /// Base64-encoded ephemeral X25519 public key (for ECDH)
    pub ephemeral_public_key: String,
    pub bundles: Vec<EdgeCertBundle>,
}

/// What one edge node receives for one poll.
pub struct EdgeRouteTable {
    pub routes: Vec<EdgeRouteEntry>,
    pub certificates: Option<EdgeCertificates>,
}

#[derive(Debug, Error)]
pub enum EdgeRouteError {
    #[error("Could not look up the node holding an edge token: {source}")]
    TokenLookup { source: sea_orm::DbErr },

    #[error("Could not query {what} for the edge route table: {source}")]
    Query {
        what: &'static str,
        source: sea_orm::DbErr,
    },
}

pub struct EdgeRouteService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl EdgeRouteService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// The node holding a bearer token, by the token's hash.
    pub async fn node_for_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<nodes::Model>, EdgeRouteError> {
        nodes::Entity::find()
            .filter(nodes::Column::TokenHash.eq(token_hash))
            .one(self.db.as_ref())
            .await
            .map_err(|source| {
                error!("Edge routes: DB error verifying token: {}", source);
                EdgeRouteError::TokenLookup { source }
            })
    }

    /// The route table for `node`. The caller has already authorized it.
    pub async fn route_table(&self, node: &nodes::Model) -> Result<EdgeRouteTable, EdgeRouteError> {
        let mut routes = self.environment_domain_routes().await?;
        self.add_preview_domain_routes(&mut routes).await?;
        self.add_custom_routes(&mut routes).await?;
        self.add_project_custom_domains(&mut routes).await?;
        let certificates = self.encrypted_certificates(node).await?;
        Ok(EdgeRouteTable {
            routes,
            certificates,
        })
    }

    /// Whether the environment's current deployment serves static files.
    async fn serves_static_files(&self, environment: &environments::Model) -> bool {
        let Some(deploy_id) = environment.current_deployment_id else {
            return false;
        };
        deployments::Entity::find_by_id(deploy_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|d| d.static_dir_location.is_some())
            .unwrap_or(false)
    }

    /// 1. Environment domains (explicit per-environment domains)
    async fn environment_domain_routes(&self) -> Result<Vec<EdgeRouteEntry>, EdgeRouteError> {
        let domains: Vec<(environment_domains::Model, Option<environments::Model>)> =
            environment_domains::Entity::find()
                .find_also_related(environments::Entity)
                .all(self.db.as_ref())
                .await
                .map_err(|source| {
                    error!("Edge routes: failed to query domains: {}", source);
                    EdgeRouteError::Query {
                        what: "environment domains",
                        source,
                    }
                })?;

        let mut routes = Vec::with_capacity(domains.len());
        for (domain_entry, env_opt) in &domains {
            let env = match env_opt {
                Some(e) => e,
                None => continue,
            };
            routes.push(EdgeRouteEntry {
                domain: domain_entry.domain.clone(),
                is_static: self.serves_static_files(env).await,
                is_wildcard: false,
                project_id: Some(env.project_id),
                environment_id: Some(env.id),
            });
        }
        Ok(routes)
    }

    /// 2. Preview domain routes: {subdomain}.{preview_domain} for all active environments
    ///    (mirrors Section 4 of the control-plane route table)
    async fn add_preview_domain_routes(
        &self,
        routes: &mut Vec<EdgeRouteEntry>,
    ) -> Result<(), EdgeRouteError> {
        let app_settings = settings::Entity::find()
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();

        let all_envs = environments::Entity::find()
            .filter(environments::Column::Subdomain.is_not_null())
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                error!(
                    "Edge routes: failed to query environments for preview domains: {}",
                    source
                );
                EdgeRouteError::Query {
                    what: "environments",
                    source,
                }
            })?;

        for env in &all_envs {
            let full_domain = PublicHostnameStrategy::Standard
                .environment_hostname(&app_settings.preview_domain, &env.subdomain);
            // Skip if already added from environment_domains
            if routes.iter().any(|r| r.domain == full_domain) {
                continue;
            }
            routes.push(EdgeRouteEntry {
                domain: full_domain,
                is_static: self.serves_static_files(env).await,
                is_wildcard: false,
                project_id: Some(env.project_id),
                environment_id: Some(env.id),
            });
        }
        Ok(())
    }

    /// 3. Custom routes (including wildcards like *.localho.st)
    async fn add_custom_routes(
        &self,
        routes: &mut Vec<EdgeRouteEntry>,
    ) -> Result<(), EdgeRouteError> {
        let custom_routes_data = custom_routes::Entity::find()
            .filter(custom_routes::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                error!("Edge routes: failed to query custom routes: {}", source);
                EdgeRouteError::Query {
                    what: "custom routes",
                    source,
                }
            })?;

        for custom_route in &custom_routes_data {
            routes.push(EdgeRouteEntry {
                domain: custom_route.domain.clone(),
                is_static: false,
                is_wildcard: custom_route.domain.starts_with("*."),
                project_id: None,
                environment_id: None,
            });
        }
        Ok(())
    }

    /// 4. Project custom domains (custom domains mapped to environments)
    async fn add_project_custom_domains(
        &self,
        routes: &mut Vec<EdgeRouteEntry>,
    ) -> Result<(), EdgeRouteError> {
        let custom_domains = project_custom_domains::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                error!(
                    "Edge routes: failed to query project custom domains: {}",
                    source
                );
                EdgeRouteError::Query {
                    what: "project custom domains",
                    source,
                }
            })?;

        for custom_domain in &custom_domains {
            // Skip domains already added from environment_domains
            if routes.iter().any(|r| r.domain == custom_domain.domain) {
                continue;
            }
            routes.push(EdgeRouteEntry {
                domain: custom_domain.domain.clone(),
                is_static: false,
                is_wildcard: false,
                project_id: Some(custom_domain.project_id),
                environment_id: Some(custom_domain.environment_id),
            });
        }
        Ok(())
    }

    /// Encrypt TLS certificates for this edge node (if it has a public key)
    /// Only edge nodes should receive TLS private keys — never workers
    async fn encrypted_certificates(
        &self,
        node: &nodes::Model,
    ) -> Result<Option<EdgeCertificates>, EdgeRouteError> {
        if node.role != "edge" {
            return Ok(None);
        }
        let Some(edge_pk) = node.edge_public_key.as_ref() else {
            return Ok(None);
        };

        // Include "active_renewal_failed": those domains still hold a valid cert that
        // edge nodes must keep serving until it actually expires.
        let active_domains = domains::Entity::find()
            .filter(domains::Column::Status.is_in(domains::CERT_SERVING_STATUSES))
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                error!("Edge routes: failed to query domains for certs: {}", source);
                EdgeRouteError::Query {
                    what: "domain certificates",
                    source,
                }
            })?;

        // Create one encryption session per sync (single ephemeral key, forward secrecy)
        let session = match temps_core::ecies::EncryptionSession::new(edge_pk) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Edge routes: invalid edge public key for node {}: {}",
                    node.id, e
                );
                return Ok(None);
            }
        };

        let mut bundles = Vec::new();

        for domain in &active_domains {
            if let (Some(cert_pem), Some(encrypted_key_pem)) =
                (&domain.certificate, &domain.private_key)
            {
                // Decrypt the private key stored in the DB (it's AES-encrypted by EncryptionService)
                let key_pem = match self.encryption_service.decrypt_string(encrypted_key_pem) {
                    Ok(k) => k,
                    Err(e) => {
                        warn!(
                            "Edge routes: failed to decrypt private key for domain {}: {}",
                            domain.domain, e
                        );
                        continue;
                    }
                };

                // Combine cert + key into a single payload
                let payload = format!("{}\n{}", cert_pem, key_pem);
                let fingerprint = temps_core::ecies::cert_fingerprint(cert_pem);

                match session.encrypt(payload.as_bytes()) {
                    Ok(bundle) => {
                        bundles.push(EdgeCertBundle {
                            domain: domain.domain.clone(),
                            ciphertext: bundle.ciphertext,
                            nonce: bundle.nonce,
                            fingerprint,
                        });
                    }
                    Err(e) => {
                        warn!(
                            "Edge routes: failed to encrypt cert for domain {}: {}",
                            domain.domain, e
                        );
                    }
                }
            }
        }

        if bundles.is_empty() {
            Ok(None)
        } else {
            Ok(Some(EdgeCertificates {
                ephemeral_public_key: session.ephemeral_public_key().to_string(),
                bundles,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn service(db: MockDatabase) -> EdgeRouteService {
        EdgeRouteService::new(
            Arc::new(db.into_connection()),
            Arc::new(
                temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
            ),
        )
    }

    fn mock() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    fn node(role: &str, edge_public_key: Option<String>) -> nodes::Model {
        serde_json::from_value(serde_json::json!({
            "id": 5,
            "name": "edge-5",
            "token_hash": "hash",
            "address": "http://10.0.0.5:3100",
            "private_address": "10.0.0.5",
            "role": role,
            "status": "active",
            "labels": {},
            "capacity": {},
            "edge_public_key": edge_public_key,
            "dns_resolver_consecutive_failures": 0,
            "public_ingress_enabled": false,
            "public_ingress_unsupported_reasons": [],
            "created_at": NOW,
            "updated_at": NOW
        }))
        .expect("node fixture matches the entity")
    }

    fn environment(id: i32, subdomain: &str, deployment: Option<i32>) -> environments::Model {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": 3,
            "name": "production",
            "slug": "production",
            "subdomain": subdomain,
            "host": "",
            "upstreams": [],
            "current_deployment_id": deployment,
            "created_at": NOW,
            "updated_at": NOW,
            "is_preview": false,
            "protected": false,
            "sleeping": false
        }))
        .expect("environment fixture matches the entity")
    }

    fn environment_domain(domain: &str, environment_id: i32) -> environment_domains::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "environment_id": environment_id,
            "domain": domain,
            "created_at": NOW
        }))
        .expect("environment domain fixture matches the entity")
    }

    fn custom_route(domain: &str) -> custom_routes::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "domain": domain,
            "host": "10.0.0.1",
            "port": 80,
            "created_at": NOW,
            "updated_at": NOW,
            "enabled": true,
            "route_type": "Http"
        }))
        .expect("custom route fixture matches the entity")
    }

    fn project_domain(domain: &str) -> project_custom_domains::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "project_id": 3,
            "environment_id": 2,
            "domain": domain,
            "status": "active",
            "created_at": NOW,
            "updated_at": NOW
        }))
        .expect("project domain fixture matches the entity")
    }

    fn certificate(domain: &str, encrypted_key: Option<String>) -> domains::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "domain": domain,
            "certificate": "CERT",
            "private_key": encrypted_key,
            "status": "active",
            "is_wildcard": false,
            "verification_method": "http-01",
            "created_at": NOW,
            "updated_at": NOW
        }))
        .expect("domain fixture matches the entity")
    }

    /// Mock results for a table with nothing up to the custom routes.
    fn empty_up_to_custom_routes(db: MockDatabase) -> MockDatabase {
        db.append_query_results([Vec::<(
            environment_domains::Model,
            Option<environments::Model>,
        )>::new()])
            .append_query_results([Vec::<settings::Model>::new()])
            .append_query_results([Vec::<environments::Model>::new()])
    }

    /// Mock results for a table with no routes at all.
    fn empty_tables(db: MockDatabase) -> MockDatabase {
        empty_up_to_custom_routes(db)
            .append_query_results([Vec::<custom_routes::Model>::new()])
            .append_query_results([Vec::<project_custom_domains::Model>::new()])
    }

    fn encoded_public_key() -> String {
        base64::engine::general_purpose::STANDARD.encode([9_u8; 32])
    }

    #[tokio::test]
    async fn a_token_hash_resolves_to_its_node_or_to_nothing() {
        let found = service(mock().append_query_results([vec![node("edge", None)]]));
        assert_eq!(
            found
                .node_for_token_hash("hash")
                .await
                .unwrap()
                .map(|n| n.id),
            Some(5)
        );

        let missing = service(mock().append_query_results([Vec::<nodes::Model>::new()]));
        assert!(missing.node_for_token_hash("hash").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failed_token_lookup_is_typed() {
        let service =
            service(mock().append_query_errors([DbErr::Custom("connection lost".to_string())]));
        assert!(matches!(
            service.node_for_token_hash("hash").await,
            Err(EdgeRouteError::TokenLookup { .. })
        ));
    }

    #[tokio::test]
    async fn an_empty_cluster_has_an_empty_table() {
        let service = service(empty_tables(mock()));
        let table = service.route_table(&node("worker", None)).await.unwrap();
        assert!(table.routes.is_empty());
        assert!(table.certificates.is_none());
    }

    /// Each section of the table has its own domain, and a domain already
    /// routed by an earlier section is not repeated by a later one.
    #[tokio::test]
    async fn the_table_combines_every_source_without_repeating_a_domain() {
        let preview_domain = AppSettings::default().preview_domain;
        let preview_host =
            PublicHostnameStrategy::Standard.environment_hostname(&preview_domain, "preview-env");
        let service = service(
            mock()
                .append_query_results([vec![(
                    environment_domain("app.example.test", 2),
                    Some(environment(2, "app", None)),
                )]])
                .append_query_results([Vec::<settings::Model>::new()])
                .append_query_results([vec![environment(4, "preview-env", None)]])
                .append_query_results([vec![custom_route("*.wild.example.test")]])
                .append_query_results([vec![
                    project_domain("app.example.test"),
                    project_domain("custom.example.test"),
                ]]),
        );

        let table = service.route_table(&node("worker", None)).await.unwrap();

        let domains: Vec<_> = table.routes.iter().map(|r| r.domain.as_str()).collect();
        assert_eq!(
            domains,
            [
                "app.example.test",
                preview_host.as_str(),
                "*.wild.example.test",
                "custom.example.test"
            ]
        );
        assert_eq!(table.routes[0].environment_id, Some(2));
        assert_eq!(table.routes[1].environment_id, Some(4));
        assert!(table.routes[2].is_wildcard);
        assert_eq!(table.routes[2].project_id, None);
        assert!(!table.routes[3].is_wildcard);
    }

    #[tokio::test]
    async fn a_domain_of_a_removed_environment_is_skipped() {
        let service = service(
            mock()
                .append_query_results([vec![(
                    environment_domain("orphan.example.test", 2),
                    None::<environments::Model>,
                )]])
                .append_query_results([Vec::<settings::Model>::new()])
                .append_query_results([Vec::<environments::Model>::new()])
                .append_query_results([Vec::<custom_routes::Model>::new()])
                .append_query_results([Vec::<project_custom_domains::Model>::new()]),
        );
        let table = service.route_table(&node("worker", None)).await.unwrap();
        assert!(table.routes.is_empty());
    }

    /// Each query's failure names what could not be read.
    #[tokio::test]
    async fn each_failed_query_names_its_source() {
        let error = || DbErr::Custom("connection lost".to_string());
        let cases: Vec<(MockDatabase, &str)> = vec![
            (mock().append_query_errors([error()]), "environment domains"),
            (
                mock()
                    .append_query_results([Vec::<(
                        environment_domains::Model,
                        Option<environments::Model>,
                    )>::new()])
                    .append_query_results([Vec::<settings::Model>::new()])
                    .append_query_errors([error()]),
                "environments",
            ),
            (
                empty_up_to_custom_routes(mock()).append_query_errors([error()]),
                "custom routes",
            ),
            (
                empty_up_to_custom_routes(mock())
                    .append_query_results([Vec::<custom_routes::Model>::new()])
                    .append_query_errors([error()]),
                "project custom domains",
            ),
            (
                empty_tables(mock()).append_query_errors([error()]),
                "domain certificates",
            ),
        ];
        for (db, expected) in cases {
            let service = service(db);
            let result = service
                .route_table(&node("edge", Some(encoded_public_key())))
                .await;
            match result {
                Err(EdgeRouteError::Query { what, .. }) => assert_eq!(what, expected),
                Err(other) => panic!("expected a query error for {expected}, got {other:?}"),
                Ok(_) => panic!("expected a query error for {expected}"),
            }
        }
    }

    /// A worker never receives TLS private keys, even with a key on its row;
    /// the mock has no certificate query scripted, so asking would fail.
    #[tokio::test]
    async fn only_edge_nodes_receive_certificates() {
        let service = service(empty_tables(mock()));
        let table = service
            .route_table(&node("worker", Some(encoded_public_key())))
            .await
            .unwrap();
        assert!(table.certificates.is_none());
    }

    #[tokio::test]
    async fn an_edge_node_without_a_key_receives_no_certificates() {
        let service = service(empty_tables(mock()));
        let table = service.route_table(&node("edge", None)).await.unwrap();
        assert!(table.certificates.is_none());
    }

    #[tokio::test]
    async fn a_malformed_edge_key_degrades_to_no_certificates() {
        let service = service(
            empty_tables(mock())
                .append_query_results([vec![certificate("app.example.test", None)]]),
        );
        let table = service
            .route_table(&node("edge", Some("not a key".to_string())))
            .await
            .unwrap();
        assert!(table.certificates.is_none());
    }

    /// Certificates go out encrypted to the node; one whose stored key cannot be
    /// decrypted is left out rather than failing the whole table.
    #[tokio::test]
    async fn certificates_are_encrypted_and_an_undecryptable_one_is_skipped() {
        let encryption =
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap();
        let encrypted_key = encryption.encrypt_string("PRIVATE KEY").unwrap();
        let service = service(empty_tables(mock()).append_query_results([vec![
            certificate("good.example.test", Some(encrypted_key)),
            certificate("broken.example.test", Some("not encrypted".to_string())),
            certificate("no-key.example.test", None),
        ]]));

        let table = service
            .route_table(&node("edge", Some(encoded_public_key())))
            .await
            .unwrap();

        let certificates = table.certificates.expect("one certificate is deliverable");
        assert_eq!(certificates.bundles.len(), 1);
        let bundle = &certificates.bundles[0];
        assert_eq!(bundle.domain, "good.example.test");
        assert_eq!(
            bundle.fingerprint,
            temps_core::ecies::cert_fingerprint("CERT")
        );
        assert!(!bundle.ciphertext.contains("PRIVATE KEY"));
        assert!(!certificates.ephemeral_public_key.is_empty());
    }
}
