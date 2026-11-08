// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test resolution (ADR-095 §5): ask a resolver for a name and report the
//! answer, its response code and how long it took.
//!
//! Two ways in:
//! - [`ZoneAuthority::resolve_as`] answers in-process, as if `source` had
//!   asked, so a restricted workload's network policy applies. This is what
//!   the agent uses for "resolve as this project's container".
//! - [`resolve_over_udp`] asks a resolver over the network, as any client
//!   would, from this host's own address.
//!
//! Neither is counted or logged by the resolver's observer: a test from the
//! console is not traffic.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::BinEncoder;
use hickory_server::net::xfer::Protocol;
use hickory_server::net::NetError;
use hickory_server::server::{Request, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponse;
use serde::{Deserialize, Serialize};

use crate::authority::ZoneAuthority;
use crate::error::ResolverError;
use crate::observe::{QueryOutcome, MAX_QUERY_NAME_LEN};

/// How long a test query over the network may take. Longer than the
/// upstream budget (2 s), so a slow forwarded name still answers SERVFAIL
/// instead of timing out here.
pub const TEST_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Record types a test resolution may ask for.
pub const TEST_QUERY_TYPES: [&str; 5] = ["A", "AAAA", "CNAME", "SRV", "TXT"];

/// One answer record, in presentation form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRecord {
    pub name: String,
    pub record_type: String,
    pub ttl: u32,
    pub data: String,
}

/// What a test resolution got back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveAnswer {
    /// The response code as sent (`NOERROR`, `NXDOMAIN`, ...).
    pub rcode: String,
    pub outcome: QueryOutcome,
    /// Whether the answer came from the internal zone (the AA bit).
    pub authoritative: bool,
    pub answers: Vec<ResolvedRecord>,
    pub latency_us: u64,
}

/// Parse a record type a test may ask for.
pub fn parse_test_query_type(value: &str) -> Result<RecordType, ResolverError> {
    let upper = value.trim().to_ascii_uppercase();
    if !TEST_QUERY_TYPES.contains(&upper.as_str()) {
        return Err(ResolverError::UnsupportedQueryType {
            value: value.to_string(),
            supported: TEST_QUERY_TYPES.join(", "),
        });
    }
    RecordType::from_str(&upper).map_err(|e| ResolverError::UnsupportedQueryType {
        value: format!("{value} ({e})"),
        supported: TEST_QUERY_TYPES.join(", "),
    })
}

/// Parse a name a test may ask for: a valid DNS name, at most 253 bytes,
/// made absolute so a search domain is never appended.
pub fn parse_test_query_name(value: &str) -> Result<Name, ResolverError> {
    let trimmed = value.trim();
    let invalid = |reason: String| ResolverError::InvalidQueryName {
        name: trimmed.chars().take(MAX_QUERY_NAME_LEN).collect(),
        reason,
    };
    if trimmed.is_empty() || trimmed == "." {
        return Err(invalid("the name is empty".into()));
    }
    if trimmed.trim_end_matches('.').len() > MAX_QUERY_NAME_LEN {
        return Err(invalid(format!(
            "longer than {MAX_QUERY_NAME_LEN} characters"
        )));
    }
    let mut name = Name::from_ascii(trimmed).map_err(|e| invalid(e.to_string()))?;
    name.set_fqdn(true);
    Ok(name)
}

fn query_message(name: Name, qtype: RecordType) -> Message {
    let id = (std::process::id() as u16)
        ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as u16));
    let mut message = Message::new(id, MessageType::Query, OpCode::Query);
    message.metadata.recursion_desired = true;
    message.add_query(Query::query(name, qtype));
    message
}

fn record_data(data: &RData) -> String {
    match data {
        RData::A(a) => a.0.to_string(),
        RData::AAAA(aaaa) => aaaa.0.to_string(),
        RData::CNAME(cname) => cname.0.to_utf8(),
        other => other.to_string(),
    }
}

/// The response code as `dig` prints it: `NOERROR`, `NXDOMAIN`, `SERVFAIL`.
/// hickory's own `to_str` is prose ("No Error").
pub fn rcode_mnemonic(code: ResponseCode) -> String {
    match code {
        ResponseCode::NoError => "NOERROR".into(),
        ResponseCode::FormErr => "FORMERR".into(),
        ResponseCode::ServFail => "SERVFAIL".into(),
        ResponseCode::NXDomain => "NXDOMAIN".into(),
        ResponseCode::NotImp => "NOTIMP".into(),
        ResponseCode::Refused => "REFUSED".into(),
        ResponseCode::YXDomain => "YXDOMAIN".into(),
        ResponseCode::YXRRSet => "YXRRSET".into(),
        ResponseCode::NXRRSet => "NXRRSET".into(),
        ResponseCode::NotAuth => "NOTAUTH".into(),
        ResponseCode::NotZone => "NOTZONE".into(),
        other => format!("RCODE{}", u16::from(other)),
    }
}

fn answer_from_message(response: &Message, latency: Duration) -> ResolveAnswer {
    let answers: Vec<ResolvedRecord> = response
        .answers
        .iter()
        .map(|record: &Record| ResolvedRecord {
            name: record.name.to_utf8().trim_end_matches('.').to_string(),
            record_type: record.record_type().to_string(),
            ttl: record.ttl,
            data: record_data(&record.data),
        })
        .collect();
    let code = response.metadata.response_code;
    ResolveAnswer {
        rcode: rcode_mnemonic(code),
        outcome: QueryOutcome::classify(
            code,
            u16::try_from(answers.len()).unwrap_or(u16::MAX),
            false,
        ),
        authoritative: response.metadata.authoritative,
        answers,
        latency_us: u64::try_from(latency.as_micros()).unwrap_or(u64::MAX),
    }
}

/// Captures the response the authority would have sent.
#[derive(Clone, Default)]
struct CaptureResponse {
    bytes: Arc<Mutex<Option<Vec<u8>>>>,
}

#[async_trait::async_trait]
impl ResponseHandler for CaptureResponse {
    async fn send_response<'a>(
        &mut self,
        response: MessageResponse<
            '_,
            'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
            impl Iterator<Item = &'a Record> + Send + 'a,
        >,
    ) -> Result<ResponseInfo, NetError> {
        let mut buffer = Vec::with_capacity(512);
        let mut encoder = BinEncoder::new(&mut buffer);
        let info = response.destructive_emit(&mut encoder)?;
        if let Ok(mut slot) = self.bytes.lock() {
            *slot = Some(buffer);
        }
        Ok(info)
    }
}

impl ZoneAuthority {
    /// Answer `name`/`qtype` as this authority would answer `source`,
    /// network policy included. Not counted or logged.
    pub async fn resolve_as(
        &self,
        source: IpAddr,
        name: Name,
        qtype: RecordType,
    ) -> Result<ResolveAnswer, ResolverError> {
        let shown = name.to_utf8();
        let failed = |reason: String| ResolverError::TestQueryFailed {
            name: shown.clone(),
            server: format!("in-process, as {source}"),
            reason,
        };
        let wire = query_message(name, qtype)
            .to_vec()
            .map_err(|e| failed(format!("encode query: {e}")))?;
        // TCP: the in-process answer is not limited to 512 bytes.
        let request = Request::from_bytes(wire, SocketAddr::new(source, 0), Protocol::Tcp)
            .map_err(|e| failed(format!("decode query: {e}")))?;
        let mut capture = CaptureResponse::default();
        let started = Instant::now();
        let _ = self.answer(&request, &mut capture).await;
        let latency = started.elapsed();
        let bytes = capture
            .bytes
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .ok_or_else(|| failed("the resolver sent no response".into()))?;
        let response =
            Message::from_vec(&bytes).map_err(|e| failed(format!("decode response: {e}")))?;
        Ok(answer_from_message(&response, latency))
    }
}

/// Ask the resolver at `server` for `name`/`qtype` over UDP, retrying over
/// TCP when the answer is truncated.
pub async fn resolve_over_udp(
    server: SocketAddr,
    name: Name,
    qtype: RecordType,
) -> Result<ResolveAnswer, ResolverError> {
    let shown = name.to_utf8();
    let failed = |reason: String| ResolverError::TestQueryFailed {
        name: shown.clone(),
        server: server.to_string(),
        reason,
    };
    let message = query_message(name, qtype);
    let id = message.metadata.id;
    let wire = message
        .to_vec()
        .map_err(|e| failed(format!("encode query: {e}")))?;
    let started = Instant::now();
    let exchange = async {
        let bind = if server.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let socket = tokio::net::UdpSocket::bind(bind)
            .await
            .map_err(|e| failed(format!("bind a UDP socket: {e}")))?;
        socket
            .connect(server)
            .await
            .map_err(|e| failed(format!("connect: {e}")))?;
        socket
            .send(&wire)
            .await
            .map_err(|e| failed(format!("send: {e}")))?;
        let mut buffer = vec![0u8; 4096];
        loop {
            let received = socket
                .recv(&mut buffer)
                .await
                .map_err(|e| failed(format!("receive: {e}")))?;
            let Ok(response) = Message::from_vec(&buffer[..received]) else {
                continue;
            };
            if response.metadata.id == id && response.metadata.message_type == MessageType::Response
            {
                return Ok(response);
            }
        }
    };
    let response = tokio::time::timeout(TEST_QUERY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            failed(format!(
                "no answer within {} s",
                TEST_QUERY_TIMEOUT.as_secs()
            ))
        })??;
    let latency = started.elapsed();
    if response.metadata.truncation {
        return resolve_over_tcp(server, &wire, id, started, &failed).await;
    }
    Ok(answer_from_message(&response, latency))
}

async fn resolve_over_tcp(
    server: SocketAddr,
    wire: &[u8],
    id: u16,
    started: Instant,
    failed: &(dyn Fn(String) -> ResolverError + Sync),
) -> Result<ResolveAnswer, ResolverError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(server)
            .await
            .map_err(|e| failed(format!("TCP connect: {e}")))?;
        let length = u16::try_from(wire.len()).map_err(|_| failed("query too large".into()))?;
        stream
            .write_all(&length.to_be_bytes())
            .await
            .map_err(|e| failed(format!("TCP send: {e}")))?;
        stream
            .write_all(wire)
            .await
            .map_err(|e| failed(format!("TCP send: {e}")))?;
        let mut prefix = [0u8; 2];
        stream
            .read_exact(&mut prefix)
            .await
            .map_err(|e| failed(format!("TCP receive: {e}")))?;
        let mut buffer = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
        stream
            .read_exact(&mut buffer)
            .await
            .map_err(|e| failed(format!("TCP receive: {e}")))?;
        Message::from_vec(&buffer).map_err(|e| failed(format!("decode response: {e}")))
    };
    let response = tokio::time::timeout(TEST_QUERY_TIMEOUT, exchange)
        .await
        .map_err(|_| failed("no TCP answer in time".into()))??;
    if response.metadata.id != id {
        return Err(failed("the TCP answer was for another query".into()));
    }
    Ok(answer_from_message(&response, started.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_codes_are_the_dig_mnemonics() {
        assert_eq!(rcode_mnemonic(ResponseCode::NoError), "NOERROR");
        assert_eq!(rcode_mnemonic(ResponseCode::NXDomain), "NXDOMAIN");
        assert_eq!(rcode_mnemonic(ResponseCode::ServFail), "SERVFAIL");
        assert_eq!(rcode_mnemonic(ResponseCode::Refused), "REFUSED");
        assert_eq!(rcode_mnemonic(ResponseCode::BADCOOKIE), "RCODE23");
    }

    #[test]
    fn query_types_are_limited() {
        assert_eq!(parse_test_query_type("a").ok(), Some(RecordType::A));
        assert_eq!(parse_test_query_type(" AAAA ").ok(), Some(RecordType::AAAA));
        assert!(matches!(
            parse_test_query_type("AXFR"),
            Err(ResolverError::UnsupportedQueryType { .. })
        ));
        assert!(parse_test_query_type("").is_err());
    }

    #[test]
    fn names_are_validated_and_made_absolute() {
        let name = parse_test_query_name("production.shop.temps.local").expect("valid");
        assert!(name.is_fqdn());
        assert_eq!(name.to_utf8(), "production.shop.temps.local.");
        assert!(matches!(
            parse_test_query_name("  "),
            Err(ResolverError::InvalidQueryName { .. })
        ));
        let long = format!("{}.temps.local", "a.".repeat(140));
        let error = parse_test_query_name(&long).expect_err("too long");
        assert!(error.to_string().contains("longer than 253"), "{error}");
        assert!(parse_test_query_name("bad..name").is_err());
    }
}
