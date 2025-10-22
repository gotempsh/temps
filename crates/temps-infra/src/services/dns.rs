use anyhow::Result;
use trust_dns_resolver::config::*;
use trust_dns_resolver::TokioAsyncResolver;

/// Result of a DNS A record lookup
#[derive(Debug, Clone)]
pub struct DnsLookupResult {
    /// List of A record IP addresses
    pub records: Vec<String>,
    /// DNS servers used for the lookup
    pub dns_servers: Vec<String>,
}

/// DNS lookup service for resolving domain names
#[derive(Clone)]
pub struct DnsService;

impl Default for DnsService {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsService {
    /// Create a new DNS service
    pub fn new() -> Self {
        Self
    }

    /// Create a fresh resolver with no caching
    async fn create_resolver(&self) -> Result<(TokioAsyncResolver, Vec<String>)> {
        let config = ResolverConfig::default();
        let mut opts = ResolverOpts::default();

        // Disable caching to get fresh data
        opts.cache_size = 0;
        opts.use_hosts_file = false;

        let resolver = TokioAsyncResolver::tokio(config.clone(), opts);

        // Extract DNS server addresses
        let dns_servers: Vec<String> = config
            .name_servers()
            .iter()
            .map(|ns| ns.socket_addr.ip().to_string())
            .collect();

        Ok((resolver, dns_servers))
    }

    /// Lookup A records for a domain name with fresh data
    pub async fn lookup_a_records(&self, domain: &str) -> Result<DnsLookupResult> {
        // Create a fresh resolver for each lookup (no caching)
        let (resolver, dns_servers) = self.create_resolver().await?;

        let response = resolver
            .ipv4_lookup(domain)
            .await
            .map_err(|e| anyhow::anyhow!("DNS lookup failed: {}", e))?;

        let records: Vec<String> = response.iter().map(|ip| ip.to_string()).collect();

        Ok(DnsLookupResult {
            records,
            dns_servers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lookup_a_records() {
        let service = DnsService::new();

        // Test with a known domain (google.com should always resolve)
        let result = service.lookup_a_records("google.com").await;
        assert!(result.is_ok());

        let lookup_result = result.unwrap();
        assert!(!lookup_result.records.is_empty());
        assert!(!lookup_result.dns_servers.is_empty());

        // Verify records are valid IP addresses
        for record in lookup_result.records {
            assert!(record.parse::<std::net::Ipv4Addr>().is_ok());
        }

        // Verify DNS servers are valid IP addresses
        for dns_server in lookup_result.dns_servers {
            assert!(dns_server.parse::<std::net::IpAddr>().is_ok());
        }
    }

    #[tokio::test]
    async fn test_lookup_nonexistent_domain() {
        let service = DnsService::new();

        // Test with a domain that doesn't exist
        let result = service
            .lookup_a_records("this-domain-definitely-does-not-exist-12345.com")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_caching() {
        let service = DnsService::new();

        // First lookup
        let result1 = service.lookup_a_records("google.com").await;
        assert!(result1.is_ok());

        // Second lookup should also work (fresh resolver each time)
        let result2 = service.lookup_a_records("cloudflare.com").await;
        assert!(result2.is_ok());
    }
}
