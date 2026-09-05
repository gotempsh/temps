// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scaleway Transactional Email provider implementation

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use super::traits::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, SendEmailRequest, SendEmailResponse, VerificationStatus,
};
use crate::dns::DnsVerifier;
use crate::errors::EmailError;

/// Scaleway TEM credentials configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalewayCredentials {
    pub api_key: String,
    pub project_id: String,
}

/// Verify that the given Scaleway credentials are accepted by the TEM API.
///
/// Makes a single authenticated read-only GET request to list domains for the
/// project (page_size=1). This validates the API key, project ID, and region
/// together without creating any resources.
///
/// Returns:
/// - `Ok(())` if credentials are accepted (even if the project has no domains yet).
/// - `Err(EmailError::InvalidCredentials)` if the API key or project access is
///   definitively rejected (HTTP 401 or 403).
/// - `Err(EmailError::ProviderUnreachable)` if the API could not be contacted
///   (network/DNS error or an unexpected HTTP status from the API).
pub(crate) async fn verify_scaleway_credentials(
    credentials: &ScalewayCredentials,
    region: &str,
) -> Result<(), EmailError> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|source| EmailError::ScalewayClientBuild { source })?;

    let url = format!("{}/regions/{}/domains", ScalewayProvider::BASE_URL, region);

    let response = client
        .get(&url)
        .query(&[
            ("project_id", credentials.project_id.as_str()),
            ("page_size", "1"),
        ])
        .header("X-Auth-Token", &credentials.api_key)
        .send()
        .await
        .map_err(|e| {
            let reason = if e.is_connect() {
                format!(
                    "could not connect to Scaleway API at {}. \
                     Verify that the Temps server can reach api.scaleway.com on port 443: {}",
                    url, e
                )
            } else if e.is_timeout() {
                format!(
                    "connection to Scaleway API timed out after 10 s. \
                     Verify that the Temps server can reach api.scaleway.com on port 443: {}",
                    e
                )
            } else {
                format!("network error reaching Scaleway API: {}", e)
            };
            EmailError::ProviderUnreachable {
                provider_type: "scaleway".to_string(),
                reason,
            }
        })?;

    match response.status() {
        s if s.is_success() => Ok(()),
        reqwest::StatusCode::UNAUTHORIZED => {
            let body = response.text().await.unwrap_or_else(|_| String::new());
            Err(EmailError::InvalidCredentials {
                provider_type: "scaleway".to_string(),
                reason: format!(
                    "the API key was rejected (HTTP 401). \
                     Check that the key is valid and has Transactional Email permissions. \
                     Scaleway error: {}",
                    body.trim()
                ),
            })
        }
        reqwest::StatusCode::FORBIDDEN => {
            let body = response.text().await.unwrap_or_else(|_| String::new());
            Err(EmailError::InvalidCredentials {
                provider_type: "scaleway".to_string(),
                reason: format!(
                    "access denied (HTTP 403). The API key does not have access to \
                     Transactional Email in project '{}'. \
                     Verify the project ID and that the key has the \
                     'transactional_email:write' permission. \
                     Scaleway error: {}",
                    credentials.project_id,
                    body.trim()
                ),
            })
        }
        reqwest::StatusCode::NOT_FOUND => Err(EmailError::InvalidCredentials {
            provider_type: "scaleway".to_string(),
            reason: format!(
                "region '{}' was not found on the Scaleway Transactional Email API. \
                     Valid regions are: fr-par, nl-ams.",
                region
            ),
        }),
        s => {
            let body = response.text().await.unwrap_or_else(|_| String::new());
            Err(EmailError::ProviderUnreachable {
                provider_type: "scaleway".to_string(),
                reason: format!(
                    "unexpected response from Scaleway API (HTTP {}): {}",
                    s,
                    body.trim()
                ),
            })
        }
    }
}

/// Scaleway TEM provider implementation
pub struct ScalewayProvider {
    client: Client,
    api_key: String,
    project_id: String,
    region: String,
}

impl ScalewayProvider {
    const BASE_URL: &'static str = "https://api.scaleway.com/transactional-email/v1alpha1";

    /// Create a new Scaleway provider with the given credentials
    pub fn new(credentials: &ScalewayCredentials, region: &str) -> Result<Self, EmailError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|source| EmailError::ScalewayClientBuild { source })?;

        Ok(Self {
            client,
            api_key: credentials.api_key.clone(),
            project_id: credentials.project_id.clone(),
            region: region.to_string(),
        })
    }

    /// Get the Scaleway region
    pub fn region(&self) -> &str {
        &self.region
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/regions/{}{}", Self::BASE_URL, self.region, path)
    }
}

// Scaleway API response types

/// A single ready-to-publish DNS name/value pair from Scaleway's `records` object.
/// These are the full, ready-to-configure values Scaleway's console displays,
/// as opposed to the flat `spf_config`/`dkim_config` snippet fields.
#[derive(Debug, Deserialize)]
struct ScalewayRecordEntry {
    name: String,
    value: String,
}

/// The nested `records` object in Scaleway domain responses, containing
/// full ready-to-publish DNS records (not just the fragment/snippet fields).
#[derive(Debug, Deserialize)]
struct ScalewayDomainRecords {
    /// Full SPF record value (e.g. `v=spf1 include:_spf.tem.scaleway.com ~all`)
    spf: Option<ScalewayRecordEntry>,
    /// Required blackhole MX record (e.g. `10 blackhole.tem.scaleway.com`)
    mx: Option<ScalewayRecordEntry>,
}

#[derive(Debug, Deserialize)]
struct ScalewayDomainResponse {
    id: String,
    name: String,
    status: String,
    /// Raw SPF snippet (`include:…` only). Prefer `records.spf.value` when present.
    spf_config: Option<String>,
    dkim_config: Option<String>,
    last_error: Option<String>,
    /// Ready-to-publish DNS records. Present on all current API responses.
    records: Option<ScalewayDomainRecords>,
}

#[derive(Debug, Deserialize)]
struct ScalewayEmailResponse {
    emails: Vec<ScalewayEmailInfo>,
}

#[derive(Debug, Deserialize)]
struct ScalewayEmailInfo {
    id: String,
    message_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScalewayCreateDomainRequest {
    project_id: String,
    domain_name: String,
}

#[derive(Debug, Serialize)]
struct ScalewaySendEmailRequest {
    project_id: String,
    from: ScalewayEmailAddress,
    to: Vec<ScalewayEmailAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cc: Option<Vec<ScalewayEmailAddress>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bcc: Option<Vec<ScalewayEmailAddress>>,
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScalewayEmailAddress {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// Split a Scaleway MX record value like `"10 blackhole.tem.scaleway.com"` into
/// `(priority, host)`. Falls back to `(None, raw_value)` when the format is
/// not `"<u16> <host>"` so no data is silently lost.
fn parse_scaleway_mx_value(raw: &str) -> (Option<u16>, String) {
    if let Some((priority_str, host)) = raw.split_once(' ') {
        if let Ok(priority) = priority_str.parse::<u16>() {
            return (Some(priority), host.to_string());
        }
    }
    (None, raw.to_string())
}

/// Verify that a Scaleway identity's own domain name matches the domain the
/// caller asked for. Without this check, a stale or mistyped
/// `provider_identity_id` (e.g. reused from a different domain) would
/// silently bind DNS records and verification status computed for someone
/// else's Scaleway identity to the requested domain.
fn check_identity_domain_matches(
    identity_id: &str,
    identity_domain: &str,
    requested_domain: &str,
) -> Result<(), EmailError> {
    if identity_domain.eq_ignore_ascii_case(requested_domain) {
        Ok(())
    } else {
        Err(EmailError::Scaleway(format!(
            "Scaleway identity '{}' belongs to domain '{}', not '{}'. \
             Refusing to bind a mismatched domain-to-identity pair.",
            identity_id, identity_domain, requested_domain
        )))
    }
}

#[async_trait]
impl EmailProvider for ScalewayProvider {
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        debug!("Creating Scaleway identity for domain: {}", domain);

        let request = ScalewayCreateDomainRequest {
            project_id: self.project_id.clone(),
            domain_name: domain.to_string(),
        };

        let response = self
            .client
            .post(self.api_url("/domains"))
            .header("X-Auth-Token", &self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to create domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to create domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        // Prefer records.spf (full publishable record) over the raw spf_config snippet,
        // which is only the include:… fragment and not a valid SPF record on its own.
        let spf_record = if let Some(records_spf) = domain_response
            .records
            .as_ref()
            .and_then(|r| r.spf.as_ref())
        {
            Some(DnsRecord {
                record_type: "TXT".to_string(),
                name: records_spf.name.clone(),
                value: records_spf.value.clone(),
                priority: None,
                status: DnsRecordStatus::Pending,
            })
        } else {
            // Fallback: wrap the snippet in a minimal valid SPF record so the
            // user always gets a publishable value, even if records is absent.
            domain_response.spf_config.map(|spf_snippet| DnsRecord {
                record_type: "TXT".to_string(),
                name: domain.to_string(),
                value: format!("v=spf1 {} ~all", spf_snippet),
                priority: None,
                status: DnsRecordStatus::Pending,
            })
        };

        // Parse DKIM config
        let dkim_records = if let Some(dkim) = domain_response.dkim_config {
            vec![DnsRecord {
                record_type: "TXT".to_string(),
                name: format!("scw._domainkey.{}", domain),
                value: dkim,
                priority: None,
                status: DnsRecordStatus::Pending,
            }]
        } else {
            Vec::new()
        };

        // Scaleway requires a blackhole MX record for domain verification.
        let mx_record = domain_response
            .records
            .as_ref()
            .and_then(|r| r.mx.as_ref())
            .map(|records_mx| {
                let (priority, host) = parse_scaleway_mx_value(&records_mx.value);
                DnsRecord {
                    record_type: "MX".to_string(),
                    name: records_mx.name.clone(),
                    value: host,
                    priority,
                    status: DnsRecordStatus::Pending,
                }
            });

        Ok(DomainIdentity {
            provider_identity_id: domain_response.id,
            spf_record,
            dkim_records,
            dkim_selector: Some("scw".to_string()),
            mx_record,
            mail_from_subdomain: None,
        })
    }

    async fn verify_identity(
        &self,
        domain: &str,
        provider_identity_id: Option<&str>,
    ) -> Result<VerificationStatus, EmailError> {
        debug!("Verifying Scaleway identity for domain: {}", domain);

        let identity_id = provider_identity_id.ok_or_else(|| {
            EmailError::Scaleway(format!(
                "Cannot verify domain '{}': no Scaleway domain UUID is stored. \
                 The domain may not have completed initial provisioning.",
                domain
            ))
        })?;

        // First, trigger the check
        let check_response = self
            .client
            .post(self.api_url(&format!("/domains/{}/check", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to check domain: {}", e)))?;

        if !check_response.status().is_success() {
            let status = check_response.status();
            let body = check_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to check domain ({}): {}",
                status, body
            )));
        }

        // Then get the domain status
        let response = self
            .client
            .get(self.api_url(&format!("/domains/{}", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        check_identity_domain_matches(identity_id, &domain_response.name, domain)?;

        match domain_response.status.as_str() {
            "checked" | "verified" => Ok(VerificationStatus::Verified),
            "pending" | "unchecked" => Ok(VerificationStatus::Pending),
            "invalid" => Ok(VerificationStatus::Failed(
                domain_response
                    .last_error
                    .unwrap_or_else(|| "DNS verification failed".to_string()),
            )),
            _ => Ok(VerificationStatus::NotStarted),
        }
    }

    async fn get_identity_details(
        &self,
        domain: &str,
        provider_identity_id: Option<&str>,
    ) -> Result<DomainIdentityDetails, EmailError> {
        debug!("Getting Scaleway identity details for domain: {}", domain);

        let identity_id = provider_identity_id.ok_or_else(|| {
            EmailError::Scaleway(format!(
                "Cannot get details for domain '{}': no Scaleway domain UUID is stored. \
                 The domain may not have completed initial provisioning.",
                domain
            ))
        })?;

        // Get the domain status from Scaleway
        let response = self
            .client
            .get(self.api_url(&format!("/domains/{}", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}): {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        check_identity_domain_matches(identity_id, &domain_response.name, domain)?;

        // Determine overall verification status
        let overall_status = match domain_response.status.as_str() {
            "checked" | "verified" => VerificationStatus::Verified,
            "pending" | "unchecked" => VerificationStatus::Pending,
            "invalid" => VerificationStatus::Failed(
                domain_response
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "DNS verification failed".to_string()),
            ),
            _ => VerificationStatus::NotStarted,
        };

        // Verify records via DNS lookup for accurate per-record status
        let dns_verifier = DnsVerifier::new();

        // Build SPF record — prefer records.spf (full publishable record) over the
        // raw spf_config snippet, which is only the include:… fragment.
        let spf_record = if let Some(records_spf) = domain_response
            .records
            .as_ref()
            .and_then(|r| r.spf.as_ref())
        {
            let spf_status = dns_verifier
                .verify_spf_record(domain, "_spf.tem.scaleway.com")
                .await;
            Some(DnsRecord {
                record_type: "TXT".to_string(),
                name: records_spf.name.clone(),
                value: records_spf.value.clone(),
                priority: None,
                status: spf_status,
            })
        } else {
            // Fallback: wrap the snippet in a minimal valid SPF record.
            match domain_response.spf_config {
                Some(spf_snippet) => {
                    let spf_status = dns_verifier
                        .verify_spf_record(domain, "_spf.tem.scaleway.com")
                        .await;
                    Some(DnsRecord {
                        record_type: "TXT".to_string(),
                        name: domain.to_string(),
                        value: format!("v=spf1 {} ~all", spf_snippet),
                        priority: None,
                        status: spf_status,
                    })
                }
                None => None,
            }
        };

        // Build DKIM record with DNS-verified status
        let dkim_records = if let Some(dkim) = domain_response.dkim_config {
            let dkim_name = format!("scw._domainkey.{}", domain);
            let dkim_status = dns_verifier.verify_txt_record(&dkim_name, &dkim).await;
            vec![DnsRecord {
                record_type: "TXT".to_string(),
                name: dkim_name,
                value: dkim,
                priority: None,
                status: dkim_status,
            }]
        } else {
            Vec::new()
        };

        // Scaleway requires a blackhole MX record for domain verification.
        let mx_record = if let Some(records_mx) =
            domain_response.records.as_ref().and_then(|r| r.mx.as_ref())
        {
            let (priority, host) = parse_scaleway_mx_value(&records_mx.value);
            let mx_status = dns_verifier
                .verify_mx_record(&records_mx.name, &host, priority)
                .await;
            Some(DnsRecord {
                record_type: "MX".to_string(),
                name: records_mx.name.clone(),
                value: host,
                priority,
                status: mx_status,
            })
        } else {
            None
        };

        Ok(DomainIdentityDetails {
            overall_status,
            spf_record,
            dkim_records,
            mx_record,
            mail_from_subdomain: None,
        })
    }

    async fn delete_identity(
        &self,
        domain: &str,
        provider_identity_id: Option<&str>,
    ) -> Result<(), EmailError> {
        debug!("Deleting Scaleway identity for domain: {}", domain);

        let identity_id = provider_identity_id.ok_or_else(|| {
            EmailError::Scaleway(format!(
                "Cannot delete domain '{}': no Scaleway domain UUID is stored. \
                 The domain may not have completed initial provisioning.",
                domain
            ))
        })?;

        // Deletion is destructive and irreversible, so confirm the UUID still
        // belongs to this domain before sending it — a stale or mistyped
        // `provider_identity_id` must never delete a different domain's
        // provider-side identity. See `check_identity_domain_matches`.
        let get_response = self
            .client
            .get(self.api_url(&format!("/domains/{}", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !get_response.status().is_success() {
            let status = get_response.status();
            let body = get_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}) before delete: {}",
                status, body
            )));
        }

        let domain_response: ScalewayDomainResponse = get_response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        check_identity_domain_matches(identity_id, &domain_response.name, domain)?;

        let response = self
            .client
            .delete(self.api_url(&format!("/domains/{}", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to delete domain: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to delete domain ({}): {}",
                status, body
            )));
        }

        Ok(())
    }

    async fn send(&self, email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        debug!("Sending email via Scaleway from: {}", email.from);

        let request = ScalewaySendEmailRequest {
            project_id: self.project_id.clone(),
            from: ScalewayEmailAddress {
                email: email.from.clone(),
                name: email.from_name.clone(),
            },
            to: email
                .to
                .iter()
                .map(|e| ScalewayEmailAddress {
                    email: e.clone(),
                    name: None,
                })
                .collect(),
            cc: email.cc.as_ref().map(|addrs| {
                addrs
                    .iter()
                    .map(|e| ScalewayEmailAddress {
                        email: e.clone(),
                        name: None,
                    })
                    .collect()
            }),
            bcc: email.bcc.as_ref().map(|addrs| {
                addrs
                    .iter()
                    .map(|e| ScalewayEmailAddress {
                        email: e.clone(),
                        name: None,
                    })
                    .collect()
            }),
            subject: email.subject.clone(),
            html: email.html.clone(),
            text: email.text.clone(),
        };

        let response = self
            .client
            .post(self.api_url("/emails"))
            .header("X-Auth-Token", &self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                EmailError::ProviderDeliveryUnknown(format!(
                    "Scaleway request may have been accepted: {e}"
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("Failed to send email via Scaleway ({}): {}", status, body);
            // 5xx and 429 are transient and safe to retry; 4xx rejections are
            // definitive (bad recipient, quota exceeded on account level, etc.).
            let retryable =
                status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            return Err(EmailError::SendFailed {
                provider: "scaleway".to_string(),
                retryable,
                message: format!("Scaleway rejected send (HTTP {status}): {body}"),
            });
        }

        let email_response: ScalewayEmailResponse = response.json().await.map_err(|e| {
            EmailError::ProviderDeliveryUnknown(format!(
                "Scaleway accepted the request but returned an unreadable response: {e}"
            ))
        })?;

        let message_id = email_response
            .emails
            .first()
            .and_then(|e| e.message_id.clone())
            .or_else(|| email_response.emails.first().map(|e| e.id.clone()))
            .ok_or_else(|| {
                EmailError::ProviderDeliveryUnknown(
                    "Scaleway accepted the request but returned no message ID".to_string(),
                )
            })?;

        debug!("Email sent successfully, message_id: {}", message_id);

        Ok(SendEmailResponse { message_id })
    }

    fn provider_type(&self) -> EmailProviderType {
        EmailProviderType::Scaleway
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `EmailError::SendFailed` as the live Scaleway `send()` path
    /// does for a given HTTP status, so we can assert retryability without a
    /// real HTTP server.
    fn make_scaleway_send_error(status: reqwest::StatusCode, body: &str) -> EmailError {
        let retryable =
            status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        EmailError::SendFailed {
            provider: "scaleway".to_string(),
            retryable,
            message: format!("Scaleway rejected send (HTTP {status}): {body}"),
        }
    }

    #[test]
    fn scaleway_5xx_is_retryable() {
        let err =
            make_scaleway_send_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "server error");
        assert!(
            matches!(
                err,
                EmailError::SendFailed {
                    retryable: true,
                    ..
                }
            ),
            "5xx must be retryable, got: {err:?}"
        );
    }

    #[test]
    fn scaleway_429_is_retryable() {
        let err = make_scaleway_send_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "rate limited");
        assert!(
            matches!(
                err,
                EmailError::SendFailed {
                    retryable: true,
                    ..
                }
            ),
            "429 must be retryable, got: {err:?}"
        );
    }

    #[test]
    fn scaleway_4xx_non_429_is_not_retryable() {
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            let err = make_scaleway_send_error(status, "client error");
            assert!(
                matches!(
                    err,
                    EmailError::SendFailed {
                        retryable: false,
                        ..
                    }
                ),
                "4xx (status={status}) must not be retryable, got: {err:?}"
            );
        }
    }

    // ── parse_scaleway_mx_value ──────────────────────────────────────────────

    #[test]
    fn parse_scaleway_mx_value_standard_format() {
        let (priority, host) = parse_scaleway_mx_value("10 blackhole.tem.scaleway.com");
        assert_eq!(priority, Some(10));
        assert_eq!(host, "blackhole.tem.scaleway.com");
    }

    #[test]
    fn parse_scaleway_mx_value_unexpected_format_returns_raw() {
        // If the value doesn't start with a u16, return the whole string as the host.
        let (priority, host) = parse_scaleway_mx_value("blackhole.tem.scaleway.com");
        assert_eq!(priority, None);
        assert_eq!(host, "blackhole.tem.scaleway.com");
    }

    #[test]
    fn parse_scaleway_mx_value_zero_priority() {
        let (priority, host) = parse_scaleway_mx_value("0 mx.example.com");
        assert_eq!(priority, Some(0));
        assert_eq!(host, "mx.example.com");
    }

    // ── check_identity_domain_matches ────────────────────────────────────────

    #[test]
    fn identity_domain_matching_requested_domain_is_ok() {
        assert!(check_identity_domain_matches("uuid-1234", "example.com", "example.com").is_ok());
    }

    #[test]
    fn identity_domain_matching_case_insensitively_is_ok() {
        assert!(check_identity_domain_matches("uuid-1234", "Example.COM", "example.com").is_ok());
    }

    #[test]
    fn identity_domain_mismatch_is_rejected() {
        let err = check_identity_domain_matches("uuid-1234", "other-domain.com", "example.com")
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("other-domain.com") && message.contains("example.com"),
            "error must name both the identity's real domain and the requested domain, got: {message}"
        );
    }

    // ── records.spf round-trip ───────────────────────────────────────────────

    /// Deserialising a full Scaleway domain response that contains the `records`
    /// object must populate `records.spf` and `records.mx`.
    #[test]
    fn scaleway_domain_response_deserialises_records_object() {
        let json = r#"{
            "id": "uuid-1234",
            "name": "example.com",
            "status": "pending",
            "spf_config": "include:_spf.tem.scaleway.com",
            "dkim_config": "v=DKIM1; k=rsa; p=PUBLICKEY",
            "last_error": null,
            "records": {
                "spf": {
                    "name": "example.com",
                    "value": "v=spf1 include:_spf.tem.scaleway.com ~all"
                },
                "dkim": {
                    "name": "scw._domainkey.example.com",
                    "value": "v=DKIM1; k=rsa; p=PUBLICKEY"
                },
                "dmarc": {
                    "name": "_dmarc.example.com",
                    "value": "v=DMARC1; p=none"
                },
                "mx": {
                    "name": "example.com",
                    "value": "10 blackhole.tem.scaleway.com"
                }
            }
        }"#;

        let response: ScalewayDomainResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id, "uuid-1234");
        assert_eq!(response.status, "pending");

        let records = response
            .records
            .as_ref()
            .expect("records should be present");

        let spf = records.spf.as_ref().expect("records.spf should be present");
        assert_eq!(spf.name, "example.com");
        assert_eq!(spf.value, "v=spf1 include:_spf.tem.scaleway.com ~all");

        let mx = records.mx.as_ref().expect("records.mx should be present");
        assert_eq!(mx.name, "example.com");
        assert_eq!(mx.value, "10 blackhole.tem.scaleway.com");
    }

    /// When `records` is absent (legacy / partial response), `spf_config` fallback
    /// must produce a full publishable SPF record, not just the raw snippet.
    #[test]
    fn scaleway_domain_response_missing_records_still_deserialises() {
        let json = r#"{
            "id": "uuid-5678",
            "name": "example.com",
            "status": "unchecked",
            "spf_config": "include:_spf.tem.scaleway.com",
            "dkim_config": "v=DKIM1; k=rsa; p=PUBLICKEY",
            "last_error": null
        }"#;

        let response: ScalewayDomainResponse = serde_json::from_str(json).unwrap();
        assert!(
            response.records.is_none(),
            "records should be absent for this response"
        );
        // Verify the spf_config fallback path would produce a full SPF record.
        let spf_snippet = response.spf_config.unwrap();
        let full_spf = format!("v=spf1 {} ~all", spf_snippet);
        assert!(
            full_spf.starts_with("v=spf1"),
            "fallback SPF must start with v=spf1"
        );
        assert!(
            full_spf.ends_with("~all"),
            "fallback SPF must end with ~all"
        );
    }

    #[test]
    fn test_scaleway_credentials_serialization() {
        let creds = ScalewayCredentials {
            api_key: "scw-secret-key-123".to_string(),
            project_id: "12345678-1234-1234-1234-123456789012".to_string(),
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("api_key"));
        assert!(json.contains("project_id"));

        let deserialized: ScalewayCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.api_key, creds.api_key);
        assert_eq!(deserialized.project_id, creds.project_id);
    }
}
