//! Resolver error type. Every variant is `Send + Sync + 'static` so it can
//! cross task boundaries (the resolver runs as a Tokio task family).

use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ResolverError {
    #[error("Failed to bind DNS UDP socket on {addr}: {source}")]
    UdpBindFailed {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to bind DNS TCP listener on {addr}: {source}")]
    TcpBindFailed {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to read zone snapshot from {path}: {source}")]
    SnapshotRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write zone snapshot to {path}: {source}")]
    SnapshotWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse zone snapshot at {path}: {reason}")]
    SnapshotParse { path: PathBuf, reason: String },

    #[error("Sync HTTP call failed for node {node_id}: {reason}")]
    SyncHttp { node_id: i32, reason: String },

    #[error("Sync response had unexpected shape (node {node_id}, status {status}): {reason}")]
    SyncBadResponse {
        node_id: i32,
        status: u16,
        reason: String,
    },

    #[error("Invalid IP literal {value:?} in record for {fqdn}")]
    InvalidIp { fqdn: String, value: String },

    #[error("Invalid record type {value:?} in record for {fqdn}")]
    InvalidRecordType { fqdn: String, value: String },

    #[error("Internal error: {0}")]
    Internal(String),
}
