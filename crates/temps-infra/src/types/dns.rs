use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request to lookup DNS A records for a domain
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsLookupRequest {
    /// Domain name to lookup
    #[schema(example = "example.com")]
    pub domain: String,
}

/// Response containing DNS A records
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsLookupResponse {
    /// Domain name that was queried
    #[schema(example = "example.com")]
    pub domain: String,

    /// List of A record IP addresses
    #[schema(example = json!(["93.184.216.34"]))]
    pub records: Vec<String>,

    /// Number of records found
    #[schema(example = 1)]
    pub count: usize,

    /// DNS servers used for the lookup
    #[schema(example = json!(["8.8.8.8", "8.8.4.4"]))]
    pub dns_servers: Vec<String>,
}

/// Error response for DNS lookup failures
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsLookupError {
    /// Error message
    #[schema(example = "DNS lookup failed: domain not found")]
    pub error: String,

    /// Domain name that failed
    #[schema(example = "nonexistent.com")]
    pub domain: String,
}
