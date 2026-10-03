// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mesh hubs (ADR 048 D4): which pairs of members go through the hub.
//!
//! Every pair starts direct. Whether two members can reach each other is
//! only known by trying, so each member reports when it last completed a
//! WireGuard handshake with each peer, and the control plane routes a pair
//! through the hub once both are reporting, neither has handshaken with the
//! other for [`GRACE`], and the hub has handshaken with both. A pair goes
//! back to direct when either member's endpoint changes (it moved, or was
//! given a reachable one), the hub is removed, or the hub stops reporting or
//! stops reaching one of them (relaying through it would drop everything); it
//! is never switched back and forth on a timer, which would break a working
//! relayed path to probe a direct one.
//!
//! WireGuard routes an address to exactly one peer, so a relayed member's
//! `/32` moves from its own entry to the hub's; the hub has a direct entry
//! for every member and forwards between them.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::mesh::NamedMeshPeer;

/// A handshake this recent means the path works (WireGuard's
/// REJECT_AFTER_TIME).
pub const LIVE: Duration = temps_wireguard::mesh::LIVE_HANDSHAKE;
/// How long a direct link gets to complete a handshake before its pair is
/// routed through the hub. Several keepalive rounds on both sides.
pub const GRACE: Duration = Duration::from_secs(150);
/// A node's report older than this says nothing about its links: the node
/// may be down, and a link to a down node is not a reason to use the hub.
pub const FRESH_REPORT: Duration = Duration::from_secs(90);

/// Whether the control plane can relay, as its last relay setup found.
/// Process-wide because the control plane is one process: its mesh
/// reconciler writes it, and link evaluation and the mesh status read it.
static CONTROL_PLANE_CAN_RELAY: AtomicBool = AtomicBool::new(true);

/// Record how the control plane's relay setup went. A control plane that
/// is the hub but could not set relaying up stops counting as a fresh
/// member, the way a node hub that cannot relay stops reporting, so its
/// pairs go back to direct instead of through a hub that drops them.
pub fn record_control_plane_relay(is_hub: bool, setup_succeeded: bool) {
    CONTROL_PLANE_CAN_RELAY.store(!is_hub || setup_succeeded, Ordering::Relaxed);
}

/// See [`record_control_plane_relay`].
pub fn control_plane_can_relay() -> bool {
    CONTROL_PLANE_CAN_RELAY.load(Ordering::Relaxed)
}

/// The member relaying for pairs that cannot reach each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hub {
    ControlPlane,
    Node(i32),
}

/// A mesh member as the routing decision sees it.
#[derive(Debug, Clone)]
pub struct Member {
    pub key: String,
    pub name: String,
    /// `None` for the control plane.
    pub node_id: Option<i32>,
    pub address: Ipv4Addr,
    pub endpoint: Option<String>,
    /// Peer key → last completed handshake. `None` when the member has not
    /// reported (an older agent, or none yet).
    pub handshakes: Option<HashMap<String, DateTime<Utc>>>,
    pub reported_at: Option<DateTime<Utc>>,
}

impl Member {
    pub fn is(&self, hub: Hub) -> bool {
        match hub {
            Hub::ControlPlane => self.node_id.is_none(),
            Hub::Node(id) => self.node_id == Some(id),
        }
    }

    /// Whether the member reported its handshakes recently enough for them
    /// to say anything.
    pub fn fresh(&self, now: DateTime<Utc>) -> bool {
        self.handshakes.is_some()
            && self
                .reported_at
                .is_some_and(|at| age(at, now) <= FRESH_REPORT)
    }

    fn handshake_with(&self, peer: &str) -> Option<DateTime<Utc>> {
        self.handshakes.as_ref()?.get(peer).copied()
    }
}

fn age(at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - at).to_std().unwrap_or_default()
}

/// A pair's route, keyed by the two public keys in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub via_hub: bool,
    pub since: DateTime<Utc>,
    pub endpoint_a: Option<String>,
    pub endpoint_b: Option<String>,
}

/// The two keys of a pair, lower first: how links are stored.
pub fn pair<'a>(x: &'a str, y: &'a str) -> (&'a str, &'a str) {
    if x < y {
        (x, y)
    } else {
        (y, x)
    }
}

/// Whether `a` and `b` handshook directly within [`LIVE`].
pub fn is_live(a: &Member, b: &Member, now: DateTime<Utc>) -> bool {
    last_handshake(a, b).is_some_and(|at| age(at, now) <= LIVE)
}

/// The most recent handshake either member saw with the other.
pub fn last_handshake(a: &Member, b: &Member) -> Option<DateTime<Utc>> {
    a.handshake_with(&b.key).max(b.handshake_with(&a.key))
}

/// Whether `hub` can carry the pair `a`–`b`: it is neither of them, it is
/// up, and it reaches both.
///
/// "Up" is the same [`FRESH_REPORT`] window every other report gets
/// ([`Member::fresh`]): a hub that stopped reporting may be down, and a down
/// hub drops every pair routed through it. Its links alone cannot say so in
/// time, because a member's last handshake with the hub stays inside
/// [`LIVE`] for up to three minutes after the hub dies. The control plane
/// reads its own interface on every tick, so it is fresh while it can relay
/// ([`record_control_plane_relay`]).
///
/// The handshakes themselves keep the [`LIVE`] bound rather than the
/// shorter report window: WireGuard re-handshakes a busy session only every
/// two minutes (REKEY_AFTER_TIME), so a healthy hub link is routinely more
/// than 90 seconds past its last handshake, and a tighter bound would bounce
/// every relayed pair back to a direct path that does not work.
pub fn relays_between(hub: &Member, a: &Member, b: &Member, now: DateTime<Utc>) -> bool {
    hub.key != a.key
        && hub.key != b.key
        && hub.fresh(now)
        && is_live(hub, a, now)
        && is_live(hub, b, now)
}

/// The route a pair should take now, given the one it has (if any).
pub fn decide(
    previous: Option<&Link>,
    a: &Member,
    b: &Member,
    hub: Option<&Member>,
    now: DateTime<Utc>,
) -> Link {
    let (first, second) = if a.key < b.key { (a, b) } else { (b, a) };
    let direct = Link {
        via_hub: false,
        since: now,
        endpoint_a: first.endpoint.clone(),
        endpoint_b: second.endpoint.clone(),
    };
    let Some(previous) = previous else {
        return direct;
    };
    let endpoints_changed =
        previous.endpoint_a != first.endpoint || previous.endpoint_b != second.endpoint;
    let hub_relays = hub.is_some_and(|hub| relays_between(hub, a, b, now));
    if previous.via_hub {
        return if hub_relays && !endpoints_changed {
            previous.clone()
        } else {
            direct
        };
    }
    if endpoints_changed {
        // A new endpoint deserves its own grace period.
        return direct;
    }
    let live = last_handshake(a, b).is_some_and(|at| age(at, now) <= LIVE);
    if hub_relays && !live && a.fresh(now) && b.fresh(now) && age(previous.since, now) >= GRACE {
        return Link {
            via_hub: true,
            since: now,
            ..direct
        };
    }
    previous.clone()
}

/// `me`'s peers with the hub applied: a member whose pair with `me` goes
/// through the hub is dropped from the list and its address added to the
/// hub's entry. Members without a link (pairings, new nodes) stay direct.
pub fn route(
    me: &str,
    peers: Vec<(String, NamedMeshPeer)>,
    links: &HashMap<(String, String), Link>,
    hub_key: Option<&str>,
) -> Vec<NamedMeshPeer> {
    let Some(hub_key) = hub_key.filter(|hub| *hub != me) else {
        return peers.into_iter().map(|(_, peer)| peer).collect();
    };
    if !peers.iter().any(|(key, _)| key == hub_key) {
        return peers.into_iter().map(|(_, peer)| peer).collect();
    }
    let relayed: HashSet<&str> = peers
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| *key != hub_key)
        .filter(|key| {
            let (a, b) = pair(me, key);
            links
                .get(&(a.to_string(), b.to_string()))
                .is_some_and(|link| link.via_hub)
        })
        .collect();
    let relayed_addresses: Vec<Ipv4Addr> = peers
        .iter()
        .filter(|(key, _)| relayed.contains(key.as_str()))
        .map(|(_, named)| named.peer.address)
        .collect();
    peers
        .iter()
        .filter(|(key, _)| !relayed.contains(key.as_str()))
        .map(|(key, named)| {
            let mut named = named.clone();
            if key == hub_key {
                named.peer.relayed = relayed_addresses.clone();
            }
            named
        })
        .collect()
}

/// How a pair is doing, for the status page and the doctor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// A recent handshake on the direct path.
    Direct,
    /// Routed through the hub.
    ViaHub,
    /// Direct, not handshaken yet, still inside the grace period.
    Connecting,
    /// Direct, no handshake, and nothing will change that: no hub, or one
    /// side is not reporting.
    Unreachable,
}

pub fn state(link: Option<&Link>, a: &Member, b: &Member, now: DateTime<Utc>) -> LinkState {
    let live = last_handshake(a, b).is_some_and(|at| age(at, now) <= LIVE);
    match link {
        Some(link) if link.via_hub => LinkState::ViaHub,
        _ if live => LinkState::Direct,
        Some(link) if age(link.since, now) < GRACE => LinkState::Connecting,
        None => LinkState::Connecting,
        Some(_) => LinkState::Unreachable,
    }
}

pub use db::*;

mod db {
    use super::*;
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
        QueryFilter, QuerySelect, Set,
    };
    use temps_entities::{mesh_links, network_config, node_mesh_reports, nodes};
    use tracing::info;

    use crate::mesh::MeshError;

    /// The mesh hub, if one is set.
    pub fn hub_from(cfg: &network_config::Model) -> Option<Hub> {
        if cfg.mesh_hub_control_plane {
            Some(Hub::ControlPlane)
        } else {
            cfg.mesh_hub_node_id.map(Hub::Node)
        }
    }

    pub async fn load_hub(db: &DatabaseConnection) -> Result<Option<Hub>, MeshError> {
        Ok(config(db).await?.as_ref().and_then(hub_from))
    }

    async fn config(db: &DatabaseConnection) -> Result<Option<network_config::Model>, MeshError> {
        Ok(network_config::Entity::find_by_id(1).one(db).await?)
    }

    /// Make `hub` the mesh hub (`None` removes it). A node must be on the
    /// mesh. Whether it can actually relay a pair is decided per pair
    /// ([`relays_between`]), so a hub that reaches nobody carries nothing.
    pub async fn set_hub(db: &DatabaseConnection, hub: Option<Hub>) -> Result<(), MeshError> {
        let cfg = config(db).await?.ok_or_else(|| MeshError::Corrupt {
            what: "network_config".into(),
            reason: "singleton row missing".into(),
        })?;
        if !cfg.wireguard_enabled {
            return Err(MeshError::Disabled);
        }
        if let Some(Hub::Node(node_id)) = hub {
            let node = nodes::Entity::find_by_id(node_id)
                .one(db)
                .await?
                .ok_or(MeshError::NodeNotFound(node_id))?;
            if node.mesh_wg_public_key.is_none() || node.mesh_wg_address.is_none() {
                return Err(MeshError::NotOnMesh(node.name));
            }
        }
        if hub == Some(Hub::ControlPlane) && cfg.control_plane_wg_public_key.is_none() {
            return Err(MeshError::NotOnMesh("control-plane".into()));
        }
        let mut active = cfg.into_active_model();
        active.mesh_hub_control_plane = Set(hub == Some(Hub::ControlPlane));
        active.mesh_hub_node_id = Set(match hub {
            Some(Hub::Node(id)) => Some(id),
            _ => None,
        });
        active.update(db).await?;
        Ok(())
    }

    /// Record a node's handshake report: peer key → seconds since the last
    /// handshake, placed on the control plane's clock. Only current members'
    /// keys are kept.
    ///
    /// Reports are trusted only for the reporting node's own pairs: a node
    /// that lies can keep its own pairs direct, or leave them unrouted, which
    /// it can do anyway by controlling its tunnel. It cannot choose the hub or
    /// affect pairs it is not part of, and a pair is live when either end saw
    /// the handshake (requiring both would misread honest reports taken
    /// between two rekeys as a dead link).
    pub async fn record_report(
        db: &DatabaseConnection,
        node_id: i32,
        seconds_since_handshake: &HashMap<String, u64>,
    ) -> Result<(), MeshError> {
        let now = Utc::now();
        let known = member_keys(db).await?;
        let handshakes: serde_json::Map<String, serde_json::Value> = seconds_since_handshake
            .iter()
            .filter(|(key, _)| known.contains(*key))
            .filter_map(|(key, seconds)| {
                let at = now - chrono::Duration::seconds(i64::try_from(*seconds).ok()?);
                Some((key.clone(), serde_json::Value::from(at.timestamp())))
            })
            .collect();
        let row = node_mesh_reports::ActiveModel {
            node_id: Set(node_id),
            handshakes: Set(serde_json::Value::Object(handshakes)),
            reported_at: Set(now),
        };
        node_mesh_reports::Entity::insert(row)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(node_mesh_reports::Column::NodeId)
                    .update_columns([
                        node_mesh_reports::Column::Handshakes,
                        node_mesh_reports::Column::ReportedAt,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    /// Every mesh member with its latest report. The control plane's view
    /// is `control_plane_handshakes` (read from its own interface).
    pub async fn members(
        db: &DatabaseConnection,
        control_plane_handshakes: &HashMap<String, DateTime<Utc>>,
    ) -> Result<Vec<Member>, MeshError> {
        let Some(cfg) = config(db).await? else {
            return Ok(Vec::new());
        };
        let Some(settings) = crate::mesh::settings_from(&cfg)? else {
            return Ok(Vec::new());
        };
        let reports: HashMap<i32, node_mesh_reports::Model> = node_mesh_reports::Entity::find()
            .all(db)
            .await?
            .into_iter()
            .map(|report| (report.node_id, report))
            .collect();
        let mut members = Vec::new();
        if let Some(key) = &cfg.control_plane_wg_public_key {
            members.push(Member {
                key: key.clone(),
                name: "control-plane".into(),
                node_id: None,
                address: settings.control_plane_address(),
                endpoint: cfg.control_plane_wg_endpoint.clone(),
                handshakes: Some(control_plane_handshakes.clone()),
                // Its view is read live, so it is fresh, unless it is the
                // hub and its relay setup failed: then nothing may be
                // routed through it.
                reported_at: control_plane_can_relay().then(Utc::now),
            });
        }
        for node in nodes::Entity::find()
            .filter(nodes::Column::MeshWgPublicKey.is_not_null())
            .filter(nodes::Column::MeshWgAddress.is_not_null())
            .all(db)
            .await?
        {
            let (Some(key), Some(Ok(address))) = (
                node.mesh_wg_public_key,
                node.mesh_wg_address.as_deref().map(str::parse),
            ) else {
                continue;
            };
            let report = reports.get(&node.id);
            members.push(Member {
                key,
                name: node.name,
                node_id: Some(node.id),
                address,
                endpoint: node.mesh_wg_endpoint,
                handshakes: report.map(|report| parse_handshakes(&report.handshakes)),
                reported_at: report.map(|report| report.reported_at),
            });
        }
        Ok(members)
    }

    /// The public keys of every mesh member: all a report is filtered by, so
    /// a report does not load every member's report.
    async fn member_keys(db: &DatabaseConnection) -> Result<HashSet<String>, MeshError> {
        let mut keys: HashSet<String> = nodes::Entity::find()
            .select_only()
            .column(nodes::Column::MeshWgPublicKey)
            .filter(nodes::Column::MeshWgPublicKey.is_not_null())
            .filter(nodes::Column::MeshWgAddress.is_not_null())
            .into_tuple::<Option<String>>()
            .all(db)
            .await?
            .into_iter()
            .flatten()
            .collect();
        if let Some(key) = config(db)
            .await?
            .and_then(|cfg| cfg.control_plane_wg_public_key)
        {
            keys.insert(key);
        }
        Ok(keys)
    }

    fn parse_handshakes(value: &serde_json::Value) -> HashMap<String, DateTime<Utc>> {
        value
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(key, at)| {
                        Some((key.clone(), DateTime::from_timestamp(at.as_i64()?, 0)?))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn load_links(
        db: &DatabaseConnection,
    ) -> Result<HashMap<(String, String), Link>, MeshError> {
        links_from(mesh_links::Entity::find().all(db).await?)
    }

    /// The links of the pairs `key` is part of.
    pub async fn load_links_of(
        db: &DatabaseConnection,
        key: &str,
    ) -> Result<HashMap<(String, String), Link>, MeshError> {
        links_from(
            mesh_links::Entity::find()
                .filter(
                    sea_orm::Condition::any()
                        .add(mesh_links::Column::KeyA.eq(key))
                        .add(mesh_links::Column::KeyB.eq(key)),
                )
                .all(db)
                .await?,
        )
    }

    fn links_from(
        rows: Vec<mesh_links::Model>,
    ) -> Result<HashMap<(String, String), Link>, MeshError> {
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    (row.key_a, row.key_b),
                    Link {
                        via_hub: row.via_hub,
                        since: row.since,
                        endpoint_a: row.endpoint_a,
                        endpoint_b: row.endpoint_b,
                    },
                )
            })
            .collect())
    }

    /// Rows per write statement (six parameters each).
    const WRITE_BATCH: usize = 500;

    /// Re-decide every pair's route and store the changes. Run by the
    /// control plane on every mesh tick.
    pub async fn evaluate(
        db: &DatabaseConnection,
        control_plane_handshakes: &HashMap<String, DateTime<Utc>>,
    ) -> Result<(), MeshError> {
        let members = members(db, control_plane_handshakes).await?;
        let hub = load_hub(db).await?;
        let hub = hub.and_then(|hub| members.iter().find(|member| member.is(hub)));
        let links = load_links(db).await?;
        let now = Utc::now();
        let mut current = HashSet::new();
        let mut changed = Vec::new();
        for (index, a) in members.iter().enumerate() {
            for b in &members[index + 1..] {
                let (key_a, key_b) = pair(&a.key, &b.key);
                let id = (key_a.to_string(), key_b.to_string());
                let previous = links.get(&id);
                let next = decide(previous, a, b, hub, now);
                if previous != Some(&next) {
                    if previous.map(|link| link.via_hub) != Some(next.via_hub) && previous.is_some()
                    {
                        info!(
                            a = %a.name,
                            b = %b.name,
                            via_hub = next.via_hub,
                            "WireGuard mesh link rerouted"
                        );
                    }
                    changed.push(mesh_links::ActiveModel {
                        key_a: Set(id.0.clone()),
                        key_b: Set(id.1.clone()),
                        via_hub: Set(next.via_hub),
                        since: Set(next.since),
                        endpoint_a: Set(next.endpoint_a),
                        endpoint_b: Set(next.endpoint_b),
                    });
                }
                current.insert(id);
            }
        }
        // Batched: a steady mesh writes nothing, a new member writes its
        // pairs in a few statements rather than one per pair.
        for batch in changed.chunks(WRITE_BATCH) {
            mesh_links::Entity::insert_many(batch.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        mesh_links::Column::KeyA,
                        mesh_links::Column::KeyB,
                    ])
                    .update_columns([
                        mesh_links::Column::ViaHub,
                        mesh_links::Column::Since,
                        mesh_links::Column::EndpointA,
                        mesh_links::Column::EndpointB,
                    ])
                    .to_owned(),
                )
                .exec(db)
                .await?;
        }
        // Pairs with a member that left (or changed key).
        let departed: Vec<_> = links.keys().filter(|id| !current.contains(*id)).collect();
        for batch in departed.chunks(WRITE_BATCH) {
            let mut which = sea_orm::Condition::any();
            for (key_a, key_b) in batch {
                which = which.add(
                    sea_orm::Condition::all()
                        .add(mesh_links::Column::KeyA.eq(key_a.as_str()))
                        .add(mesh_links::Column::KeyB.eq(key_b.as_str())),
                );
            }
            mesh_links::Entity::delete_many()
                .filter(which)
                .exec(db)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_wireguard::mesh::MeshPeer;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    fn member(key: &str, node_id: Option<i32>, reported: Option<i64>) -> Member {
        Member {
            key: key.into(),
            name: key.into(),
            node_id,
            address: Ipv4Addr::new(10, 201, 0, node_id.unwrap_or(1) as u8 + 1),
            endpoint: Some(format!("198.51.100.{}:51820", node_id.unwrap_or(1))),
            handshakes: reported.map(|_| HashMap::new()),
            reported_at: reported.map(at),
        }
    }

    /// A hub (the control plane) that handshook with each of `reaches` at `now`.
    fn hub_reaching(reaches: &[&Member], now: i64) -> Member {
        let mut hub = member("hub", None, Some(now));
        hub.handshakes = Some(
            reaches
                .iter()
                .map(|member| (member.key.clone(), at(now - 20)))
                .collect(),
        );
        hub
    }

    fn direct_since(seconds: i64, a: &Member, b: &Member) -> Link {
        let (first, second) = if a.key < b.key { (a, b) } else { (b, a) };
        Link {
            via_hub: false,
            since: at(seconds),
            endpoint_a: first.endpoint.clone(),
            endpoint_b: second.endpoint.clone(),
        }
    }

    #[test]
    fn a_new_pair_starts_direct() {
        let (a, b) = (member("a", Some(2), Some(0)), member("b", Some(3), Some(0)));
        let hub = hub_reaching(&[&a, &b], 0);
        let link = decide(None, &a, &b, Some(&hub), at(0));
        assert!(!link.via_hub);
        assert_eq!(link.since, at(0));
    }

    #[test]
    fn a_pair_that_never_handshook_moves_to_the_hub_after_the_grace_period() {
        let now = 200;
        let (a, b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let hub_member = hub_reaching(&[&a, &b], now);
        let hub = Some(&hub_member);

        let young = direct_since(now - 60, &a, &b);
        assert_eq!(decide(Some(&young), &a, &b, hub, at(now)), young);

        let old = direct_since(0, &a, &b);
        let next = decide(Some(&old), &a, &b, hub, at(now));
        assert!(next.via_hub);
        assert_eq!(next.since, at(now));

        // Without a hub nothing changes: the doctor reports the pair.
        assert_eq!(decide(Some(&old), &a, &b, None, at(now)), old);
    }

    #[test]
    fn a_live_direct_link_stays_direct() {
        let now = 1000;
        let mut a = member("a", Some(2), Some(now));
        let b = member("b", Some(3), Some(now));
        a.handshakes = Some(HashMap::from([("b".to_string(), at(now - 30))]));
        let old = direct_since(0, &a, &b);
        let hub = hub_reaching(&[&a, &b], now);
        assert_eq!(decide(Some(&old), &a, &b, Some(&hub), at(now)), old);
    }

    #[test]
    fn a_silent_member_is_not_a_reason_to_use_the_hub() {
        let now = 1000;
        let a = member("a", Some(2), Some(now));
        let stale = member("b", Some(3), Some(now - 600));
        let old_agent = member("c", Some(4), None);
        let hub_member = hub_reaching(&[&a, &stale, &old_agent], now);
        let hub = Some(&hub_member);
        let link = direct_since(0, &a, &stale);
        assert_eq!(decide(Some(&link), &a, &stale, hub, at(now)), link);
        let link = direct_since(0, &a, &old_agent);
        assert_eq!(decide(Some(&link), &a, &old_agent, hub, at(now)), link);
    }

    #[test]
    fn the_hub_itself_is_always_direct() {
        let now = 1000;
        let a = member("a", Some(2), Some(now));
        let hub_node = member("h", Some(9), Some(now));
        let link = direct_since(0, &a, &hub_node);
        assert_eq!(
            decide(Some(&link), &a, &hub_node, Some(&hub_node), at(now)),
            link
        );
    }

    #[test]
    fn a_hub_that_does_not_reach_both_members_carries_nothing() {
        let now = 1000;
        let (a, b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let half = hub_reaching(&[&a], now);
        let old = direct_since(0, &a, &b);
        assert_eq!(
            decide(Some(&old), &a, &b, Some(&half), at(now)),
            old,
            "routing through a hub that cannot reach b would drop everything"
        );

        // A relayed pair whose hub lost one side goes back to direct.
        let relayed = Link {
            via_hub: true,
            ..direct_since(500, &a, &b)
        };
        let next = decide(Some(&relayed), &a, &b, Some(&half), at(now));
        assert!(!next.via_hub);
        assert_eq!(next.since, at(now));
    }

    #[test]
    fn a_relayed_pair_returns_to_direct_when_an_endpoint_changes_or_the_hub_goes() {
        let now = 1000;
        let (a, mut b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let hub_member = hub_reaching(&[&a, &b], now);
        let hub = Some(&hub_member);
        let relayed = Link {
            via_hub: true,
            ..direct_since(500, &a, &b)
        };
        assert_eq!(decide(Some(&relayed), &a, &b, hub, at(now)), relayed);
        assert!(!decide(Some(&relayed), &a, &b, None, at(now)).via_hub);
        b.endpoint = Some("203.0.113.9:51820".into());
        let next = decide(Some(&relayed), &a, &b, hub, at(now));
        assert!(!next.via_hub);
        assert_eq!(next.endpoint_b, b.endpoint);
    }

    /// A node hub that last reported at `reported` and, at that report,
    /// had handshaken with each of `reaches` at `handshake`.
    fn node_hub(reaches: &[&Member], reported: i64, handshake: i64) -> Member {
        let mut hub = member("h", Some(9), Some(reported));
        hub.handshakes = Some(
            reaches
                .iter()
                .map(|member| (member.key.clone(), at(handshake)))
                .collect(),
        );
        hub
    }

    #[test]
    fn a_control_plane_hub_that_cannot_relay_is_not_fresh() {
        // Only this test writes the flag; it ends in the default state.
        record_control_plane_relay(true, false);
        assert!(!control_plane_can_relay(), "a failed hub carries no pairs");
        record_control_plane_relay(false, false);
        assert!(
            control_plane_can_relay(),
            "a control plane that is not the hub has nothing to relay"
        );
        record_control_plane_relay(true, true);
        assert!(control_plane_can_relay());
    }

    #[test]
    fn a_hub_relays_only_while_its_own_report_is_fresh() {
        let now = 1000;
        let fresh_window = FRESH_REPORT.as_secs() as i64;
        let (a, b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let relayed = Link {
            via_hub: true,
            ..direct_since(500, &a, &b)
        };

        // Reported 89s ago, handshakes 20s before that: still up.
        let up = node_hub(&[&a, &b], now - (fresh_window - 1), now - fresh_window - 19);
        assert!(relays_between(&up, &a, &b, at(now)));
        assert_eq!(decide(Some(&relayed), &a, &b, Some(&up), at(now)), relayed);

        // Reported 91s ago: it may be down. Its handshakes are still inside
        // LIVE, but the pair goes back to direct instead of waiting up to
        // three minutes on a hub that drops everything.
        let silent = node_hub(&[&a, &b], now - (fresh_window + 1), now - fresh_window - 19);
        assert!(!relays_between(&silent, &a, &b, at(now)));
        let next = decide(Some(&relayed), &a, &b, Some(&silent), at(now));
        assert!(!next.via_hub);
        assert_eq!(next.since, at(now));

        // Nor does a silent hub take a pair over.
        let old = direct_since(0, &a, &b);
        assert_eq!(decide(Some(&old), &a, &b, Some(&silent), at(now)), old);

        // A hub that never reported (an older agent) relays nothing.
        let mut unreported = node_hub(&[&a, &b], now, now - 20);
        unreported.reported_at = None;
        assert!(!relays_between(&unreported, &a, &b, at(now)));
    }

    #[test]
    fn a_fresh_hub_between_rekeys_keeps_its_relayed_pairs() {
        // WireGuard re-handshakes a busy session every 120s, so a healthy
        // hub link is often older than the 90s report window: that must not
        // bounce the pair back to a direct path that does not work.
        let now = 1000;
        let (a, b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let hub = node_hub(&[&a, &b], now - 10, now - 120);
        let relayed = Link {
            via_hub: true,
            ..direct_since(500, &a, &b)
        };
        assert!(relays_between(&hub, &a, &b, at(now)));
        assert_eq!(decide(Some(&relayed), &a, &b, Some(&hub), at(now)), relayed);

        // Past WireGuard's REJECT_AFTER_TIME the hub link is dead.
        let dead = node_hub(&[&a, &b], now - 10, now - LIVE.as_secs() as i64 - 1);
        assert!(!relays_between(&dead, &a, &b, at(now)));
    }

    fn named(key: &str, last: u8) -> (String, NamedMeshPeer) {
        (
            key.to_string(),
            NamedMeshPeer {
                name: key.to_string(),
                peer: MeshPeer {
                    public_key: key.to_string(),
                    endpoint: None,
                    address: Ipv4Addr::new(10, 201, 0, last),
                    relayed: Vec::new(),
                },
            },
        )
    }

    #[test]
    fn relayed_members_ride_on_the_hub_entry() {
        let relayed = Link {
            via_hub: true,
            since: at(0),
            endpoint_a: None,
            endpoint_b: None,
        };
        let links = HashMap::from([(("a".to_string(), "c".to_string()), relayed)]);
        let peers = vec![named("hub", 1), named("b", 3), named("c", 4)];

        let routed = route("a", peers.clone(), &links, Some("hub"));
        let names: Vec<_> = routed.iter().map(|peer| peer.name.as_str()).collect();
        assert_eq!(names, vec!["hub", "b"]);
        assert_eq!(routed[0].peer.relayed, vec![Ipv4Addr::new(10, 201, 0, 4)]);

        // The hub itself keeps a direct entry for everyone.
        let routed = route("hub", peers.clone(), &links, Some("hub"));
        assert!(routed.iter().all(|peer| peer.peer.relayed.is_empty()));

        // No hub: everything stays direct.
        assert_eq!(route("a", peers, &links, None).len(), 3);
    }

    #[test]
    fn link_states_say_what_the_operator_sees() {
        let now = 1000;
        let (a, b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        assert_eq!(
            state(Some(&direct_since(now - 10, &a, &b)), &a, &b, at(now)),
            LinkState::Connecting
        );
        assert_eq!(
            state(Some(&direct_since(0, &a, &b)), &a, &b, at(now)),
            LinkState::Unreachable
        );
        let relayed = Link {
            via_hub: true,
            ..direct_since(0, &a, &b)
        };
        assert_eq!(state(Some(&relayed), &a, &b, at(now)), LinkState::ViaHub);

        // No decision yet: connecting. A recent handshake: direct, whatever
        // the stored link says.
        assert_eq!(state(None, &a, &b, at(now)), LinkState::Connecting);
        let mut live = a.clone();
        live.handshakes = Some(HashMap::from([("b".to_string(), at(now - 30))]));
        assert_eq!(
            state(Some(&direct_since(0, &live, &b)), &live, &b, at(now)),
            LinkState::Direct
        );
    }

    #[test]
    fn a_new_endpoint_restarts_the_grace_period_of_a_direct_link() {
        let now = 1000;
        let (a, mut b) = (
            member("a", Some(2), Some(now)),
            member("b", Some(3), Some(now)),
        );
        let hub = hub_reaching(&[&a, &b], now);
        let old = direct_since(0, &a, &b);
        b.endpoint = Some("203.0.113.9:51820".into());
        let next = decide(Some(&old), &a, &b, Some(&hub), at(now));
        assert!(!next.via_hub, "the moved member gets a fresh chance");
        assert_eq!(next.since, at(now));
        assert_eq!(next.endpoint_b, b.endpoint);
    }

    #[test]
    fn without_the_hub_among_the_peers_every_member_stays_direct() {
        let relayed = Link {
            via_hub: true,
            since: at(0),
            endpoint_a: None,
            endpoint_b: None,
        };
        let links = HashMap::from([(("a".to_string(), "c".to_string()), relayed)]);
        let peers = vec![named("b", 3), named("c", 4)];
        // The hub is not a peer of `a` (removed, or not registered yet):
        // routing through it would drop c's traffic, so c stays direct.
        let routed = route("a", peers, &links, Some("hub"));
        let names: Vec<_> = routed.iter().map(|peer| peer.name.as_str()).collect();
        assert_eq!(names, vec!["b", "c"]);
        assert!(routed.iter().all(|peer| peer.peer.relayed.is_empty()));
    }
}
