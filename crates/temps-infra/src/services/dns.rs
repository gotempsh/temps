use anyhow::Result;
use hickory_resolver::config::*;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::system_conf::read_system_conf;
use hickory_resolver::Resolver;

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

    /// Create a fresh resolver with no caching.
    ///
    /// `ResolverConfig::default()` in hickory 0.26 has no name servers —
    /// it returns an empty config, so a resolver built from it fails
    /// every query with `no connections available`. The previous
    /// (hickory 0.25) `ResolverConfig::default()` mirrored Google's
    /// public DNS, which is why this regressed silently during the 0.26
    /// upgrade. Read `/etc/resolv.conf` first (so we honour the user's
    /// configured resolvers in production); fall back to Cloudflare's
    /// public 1.1.1.1 / 1.0.0.1 if the system file is unreadable or
    /// empty (containers, restricted CI environments).
    async fn create_resolver(&self) -> Result<(Resolver<TokioRuntimeProvider>, Vec<String>)> {
        let (mut config, mut opts) = match read_system_conf() {
            Ok((cfg, opts)) if !cfg.name_servers().is_empty() => (cfg, opts),
            _ => {
                let mut cfg = ResolverConfig::default();
                cfg.add_name_server(NameServerConfig::udp_and_tcp(std::net::IpAddr::V4(
                    std::net::Ipv4Addr::new(1, 1, 1, 1),
                )));
                cfg.add_name_server(NameServerConfig::udp_and_tcp(std::net::IpAddr::V4(
                    std::net::Ipv4Addr::new(1, 0, 0, 1),
                )));
                (cfg, ResolverOpts::default())
            }
        };

        // Disable caching to get fresh data
        opts.cache_size = 0;
        opts.use_hosts_file = ResolveHosts::Never;

        // Extract DNS server addresses (hickory 0.26: NameServerConfig.ip).
        let dns_servers: Vec<String> = config
            .name_servers()
            .iter()
            .map(|ns| ns.ip.to_string())
            .collect();

        // `config` ends here — clone for the builder isn't necessary since we
        // already snapshotted `dns_servers`; pass it by move.
        let _ = &mut config; // silence "unused mut" if optimised away
        let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build DNS resolver: {}", e))?;

        Ok((resolver, dns_servers))
    }

    /// Lookup A records for a domain name with fresh data
    pub async fn lookup_a_records(&self, domain: &str) -> Result<DnsLookupResult> {
        use hickory_resolver::proto::rr::{RData, RecordType};

        // Create a fresh resolver for each lookup (no caching)
        let (resolver, dns_servers) = self.create_resolver().await?;

        // Generic `lookup` returns a `Lookup`; pull the A rdata out of each
        // answer record (hickory 0.26 — record.data is the typed RData).
        let response = resolver
            .lookup(domain, RecordType::A)
            .await
            .map_err(|e| anyhow::anyhow!("DNS lookup failed: {}", e))?;

        let records: Vec<String> = response
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::A(a) => Some(a.0.to_string()),
                _ => None,
            })
            .collect();

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
