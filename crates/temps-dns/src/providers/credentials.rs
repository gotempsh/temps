//! DNS provider credentials
//!
//! This module defines the credential structures for each supported DNS provider.
//! These are stored encrypted in the database.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Cloudflare credentials
///
/// Cloudflare supports two authentication methods:
/// 1. API Token (recommended) - Scoped tokens with specific permissions
/// 2. API Key + Email (legacy) - Global API key with email address
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CloudflareCredentials {
    /// API Token (recommended)
    /// Create at: https://dash.cloudflare.com/profile/api-tokens
    /// Required permissions: Zone:DNS:Edit
    #[schema(example = "your-api-token")]
    pub api_token: String,

    /// Optional: Account ID for multi-account scenarios
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// Namecheap credentials
///
/// Namecheap requires:
/// - API access enabled on your account (must request access)
/// - Whitelisted IP address(es)
///
/// API Key can be found at: https://ap.www.namecheap.com/settings/tools/apiaccess/
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NamecheapCredentials {
    /// Namecheap username (same as login username)
    #[schema(example = "your-username")]
    pub api_user: String,

    /// API Key from Namecheap dashboard
    #[schema(example = "your-api-key")]
    pub api_key: String,

    /// Client IP address (must be whitelisted in Namecheap)
    /// If not provided, the server's public IP will be used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,

    /// Use sandbox environment for testing
    #[serde(default)]
    pub sandbox: bool,
}

/// Route53 (AWS) credentials
///
/// AWS credentials for Route53 DNS management.
/// Create an IAM user with Route53FullAccess or a custom policy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Route53Credentials {
    /// AWS Access Key ID
    #[schema(example = "AKIAIOSFODNN7EXAMPLE")]
    pub access_key_id: String,

    /// AWS Secret Access Key
    #[schema(example = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")]
    pub secret_access_key: String,

    /// Optional: Session token (for temporary credentials)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,

    /// Optional: AWS Region (defaults to us-east-1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// DigitalOcean credentials
///
/// DigitalOcean uses a simple API token for authentication.
/// Create at: https://cloud.digitalocean.com/account/api/tokens
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DigitalOceanCredentials {
    /// Personal Access Token with read/write scope
    #[schema(example = "dop_v1_your-token")]
    pub api_token: String,
}

/// Google Cloud DNS credentials
///
/// GCP uses service account credentials for authentication.
/// Create a service account with DNS Administrator role.
/// Download the JSON key file from the GCP Console.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GcpCredentials {
    /// Service account email (from JSON key file)
    #[schema(example = "dns-admin@myproject.iam.gserviceaccount.com")]
    pub service_account_email: String,

    /// Private key (PEM format, from JSON key file)
    #[schema(example = "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----")]
    pub private_key: String,

    /// GCP Project ID
    #[schema(example = "my-gcp-project")]
    pub project_id: String,
}

/// Azure DNS credentials
///
/// Azure uses service principal (app registration) credentials.
/// Create an app registration with DNS Zone Contributor role.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AzureCredentials {
    /// Azure Tenant ID (Directory ID)
    #[schema(example = "00000000-0000-0000-0000-000000000000")]
    pub tenant_id: String,

    /// Client ID (Application ID)
    #[schema(example = "00000000-0000-0000-0000-000000000000")]
    pub client_id: String,

    /// Client Secret
    #[schema(example = "your-client-secret")]
    pub client_secret: String,

    /// Azure Subscription ID
    #[schema(example = "00000000-0000-0000-0000-000000000000")]
    pub subscription_id: String,

    /// Resource Group name containing DNS zones
    #[schema(example = "my-resource-group")]
    pub resource_group: String,
}

/// Unified provider credentials enum
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderCredentials {
    Cloudflare(CloudflareCredentials),
    Namecheap(NamecheapCredentials),
    Route53(Route53Credentials),
    DigitalOcean(DigitalOceanCredentials),
    Gcp(GcpCredentials),
    Azure(AzureCredentials),
}

impl ProviderCredentials {
    /// Get a masked representation of credentials for display
    pub fn masked(&self) -> serde_json::Value {
        match self {
            ProviderCredentials::Cloudflare(c) => {
                serde_json::json!({
                    "type": "cloudflare",
                    "api_token": mask_string(&c.api_token),
                    "account_id": c.account_id.as_ref().map(|s| mask_string(s)),
                })
            }
            ProviderCredentials::Namecheap(c) => {
                serde_json::json!({
                    "type": "namecheap",
                    "api_user": c.api_user.clone(),
                    "api_key": mask_string(&c.api_key),
                    "client_ip": c.client_ip.clone(),
                    "sandbox": c.sandbox,
                })
            }
            ProviderCredentials::Route53(c) => {
                serde_json::json!({
                    "type": "route53",
                    "access_key_id": mask_string(&c.access_key_id),
                    "secret_access_key": "***",
                    "region": c.region.clone(),
                })
            }
            ProviderCredentials::DigitalOcean(c) => {
                serde_json::json!({
                    "type": "digitalocean",
                    "api_token": mask_string(&c.api_token),
                })
            }
            ProviderCredentials::Gcp(c) => {
                serde_json::json!({
                    "type": "gcp",
                    "service_account_email": c.service_account_email.clone(),
                    "private_key": "***",
                    "project_id": c.project_id.clone(),
                })
            }
            ProviderCredentials::Azure(c) => {
                serde_json::json!({
                    "type": "azure",
                    "tenant_id": mask_string(&c.tenant_id),
                    "client_id": mask_string(&c.client_id),
                    "client_secret": "***",
                    "subscription_id": mask_string(&c.subscription_id),
                    "resource_group": c.resource_group.clone(),
                })
            }
        }
    }
}

/// Mask a string, showing only first 4 and last 4 characters
fn mask_string(s: &str) -> String {
    if s.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_string() {
        assert_eq!(mask_string("short"), "***");
        assert_eq!(mask_string("12345678"), "***");
        assert_eq!(mask_string("123456789"), "1234...6789");
        assert_eq!(mask_string("AKIAIOSFODNN7EXAMPLE"), "AKIA...MPLE");
    }

    #[test]
    fn test_cloudflare_credentials_masked() {
        let creds = ProviderCredentials::Cloudflare(CloudflareCredentials {
            api_token: "very-long-api-token-here".to_string(),
            account_id: Some("account-id-12345".to_string()),
        });

        let masked = creds.masked();
        assert_eq!(masked["type"], "cloudflare");
        assert_eq!(masked["api_token"], "very...here");
        assert_eq!(masked["account_id"], "acco...2345");
    }

    #[test]
    fn test_namecheap_credentials_serialization() {
        let creds = NamecheapCredentials {
            api_user: "testuser".to_string(),
            api_key: "test-api-key".to_string(),
            client_ip: Some("1.2.3.4".to_string()),
            sandbox: true,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let parsed: NamecheapCredentials = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.api_user, "testuser");
        assert_eq!(parsed.api_key, "test-api-key");
        assert_eq!(parsed.client_ip, Some("1.2.3.4".to_string()));
        assert!(parsed.sandbox);
    }

    #[test]
    fn test_provider_credentials_tagged_serialization() {
        let creds = ProviderCredentials::Cloudflare(CloudflareCredentials {
            api_token: "test-token".to_string(),
            account_id: None,
        });

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("\"type\":\"cloudflare\""));

        let parsed: ProviderCredentials = serde_json::from_str(&json).unwrap();
        match parsed {
            ProviderCredentials::Cloudflare(c) => {
                assert_eq!(c.api_token, "test-token");
            }
            _ => panic!("Expected Cloudflare credentials"),
        }
    }
}
