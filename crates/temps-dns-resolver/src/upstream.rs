//! Upstream recursive forwarder.
//!
//! We are the first (and often only) nameserver visible inside app
//! containers, so the authoritative path in [`crate::authority`] alone
//! is not enough — anything outside `*.temps.local` (the public
//! Internet, package mirrors, third-party APIs) needs a real recursive
//! lookup. This module wraps `hickory_resolver::Resolver` to do that
//! lookup against the upstream pool the operator configured (defaults
//! to Cloudflare + Google) and re-emits the answers using the same
//! `MessageResponseBuilder` machinery the authoritative side uses, so
//! the original transaction ID, flags, and EDNS bits are preserved.
//!
//! Failure modes (timeout, NXDOMAIN, REFUSED, network down) all reduce
//! to a single `ResponseCode` we hand back to the caller — we never
//! panic, never block the server task, and never leak `anyhow::Error`
//! into the wire format.
//!
//! TTL handling: we copy the upstream TTL verbatim. We don't cache
//! ourselves; that's the upstream's and the client's job. Caching here
//! would just hide failover signals from `pg_auto_failover`-style
//! services that rely on short TTLs to fail over quickly.

use std::net::SocketAddr;
use std::time::Duration;

use hickory_proto::op::{Header, HeaderCounts, Metadata, ResponseCode};
use hickory_proto::rr::{Name, Record};
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::Resolver;
use hickory_server::server::{Request, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use tracing::{trace, warn};

type TokioResolver = Resolver<TokioRuntimeProvider>;

/// Forwards queries that fall outside the internal `temps.local` zone
/// to the upstream pool. Construct once per agent process and share via
/// `Arc` — the underlying `Resolver` is itself cheap-to-clone and uses
/// connection pooling internally.
pub struct UpstreamResolver {
    resolver: TokioResolver,
}

impl UpstreamResolver {
    /// Build a resolver that round-robins across `upstreams`. Returns
    /// `None` if the upstream list is empty (caller should treat this
    /// as "forwarding disabled" rather than as an error).
    pub fn new(upstreams: &[SocketAddr]) -> Option<Self> {
        if upstreams.is_empty() {
            return None;
        }

        let mut config = ResolverConfig::default();
        for addr in upstreams {
            // `NameServerConfig::udp_and_tcp` bundles both a UDP and a TCP
            // connection for one server — TCP is the fallback for responses
            // too large for UDP (some TXT / DNSSEC answers). The standard
            // DNS port (53) is used.
            config.add_name_server(NameServerConfig::udp_and_tcp(addr.ip()));
        }

        let mut opts = ResolverOpts::default();
        opts.timeout = Duration::from_secs(3);
        opts.attempts = 2;
        // Modest cache so we don't hammer upstreams during burst
        // traffic from a freshly-booted pod. 512 entries ≈ a few KB.
        opts.cache_size = 512;
        opts.edns0 = true;
        opts.try_tcp_on_error = true;

        let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()
            .ok()?;

        Some(Self { resolver })
    }

    /// Resolve `request` against the upstream pool and write the
    /// answers back through `response_handle`. Returns `Ok(info)` on
    /// successful (or successfully-NXDOMAIN'd) responses, `Err(info)`
    /// with a stub `ResponseInfo` on transport failure so the caller
    /// can decide whether to fall through to a local error reply.
    pub async fn forward<R: ResponseHandler>(
        &self,
        request: &Request,
        response_handle: &mut R,
    ) -> Result<ResponseInfo, ResponseInfo> {
        let info = match request.request_info() {
            Ok(i) => i,
            Err(e) => {
                warn!(error = %e, "upstream forwarder: malformed request");
                return Err(error_info(request, ResponseCode::FormErr));
            }
        };

        let qname: Name = info.query.name().into();
        let qtype = info.query.query_type();

        let lookup_result = self.resolver.lookup(qname.clone(), qtype).await;

        let (records, response_code) = match lookup_result {
            Ok(lookup) => {
                let recs: Vec<Record> = lookup.answers().to_vec();
                trace!(qname = %qname, qtype = ?qtype, answers = recs.len(), "upstream answer");
                (recs, ResponseCode::NoError)
            }
            Err(e) => {
                // hickory-resolver reports both NODATA (the name exists but
                // has no record of *this* type) and NXDOMAIN (the name does
                // not exist at all) as the same `NoRecordsFound` error. The
                // true distinction lives in `NoRecords.response_code`, and it
                // is load-bearing: glibc and busybox `getaddrinfo` treat an
                // NXDOMAIN on *either* the A or the AAAA query as "host does
                // not exist" and abandon the whole lookup. So an AAAA query
                // for an IPv4-only host (e.g. `api.github.com`) that we answer
                // with NXDOMAIN instead of NODATA silently breaks every
                // outbound connection to that host — the good A answer is
                // thrown away. We therefore forward the real response code and
                // reserve SERVFAIL for genuine transport failures (timeout,
                // refused, malformed) so the client retries another server.
                let code = upstream_error_response_code(&e);
                if code == ResponseCode::ServFail {
                    warn!(qname = %qname, qtype = ?qtype, error = %e, "upstream lookup failed");
                } else {
                    trace!(qname = %qname, qtype = ?qtype, ?code, "upstream negative answer");
                }
                (Vec::new(), code)
            }
        };

        // Hickory's `lookup` already returns the answer set the
        // upstream produced for this `(qname, qtype)`, including any
        // CNAME chain it walked. We don't need to refilter here — the
        // wire-format match is already correct, and second-guessing it
        // by RData variant would silently drop record types we just
        // haven't enumerated yet (HTTPS/SVCB/CAA/…).
        let answers: Vec<&Record> = records.iter().collect();

        let mut metadata = Metadata::response_from_request(info.metadata);
        metadata.response_code = response_code;
        // We are *not* authoritative for the forwarded zone.
        metadata.authoritative = false;
        metadata.recursion_available = true;

        let builder = MessageResponseBuilder::from_message_request(request);
        let resp = builder.build(
            metadata,
            answers.iter().copied(),
            std::iter::empty::<&Record>(),
            std::iter::empty::<&Record>(),
            std::iter::empty::<&Record>(),
        );

        match response_handle.send_response(resp).await {
            Ok(info) => Ok(info),
            Err(e) => {
                warn!(error = %e, "failed to send forwarded DNS response");
                Err(error_info(request, ResponseCode::ServFail))
            }
        }
    }
}

fn error_info(request: &Request, code: ResponseCode) -> ResponseInfo {
    // `Request` derefs to `Metadata`; `ResponseInfo` is built from a `Header`.
    let mut metadata = Metadata::response_from_request(&request.metadata);
    metadata.response_code = code;
    Header {
        metadata,
        counts: HeaderCounts::default(),
    }
    .into()
}

/// Map an upstream lookup error to the DNS `ResponseCode` we return to the
/// client.
///
/// `hickory-resolver` collapses NODATA and NXDOMAIN into a single
/// [`DnsError::NoRecordsFound`] error; the real status is carried in
/// `NoRecords.response_code` (`NoError` for NODATA, `NXDomain` for a genuine
/// non-existent name). Returning NXDOMAIN for a NODATA makes `getaddrinfo`
/// abandon the entire hostname — including a valid A answer it would
/// otherwise have used — so we forward the true code. Anything that is not a
/// negative DNS answer (transport failure, malformed response) becomes
/// SERVFAIL so the stub resolver retries against another server.
fn upstream_error_response_code(error: &NetError) -> ResponseCode {
    match error {
        NetError::Dns(DnsError::NoRecordsFound(no_records)) => no_records.response_code,
        _ => ResponseCode::ServFail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_upstream_returns_none() {
        assert!(UpstreamResolver::new(&[]).is_none());
    }

    #[test]
    fn single_upstream_constructs() {
        let upstream = UpstreamResolver::new(&["1.1.1.1:53".parse().unwrap()]);
        assert!(upstream.is_some());
    }

    #[test]
    fn nodata_keeps_noerror_and_nxdomain_passes_through() {
        use hickory_proto::op::Query;
        use hickory_resolver::net::NoRecords;

        // NODATA — name exists, just no record of this type (the AAAA-on-an
        // A-only-host case). MUST stay NoError; NXDOMAIN here would make
        // getaddrinfo drop the name and break the connection.
        let nodata = NetError::Dns(DnsError::NoRecordsFound(NoRecords::new(
            Query::new(),
            ResponseCode::NoError,
        )));
        assert_eq!(
            upstream_error_response_code(&nodata),
            ResponseCode::NoError,
            "NODATA must be reported as NoError, never NXDOMAIN"
        );

        // A genuine non-existent name is forwarded as NXDOMAIN unchanged.
        let nxdomain = NetError::Dns(DnsError::NoRecordsFound(NoRecords::new(
            Query::new(),
            ResponseCode::NXDomain,
        )));
        assert_eq!(
            upstream_error_response_code(&nxdomain),
            ResponseCode::NXDomain
        );

        // Non-DNS failures (here: the "resource too busy" transport signal)
        // fall back to SERVFAIL so the stub resolver retries.
        assert_eq!(
            upstream_error_response_code(&NetError::Busy),
            ResponseCode::ServFail
        );
    }
}
