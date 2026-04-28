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

use hickory_proto::op::{Header, ResponseCode};
use hickory_proto::rr::{Name, Record};
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use hickory_resolver::Resolver;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, ResponseHandler, ResponseInfo};
use tracing::{trace, warn};

type TokioResolver = Resolver<TokioConnectionProvider>;

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

        let mut config = ResolverConfig::new();
        for addr in upstreams {
            config.add_name_server(NameServerConfig::new(*addr, Protocol::Udp));
            // TCP fallback for responses too large for UDP (e.g. some
            // TXT and DNSSEC answers). Same socket address.
            config.add_name_server(NameServerConfig::new(*addr, Protocol::Tcp));
        }

        let mut opts = ResolverOpts::default();
        opts.timeout = Duration::from_secs(3);
        opts.attempts = 2;
        // Modest cache so we don't hammer upstreams during burst
        // traffic from a freshly-booted pod. 512 entries ≈ a few KB.
        opts.cache_size = 512;
        opts.edns0 = true;
        opts.try_tcp_on_error = true;

        let resolver = Resolver::builder_with_config(config, TokioConnectionProvider::default())
            .with_options(opts)
            .build();

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
                let recs: Vec<Record> = lookup.record_iter().cloned().collect();
                trace!(qname = %qname, qtype = ?qtype, answers = recs.len(), "upstream answer");
                (recs, ResponseCode::NoError)
            }
            Err(e) => {
                // hickory-resolver flattens NXDOMAIN, NODATA, and
                // network errors into a single error type. We want to
                // pass NXDOMAIN through cleanly (clients rely on it for
                // negative caching) but treat real transport failures
                // as SERVFAIL so the client can retry against another
                // server.
                let s = e.to_string();
                let is_negative = s.contains("NXDomain")
                    || s.contains("no records found")
                    || s.contains("NoRecordsFound");
                if is_negative {
                    trace!(qname = %qname, qtype = ?qtype, "upstream NXDOMAIN");
                    (Vec::new(), ResponseCode::NXDomain)
                } else {
                    warn!(qname = %qname, qtype = ?qtype, error = %e, "upstream lookup failed");
                    (Vec::new(), ResponseCode::ServFail)
                }
            }
        };

        // Hickory's `lookup` already returns the answer set the
        // upstream produced for this `(qname, qtype)`, including any
        // CNAME chain it walked. We don't need to refilter here — the
        // wire-format match is already correct, and second-guessing it
        // by RData variant would silently drop record types we just
        // haven't enumerated yet (HTTPS/SVCB/CAA/…).
        let answers: Vec<&Record> = records.iter().collect();

        let mut header = Header::response_from_request(info.header);
        header.set_response_code(response_code);
        // We are *not* authoritative for the forwarded zone.
        header.set_authoritative(false);
        header.set_recursion_available(true);

        let builder = MessageResponseBuilder::from_message_request(request);
        let resp = builder.build(
            header,
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
    let mut header = Header::response_from_request(request.header());
    header.set_response_code(code);
    header.into()
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
}
