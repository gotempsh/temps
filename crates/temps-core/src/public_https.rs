// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Result shared by unattended public HTTPS provisioning and the console.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The last outcome for a single hostname. URLs are absent until known.
/// A ready result requires a verified HTTPS response for the public URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicHttpsResult {
    pub status: PublicHttpsStatus,
    pub console_url: Option<String>,
    pub public_url: Option<String>,
    pub ingress: PublicIngress,
    pub tls: PublicTls,
    pub blockers: Vec<PublicHttpsBlocker>,
    /// Command the operator can run after applying the suggested fix.
    pub next: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicHttpsStatus {
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicIngress {
    Direct,
    Tunnel,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicTls {
    Issued,
    Pending,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicHttpsBlocker {
    pub code: PublicHttpsBlockerCode,
    pub detail: String,
    pub fix: String,
}

/// Machine-readable diagnosis. A probe-service failure carries its own
/// code because it gives no evidence about the state of the firewall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicHttpsBlockerCode {
    DockerUnavailable,
    #[serde(rename = "inbound_80_blocked")]
    Inbound80Blocked,
    #[serde(rename = "inbound_443_blocked")]
    Inbound443Blocked,
    NoPublicIp,
    DnsPointsElsewhere,
    AcmeFailed,
    ProbeUnavailable,
    WildcardDnsUnavailable,
}

/// Evidence from an outside observer for a connection to this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortObservation {
    /// The observer received the response specific to this host and run.
    Reached,
    /// The observer could not connect to the requested address and port.
    Blocked,
    /// The observer failed or could not establish which host responded.
    Inconclusive,
}

/// Select the way in for this hostname. A tunnel is available only after
/// its route has been verified for this hostname. DNS-01 can issue a
/// certificate but cannot carry incoming HTTPS traffic through a firewall.
pub fn decide_ingress(
    http: PortObservation,
    https: PortObservation,
    verified_domain_tunnel: bool,
) -> Result<PublicIngress, Vec<PublicHttpsBlockerCode>> {
    use PortObservation::*;
    use PublicHttpsBlockerCode::*;

    if http == Reached && https == Reached {
        return Ok(PublicIngress::Direct);
    }
    if verified_domain_tunnel {
        return Ok(PublicIngress::Tunnel);
    }

    let mut blockers = Vec::with_capacity(2);
    if http == Blocked {
        blockers.push(Inbound80Blocked);
    }
    if https == Blocked {
        blockers.push(Inbound443Blocked);
    }
    if http == Inconclusive || https == Inconclusive {
        blockers.push(ProbeUnavailable);
    }
    Err(blockers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_depends_on_observed_ports_and_this_domains_tunnel() {
        use PortObservation::*;
        use PublicHttpsBlockerCode::*;
        let cases = [
            ((Reached, Reached), Ok(PublicIngress::Direct)),
            ((Blocked, Reached), Err(vec![Inbound80Blocked])),
            ((Reached, Blocked), Err(vec![Inbound443Blocked])),
            (
                (Blocked, Blocked),
                Err(vec![Inbound80Blocked, Inbound443Blocked]),
            ),
            ((Inconclusive, Reached), Err(vec![ProbeUnavailable])),
            ((Reached, Inconclusive), Err(vec![ProbeUnavailable])),
            ((Inconclusive, Inconclusive), Err(vec![ProbeUnavailable])),
            (
                (Inconclusive, Blocked),
                Err(vec![Inbound443Blocked, ProbeUnavailable]),
            ),
            (
                (Blocked, Inconclusive),
                Err(vec![Inbound80Blocked, ProbeUnavailable]),
            ),
        ];
        for ((http, https), expected) in cases {
            assert_eq!(decide_ingress(http, https, false), expected);
            assert_eq!(
                decide_ingress(http, https, true),
                Ok(expected.unwrap_or(PublicIngress::Tunnel))
            );
        }
    }

    #[test]
    fn blocked_output_keeps_urls_nullable_and_codes_stable() {
        let report = PublicHttpsResult {
            status: PublicHttpsStatus::Blocked,
            console_url: Some("https://console.example.com".into()),
            public_url: None,
            ingress: PublicIngress::None,
            tls: PublicTls::Unavailable,
            blockers: vec![PublicHttpsBlocker {
                code: PublicHttpsBlockerCode::Inbound80Blocked,
                detail: "The external HTTP connection timed out.".into(),
                fix: "Allow inbound TCP port 80 in the provider firewall.".into(),
            }],
            next: "temps up --yes --json".into(),
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "status":"blocked", "console_url":"https://console.example.com",
                "public_url":null, "ingress":"none", "tls":"unavailable",
                "blockers":[{
                    "code":"inbound_80_blocked", "detail":"The external HTTP connection timed out.",
                    "fix":"Allow inbound TCP port 80 in the provider firewall."
                }],
                "next":"temps up --yes --json"
            })
        );
        assert_eq!(
            serde_json::from_value::<PublicHttpsResult>(value).unwrap(),
            report
        );
    }

    #[test]
    fn blocker_codes_round_trip() {
        use PublicHttpsBlockerCode::*;
        for (code, name) in [
            (DockerUnavailable, "docker_unavailable"),
            (Inbound80Blocked, "inbound_80_blocked"),
            (Inbound443Blocked, "inbound_443_blocked"),
            (NoPublicIp, "no_public_ip"),
            (DnsPointsElsewhere, "dns_points_elsewhere"),
            (AcmeFailed, "acme_failed"),
            (ProbeUnavailable, "probe_unavailable"),
            (WildcardDnsUnavailable, "wildcard_dns_unavailable"),
        ] {
            let json = serde_json::Value::String(name.into());
            assert_eq!(serde_json::to_value(code).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<PublicHttpsBlockerCode>(json).unwrap(),
                code
            );
        }
    }
}
