// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps-workload-netpol` writes microsandbox network policies without
//! depending on the SDK. This pins its output to the SDK this crate links:
//! every translated policy must deserialize into `microsandbox::NetworkPolicy`
//! and serialize back to the same JSON, so a field the SDK renames or drops
//! fails here instead of silently opening a microVM's network.

#![cfg(not(any(target_env = "musl", all(target_os = "macos", target_arch = "x86_64"))))]

use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::{
    to_sdk_policy, Action, Direction, IdentityMap, L4Protocol, Origin, Peer, PortRange, Profile,
    Rule, WorkloadPolicy,
};

fn rule(
    id: i64,
    direction: Direction,
    peer: Peer,
    ports: Option<PortRange>,
    protocol: L4Protocol,
    action: Action,
) -> Rule {
    Rule {
        id,
        direction,
        peer,
        ports,
        protocol,
        action,
        origin: Origin::Operator,
    }
}

fn round_trips(policy: &WorkloadPolicy, ids: &IdentityMap) {
    let translated = to_sdk_policy(policy, ids);
    let ours = serde_json::to_value(&translated.policy).expect("serialize");
    let sdk: microsandbox::NetworkPolicy = serde_json::from_value(ours.clone())
        .unwrap_or_else(|e| panic!("the SDK rejects {ours}: {e}"));
    let back = serde_json::to_value(&sdk).expect("serialize the SDK policy");
    assert_eq!(
        back, ours,
        "the SDK reads the policy differently than it was written"
    );
}

#[test]
fn every_profile_and_rule_shape_round_trips_through_the_sdk() {
    let subject = WorkloadRef::new(WorkloadKind::Sandbox, 1);
    let db = WorkloadRef::new(WorkloadKind::Service, 9);
    let ids = IdentityMap::new()
        .with(subject, "172.20.0.5".parse().unwrap())
        .with(db, "172.20.0.9".parse().unwrap())
        .with(db, "fd00::9".parse().unwrap());
    let rules = vec![
        rule(
            1,
            Direction::Egress,
            Peer::Workload(db),
            Some(PortRange::single(5432)),
            L4Protocol::Tcp,
            Action::Allow,
        ),
        rule(
            2,
            Direction::Egress,
            Peer::Cidr("10.0.0.0/8".parse().unwrap()),
            None,
            L4Protocol::Any,
            Action::Deny,
        ),
        rule(
            3,
            Direction::Egress,
            Peer::Cidr("192.0.2.0/24".parse().unwrap()),
            Some(PortRange {
                start: 5000,
                end: 5010,
            }),
            L4Protocol::Any,
            Action::Allow,
        ),
        rule(
            4,
            Direction::Egress,
            Peer::Cidr("0.0.0.0/0".parse().unwrap()),
            Some(PortRange::single(53)),
            L4Protocol::Udp,
            Action::Allow,
        ),
        rule(
            5,
            Direction::Ingress,
            Peer::Workload(db),
            Some(PortRange::single(8080)),
            L4Protocol::Tcp,
            Action::Allow,
        ),
    ];
    for egress in [Profile::Open, Profile::Restricted, Profile::None] {
        for ingress in [Profile::Open, Profile::Restricted, Profile::None] {
            round_trips(
                &WorkloadPolicy {
                    subject,
                    egress,
                    ingress,
                    rules: rules.clone(),
                },
                &ids,
            );
        }
    }
}
