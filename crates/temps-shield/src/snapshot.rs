// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Shield snapshot (ADR 066, I3): every list an ingress matches
//! against, in one compact, versioned body.
//!
//! Each list's networks are collapsed (nested ones dropped), sorted and
//! stored as the distance from the previous network's start plus a prefix
//! length. postcard writes integers as LEB128 varints, so the deltas of a
//! dense list take a byte or two each, and zstd then shrinks the body
//! further.

use std::io::Read;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::net::{collapse, mask4, mask6, Network};
use crate::verdict::ListVerdict;

/// Decompressed bodies larger than this are refused, so a corrupt or
/// hostile body cannot exhaust memory. The full IPv6 bogon list, the
/// largest, is about 1 MB decompressed.
const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
const ZSTD_LEVEL: i32 = 19;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("the snapshot could not be encoded: {0}")]
    Encode(postcard::Error),
    #[error("the snapshot could not be decoded: {0}")]
    Decode(postcard::Error),
    #[error("the snapshot could not be compressed or decompressed: {0}")]
    Compression(#[from] std::io::Error),
    #[error("the snapshot decompresses to more than {MAX_DECODED_BYTES} bytes")]
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShieldSnapshot {
    /// 0 for the built-in baseline; stored revisions start at 1.
    pub revision: u64,
    pub generated_at: i64,
    pub lists: Vec<SourceList>,
}

impl ShieldSnapshot {
    pub fn list(&self, source: &str) -> Option<&SourceList> {
        self.lists.iter().find(|list| list.source == source)
    }

    /// This snapshot with `list` in place of the list of the same source.
    pub fn with_list(&self, list: SourceList) -> Vec<SourceList> {
        let mut lists: Vec<SourceList> = self
            .lists
            .iter()
            .filter(|existing| existing.source != list.source)
            .cloned()
            .collect();
        lists.push(list);
        lists.sort_by(|a, b| a.source.cmp(&b.source));
        lists
    }

    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        decode(bytes)
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, SnapshotError> {
    let body = postcard::to_allocvec(value).map_err(SnapshotError::Encode)?;
    Ok(zstd::encode_all(body.as_slice(), ZSTD_LEVEL)?)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, SnapshotError> {
    let mut body = Vec::new();
    zstd::Decoder::new(bytes)?
        .take(MAX_DECODED_BYTES + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_DECODED_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    postcard::from_bytes(&body).map_err(SnapshotError::Decode)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceList {
    pub source: String,
    pub verdict: ListVerdict,
    /// When the publisher says the list was made, else when it was fetched.
    pub published_at: i64,
    /// `None` for lists that never go stale.
    pub max_age_secs: Option<u64>,
    /// (start minus the previous start, prefix), collapsed and sorted.
    v4: Vec<(u32, u8)>,
    v6: Vec<(u128, u8)>,
}

impl SourceList {
    pub fn new(
        source: impl Into<String>,
        verdict: ListVerdict,
        published_at: i64,
        max_age_secs: Option<u64>,
        networks: impl IntoIterator<Item = Network>,
    ) -> Self {
        let (mut v4, mut v6) = (Vec::new(), Vec::new());
        for network in networks {
            match network {
                Network::V4 { start, prefix } => v4.push((start, prefix)),
                Network::V6 { start, prefix } => v6.push((start, prefix)),
            }
        }
        collapse(&mut v4, |(start, prefix), x| x & mask4(prefix) == start);
        collapse(&mut v6, |(start, prefix), x| x & mask6(prefix) == start);
        Self {
            source: source.into(),
            verdict,
            published_at,
            max_age_secs,
            v4: deltas(&v4, u32::wrapping_sub),
            v6: deltas(&v6, u128::wrapping_sub),
        }
    }

    /// One list on its own, as the control plane keeps the last good copy
    /// of each source.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        decode(bytes)
    }

    /// SHA-256 of the networks alone, so a list fetched again with nothing
    /// changed but its date compares equal.
    pub fn digest(&self) -> Result<[u8; 32], SnapshotError> {
        let networks =
            postcard::to_allocvec(&(&self.v4, &self.v6)).map_err(SnapshotError::Encode)?;
        Ok(Sha256::digest(&networks).into())
    }

    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The IPv4 networks, collapsed and sorted, as (start, prefix).
    pub(crate) fn v4(&self) -> Vec<(u32, u8)> {
        starts(&self.v4, u32::wrapping_add)
    }

    pub(crate) fn v6(&self) -> Vec<(u128, u8)> {
        starts(&self.v6, u128::wrapping_add)
    }

    pub fn networks(&self) -> impl Iterator<Item = Network> + '_ {
        let v4 = self
            .v4()
            .into_iter()
            .map(|(start, prefix)| Network::V4 { start, prefix });
        let v6 = self
            .v6()
            .into_iter()
            .map(|(start, prefix)| Network::V6 { start, prefix });
        v4.chain(v6)
    }
}

/// Sorted starts to differences from the previous start.
fn deltas<T: Copy + Default>(sorted: &[(T, u8)], sub: impl Fn(T, T) -> T) -> Vec<(T, u8)> {
    let mut previous = T::default();
    sorted
        .iter()
        .map(|&(start, prefix)| {
            let delta = sub(start, previous);
            previous = start;
            (delta, prefix)
        })
        .collect()
}

fn starts<T: Copy + Default>(deltas: &[(T, u8)], add: impl Fn(T, T) -> T) -> Vec<(T, u8)> {
    let mut previous = T::default();
    deltas
        .iter()
        .map(|&(delta, prefix)| {
            previous = add(previous, delta);
            (previous, prefix)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(texts: &[&str]) -> Vec<Network> {
        texts.iter().map(|t| Network::parse(t).unwrap()).collect()
    }

    #[test]
    fn a_list_keeps_its_networks_collapsed_and_sorted() {
        let list = SourceList::new(
            "test",
            ListVerdict::Block,
            1,
            None,
            nets(&[
                "11.0.0.0/8",
                "10.1.0.0/16",
                "10.0.0.0/8",
                "2001:db8::/32",
                "1.2.3.4",
            ]),
        );
        let got: Vec<String> = list.networks().map(|n| n.to_string()).collect();
        assert_eq!(
            got,
            ["1.2.3.4/32", "10.0.0.0/8", "11.0.0.0/8", "2001:db8::/32"]
        );
        assert_eq!(list.len(), 4);
    }

    #[test]
    fn snapshots_round_trip_through_the_compressed_body() {
        let snapshot = ShieldSnapshot {
            revision: 7,
            generated_at: 1_800_000_000,
            lists: vec![
                SourceList::new(
                    "a",
                    ListVerdict::Log,
                    5,
                    Some(60),
                    nets(&["0.0.0.0/0", "::/0"]),
                ),
                SourceList::new("b", ListVerdict::Block, 6, None, nets(&["255.255.255.255"])),
            ],
        };
        let bytes = snapshot.encode().unwrap();
        assert_eq!(ShieldSnapshot::decode(&bytes).unwrap(), snapshot);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(ShieldSnapshot::decode(b"not zstd").is_err());
        let not_postcard = zstd::encode_all(&b"\xff\xff\xff\xff\xff"[..], 1).unwrap();
        assert!(ShieldSnapshot::decode(&not_postcard).is_err());
    }

    /// The densest list there is, the IPv6 full bogons, is about 158,000
    /// networks; delta coding keeps a dense list to a few bytes per entry.
    #[test]
    fn dense_lists_stay_small() {
        let networks = (0..100_000u128).map(|i| Network::V6 {
            start: (0x2a00u128 << 112) | (i << 80),
            prefix: 48,
        });
        let snapshot = ShieldSnapshot {
            revision: 1,
            generated_at: 0,
            lists: vec![SourceList::new(
                "dense",
                ListVerdict::Log,
                0,
                None,
                networks,
            )],
        };
        let bytes = snapshot.encode().unwrap();
        assert!(bytes.len() < 100_000, "{} bytes", bytes.len());
    }

    #[test]
    fn a_list_round_trips_and_its_digest_ignores_the_date() {
        let list = SourceList::new("a", ListVerdict::Block, 5, Some(60), nets(&["10.0.0.0/8"]));
        assert_eq!(SourceList::decode(&list.encode().unwrap()).unwrap(), list);
        let mut later = list.clone();
        later.published_at = 6;
        assert_eq!(later.digest().unwrap(), list.digest().unwrap());
        let other = SourceList::new("a", ListVerdict::Block, 5, Some(60), nets(&["10.0.0.0/9"]));
        assert_ne!(other.digest().unwrap(), list.digest().unwrap());
    }

    #[test]
    fn replacing_one_list_keeps_the_others() {
        let snapshot = ShieldSnapshot {
            revision: 1,
            generated_at: 0,
            lists: vec![
                SourceList::new("a", ListVerdict::Log, 1, None, nets(&["10.0.0.0/8"])),
                SourceList::new("b", ListVerdict::Log, 1, None, nets(&["11.0.0.0/8"])),
            ],
        };
        let lists = snapshot.with_list(SourceList::new("a", ListVerdict::Block, 2, None, []));
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].verdict, ListVerdict::Block);
        assert!(lists[0].is_empty());
        assert_eq!(lists[1], snapshot.lists[1]);
    }
}
