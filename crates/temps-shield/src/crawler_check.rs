// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Verifying a claimed crawler (ADR 066, D7): the address's reverse DNS
//! name must be one of the crawler's domains, and that name must resolve
//! back to the address. Answers are cached per address for a day, so a
//! crawler costs two lookups a day on each ingress, not two per request.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;

use crate::crawlers::Crawler;

/// How long an answer is kept, either way.
pub const VERIFIED_TTL: Duration = Duration::from_secs(24 * 3600);
const MAX_CACHED: u64 = 100_000;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// The two lookups verification needs.
#[async_trait]
pub trait CrawlerDns: Send + Sync {
    /// The PTR names of `ip`.
    async fn reverse(&self, ip: IpAddr) -> Vec<String>;
    /// The addresses `name` resolves to.
    async fn forward(&self, name: &str) -> Vec<IpAddr>;
}

/// The system resolver.
pub struct SystemDns {
    resolver: hickory_resolver::TokioResolver,
}

impl SystemDns {
    pub fn new() -> Result<Self, String> {
        let mut builder = hickory_resolver::TokioResolver::builder_tokio()
            .map_err(|e| format!("reading the system DNS configuration: {e}"))?;
        builder.options_mut().timeout = LOOKUP_TIMEOUT;
        builder.options_mut().attempts = 2;
        let resolver = builder
            .build()
            .map_err(|e| format!("building the DNS resolver: {e}"))?;
        Ok(Self { resolver })
    }
}

#[async_trait]
impl CrawlerDns for SystemDns {
    async fn reverse(&self, ip: IpAddr) -> Vec<String> {
        match self.resolver.reverse_lookup(ip).await {
            Ok(lookup) => lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    hickory_resolver::proto::rr::RData::PTR(ptr) => Some(ptr.0.to_ascii()),
                    _ => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn forward(&self, name: &str) -> Vec<IpAddr> {
        let fqdn = format!("{}.", name.trim_end_matches('.'));
        match self.resolver.lookup_ip(fqdn.as_str()).await {
            Ok(lookup) => lookup.iter().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Verifies claimed crawlers, caching each address's answer.
pub struct CrawlerVerifier {
    dns: Arc<dyn CrawlerDns>,
    /// The crawler an address was verified as, or `None` once it failed.
    cache: Cache<IpAddr, Option<&'static str>>,
}

impl CrawlerVerifier {
    pub fn new(dns: Arc<dyn CrawlerDns>) -> Self {
        Self {
            dns,
            cache: Cache::builder()
                .max_capacity(MAX_CACHED)
                .time_to_live(VERIFIED_TTL)
                .build(),
        }
    }

    /// With the system resolver.
    pub fn system() -> Result<Self, String> {
        Ok(Self::new(Arc::new(SystemDns::new()?)))
    }

    /// Whether `ip` really is `crawler`.
    pub async fn verify(&self, ip: IpAddr, crawler: &'static Crawler) -> bool {
        let ip = ip.to_canonical();
        if let Some(answer) = self.cache.get(&ip).await {
            return answer == Some(crawler.name);
        }
        let verified = self.lookup(ip, crawler).await;
        self.cache
            .insert(ip, verified.then_some(crawler.name))
            .await;
        verified
    }

    async fn lookup(&self, ip: IpAddr, crawler: &Crawler) -> bool {
        let names = match tokio::time::timeout(LOOKUP_TIMEOUT * 2, self.dns.reverse(ip)).await {
            Ok(names) => names,
            Err(_) => return false,
        };
        for name in names.iter().filter(|name| crawler.owns(name)) {
            let forward = tokio::time::timeout(LOOKUP_TIMEOUT * 2, self.dns.forward(name)).await;
            if forward.is_ok_and(|ips| ips.iter().any(|addr| addr.to_canonical() == ip)) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawlers::claimed;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeDns {
        ptr: HashMap<IpAddr, Vec<String>>,
        a: HashMap<String, Vec<IpAddr>>,
        lookups: AtomicUsize,
    }

    #[async_trait]
    impl CrawlerDns for FakeDns {
        async fn reverse(&self, ip: IpAddr) -> Vec<String> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            self.ptr.get(&ip).cloned().unwrap_or_default()
        }
        async fn forward(&self, name: &str) -> Vec<IpAddr> {
            self.a.get(name).cloned().unwrap_or_default()
        }
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[tokio::test]
    async fn a_real_crawler_resolves_both_ways_and_is_cached() {
        let real = ip("66.249.66.1");
        let spoofed = ip("203.0.113.5");
        let mut dns = FakeDns::default();
        dns.ptr
            .insert(real, vec!["crawl-66-249-66-1.googlebot.com.".into()]);
        dns.a
            .insert("crawl-66-249-66-1.googlebot.com.".into(), vec![real]);
        // A PTR record its owner points at Google, which does not resolve back.
        dns.ptr
            .insert(spoofed, vec!["crawl-1.googlebot.com.".into()]);
        dns.a
            .insert("crawl-1.googlebot.com.".into(), vec![ip("66.249.66.9")]);
        let dns = Arc::new(dns);
        let verifier = CrawlerVerifier::new(dns.clone());
        let google = claimed("Googlebot").unwrap();
        let bing = claimed("bingbot").unwrap();
        assert!(verifier.verify(real, google).await);
        assert!(verifier.verify(real, google).await);
        assert_eq!(
            dns.lookups.load(Ordering::Relaxed),
            1,
            "the answer is cached"
        );
        assert!(
            !verifier.verify(real, bing).await,
            "verified as Googlebot only"
        );
        assert!(!verifier.verify(spoofed, google).await);
        assert!(!verifier.verify(ip("198.51.100.1"), google).await);
    }
}
