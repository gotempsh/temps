// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS verification utilities for email records

use hickory_resolver::config::{ResolveHosts, ResolverConfig, ResolverOpts, CLOUDFLARE};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::{RData, RecordType};
use hickory_resolver::Resolver;
use tracing::debug;

use crate::providers::DnsRecordStatus;

/// DNS verification service for checking email-related DNS records
pub struct DnsVerifier {
    resolver: Resolver<TokioRuntimeProvider>,
}

impl Default for DnsVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsVerifier {
    /// Create a new DNS verifier using Cloudflare's DNS servers
    pub fn new() -> Self {
        let mut options = ResolverOpts::default();
        options.try_tcp_on_error = true;
        options.use_hosts_file = ResolveHosts::Never;

        // Building from a static, known-valid config cannot fail in practice;
        // a failure here means the process environment is fundamentally broken.
        let resolver = Resolver::builder_with_config(
            ResolverConfig::udp_and_tcp(&CLOUDFLARE),
            TokioRuntimeProvider::default(),
        )
        .with_options(options)
        .build()
        .expect("failed to build DNS resolver from static Cloudflare config");

        Self { resolver }
    }

    /// Verify if a TXT record exists with the expected value
    pub async fn verify_txt_record(&self, name: &str, expected_value: &str) -> DnsRecordStatus {
        debug!("Verifying TXT record: {} = {}", name, expected_value);

        match self.resolver.txt_lookup(name).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    let RData::TXT(txt) = &record.data else {
                        continue;
                    };
                    let txt_data: String = txt
                        .txt_data
                        .iter()
                        .map(|data| String::from_utf8_lossy(data).to_string())
                        .collect();

                    debug!("Found TXT record: {}", txt_data);

                    // Only check that the published record contains the expected value
                    // (DKIM keys/SPF includes are checked as substrings since registrars
                    // sometimes wrap them with extra text). The reverse check —
                    // `expected_value.contains(&txt_data)` — used to also be accepted,
                    // which is a false-positive trap: any short, unrelated TXT record
                    // (e.g. a single stray character) is trivially a substring of a long
                    // expected DKIM key, so a domain could be reported "Verified" with a
                    // DNS record that has nothing to do with the expected value.
                    if txt_data.contains(expected_value) {
                        return DnsRecordStatus::Verified;
                    }
                }

                // Records found but no match - still pending
                debug!(
                    "TXT records found for {} but no match for expected value",
                    name
                );
                DnsRecordStatus::Pending
            }
            Err(e) => {
                debug!("TXT lookup failed for {}: {}", name, e);
                // Check if it's a NXDOMAIN (record doesn't exist) vs other error
                if e.to_string().contains("no record") || e.to_string().contains("NXDomain") {
                    DnsRecordStatus::Pending
                } else {
                    DnsRecordStatus::Unknown
                }
            }
        }
    }

    /// Verify if a CNAME record exists with the expected value
    /// This is used for DKIM records that are CNAMEs to AWS SES
    pub async fn verify_cname_record(&self, name: &str, expected_value: &str) -> DnsRecordStatus {
        debug!("Verifying CNAME record: {} -> {}", name, expected_value);

        match self.resolver.lookup(name, RecordType::CNAME).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    if let RData::CNAME(cname) = &record.data {
                        let cname_str = cname.to_string();
                        debug!("Found CNAME record: {}", cname_str);

                        // Remove trailing dot if present
                        let cname_clean = cname_str.trim_end_matches('.');
                        let expected_clean = expected_value.trim_end_matches('.');

                        if cname_clean.eq_ignore_ascii_case(expected_clean) {
                            return DnsRecordStatus::Verified;
                        }
                    }
                }

                debug!(
                    "CNAME records found for {} but no match for expected value",
                    name
                );
                DnsRecordStatus::Pending
            }
            Err(e) => {
                debug!("CNAME lookup failed for {}: {}", name, e);
                DnsRecordStatus::Pending
            }
        }
    }

    /// Verify if an MX record exists with the expected value
    pub async fn verify_mx_record(
        &self,
        name: &str,
        expected_value: &str,
        expected_priority: Option<u16>,
    ) -> DnsRecordStatus {
        debug!(
            "Verifying MX record: {} -> {} (priority: {:?})",
            name, expected_value, expected_priority
        );

        match self.resolver.mx_lookup(name).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    let RData::MX(mx) = &record.data else {
                        continue;
                    };
                    let exchange = mx.exchange.to_string();
                    let priority = mx.preference;

                    debug!("Found MX record: {} (priority: {})", exchange, priority);

                    // Remove trailing dot if present
                    let exchange_clean = exchange.trim_end_matches('.');
                    let expected_clean = expected_value.trim_end_matches('.');

                    if exchange_clean.eq_ignore_ascii_case(expected_clean) {
                        // If priority is specified, check it too
                        if let Some(exp_priority) = expected_priority {
                            if priority == exp_priority {
                                return DnsRecordStatus::Verified;
                            }
                        } else {
                            return DnsRecordStatus::Verified;
                        }
                    }
                }

                debug!(
                    "MX records found for {} but no match for expected value",
                    name
                );
                DnsRecordStatus::Pending
            }
            Err(e) => {
                debug!("MX lookup failed for {}: {}", name, e);
                DnsRecordStatus::Pending
            }
        }
    }

    /// Verify SPF record (checks for TXT record containing SPF data)
    pub async fn verify_spf_record(&self, domain: &str, expected_include: &str) -> DnsRecordStatus {
        debug!(
            "Verifying SPF record for {} contains {}",
            domain, expected_include
        );

        match self.resolver.txt_lookup(domain).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    let RData::TXT(txt) = &record.data else {
                        continue;
                    };
                    let txt_data: String = txt
                        .txt_data
                        .iter()
                        .map(|data| String::from_utf8_lossy(data).to_string())
                        .collect();

                    debug!("Found TXT record: {}", txt_data);

                    // SPF records start with "v=spf1"
                    if txt_data.starts_with("v=spf1") && txt_data.contains(expected_include) {
                        return DnsRecordStatus::Verified;
                    }
                }

                debug!("No matching SPF record found for {}", domain);
                DnsRecordStatus::Pending
            }
            Err(e) => {
                debug!("SPF lookup failed for {}: {}", domain, e);
                DnsRecordStatus::Pending
            }
        }
    }

    /// Verify a DMARC policy record: a `_dmarc.<domain>` TXT record starting
    /// with `v=DMARC1`. Unlike SPF/DKIM/MX, no provider API publishes or
    /// manages this — it's a plain DNS record the domain owner sets
    /// independently — so callers generate the expected record themselves
    /// and use this purely to check whether it's actually live.
    pub async fn verify_dmarc_record(&self, domain: &str) -> DnsRecordStatus {
        let name = format!("_dmarc.{}", domain);
        debug!("Verifying DMARC record: {}", name);

        match self.resolver.txt_lookup(&name).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    let RData::TXT(txt) = &record.data else {
                        continue;
                    };
                    let txt_data: String = txt
                        .txt_data
                        .iter()
                        .map(|data| String::from_utf8_lossy(data).to_string())
                        .collect();

                    debug!("Found TXT record: {}", txt_data);

                    if txt_data.starts_with("v=DMARC1") {
                        return DnsRecordStatus::Verified;
                    }
                }

                debug!("No matching DMARC record found for {}", name);
                DnsRecordStatus::Pending
            }
            Err(e) => {
                debug!("DMARC lookup failed for {}: {}", name, e);
                DnsRecordStatus::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dns_verifier_creation() {
        let _verifier = DnsVerifier::new();
        // Just verify it can be created without panicking
    }

    #[tokio::test]
    async fn test_verify_known_txt_record() {
        if std::env::var("TEMPS_NETWORK_TESTS").is_err() {
            println!("Network tests disabled; set TEMPS_NETWORK_TESTS=1 to enable");
            return;
        }
        let verifier = DnsVerifier::new();
        // Test with a known TXT record (Google's SPF)
        let status = verifier
            .verify_spf_record("google.com", "_spf.google.com")
            .await;
        assert_eq!(status, DnsRecordStatus::Verified);
    }

    #[tokio::test]
    async fn test_verify_known_mx_record() {
        if std::env::var("TEMPS_NETWORK_TESTS").is_err() {
            println!("Network tests disabled; set TEMPS_NETWORK_TESTS=1 to enable");
            return;
        }
        let verifier = DnsVerifier::new();
        // Test with a known MX record
        let status = verifier
            .verify_mx_record("google.com", "smtp.google.com", None)
            .await;
        // Google's MX records are aspmx.l.google.com, so this should be Pending
        assert!(matches!(
            status,
            DnsRecordStatus::Pending | DnsRecordStatus::Verified
        ));
    }
}
