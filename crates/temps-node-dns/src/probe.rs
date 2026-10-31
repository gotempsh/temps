// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Readiness probe for a running resolver.
//!
//! A resolver's address is only handed to containers once it answers this
//! probe: a fixed TXT marker served from the resolver's own zone, asked over
//! UDP with a one-second deadline. Task liveness alone is not enough -- a
//! resolver whose socket stopped answering would otherwise stay published,
//! and every container created meanwhile would pay a timeout per lookup.

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU16, Ordering},
    OnceLock,
};
use std::time::Duration;

use hickory_proto::{
    op::{Message, MessageType, OpCode, Query, ResponseCode},
    rr::{DNSClass, Name, RData, RecordType},
};

/// Whether the resolver at `address` answers the readiness marker
/// ([`crate::RESOLVER_MARKER_NAME`]) authoritatively within one second.
pub async fn probe_resolver(address: SocketAddr) -> bool {
    let bind_address = if address.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = match tokio::net::UdpSocket::bind(bind_address).await {
        Ok(socket) => socket,
        Err(_) => return false,
    };
    if socket.connect(address).await.is_err() {
        return false;
    }
    static NEXT_ID: OnceLock<AtomicU16> = OnceLock::new();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            (duration.as_nanos() as u16) ^ std::process::id() as u16
        });
    let id = NEXT_ID
        .get_or_init(|| AtomicU16::new(seed))
        .fetch_add(1, Ordering::Relaxed);
    let mut marker_name = match Name::from_ascii(crate::RESOLVER_MARKER_NAME) {
        Ok(name) => name,
        Err(_) => return false,
    };
    // Wire-decoded names are absolute; compare against an absolute question.
    marker_name.set_fqdn(true);
    let mut query = Message::new(id, MessageType::Query, OpCode::Query);
    query.add_query(Query::query(marker_name.clone(), RecordType::TXT));
    let wire = match query.to_vec() {
        Ok(wire) => wire,
        Err(_) => return false,
    };
    if socket.send(&wire).await.is_err() {
        return false;
    }
    let mut response = [0u8; 512];
    let received =
        match tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut response)).await {
            Ok(Ok(received)) => received,
            _ => return false,
        };
    let Ok(response) = Message::from_vec(&response[..received]) else {
        return false;
    };
    let metadata = response.metadata;
    let question_matches = response.queries.as_slice().first().is_some_and(|question| {
        response.queries.len() == 1
            && question.name() == &marker_name
            && question.query_type() == RecordType::TXT
            && question.query_class() == DNSClass::IN
    });
    let answer_matches = response.answers.as_slice().first().is_some_and(|answer| {
        response.answers.len() == 1
            && answer.name == marker_name
            && answer.record_type() == RecordType::TXT
            && answer.dns_class == DNSClass::IN
            && matches!(&answer.data, RData::TXT(txt) if txt.txt_data.len() == 1 && txt.txt_data[0].as_ref() == crate::RESOLVER_MARKER_VALUE.as_bytes())
    });
    metadata.id == id
        && metadata.message_type == MessageType::Response
        && metadata.op_code == OpCode::Query
        && metadata.authoritative
        && !metadata.truncation
        && metadata.response_code == ResponseCode::NoError
        && response.authorities.is_empty()
        && response.additionals.is_empty()
        && question_matches
        && answer_matches
}
