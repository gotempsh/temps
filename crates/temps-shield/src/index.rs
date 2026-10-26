// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The in-memory form of a snapshot, searched on every request (ADR 066, I3).
//!
//! Each list keeps its networks sorted and disjoint, so the only network
//! that can hold an address is the last one starting at or before it: one
//! binary search per list and family, no allocation unless something
//! matches.

use std::net::IpAddr;
use std::sync::Arc;

use crate::baseline;
use crate::net::{collapse, mask4, mask6, Address, Network};
use crate::snapshot::{ShieldSnapshot, SourceList};
use crate::verdict::{Action, ListVerdict, Reason, Verdict};

pub struct Index {
    revision: u64,
    lists: Vec<IndexedList>,
}

struct IndexedList {
    source: Arc<str>,
    verdict: ListVerdict,
    published_at: i64,
    max_age_secs: Option<u64>,
    v4: Vec<(u32, u8)>,
    v6: Vec<(u128, u8)>,
}

impl IndexedList {
    fn new(list: &SourceList) -> Self {
        // A stored body is trusted to be collapsed and sorted, but the
        // lookup is only correct if it is, so make sure.
        let mut v4: Vec<_> = list.v4().into_iter().filter(|&(_, p)| p <= 32).collect();
        let mut v6: Vec<_> = list.v6().into_iter().filter(|&(_, p)| p <= 128).collect();
        collapse(&mut v4, |(start, prefix), x| x & mask4(prefix) == start);
        collapse(&mut v6, |(start, prefix), x| x & mask6(prefix) == start);
        Self {
            source: Arc::from(list.source.as_str()),
            verdict: list.verdict,
            published_at: list.published_at,
            max_age_secs: list.max_age_secs,
            v4,
            v6,
        }
    }

    fn find(&self, address: Address) -> Option<Network> {
        match address {
            Address::V4(x) => {
                let i = self.v4.partition_point(|&(start, _)| start <= x);
                let (start, prefix) = *self.v4.get(i.checked_sub(1)?)?;
                (x & mask4(prefix) == start).then_some(Network::V4 { start, prefix })
            }
            Address::V6(x) => {
                let i = self.v6.partition_point(|&(start, _)| start <= x);
                let (start, prefix) = *self.v6.get(i.checked_sub(1)?)?;
                (x & mask6(prefix) == start).then_some(Network::V6 { start, prefix })
            }
        }
    }

    fn is_stale(&self, now: i64) -> bool {
        self.max_age_secs.is_some_and(|max_age| {
            let age = now.saturating_sub(self.published_at);
            u64::try_from(age).is_ok_and(|age| age > max_age)
        })
    }
}

impl Index {
    /// The built-in lists alone, for an ingress that has not loaded a
    /// snapshot yet.
    pub fn baseline() -> Self {
        Self::new(&ShieldSnapshot {
            revision: 0,
            generated_at: 0,
            lists: Vec::new(),
        })
    }

    /// The snapshot's lists plus the built-in ones. A stored list with the
    /// name of a built-in one replaces it.
    pub fn new(snapshot: &ShieldSnapshot) -> Self {
        let mut lists: Vec<IndexedList> = snapshot.lists.iter().map(IndexedList::new).collect();
        for list in baseline::lists() {
            if snapshot.list(&list.source).is_none() {
                lists.push(IndexedList::new(&list));
            }
        }
        Self {
            revision: snapshot.revision,
            lists,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every list holding `ip`, and the strongest verdict among them. A list
    /// past its maximum age only logs.
    pub fn evaluate(&self, ip: IpAddr, now: i64) -> Verdict {
        let address = Address::from(ip);
        let mut verdict = Verdict::PASS;
        for list in &self.lists {
            let Some(network) = list.find(address) else {
                continue;
            };
            let stale = list.is_stale(now);
            let action = if stale {
                Action::Log
            } else {
                Action::from(list.verdict)
            };
            verdict.action = verdict.action.max(action);
            verdict.reasons.push(Reason {
                source: Arc::clone(&list.source),
                network,
                stale,
            });
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn list(source: &str, verdict: ListVerdict, nets: &[&str]) -> SourceList {
        let nets = nets.iter().map(|t| Network::parse(t).unwrap());
        SourceList::new(source, verdict, NOW, Some(3600), nets)
    }

    fn index(lists: Vec<SourceList>) -> Index {
        Index::new(&ShieldSnapshot {
            revision: 3,
            generated_at: NOW,
            lists,
        })
    }

    #[test]
    fn an_address_on_no_list_passes() {
        let index = index(vec![list("drop", ListVerdict::Block, &["185.93.89.0/24"])]);
        assert_eq!(index.evaluate(ip("8.8.8.8"), NOW), Verdict::PASS);
        assert_eq!(index.evaluate(ip("185.93.90.1"), NOW), Verdict::PASS);
        assert_eq!(index.evaluate(ip("2606:4700::1"), NOW), Verdict::PASS);
        assert_eq!(index.revision(), 3);
    }

    #[test]
    fn the_strongest_list_wins_and_every_match_is_a_reason() {
        let index = index(vec![
            list("bogons", ListVerdict::Log, &["185.0.0.0/8"]),
            list(
                "drop",
                ListVerdict::Block,
                &["10.0.0.0/8", "185.93.89.0/24", "2001:678:254::/48"],
            ),
            list("scanners", ListVerdict::Challenge, &["185.93.89.7"]),
        ]);
        let verdict = index.evaluate(ip("185.93.89.7"), NOW);
        assert_eq!(verdict.action, Action::Block);
        assert_eq!(
            verdict.reason_strings(),
            [
                "bogons (185.0.0.0/8)",
                "drop (185.93.89.0/24)",
                "scanners (185.93.89.7/32)"
            ]
        );
        assert_eq!(index.evaluate(ip("185.1.1.1"), NOW).action, Action::Log);
        assert_eq!(
            index.evaluate(ip("2001:678:254:9::1"), NOW).action,
            Action::Block
        );
        assert_eq!(
            index.evaluate(ip("::ffff:10.2.3.4"), NOW).action,
            Action::Block
        );
    }

    #[test]
    fn a_stale_list_only_logs() {
        let index = index(vec![list("drop", ListVerdict::Block, &["185.93.89.0/24"])]);
        assert_eq!(
            index.evaluate(ip("185.93.89.1"), NOW + 3600).action,
            Action::Block
        );
        let verdict = index.evaluate(ip("185.93.89.1"), NOW + 3601);
        assert_eq!(verdict.action, Action::Log);
        assert_eq!(verdict.reason_strings(), ["drop (185.93.89.0/24, stale)"]);
    }

    #[test]
    fn the_baseline_applies_with_or_without_a_snapshot() {
        for index in [
            Index::baseline(),
            index(vec![list("drop", ListVerdict::Block, &[])]),
        ] {
            let verdict = index.evaluate(ip("192.0.2.10"), NOW);
            assert_eq!(verdict.action, Action::Log);
            assert_eq!(verdict.reason_strings(), ["iana_reserved (192.0.2.0/24)"]);
            assert_eq!(index.evaluate(ip("127.0.0.1"), NOW), Verdict::PASS);
            assert_eq!(index.evaluate(ip("10.0.0.1"), NOW), Verdict::PASS);
        }
    }

    #[test]
    fn a_list_on_the_edges_of_the_address_space() {
        let index = index(vec![list(
            "all",
            ListVerdict::Challenge,
            &["0.0.0.0/0", "ffff::/16"],
        )]);
        assert_eq!(
            index.evaluate(ip("255.255.255.255"), NOW).action,
            Action::Challenge
        );
        assert_eq!(
            index
                .evaluate(ip("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), NOW)
                .action,
            Action::Challenge
        );
        assert_eq!(index.evaluate(ip("fffe::1"), NOW), Verdict::PASS);
    }
}
