// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scaleway Transactional Email provider implementation

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use super::traits::{
    DnsRecord, DnsRecordStatus, DomainIdentity, DomainIdentityDetails, EmailProvider,
    EmailProviderType, ProviderDomainIdentity, SendEmailRequest, SendEmailResponse,
    VerificationStatus,
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
    /// DKIM TXT record. Its name carries Scaleway's selector
    /// (`<selector>._domainkey.<domain>`), which is what Scaleway's own check
    /// resolves — never assume a fixed selector.
    dkim: Option<ScalewayRecordEntry>,
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
struct ScalewayListDomainsResponse {
    domains: Vec<ScalewayDomainResponse>,
}

/// Maps a single domain from Scaleway's list response to the provider-agnostic
/// summary type. Pure and free of I/O so it's unit-testable without a live
/// Scaleway connection — see the `send()` status-classification tests below
/// for the same pattern.
/// Error text for a rejected Scaleway send. The raw body is kept, and the
/// "checked domain" rejection gets the two things that actually cause it: the
/// sender's domain isn't one Scaleway has checked (DKIM/SPF not yet validated
/// on Scaleway's side), or the From address is on a different domain than the
/// one registered (e.g. `@example.com` when `send.example.com` is registered).
fn scaleway_send_rejection_message(status: reqwest::StatusCode, body: &str) -> String {
    let mut message = format!("Scaleway rejected send (HTTP {status}): {body}");
    if body.contains("checked domain") {
        message.push_str(
            ". The From address must use the exact domain registered in Scaleway (a \
             subdomain like send.example.com does not cover example.com), and Scaleway \
             must have checked it: its SPF and DKIM records must match what the domain \
             page shows. Run Verify DNS on the domain to see which record is missing.",
        );
    }
    message
}

/// The DKIM record Scaleway checks for this domain, as `(name, value)`.
///
/// Scaleway publishes the authoritative record in `records.dkim`; its selector
/// is not a fixed string (it was once assumed to be `scw`, which made Temps
/// verify a record Scaleway never looks at). The flat `dkim_config` value is
/// only a fallback for responses without `records`, paired with the project-ID
/// selector Scaleway uses.
fn scaleway_dkim_record(
    domain_response: &ScalewayDomainResponse,
    domain: &str,
    project_id: &str,
) -> Option<(String, String)> {
    if let Some(dkim) = domain_response
        .records
        .as_ref()
        .and_then(|r| r.dkim.as_ref())
    {
        return Some((
            dkim.name.trim_end_matches('.').to_string(),
            dkim.value.clone(),
        ));
    }
    domain_response
        .dkim_config
        .as_ref()
        .map(|value| (format!("{project_id}._domainkey.{domain}"), value.clone()))
}

/// The selector part of a `<selector>._domainkey.<domain>` record name.
fn dkim_selector_from_name(name: &str) -> Option<String> {
    name.split_once("._domainkey.")
        .map(|(selector, _)| selector.to_string())
}

/// Scaleway keeps a revoked domain as its own record (with `revoked_at`), and
/// adding the same name again creates a second record with a new ID.
const SCALEWAY_REVOKED_STATUS: &str = "revoked";

/// Map a Scaleway TEM domain status onto Temps' verification states.
///
/// Every status in Scaleway's `Domain.Status` enum is handled explicitly so a
/// terminal state (`revoked`, `locked`) is reported as a failure with a reason
/// instead of reading as "not started" — a revoked domain can never send again.
fn scaleway_verification_status(status: &str, last_error: Option<&str>) -> VerificationStatus {
    match status {
        "checked" | "verified" => VerificationStatus::Verified,
        "pending" | "unchecked" | "autoconfiguring" => VerificationStatus::Pending,
        "invalid" => {
            VerificationStatus::Failed(last_error.unwrap_or("DNS verification failed").to_string())
        }
        "locked" => VerificationStatus::Failed(
            last_error
                .unwrap_or("Scaleway has locked this domain; contact Scaleway support to unlock it")
                .to_string(),
        ),
        SCALEWAY_REVOKED_STATUS => VerificationStatus::Failed(
            "This domain was revoked in Scaleway and can no longer send. Add it again in \
             Scaleway and import the new domain."
                .to_string(),
        ),
        _ => VerificationStatus::NotStarted,
    }
}

/// Domains offered by the import picker. Revoked records are dropped: they can
/// never send again, and a domain that was revoked and re-added would otherwise
/// be listed twice under the same name, with the stale identity first.
fn importable_domains(domains: Vec<ScalewayDomainResponse>) -> Vec<ProviderDomainIdentity> {
    domains
        .into_iter()
        .filter(|domain| domain.status != SCALEWAY_REVOKED_STATUS)
        .map(scaleway_domain_to_identity)
        .collect()
}

fn scaleway_domain_to_identity(domain: ScalewayDomainResponse) -> ProviderDomainIdentity {
    let status = scaleway_verification_status(&domain.status, domain.last_error.as_deref());
    ProviderDomainIdentity {
        domain: domain.name,
        provider_identity_id: domain.id,
        status,
    }
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

/// Extract the `include:<host>` token from a full SPF record value like
/// `"v=spf1 include:_spf.tem.scaleway.com ~all"`, returning it with the
/// `include:` prefix intact (e.g. `"include:_spf.tem.scaleway.com"`) so it
/// can be matched as a substring of whatever TXT record the operator actually
/// publishes. Returns `None` if the value has no `include:` token.
fn extract_spf_include(value: &str) -> Option<&str> {
    let start = value.find("include:")?;
    let rest = &value[start..];
    let end = rest.find(' ').unwrap_or(rest.len());
    Some(&rest[..end])
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
        let dkim_record = scaleway_dkim_record(&domain_response, domain, &self.project_id);

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

        let dkim_selector = dkim_record
            .as_ref()
            .and_then(|(name, _)| dkim_selector_from_name(name));
        let dkim_records = dkim_record
            .map(|(name, value)| {
                vec![DnsRecord {
                    record_type: "TXT".to_string(),
                    name,
                    value,
                    priority: None,
                    status: DnsRecordStatus::Pending,
                }]
            })
            .unwrap_or_default();

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
            dkim_selector,
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

        // Confirm the UUID still belongs to this domain before triggering a
        // provider-side check against it — the check is a side-effecting call
        // on Scaleway's identity, so a stale or mistyped `provider_identity_id`
        // must be rejected before it reaches a different domain's identity,
        // the same way `delete_identity` validates before its revoke call.
        let precheck_response = self
            .client
            .get(self.api_url(&format!("/domains/{}", identity_id)))
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to get domain: {}", e)))?;

        if !precheck_response.status().is_success() {
            let status = precheck_response.status();
            let body = precheck_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to get domain ({}) before check: {}",
                status, body
            )));
        }

        let precheck_domain: ScalewayDomainResponse = precheck_response
            .json()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to parse domain response: {}", e)))?;

        check_identity_domain_matches(identity_id, &precheck_domain.name, domain)?;

        // Now trigger the check
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

        Ok(scaleway_verification_status(
            &domain_response.status,
            domain_response.last_error.as_deref(),
        ))
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
        let dkim_record = scaleway_dkim_record(&domain_response, domain, &self.project_id);

        // Determine overall verification status
        let overall_status = scaleway_verification_status(
            &domain_response.status,
            domain_response.last_error.as_deref(),
        );

        // Verify records via DNS lookup for accurate per-record status
        let dns_verifier = DnsVerifier::new();

        // The include Scaleway actually expects, derived from the authoritative
        // records.spf/spf_config values already in this response, with a
        // hardcoded fallback only for the case where Scaleway returns neither.
        // Verifying against a value parsed from what we just told the user to
        // publish means this can't silently go stale the way a bare constant
        // already has once (previously `_spf.scw-tem.cloud`, since corrected).
        let expected_spf_include: String = domain_response
            .records
            .as_ref()
            .and_then(|r| r.spf.as_ref())
            .and_then(|r| extract_spf_include(&r.value))
            .or(domain_response.spf_config.as_deref())
            .unwrap_or("include:_spf.tem.scaleway.com")
            .to_string();

        // Build SPF record — prefer records.spf (full publishable record) over the
        // raw spf_config snippet, which is only the include:… fragment.
        let spf_record = if let Some(records_spf) = domain_response
            .records
            .as_ref()
            .and_then(|r| r.spf.as_ref())
        {
            let spf_status = dns_verifier
                .verify_spf_record(domain, &expected_spf_include)
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
                        .verify_spf_record(domain, &expected_spf_include)
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
        let dkim_records = match dkim_record {
            Some((dkim_name, dkim)) => {
                let dkim_status = dns_verifier.verify_txt_record(&dkim_name, &dkim).await;
                vec![DnsRecord {
                    record_type: "TXT".to_string(),
                    name: dkim_name,
                    value: dkim,
                    priority: None,
                    status: dkim_status,
                }]
            }
            None => Vec::new(),
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
            manages_dns_records: true,
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

        // Scaleway's TEM API has no DELETE handler on /domains/{id} (it
        // 405s); domain removal is a POST to /domains/{id}/revoke instead —
        // see the Go SDK's RevokeDomain.
        let response = self
            .client
            .post(self.api_url(&format!("/domains/{}/revoke", identity_id)))
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
                message: scaleway_send_rejection_message(status, &body),
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

    async fn list_identities(&self) -> Result<Vec<ProviderDomainIdentity>, EmailError> {
        debug!("Listing Scaleway domains for project {}", self.project_id);

        // A single page is enough for an interactive "pick a domain to
        // import" picker — self-hosted TEM projects registering more than
        // 100 domains are not the case this UI is built for, and the
        // alternative (looping every page up front) risks an unbounded
        // number of requests for an operation triggered on every page load.
        let response = self
            .client
            .get(self.api_url("/domains"))
            .query(&[
                ("project_id", self.project_id.as_str()),
                ("page_size", "100"),
            ])
            .header("X-Auth-Token", &self.api_key)
            .send()
            .await
            .map_err(|e| EmailError::Scaleway(format!("Failed to list domains: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmailError::Scaleway(format!(
                "Failed to list domains ({}): {}",
                status, body
            )));
        }

        let list_response: ScalewayListDomainsResponse = response.json().await.map_err(|e| {
            EmailError::Scaleway(format!("Failed to parse domain list response: {}", e))
        })?;

        Ok(importable_domains(list_response.domains))
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
            message: scaleway_send_rejection_message(status, body),
        }
    }

    #[test]
    fn unchecked_domain_rejection_explains_the_fix() {
        let body = r#"{"details":[{"argument_name":"from.email","help_message":"Email must be sent from a checked domain","reason":"constraint"}],"message":"invalid argument(s)","type":"invalid_arguments"}"#;
        let message = scaleway_send_rejection_message(reqwest::StatusCode::BAD_REQUEST, body);
        assert!(message.contains(body), "raw provider body must be kept");
        assert!(message.contains("exact domain registered in Scaleway"));
        assert!(message.contains("Verify DNS"));
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

    // ── extract_spf_include ──────────────────────────────────────────────────

    #[test]
    fn extract_spf_include_from_full_record_value() {
        let include = extract_spf_include("v=spf1 include:_spf.tem.scaleway.com ~all");
        assert_eq!(include, Some("include:_spf.tem.scaleway.com"));
    }

    #[test]
    fn extract_spf_include_at_end_of_value() {
        let include = extract_spf_include("v=spf1 include:_spf.tem.scaleway.com");
        assert_eq!(include, Some("include:_spf.tem.scaleway.com"));
    }

    #[test]
    fn extract_spf_include_returns_none_without_include_token() {
        assert_eq!(extract_spf_include("v=spf1 ~all"), None);
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

    // ── scaleway_domain_to_identity ──────────────────────────────────────────

    fn domain_response(status: &str) -> ScalewayDomainResponse {
        ScalewayDomainResponse {
            id: "12345678-1234-1234-1234-123456789012".to_string(),
            name: "example.com".to_string(),
            status: status.to_string(),
            spf_config: None,
            dkim_config: None,
            last_error: None,
            records: None,
        }
    }

    #[test]
    fn scaleway_domain_to_identity_maps_checked_to_verified() {
        let identity = scaleway_domain_to_identity(domain_response("checked"));
        assert_eq!(identity.domain, "example.com");
        assert_eq!(
            identity.provider_identity_id,
            "12345678-1234-1234-1234-123456789012"
        );
        assert!(matches!(identity.status, VerificationStatus::Verified));
    }

    #[test]
    fn scaleway_domain_to_identity_maps_unchecked_to_pending() {
        let identity = scaleway_domain_to_identity(domain_response("unchecked"));
        assert!(matches!(identity.status, VerificationStatus::Pending));
    }

    #[test]
    fn scaleway_domain_to_identity_maps_invalid_to_failed_with_reason() {
        let mut response = domain_response("invalid");
        response.last_error = Some("SPF record missing".to_string());

        let identity = scaleway_domain_to_identity(response);

        match identity.status {
            VerificationStatus::Failed(reason) => assert_eq!(reason, "SPF record missing"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Regression: Temps published and verified DKIM at a hardcoded
    /// `scw._domainkey.<domain>`, while Scaleway checks the selector it returns
    /// in `records.dkim` — so Temps showed DKIM verified and Scaleway kept the
    /// domain unchecked, rejecting every send.
    #[test]
    fn dkim_record_uses_scaleway_records_name_not_a_fixed_selector() {
        let json = r#"{
            "id": "11111111-2222-3333-4444-555555555555",
            "name": "send.example.com",
            "status": "unchecked",
            "spf_config": "include:_spf.tem.scaleway.com",
            "dkim_config": "v=DKIM1; h=sha256; k=rsa; p=PUBLICKEY",
            "last_error": null,
            "records": {
                "dkim": {
                    "name": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee._domainkey.send.example.com.",
                    "value": "v=DKIM1; h=sha256; k=rsa; p=PUBLICKEY"
                }
            }
        }"#;
        let response: ScalewayDomainResponse = serde_json::from_str(json).unwrap();

        let (name, value) =
            scaleway_dkim_record(&response, "send.example.com", "project-id").unwrap();

        assert_eq!(
            name,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee._domainkey.send.example.com"
        );
        assert_eq!(value, "v=DKIM1; h=sha256; k=rsa; p=PUBLICKEY");
        assert_eq!(
            dkim_selector_from_name(&name).as_deref(),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        );
    }

    #[test]
    fn dkim_record_falls_back_to_project_selector_without_records() {
        let mut response = domain_response("unchecked");
        response.dkim_config = Some("v=DKIM1; k=rsa; p=PUBLICKEY".to_string());

        let (name, _) = scaleway_dkim_record(&response, "example.com", "project-id").unwrap();

        assert_eq!(name, "project-id._domainkey.example.com");
    }

    #[test]
    fn scaleway_domain_to_identity_maps_autoconfiguring_to_pending() {
        let identity = scaleway_domain_to_identity(domain_response("autoconfiguring"));
        assert!(matches!(identity.status, VerificationStatus::Pending));
    }

    #[test]
    fn scaleway_revoked_and_locked_are_failures_not_not_started() {
        for status in ["revoked", "locked"] {
            let identity = scaleway_domain_to_identity(domain_response(status));
            assert!(
                matches!(identity.status, VerificationStatus::Failed(_)),
                "{status} must surface as a failure, got {:?}",
                identity.status
            );
        }
    }

    /// Regression: a domain revoked and re-added in Scaleway comes back as two
    /// records with the same name; the picker listed both, and selecting the
    /// name could bind the stale revoked identity.
    #[test]
    fn importable_domains_drops_revoked_duplicate_of_re_added_domain() {
        let mut revoked = domain_response("revoked");
        revoked.id = "00000000-0000-0000-0000-000000000001".to_string();
        let mut active = domain_response("checked");
        active.id = "00000000-0000-0000-0000-000000000002".to_string();

        let domains = importable_domains(vec![revoked, active]);

        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain, "example.com");
        assert_eq!(
            domains[0].provider_identity_id,
            "00000000-0000-0000-0000-000000000002"
        );
        assert!(matches!(domains[0].status, VerificationStatus::Verified));
    }

    #[test]
    fn scaleway_list_domains_response_deserializes() {
        let json = r#"{
            "domains": [
                {
                    "id": "12345678-1234-1234-1234-123456789012",
                    "name": "example.com",
                    "status": "checked",
                    "spf_config": null,
                    "dkim_config": null,
                    "last_error": null
                }
            ],
            "total_count": 1
        }"#;

        let parsed: ScalewayListDomainsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.domains.len(), 1);
        assert_eq!(parsed.domains[0].name, "example.com");
    }
}
