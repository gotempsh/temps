//! Pebble challtestsrv-backed DNS provider (LOCAL DEV/TEST ONLY)
//!
//! Publishes DNS-01 challenge TXT records to `pebble-challtestsrv`'s
//! management API instead of a real registrar, so DNS-01 auto-renewal
//! (`TlsService::try_dns01_renewal_with_provider`) can be exercised end to
//! end against a local Pebble ACME server -- no real domain or DNS account
//! required. `pebble-challtestsrv` ships alongside Pebble in
//! `ghcr.io/letsencrypt/pebble-challtestsrv` and exposes a write-only
//! management API (`/set-txt`, `/clear-txt`) on port 8055 by default.
//!
//! NEVER point this at anything other than a local Pebble test setup: it
//! has no zone-ownership model (`can_manage_domain` always returns `true`)
//! and no authentication.

use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

use super::credentials::PebbleCredentials;
use super::traits::{
    DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
    DnsRecordRequest, DnsRecordType, DnsZone,
};
use crate::errors::DnsError;

#[derive(Serialize)]
struct SetTxtRequest {
    host: String,
    value: String,
}

#[derive(Serialize)]
struct ClearTxtRequest {
    host: String,
}

/// DNS provider backed by `pebble-challtestsrv`'s mock DNS server.
pub struct PebbleDnsProvider {
    client: Client,
    /// Base URL of `pebble-challtestsrv`'s management API, e.g.
    /// `http://localhost:8055`.
    management_url: String,
}

/// Env var that must be set to `1` for this provider to be constructible.
///
/// `PebbleDnsProvider` has no authentication and, unlike every other
/// provider in this crate, is intentionally allowed to target loopback and
/// private addresses -- it exists solely to drive a local Pebble test
/// instance. Gating it behind an explicit opt-in keeps it out of production
/// instances by default (mirrors the existing `ACME_INSECURE` precedent).
const ALLOW_PEBBLE_PROVIDER_ENV: &str = "TEMPS_ALLOW_PEBBLE_PROVIDER";

impl PebbleDnsProvider {
    pub fn new(credentials: PebbleCredentials) -> Result<Self, DnsError> {
        if std::env::var(ALLOW_PEBBLE_PROVIDER_ENV).as_deref() != Ok("1") {
            return Err(DnsError::Validation(format!(
                "PebbleDnsProvider is disabled: it has no authentication and is intended only \
                 for local ACME testing against a Pebble instance. Set {}=1 to enable it.",
                ALLOW_PEBBLE_PROVIDER_ENV
            )));
        }

        let management_url = credentials.management_url.trim_end_matches('/').to_string();

        temps_core::url_validation::validate_loopback_or_private_url(&management_url).map_err(
            |e| {
                DnsError::Validation(format!(
                    "Invalid Pebble management_url: {} (must be a loopback or private address -- \
                     this provider only ever talks to a local Pebble instance)",
                    e
                ))
            },
        )?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| DnsError::ApiError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            client,
            management_url,
        })
    }

    /// challtestsrv addresses records by fully-qualified, dot-terminated
    /// host, e.g. `_acme-challenge.example.com.`. `name` is the record name
    /// relative to `domain` (`"_acme-challenge"` or `"@"` for the apex).
    fn fqdn(domain: &str, name: &str) -> String {
        let host = if name.is_empty() || name == "@" {
            domain.to_string()
        } else {
            format!("{}.{}", name, domain)
        };
        format!("{}.", host)
    }

    async fn set_txt(&self, host: &str, value: &str) -> Result<(), DnsError> {
        let response = self
            .client
            .post(format!("{}/set-txt", self.management_url))
            .json(&SetTxtRequest {
                host: host.to_string(),
                value: value.to_string(),
            })
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::debug!(
                "challtestsrv /set-txt for host '{}' returned status {}: {}",
                host,
                status,
                body.chars().take(500).collect::<String>()
            );
            return Err(DnsError::ApiError(format!(
                "challtestsrv /set-txt failed for host '{}' with status {}",
                host, status
            )));
        }
        Ok(())
    }

    async fn clear_txt(&self, host: &str) -> Result<(), DnsError> {
        let response = self
            .client
            .post(format!("{}/clear-txt", self.management_url))
            .json(&ClearTxtRequest {
                host: host.to_string(),
            })
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::debug!(
                "challtestsrv /clear-txt for host '{}' returned status {}: {}",
                host,
                status,
                body.chars().take(500).collect::<String>()
            );
            return Err(DnsError::ApiError(format!(
                "challtestsrv /clear-txt failed for host '{}' with status {}",
                host, status
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl DnsProvider for PebbleDnsProvider {
    fn provider_type(&self) -> DnsProviderType {
        DnsProviderType::Pebble
    }

    fn capabilities(&self) -> DnsProviderCapabilities {
        DnsProviderCapabilities {
            txt_record: true,
            wildcard: true,
            ..Default::default()
        }
    }

    async fn test_connection(&self) -> Result<bool, DnsError> {
        // management_url's literal-IP case was already validated synchronously
        // in new(); a hostname needs its resolved IPs checked here, same as
        // validate_external_url/validate_domain_async's split in temps-webhooks.
        if let Ok(parsed) = reqwest::Url::parse(&self.management_url) {
            if let Some(host) = parsed.host_str() {
                if host.parse::<std::net::IpAddr>().is_err() {
                    temps_core::url_validation::validate_loopback_or_private_domain_async(host)
                        .await
                        .map_err(|e| {
                            DnsError::Validation(format!(
                                "Pebble management_url host '{}' failed validation: {}",
                                host, e
                            ))
                        })?;
                }
            }
        }

        // challtestsrv has no health endpoint; a throwaway set-txt/clear-txt
        // round trip is the simplest liveness probe available.
        let probe_host = "_temps-pebble-probe.invalid.";
        self.set_txt(probe_host, "probe").await?;
        self.clear_txt(probe_host).await?;
        Ok(true)
    }

    async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
        // challtestsrv has no concept of zones -- it answers authoritatively
        // for any hostname it has been told a record for.
        Ok(vec![])
    }

    async fn get_zone(&self, domain: &str) -> Result<Option<DnsZone>, DnsError> {
        Ok(Some(DnsZone {
            id: domain.to_string(),
            name: domain.to_string(),
            status: "active".to_string(),
            nameservers: vec![],
            metadata: Default::default(),
        }))
    }

    async fn can_manage_domain(&self, _domain: &str) -> bool {
        // No zone-ownership model -- challtestsrv will serve records for any
        // hostname it's told about. Restrict use to local Pebble testing.
        true
    }

    async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
        // challtestsrv's management API is write-only (set/clear); it has no
        // "list" endpoint, so this always reports empty. remove_record() is
        // overridden below rather than relying on the trait's default
        // list-then-delete implementation, which would be a no-op here.
        Ok(vec![])
    }

    async fn get_record(
        &self,
        _domain: &str,
        _name: &str,
        _record_type: DnsRecordType,
    ) -> Result<Option<DnsRecord>, DnsError> {
        Ok(None)
    }

    async fn create_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        let DnsRecordContent::TXT { content } = &request.content else {
            return Err(DnsError::NotSupported(
                "PebbleDnsProvider only supports TXT records (ACME DNS-01 challenges)".to_string(),
            ));
        };

        let fqdn = Self::fqdn(domain, &request.name);
        self.set_txt(&fqdn, content).await?;

        Ok(DnsRecord {
            id: Some(fqdn.clone()),
            zone: domain.to_string(),
            name: request.name,
            fqdn,
            content: request.content,
            ttl: request.ttl.unwrap_or(60),
            proxied: false,
            metadata: Default::default(),
        })
    }

    async fn update_record(
        &self,
        domain: &str,
        _record_id: &str,
        request: DnsRecordRequest,
    ) -> Result<DnsRecord, DnsError> {
        // challtestsrv's /set-txt overwrites the value for a host in place,
        // so "update" is identical to "create".
        self.create_record(domain, request).await
    }

    async fn delete_record(&self, _domain: &str, record_id: &str) -> Result<(), DnsError> {
        // `record_id` is the FQDN we minted in create_record().
        self.clear_txt(record_id).await
    }

    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        if record_type != DnsRecordType::TXT {
            return Ok(());
        }
        // The trait's default remove_record() lists then deletes by ID, but
        // list_records() always returns empty here (challtestsrv has no read
        // API) -- clear directly by the FQDN we'd have created instead.
        let fqdn = Self::fqdn(domain, name);
        self.clear_txt(&fqdn).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn fqdn_appends_dot_and_joins_name() {
        assert_eq!(
            PebbleDnsProvider::fqdn("example.com", "_acme-challenge"),
            "_acme-challenge.example.com."
        );
    }

    #[test]
    fn fqdn_treats_apex_marker_as_bare_domain() {
        assert_eq!(PebbleDnsProvider::fqdn("example.com", "@"), "example.com.");
        assert_eq!(PebbleDnsProvider::fqdn("example.com", ""), "example.com.");
    }

    // These tests mutate the process-wide TEMPS_ALLOW_PEBBLE_PROVIDER env var,
    // so they (and integration_tests, which also constructs providers) must
    // not run concurrently with each other.
    #[test]
    #[serial(pebble_env_gate)]
    fn new_rejects_when_gate_env_unset() {
        std::env::remove_var(ALLOW_PEBBLE_PROVIDER_ENV);
        let result = PebbleDnsProvider::new(PebbleCredentials {
            management_url: "http://localhost:8055".to_string(),
        });
        assert!(matches!(result, Err(DnsError::Validation(_))));
    }

    #[test]
    #[serial(pebble_env_gate)]
    fn new_rejects_public_management_url_even_when_gated() {
        std::env::set_var(ALLOW_PEBBLE_PROVIDER_ENV, "1");
        let result = PebbleDnsProvider::new(PebbleCredentials {
            management_url: "http://93.184.216.34:8055".to_string(),
        });
        std::env::remove_var(ALLOW_PEBBLE_PROVIDER_ENV);
        assert!(matches!(result, Err(DnsError::Validation(_))));
    }

    #[test]
    #[serial(pebble_env_gate)]
    fn new_accepts_loopback_url_when_gated() {
        std::env::set_var(ALLOW_PEBBLE_PROVIDER_ENV, "1");
        let result = PebbleDnsProvider::new(PebbleCredentials {
            management_url: "http://localhost:8055".to_string(),
        });
        std::env::remove_var(ALLOW_PEBBLE_PROVIDER_ENV);
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serial_test::serial;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider_for(mock_server: &MockServer) -> PebbleDnsProvider {
        // Safety gate is opt-in (see ALLOW_PEBBLE_PROVIDER_ENV); these tests
        // exercise the provider directly against a loopback wiremock server,
        // which is exactly the intended use, so enable it for the process.
        std::env::set_var(ALLOW_PEBBLE_PROVIDER_ENV, "1");
        PebbleDnsProvider::new(PebbleCredentials {
            management_url: mock_server.uri(),
        })
        .unwrap()
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn create_record_publishes_txt_via_set_txt() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/set-txt"))
            .and(body_json(serde_json::json!({
                "host": "_acme-challenge.example.com.",
                "value": "token-value"
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let provider = provider_for(&mock_server);
        let record = provider
            .create_record(
                "example.com",
                DnsRecordRequest {
                    name: "_acme-challenge".to_string(),
                    content: DnsRecordContent::TXT {
                        content: "token-value".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await
            .unwrap();

        assert_eq!(record.fqdn, "_acme-challenge.example.com.");
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn create_record_rejects_non_txt_content() {
        let mock_server = MockServer::start().await;
        let provider = provider_for(&mock_server);

        let result = provider
            .create_record(
                "example.com",
                DnsRecordRequest {
                    name: "www".to_string(),
                    content: DnsRecordContent::A {
                        address: "192.0.2.1".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await;

        assert!(matches!(result, Err(DnsError::NotSupported(_))));
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn remove_record_calls_clear_txt_directly_without_listing() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/clear-txt"))
            .and(body_json(serde_json::json!({
                "host": "_acme-challenge.example.com."
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let provider = provider_for(&mock_server);
        provider
            .remove_record("example.com", "_acme-challenge", DnsRecordType::TXT)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn remove_record_is_noop_for_non_txt_types() {
        let mock_server = MockServer::start().await;
        // No mock registered for /clear-txt -- if the provider called it,
        // wiremock would return a 404 and this test would fail.
        let provider = provider_for(&mock_server);

        provider
            .remove_record("example.com", "www", DnsRecordType::A)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn set_txt_error_response_surfaces_as_api_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/set-txt"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock_server)
            .await;

        let provider = provider_for(&mock_server);
        let result = provider
            .create_record(
                "example.com",
                DnsRecordRequest {
                    name: "_acme-challenge".to_string(),
                    content: DnsRecordContent::TXT {
                        content: "v".to_string(),
                    },
                    ttl: None,
                    proxied: false,
                },
            )
            .await;

        assert!(matches!(result, Err(DnsError::ApiError(_))));
    }

    #[tokio::test]
    #[serial(pebble_env_gate)]
    async fn can_manage_domain_always_true() {
        let mock_server = MockServer::start().await;
        let provider = provider_for(&mock_server);
        assert!(provider.can_manage_domain("anything.example").await);
    }
}
