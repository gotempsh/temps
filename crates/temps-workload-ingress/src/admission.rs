// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What an endpoint may be. Checked before it is stored, so the compiler
//! only ever sees endpoints it can enforce.

use crate::compiled::parse_cidr;
use crate::endpoint::{AccessPolicy, Endpoint, EndpointProtocol, HostClaimKind, ProductKind};
use crate::host::{normalize_host, HostError, ReservedHosts};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("endpoint {endpoint_id} of {owner} has no hosts")]
    NoHosts { endpoint_id: i64, owner: String },
    #[error("endpoint {endpoint_id}: {source}")]
    Host {
        endpoint_id: i64,
        #[source]
        source: HostError,
    },
    #[error("endpoint {endpoint_id} claims '{host}', which the platform reserves (console, public ingress or preview domain)")]
    ReservedHost { endpoint_id: i64, host: String },
    #[error("endpoint {endpoint_id} claims '{host}' under the preview domain as a {kind:?} host; only generated and preview hosts live there")]
    PreviewDomainClaim {
        endpoint_id: i64,
        host: String,
        kind: HostClaimKind,
    },
    #[error(
        "endpoint {endpoint_id} has no access policy; say `public` explicitly to admit everyone"
    )]
    NoAccessPolicy { endpoint_id: i64 },
    #[error("endpoint {endpoint_id} is {protocol:?}, which cannot carry the {policy} policy: it needs an HTTP exchange")]
    PolicyNeedsHttp {
        endpoint_id: i64,
        protocol: EndpointProtocol,
        policy: &'static str,
    },
    #[error("endpoint {endpoint_id} has path prefix '{prefix}': {reason}")]
    InvalidPathPrefix {
        endpoint_id: i64,
        prefix: String,
        reason: &'static str,
    },
    #[error("endpoint {endpoint_id} exposes database {database_id} publicly without {missing}; a public database endpoint needs TLS and at least one IP rule")]
    UnsafeDatabaseEndpoint {
        endpoint_id: i64,
        database_id: i64,
        missing: &'static str,
    },
    #[error("endpoint {endpoint_id} has IP rule '{cidr}', which is not an address block")]
    InvalidIpRule { endpoint_id: i64, cidr: String },
    #[error("endpoint {endpoint_id} has country code '{code}': expected two letters (ISO 3166-1 alpha-2)")]
    InvalidCountry { endpoint_id: i64, code: String },
    #[error("endpoint {endpoint_id} has a rate limit of {requests} per {per_secs}s: both must be positive")]
    InvalidRateLimit {
        endpoint_id: i64,
        requests: u32,
        per_secs: u32,
    },
    #[error(
        "endpoint {endpoint_id} combines `public` with other policies; `public` only stands alone"
    )]
    PublicWithOthers { endpoint_id: i64 },
}

/// Checks an endpoint and returns it with its hosts normalised.
pub fn admit(mut endpoint: Endpoint, reserved: &ReservedHosts) -> Result<Endpoint, AdmissionError> {
    let id = endpoint.id;
    if endpoint.hosts.is_empty() {
        return Err(AdmissionError::NoHosts {
            endpoint_id: id,
            owner: endpoint.owner.to_string(),
        });
    }
    for claim in &mut endpoint.hosts {
        claim.host = normalize_host(&claim.host).map_err(|source| AdmissionError::Host {
            endpoint_id: id,
            source,
        })?;
        if reserved.is_reserved(&claim.host) {
            return Err(AdmissionError::ReservedHost {
                endpoint_id: id,
                host: claim.host.clone(),
            });
        }
        if reserved.is_under_preview_domain(&claim.host)
            && !matches!(
                claim.kind,
                HostClaimKind::Generated | HostClaimKind::Preview
            )
        {
            return Err(AdmissionError::PreviewDomainClaim {
                endpoint_id: id,
                host: claim.host.clone(),
                kind: claim.kind,
            });
        }
    }

    if endpoint.access.is_empty() {
        return Err(AdmissionError::NoAccessPolicy { endpoint_id: id });
    }
    if endpoint.access.len() > 1
        && endpoint
            .access
            .iter()
            .any(|p| matches!(p, AccessPolicy::Public))
    {
        return Err(AdmissionError::PublicWithOthers { endpoint_id: id });
    }
    for policy in &endpoint.access {
        if endpoint.protocol != EndpointProtocol::Http && policy.needs_http() {
            return Err(AdmissionError::PolicyNeedsHttp {
                endpoint_id: id,
                protocol: endpoint.protocol,
                policy: policy.name(),
            });
        }
        match policy {
            AccessPolicy::IpRules { rules } => {
                for rule in rules {
                    if parse_cidr(&rule.cidr).is_none() {
                        return Err(AdmissionError::InvalidIpRule {
                            endpoint_id: id,
                            cidr: rule.cidr.clone(),
                        });
                    }
                }
            }
            AccessPolicy::Geo { allow, deny } => {
                for code in allow.iter().chain(deny) {
                    if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
                        return Err(AdmissionError::InvalidCountry {
                            endpoint_id: id,
                            code: code.clone(),
                        });
                    }
                }
            }
            AccessPolicy::RateLimit { limit } => {
                if limit.requests == 0 || limit.per_secs == 0 {
                    return Err(AdmissionError::InvalidRateLimit {
                        endpoint_id: id,
                        requests: limit.requests,
                        per_secs: limit.per_secs,
                    });
                }
            }
            AccessPolicy::Public
            | AccessPolicy::Password { .. }
            | AccessPolicy::TempsLogin { .. }
            | AccessPolicy::SignedLink
            | AccessPolicy::Captcha => {}
        }
    }

    if let Some(prefix) = &endpoint.path_prefix {
        let reason = if endpoint.protocol != EndpointProtocol::Http {
            Some("only HTTP endpoints route by path")
        } else if !prefix.starts_with('/') {
            Some("it must start with '/'")
        } else if prefix.len() > 1 && prefix.ends_with('/') {
            Some("it must not end with '/'")
        } else if prefix.split('/').any(|s| s == "." || s == "..") {
            Some("it must not contain '.' or '..' segments")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(AdmissionError::InvalidPathPrefix {
                endpoint_id: id,
                prefix: prefix.clone(),
                reason,
            });
        }
    }

    if endpoint.owner.kind == ProductKind::Database {
        let tls = endpoint.protocol == EndpointProtocol::TlsPassthrough
            || (endpoint.protocol == EndpointProtocol::Tcp && endpoint.server_requires_tls);
        let ip_rules = endpoint
            .access
            .iter()
            .any(|p| matches!(p, AccessPolicy::IpRules { rules } if !rules.is_empty()));
        let missing = match (tls, ip_rules) {
            (true, true) => None,
            (false, true) => Some("TLS"),
            (true, false) => Some("an IP rule"),
            (false, false) => Some("TLS or an IP rule"),
        };
        if let Some(missing) = missing {
            return Err(AdmissionError::UnsafeDatabaseEndpoint {
                endpoint_id: id,
                database_id: endpoint.owner.id,
                missing,
            });
        }
    }
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{EndpointTarget, HostClaim, IpRule, ProductRef, RateLimit};

    fn reserved() -> ReservedHosts {
        ReservedHosts::new(
            ["console.example.test".to_string()],
            Some("preview.example.test".to_string()),
        )
    }

    fn endpoint(owner: ProductKind, host: &str, access: Vec<AccessPolicy>) -> Endpoint {
        Endpoint {
            id: 7,
            owner: ProductRef { kind: owner, id: 3 },
            target: EndpointTarget::Workload {
                workload_id: 1,
                revision: 1,
                port: 3000,
            },
            hosts: vec![HostClaim {
                host: host.to_string(),
                kind: HostClaimKind::CustomDomain,
                claim_id: 1,
            }],
            path_prefix: None,
            protocol: EndpointProtocol::Http,
            server_requires_tls: false,
            access,
        }
    }

    fn ip_rules() -> AccessPolicy {
        AccessPolicy::IpRules {
            rules: vec![IpRule {
                cidr: "192.0.2.0/24".into(),
                allow: true,
            }],
        }
    }

    #[test]
    fn a_plain_public_endpoint_is_admitted_with_its_host_normalised() {
        let admitted = admit(
            endpoint(
                ProductKind::Environment,
                "Shop.Example.Test.",
                vec![AccessPolicy::Public],
            ),
            &reserved(),
        )
        .unwrap();
        assert_eq!(admitted.hosts[0].host, "shop.example.test");
    }

    #[test]
    fn reserved_hosts_and_preview_squatting_are_refused() {
        let err = admit(
            endpoint(
                ProductKind::Environment,
                "console.example.test",
                vec![AccessPolicy::Public],
            ),
            &reserved(),
        )
        .unwrap_err();
        assert!(matches!(err, AdmissionError::ReservedHost { .. }), "{err}");
        let err = admit(
            endpoint(
                ProductKind::Environment,
                "pr-1.preview.example.test",
                vec![AccessPolicy::Public],
            ),
            &reserved(),
        )
        .unwrap_err();
        assert!(
            matches!(err, AdmissionError::PreviewDomainClaim { .. }),
            "{err}"
        );
        let mut preview = endpoint(
            ProductKind::Sandbox,
            "pr-1.preview.example.test",
            vec![AccessPolicy::Public],
        );
        preview.hosts[0].kind = HostClaimKind::Preview;
        assert!(admit(preview, &reserved()).is_ok());
    }

    #[test]
    fn a_policy_is_required_and_public_stands_alone() {
        assert!(matches!(
            admit(
                endpoint(ProductKind::Environment, "shop.example.test", vec![]),
                &reserved()
            ),
            Err(AdmissionError::NoAccessPolicy { endpoint_id: 7 })
        ));
        assert!(matches!(
            admit(
                endpoint(
                    ProductKind::Environment,
                    "shop.example.test",
                    vec![AccessPolicy::Public, AccessPolicy::Captcha]
                ),
                &reserved()
            ),
            Err(AdmissionError::PublicWithOthers { .. })
        ));
    }

    #[test]
    fn http_only_policies_are_refused_on_tcp() {
        let mut e = endpoint(
            ProductKind::Environment,
            "shop.example.test",
            vec![AccessPolicy::TempsLogin {
                permission: "projects:read".into(),
            }],
        );
        e.protocol = EndpointProtocol::Tcp;
        let err = admit(e, &reserved()).unwrap_err();
        assert_eq!(err.to_string(), "endpoint 7 is Tcp, which cannot carry the temps_login policy: it needs an HTTP exchange");
    }

    #[test]
    fn a_public_database_needs_tls_and_an_ip_rule() {
        let mut db = endpoint(ProductKind::Database, "db.example.test", vec![ip_rules()]);
        db.protocol = EndpointProtocol::Tcp;
        let err = admit(db.clone(), &reserved()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "endpoint 7 exposes database 3 publicly without TLS; a public database endpoint needs TLS and at least one IP rule"
        );
        db.server_requires_tls = true;
        assert!(admit(db.clone(), &reserved()).is_ok());
        db.access = vec![AccessPolicy::Public];
        assert!(matches!(
            admit(db.clone(), &reserved()),
            Err(AdmissionError::UnsafeDatabaseEndpoint {
                missing: "an IP rule",
                ..
            })
        ));
        db.protocol = EndpointProtocol::TlsPassthrough;
        db.access = vec![ip_rules()];
        assert!(admit(db, &reserved()).is_ok());
    }

    #[test]
    fn malformed_policies_and_prefixes_are_refused() {
        let check = |access, prefix: Option<&str>| {
            let mut e = endpoint(ProductKind::Environment, "shop.example.test", access);
            e.path_prefix = prefix.map(str::to_string);
            admit(e, &reserved())
        };
        assert!(matches!(
            check(
                vec![AccessPolicy::IpRules {
                    rules: vec![IpRule {
                        cidr: "10.0.0.1/8".into(),
                        allow: true
                    }]
                }],
                None
            ),
            Err(AdmissionError::InvalidIpRule { .. })
        ));
        assert!(matches!(
            check(
                vec![AccessPolicy::Geo {
                    allow: vec!["ESP".into()],
                    deny: vec![]
                }],
                None
            ),
            Err(AdmissionError::InvalidCountry { .. })
        ));
        assert!(matches!(
            check(
                vec![AccessPolicy::RateLimit {
                    limit: RateLimit {
                        requests: 0,
                        per_secs: 1
                    }
                }],
                None
            ),
            Err(AdmissionError::InvalidRateLimit { .. })
        ));
        for prefix in ["api", "/api/", "/a/../b"] {
            assert!(
                matches!(
                    check(vec![AccessPolicy::Public], Some(prefix)),
                    Err(AdmissionError::InvalidPathPrefix { .. })
                ),
                "{prefix}"
            );
        }
        assert!(check(vec![AccessPolicy::Public], Some("/api")).is_ok());
    }
}
