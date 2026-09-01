// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MX-record resolution for a domain.
//!
//! Uses `hickory-resolver` configured against Cloudflare DNS. The set of MX
//! hosts (ordered by preference, lowest = highest priority) feeds the SMTP
//! probing stage.

use hickory_resolver::config::{ResolveHosts, ResolverConfig, ResolverOpts, CLOUDFLARE};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use hickory_resolver::Resolver;
use tracing::debug;

/// MX records for a domain, ordered by ascending preference (most-preferred
/// mail server first).
#[derive(Debug, Clone, Default)]
pub struct MxRecords {
    /// MX exchange host names, most-preferred first. Trailing dots stripped.
    pub hosts: Vec<String>,
    /// Lookup error, when resolution failed for a reason other than "no MX".
    pub error: Option<String>,
}

impl MxRecords {
    /// Whether the domain advertises at least one mail exchanger.
    pub fn accepts_mail(&self) -> bool {
        !self.hosts.is_empty()
    }
}

/// Resolve the MX records for `domain` using Cloudflare DNS.
pub async fn lookup_mx(domain: &str) -> MxRecords {
    let mut opts = ResolverOpts::default();
    opts.try_tcp_on_error = true;
    opts.use_hosts_file = ResolveHosts::Never;

    let resolver = match Resolver::builder_with_config(
        ResolverConfig::udp_and_tcp(&CLOUDFLARE),
        TokioRuntimeProvider::default(),
    )
    .with_options(opts)
    .build()
    {
        Ok(r) => r,
        Err(e) => {
            return MxRecords {
                hosts: Vec::new(),
                error: Some(format!("failed to build DNS resolver: {e}")),
            }
        }
    };

    match resolver.mx_lookup(domain).await {
        Ok(lookup) => {
            // `mx_lookup` yields a generic `Lookup`; pull the MX rdata out of
            // each answer record. Collect (preference, exchange) then sort so
            // the most-preferred server is probed first.
            let mut records: Vec<(u16, String)> = lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => Some((
                        mx.preference,
                        mx.exchange.to_string().trim_end_matches('.').to_string(),
                    )),
                    _ => None,
                })
                .filter(|(_, host)| !host.is_empty())
                .collect();
            records.sort_by_key(|(pref, _)| *pref);

            let hosts: Vec<String> = records.into_iter().map(|(_, host)| host).collect();
            debug!("MX lookup for {domain}: {} record(s)", hosts.len());
            MxRecords { hosts, error: None }
        }
        Err(e) => {
            // No-records / NXDOMAIN is a normal "domain does not accept mail"
            // answer, not a lookup failure — surface it as empty, no error.
            let msg = e.to_string();
            if msg.contains("no record") || msg.contains("NXDomain") || e.is_no_records_found() {
                debug!("MX lookup for {domain}: no records");
                MxRecords {
                    hosts: Vec::new(),
                    error: None,
                }
            } else {
                debug!("MX lookup for {domain} failed: {msg}");
                MxRecords {
                    hosts: Vec::new(),
                    error: Some(msg),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepts_mail() {
        let empty = MxRecords::default();
        assert!(!empty.accepts_mail());

        let with_hosts = MxRecords {
            hosts: vec!["mx.example.com".to_string()],
            error: None,
        };
        assert!(with_hosts.accepts_mail());
    }

    #[tokio::test]
    async fn test_lookup_real_mx() {
        if std::env::var("TEMPS_NETWORK_TESTS").is_err() {
            println!("Network tests disabled; set TEMPS_NETWORK_TESTS=1 to enable");
            return;
        }
        // gmail.com always has MX records.
        let mx = lookup_mx("gmail.com").await;
        assert!(mx.accepts_mail());
        assert!(mx.error.is_none());
    }

    #[tokio::test]
    async fn test_lookup_domain_without_mx() {
        if std::env::var("TEMPS_NETWORK_TESTS").is_err() {
            println!("Network tests disabled; set TEMPS_NETWORK_TESTS=1 to enable");
            return;
        }
        // A non-existent domain must come back as "no mail", not an error.
        let mx = lookup_mx("this-domain-definitely-does-not-exist-temps.invalid").await;
        assert!(!mx.accepts_mail());
    }
}
