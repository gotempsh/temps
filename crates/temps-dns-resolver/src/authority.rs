//! Hickory `RequestHandler` backed by [`ZoneStore`].
//!
//! Implements the minimal slice of the DNS protocol we actually serve:
//! authoritative `A`, `AAAA`, and `CNAME` answers for `*.temps.local`.
//! Queries that fall outside the internal zone are forwarded to the
//! upstream public resolvers configured on the agent (Cloudflare /
//! Google by default) — without this, any container using us as its
//! first nameserver would get NXDOMAIN for everything that isn't
//! `*.temps.local`, breaking package installs and outbound calls.
//!
//! ## Why a hand-rolled handler instead of `Catalog` + `InMemoryAuthority`
//!
//! `Catalog` requires the full SOA/NS machinery and serial-number
//! management that we don't need (and would have to fake for a zone
//! whose contents change every few seconds). The handler is ~80 lines and
//! exercises the same wire-format primitives, so we avoid an awkward
//! impedance match.

use std::sync::Arc;

use hickory_proto::op::{Header, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A as RDataA, AAAA as RDataAAAA, CNAME as RDataCNAME};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use std::net::IpAddr;
use std::str::FromStr;
use tracing::{debug, trace, warn};

use crate::record::ZoneRecord;
use crate::upstream::UpstreamResolver;
use crate::zone_store::ZoneStore;

/// Suffix that identifies our authoritative zone. Anything matching this
/// (case-insensitive, with or without trailing dot) is answered from the
/// `ZoneStore`; everything else is forwarded upstream.
const TEMPS_ZONE_SUFFIX: &str = "temps.local";

pub struct ZoneAuthority {
    zone: Arc<ZoneStore>,
    upstream: Option<Arc<UpstreamResolver>>,
}

impl ZoneAuthority {
    pub fn new(zone: Arc<ZoneStore>) -> Self {
        Self {
            zone,
            upstream: None,
        }
    }

    pub fn with_upstream(mut self, upstream: Arc<UpstreamResolver>) -> Self {
        self.upstream = Some(upstream);
        self
    }
}

fn is_internal_zone(qname: &str) -> bool {
    let trimmed = qname.trim_end_matches('.').to_ascii_lowercase();
    trimmed == TEMPS_ZONE_SUFFIX || trimmed.ends_with(&format!(".{TEMPS_ZONE_SUFFIX}"))
}

#[async_trait::async_trait]
impl RequestHandler for ZoneAuthority {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let info = match request.request_info() {
            Ok(i) => i,
            Err(e) => {
                warn!(error = %e, "failed to parse DNS request");
                return reply_error(request, &mut response_handle, ResponseCode::FormErr).await;
            }
        };

        // Only standard queries are supported.
        if info.header.op_code() != OpCode::Query
            || info.header.message_type() != MessageType::Query
        {
            trace!(
                op = ?info.header.op_code(),
                ty = ?info.header.message_type(),
                "rejecting non-Query DNS message"
            );
            return reply_error(request, &mut response_handle, ResponseCode::NotImp).await;
        }

        let qname: Name = info.query.name().into();
        let qtype = info.query.query_type();
        let qname_str = qname.to_utf8();
        let in_zone = is_internal_zone(&qname_str);

        // Outside-zone queries are forwarded recursively. We are the
        // *only* nameserver app containers see, so falling through to
        // NXDOMAIN here would break `apt-get`, `wget`, package installs,
        // outbound API calls, …
        if !in_zone {
            if let Some(upstream) = &self.upstream {
                trace!(qname = %qname_str, qtype = ?qtype, "forwarding to upstream");
                return upstream
                    .forward(request, &mut response_handle)
                    .await
                    .unwrap_or_else(|info| info);
            }
            // No upstream configured — degrade to NXDOMAIN as the old
            // behaviour did, which keeps strict authoritative-only
            // deployments compatible.
            return reply_error(request, &mut response_handle, ResponseCode::NXDomain).await;
        }

        let snapshot = self.zone.snapshot();
        // First check whether the name exists at all in our zone, then
        // narrow to the requested qtype. The two failure modes have
        // different DNS semantics:
        //   - name exists, no records of this type → NoError, empty
        //     answer (NODATA). Critical for AAAA queries on names that
        //     are A-only — busybox + glibc getaddrinfo treat NXDOMAIN
        //     on either A or AAAA as "host doesn't exist" and refuse
        //     the connection, so returning NXDOMAIN here breaks every
        //     IPv4-only internal name.
        //   - name doesn't exist at all → NXDOMAIN.
        let any_match = snapshot.lookup(&qname_str).next().is_some();
        let matches: Vec<&ZoneRecord> = snapshot
            .lookup(&qname_str)
            .filter(|r| matches_qtype(r, qtype))
            .collect();

        debug!(
            qname = %qname_str,
            qtype = ?qtype,
            answers = matches.len(),
            any_match,
            "DNS query"
        );

        if matches.is_empty() {
            if any_match {
                // NODATA: name exists, just not for this qtype. Reply
                // NoError with no answer rrs and the AA bit set.
                return reply_nodata(request, &mut response_handle, info.header).await;
            }
            // Genuine NXDOMAIN.
            return reply_error(request, &mut response_handle, ResponseCode::NXDomain).await;
        }

        // Build records.
        let answers: Vec<Record> = matches
            .iter()
            .filter_map(|r| build_answer(&qname, r))
            .collect();

        if answers.is_empty() {
            // We had matching FQDN+type rows but none were valid (e.g. all
            // had garbage IPs). Treat as SERVFAIL — the data is broken
            // upstream and the resolver shouldn't lie to the client.
            return reply_error(request, &mut response_handle, ResponseCode::ServFail).await;
        }

        let mut header = Header::response_from_request(info.header);
        header.set_authoritative(true);
        header.set_response_code(ResponseCode::NoError);

        let builder = MessageResponseBuilder::from_message_request(request);
        let resp = builder.build(
            header,
            answers.iter(),
            std::iter::empty::<&Record>(),
            std::iter::empty::<&Record>(),
            std::iter::empty::<&Record>(),
        );

        match response_handle.send_response(resp).await {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "failed to send DNS response");
                error_info(request, ResponseCode::ServFail)
            }
        }
    }
}

fn matches_qtype(record: &ZoneRecord, qtype: RecordType) -> bool {
    let kind = match record.kind() {
        Ok(k) => k,
        Err(_) => return false,
    };
    use crate::record::RecordKind;
    match (kind, qtype) {
        // CNAME is returned for any QTYPE per RFC 1034 §3.6.2; the client
        // re-resolves the target. We don't auto-chase here.
        (RecordKind::Cname, _) => true,
        (RecordKind::A, RecordType::A) => true,
        (RecordKind::Aaaa, RecordType::AAAA) => true,
        (RecordKind::Srv, RecordType::SRV) => true,
        // ANY: return everything we have.
        (_, RecordType::ANY) => true,
        _ => false,
    }
}

fn build_answer(qname: &Name, record: &ZoneRecord) -> Option<Record> {
    let kind = record.kind().ok()?;
    use crate::record::RecordKind;
    let ttl = record.ttl.max(0) as u32;
    let rdata = match kind {
        RecordKind::A => match record.ip().ok()?? {
            IpAddr::V4(v4) => RData::A(RDataA(v4)),
            IpAddr::V6(_) => return None,
        },
        RecordKind::Aaaa => match record.ip().ok()?? {
            IpAddr::V6(v6) => RData::AAAA(RDataAAAA(v6)),
            IpAddr::V4(_) => return None,
        },
        RecordKind::Cname => {
            let target = record.cname_target()?;
            let name = Name::from_str(target).ok()?;
            RData::CNAME(RDataCNAME(name))
        }
        RecordKind::Srv => {
            // SRV is in the schema for forward-compatibility; we don't
            // synthesise weight/priority today. Return None so the answer
            // list filters it out — the schema-level CHECK constraint
            // already prevents anyone *writing* SRV through the registry.
            return None;
        }
    };
    Some(Record::from_rdata(qname.clone(), ttl, rdata))
}

async fn reply_error<R: ResponseHandler>(
    request: &Request,
    response_handle: &mut R,
    code: ResponseCode,
) -> ResponseInfo {
    let builder = MessageResponseBuilder::from_message_request(request);
    let resp = builder.error_msg(request.header(), code);
    match response_handle.send_response(resp).await {
        Ok(info) => info,
        Err(e) => {
            warn!(error = %e, "failed to send DNS error response");
            error_info(request, code)
        }
    }
}

/// Reply NODATA — name exists in the zone, but no records of the
/// requested type. Status is NoError, AA is set, answer/authority/
/// additional sections are empty. Resolvers and stub libraries
/// (glibc getaddrinfo, busybox) treat this as "try the other type"
/// instead of giving up on the name entirely.
async fn reply_nodata<R: ResponseHandler>(
    request: &Request,
    response_handle: &mut R,
    request_header: &Header,
) -> ResponseInfo {
    let mut header = Header::response_from_request(request_header);
    header.set_authoritative(true);
    header.set_response_code(ResponseCode::NoError);

    let builder = MessageResponseBuilder::from_message_request(request);
    let resp = builder.build(
        header,
        std::iter::empty::<&Record>(),
        std::iter::empty::<&Record>(),
        std::iter::empty::<&Record>(),
        std::iter::empty::<&Record>(),
    );
    match response_handle.send_response(resp).await {
        Ok(info) => info,
        Err(e) => {
            warn!(error = %e, "failed to send DNS NODATA response");
            error_info(request, ResponseCode::NoError)
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
    use crate::record::ZoneRecord;
    use std::path::PathBuf;

    fn rec(record_type: &str, target: &str) -> ZoneRecord {
        ZoneRecord {
            id: 1,
            fqdn: "x.temps.local".into(),
            record_type: record_type.into(),
            target_ip: Some(target.into()),
            target_port: None,
            ttl: 30,
            owner_kind: "static".into(),
            owner_id: 1,
            node_id: None,
            generation: 1,
        }
    }

    #[test]
    fn matches_qtype_a_for_a_query() {
        assert!(matches_qtype(&rec("A", "1.2.3.4"), RecordType::A));
        assert!(!matches_qtype(&rec("A", "1.2.3.4"), RecordType::AAAA));
    }

    #[test]
    fn matches_qtype_cname_matches_any_query_type() {
        assert!(matches_qtype(&rec("CNAME", "y.temps.local"), RecordType::A));
        assert!(matches_qtype(
            &rec("CNAME", "y.temps.local"),
            RecordType::AAAA
        ));
    }

    #[test]
    fn matches_any_returns_all() {
        assert!(matches_qtype(&rec("A", "1.2.3.4"), RecordType::ANY));
        assert!(matches_qtype(&rec("AAAA", "fd00::1"), RecordType::ANY));
    }

    #[test]
    fn build_answer_emits_a_record() {
        let qname = Name::from_str("x.temps.local.").unwrap();
        let answer = build_answer(&qname, &rec("A", "172.20.5.10")).unwrap();
        assert_eq!(answer.ttl(), 30);
        match answer.data() {
            RData::A(RDataA(v4)) => assert_eq!(v4.to_string(), "172.20.5.10"),
            other => panic!("expected A, got {other:?}"),
        }
    }

    #[test]
    fn build_answer_skips_garbage_ip() {
        let qname = Name::from_str("x.temps.local.").unwrap();
        assert!(build_answer(&qname, &rec("A", "not.an.ip")).is_none());
    }

    #[test]
    fn build_answer_emits_aaaa_record() {
        let qname = Name::from_str("x.temps.local.").unwrap();
        let answer = build_answer(&qname, &rec("AAAA", "fd00::1")).unwrap();
        match answer.data() {
            RData::AAAA(RDataAAAA(v6)) => assert!(v6.to_string().contains("fd00")),
            other => panic!("expected AAAA, got {other:?}"),
        }
    }

    /// Smoke test that `ZoneAuthority::new` accepts an `Arc<ZoneStore>`.
    /// The full request-handling path is covered by the integration test
    /// in tests/end_to_end.rs (real UDP socket + hickory client).
    #[test]
    fn authority_constructs() {
        let zone = Arc::new(ZoneStore::new(PathBuf::from("/dev/null")));
        let _ = ZoneAuthority::new(zone);
    }
}
