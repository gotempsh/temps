// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The in-memory form of a snapshot, searched on every request (ADR 066, I3).
//!
//! Each list keeps its networks sorted and disjoint, so the only network
//! that can hold an address is the last one starting at or before it: one
//! binary search per list and family, no allocation unless something
//! matches.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use crate::baseline;
use crate::net::{collapse, mask4, mask6, Address, Network};
use crate::snapshot::{ShieldSnapshot, SourceList};
use crate::verdict::{Action, ListVerdict, Reason, Subject, Verdict};

pub struct Index {
    revision: u64,
    lists: Vec<IndexedList>,
    /// Lists for one project, by project id.
    scoped: HashMap<i32, Vec<IndexedList>>,
    /// Fingerprint entries, by fingerprint.
    ja4: HashMap<Arc<str>, Vec<IndexedJa4>>,
}

struct IndexedList {
    source: Arc<str>,
    verdict: ListVerdict,
    published_at: i64,
    max_age_secs: Option<u64>,
    v4: Vec<(u32, u8)>,
    v6: Vec<(u128, u8)>,
}

struct IndexedJa4 {
    source: Arc<str>,
    verdict: ListVerdict,
    project_id: Option<i32>,
    expires_at: Option<i64>,
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

/// Matches gathered for one request, before an allow rule decides them.
#[derive(Default)]
struct Matches {
    verdict: Verdict,
    allowed_by: Option<Reason>,
}

impl Matches {
    fn add(&mut self, verdict: ListVerdict, reason: Reason) {
        if verdict == ListVerdict::Allow {
            self.allowed_by.get_or_insert(reason);
            return;
        }
        let action = if reason.stale {
            Action::Log
        } else {
            Action::from(verdict)
        };
        self.verdict.add(action, reason);
    }

    fn finish(self) -> Verdict {
        self.lookup().verdict
    }

    fn lookup(self) -> Lookup {
        match self.allowed_by {
            // An allow rule lets the client through: nothing else it
            // matched is acted on or recorded.
            Some(_) => Lookup {
                verdict: Verdict::PASS,
                allowed: true,
            },
            None => Lookup {
                verdict: self.verdict,
                allowed: false,
            },
        }
    }
}

/// What the lists say about a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub verdict: Verdict,
    /// An operator allow rule matched: the client passes whatever else
    /// it sends.
    pub allowed: bool,
}

impl Index {
    /// The built-in lists alone, for an ingress that has not loaded a
    /// snapshot yet.
    pub fn baseline() -> Self {
        Self::new(&ShieldSnapshot::default())
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
        let mut scoped: HashMap<i32, Vec<IndexedList>> = HashMap::new();
        for scoped_list in &snapshot.scoped {
            scoped
                .entry(scoped_list.project_id)
                .or_default()
                .push(IndexedList::new(&scoped_list.list));
        }
        let mut ja4: HashMap<Arc<str>, Vec<IndexedJa4>> = HashMap::new();
        for entry in &snapshot.ja4 {
            ja4.entry(Arc::from(entry.fingerprint.as_str()))
                .or_default()
                .push(IndexedJa4 {
                    source: Arc::from(entry.source.as_str()),
                    verdict: entry.verdict,
                    project_id: entry.project_id,
                    expires_at: entry.expires_at,
                });
        }
        Self {
            revision: snapshot.revision,
            lists,
            scoped,
            ja4,
        }
    }

    /// An index of `lists` alone, without the baseline: what one node adds
    /// to the snapshot on its own, such as netwatch's bans (D12).
    pub fn local(lists: &[SourceList]) -> Self {
        Self {
            revision: 0,
            lists: lists.iter().map(IndexedList::new).collect(),
            scoped: HashMap::new(),
            ja4: HashMap::new(),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every list that applies to every project and holds `ip`, and the
    /// strongest verdict among them. A list past its maximum age only logs.
    pub fn evaluate(&self, ip: IpAddr, now: i64) -> Verdict {
        let mut matches = Matches::default();
        self.match_lists(&self.lists, Address::from(ip), now, &mut matches);
        matches.finish()
    }

    /// Everything that applies to a request to `project_id`: the lists for
    /// every project and for this one that hold the address, the lists this
    /// node adds on its own (`local`), and the fingerprint entries for the
    /// client's JA4. An allow rule among them passes the client and drops
    /// every other match.
    pub fn evaluate_client(
        &self,
        local: Option<&Index>,
        ip: Option<IpAddr>,
        ja4: Option<&str>,
        project_id: i32,
        now: i64,
    ) -> Lookup {
        let mut matches = Matches::default();
        if let Some(ip) = ip {
            let address = Address::from(ip);
            self.match_lists(&self.lists, address, now, &mut matches);
            if let Some(lists) = self.scoped.get(&project_id) {
                self.match_lists(lists, address, now, &mut matches);
            }
            if let Some(local) = local {
                self.match_lists(&local.lists, address, now, &mut matches);
            }
        }
        if let Some(entries) = ja4.and_then(|ja4| self.ja4.get_key_value(ja4)) {
            let (fingerprint, entries) = entries;
            for entry in entries {
                let applies = entry.project_id.is_none_or(|id| id == project_id)
                    && entry.expires_at.is_none_or(|at| now < at);
                if applies {
                    matches.add(
                        entry.verdict,
                        Reason {
                            source: Arc::clone(&entry.source),
                            subject: Subject::Ja4(Arc::clone(fingerprint)),
                            stale: false,
                        },
                    );
                }
            }
        }
        matches.lookup()
    }

    fn match_lists(
        &self,
        lists: &[IndexedList],
        address: Address,
        now: i64,
        matches: &mut Matches,
    ) {
        for list in lists {
            if let Some(network) = list.find(address) {
                matches.add(
                    list.verdict,
                    Reason {
                        source: Arc::clone(&list.source),
                        subject: Subject::Network(network),
                        stale: list.is_stale(now),
                    },
                );
            }
        }
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
            ..ShieldSnapshot::default()
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
    fn project_lists_and_fingerprints_apply_where_they_are_scoped() {
        let ja4 = "t13d1516h2_8daaf6152771_02713d6af862";
        let index = Index::new(&ShieldSnapshot {
            revision: 4,
            generated_at: NOW,
            lists: vec![list("drop", ListVerdict::Block, &["185.93.89.0/24"])],
            scoped: vec![crate::snapshot::ScopedList {
                project_id: 7,
                list: list("operator", ListVerdict::Challenge, &["45.33.32.0/24"]),
            }],
            ja4: vec![crate::snapshot::Ja4Entry {
                fingerprint: ja4.into(),
                source: "observed".into(),
                verdict: ListVerdict::Challenge,
                project_id: None,
                expires_at: Some(NOW + 60),
            }],
        });
        let at = |addr: &str, project| {
            index
                .evaluate_client(None, Some(ip(addr)), None, project, NOW)
                .verdict
        };
        assert_eq!(at("45.33.32.9", 7).action, Action::Challenge);
        assert_eq!(at("45.33.32.9", 8), Verdict::PASS);
        assert_eq!(at("185.93.89.1", 8).action, Action::Block);
        let fingerprinted = index.evaluate_client(None, None, Some(ja4), 8, NOW).verdict;
        assert_eq!(fingerprinted.action, Action::Challenge);
        assert_eq!(
            fingerprinted.reason_strings(),
            [format!("observed (ja4 {ja4})")]
        );
        assert_eq!(
            index
                .evaluate_client(None, None, Some(ja4), 8, NOW + 60)
                .verdict,
            Verdict::PASS
        );
    }

    #[test]
    fn an_allow_rule_lets_the_client_through_whatever_else_it_matched() {
        let index = Index::new(&ShieldSnapshot {
            revision: 4,
            generated_at: NOW,
            lists: vec![list("drop", ListVerdict::Block, &["185.93.89.0/24"])],
            scoped: vec![crate::snapshot::ScopedList {
                project_id: 7,
                list: list("operator", ListVerdict::Allow, &["185.93.89.7"]),
            }],
            ..ShieldSnapshot::default()
        });
        let allowed = index.evaluate_client(None, Some(ip("185.93.89.7")), None, 7, NOW);
        assert!(allowed.allowed);
        assert_eq!(allowed.verdict, Verdict::PASS);
        // It wins over what this node adds too.
        let netwatch = Index::local(&[list("netwatch", ListVerdict::Block, &["185.93.89.7"])]);
        assert_eq!(
            index
                .evaluate_client(Some(&netwatch), Some(ip("185.93.89.7")), None, 7, NOW)
                .verdict,
            Verdict::PASS
        );
        assert_eq!(
            index
                .evaluate_client(Some(&netwatch), Some(ip("185.93.89.7")), None, 9, NOW)
                .verdict
                .reason_strings(),
            ["drop (185.93.89.0/24)", "netwatch (185.93.89.7/32)"]
        );
        assert_eq!(
            index
                .evaluate_client(None, Some(ip("185.93.89.7")), None, 8, NOW)
                .verdict
                .action,
            Action::Block
        );
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
