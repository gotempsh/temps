// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who a tailnet visitor is, for `tailnet_users` addresses (ADR 065 D6).
//!
//! Two places ask, and nowhere else:
//!
//! - **A Serve socket.** tailscaled set `Tailscale-User-Login` for a user's
//!   device and deleted any client copy, so it is read there (Q-decoded,
//!   refused if it does not decode). A tagged device carries no user: its
//!   tags come from `whois` of the `X-Forwarded-For` tailscaled set.
//! - **The private listener on `tailscale0`.** No Serve headers exist and
//!   none are read: `whois` of the TCP peer says who it is.
//!
//! `whois` is talking to tailscaled, so it is possible only once the worker
//! owner allowed it with `--tailscale-serve` ([`TailnetIdentity::set_api`]
//! is called by the Serve manager, which opens the LocalAPI only then).
//! Without it every `tailnet_users` request is refused.
//!
//! Answers are cached for [`WHOIS_TTL`] per visitor address, in a bounded
//! map read without a lock; a miss costs one LocalAPI call. Failures are not
//! cached, and every failure refuses. Each answer carries the epoch it was
//! asked in; [`TailnetIdentity::set_api`] starts a new one, so an answer to
//! a question asked before consent was withdrawn is never used after it.
//!
//! Two visitors are never admitted, whatever the list says: the worker's own
//! node (a process on the worker, or a host-network workload, would
//! otherwise arrive as the machine's owner), and a tag of a node shared in
//! from another tailnet (its tags are named by that tailnet's admins).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use axum::http::HeaderValue;
use temps_core::tailnet::{self, messages, PrincipalKind, TailnetPrincipal};
use tracing::debug;

use crate::tailscale_api::LocalApi;

/// How long a `whois` answer is used (D6).
pub const WHOIS_TTL: Duration = Duration::from_secs(30);
/// Most visitor addresses cached at once.
pub const MAX_CACHED: usize = 4096;

/// A tailnet node as `whois` described it, lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visitor {
    /// The user, for an untagged device.
    pub login: Option<String>,
    /// Only tags of this tailnet's own nodes: a node shared in from another
    /// tailnet has none here.
    pub tags: Vec<String>,
    /// A tagged node shared in from another tailnet: no tag entry admits it.
    pub shared_tagged: bool,
    /// The stable node id, to recognise the worker itself.
    pub node_id: String,
}

/// The worker's own tailnet node, from tailscaled's `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfNode {
    pub stable_id: String,
    pub ips: Vec<IpAddr>,
}

/// Whether to serve the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Served; `login` is passed to the app as `x-temps-user-login`.
    Admit { login: Option<String> },
    /// Refused, with what to show the visitor.
    Refuse(String),
}

#[derive(Clone)]
struct Cached {
    visitor: Option<Visitor>,
    at: Instant,
    epoch: u64,
}

struct ApiSlot(Arc<dyn LocalApi>);

/// Shared by the Serve sockets and the private listener of one agent.
pub struct TailnetIdentity {
    api: ArcSwapOption<ApiSlot>,
    cache: ArcSwap<HashMap<IpAddr, Cached>>,
    /// Bumped by every change of `api`; cached answers of an older epoch
    /// are never used.
    epoch: AtomicU64,
    /// Who the worker itself is. Unknown until tailscaled was asked, and
    /// until then nobody is admitted.
    self_node: ArcSwapOption<SelfNode>,
}

impl Default for TailnetIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl TailnetIdentity {
    pub fn new() -> Self {
        Self {
            api: ArcSwapOption::empty(),
            cache: ArcSwap::from_pointee(HashMap::new()),
            epoch: AtomicU64::new(0),
            self_node: ArcSwapOption::empty(),
        }
    }

    /// The worker's own node, from the Serve manager's latest `status`.
    pub fn set_self(&self, node: Option<SelfNode>) {
        self.self_node.store(node.map(Arc::new));
    }

    /// Whether the request comes from the worker's own node; `None` while
    /// that is not known yet.
    fn is_the_worker(&self, ip: IpAddr, node_id: Option<&str>) -> Option<bool> {
        let me = self.self_node.load_full()?;
        Some(me.ips.contains(&ip) || node_id.is_some_and(|id| !id.is_empty() && id == me.stable_id))
    }

    /// The LocalAPI to ask, or `None` when the agent may not (no consent,
    /// not root). A change drops every cached answer.
    pub fn set_api(&self, api: Option<Arc<dyn LocalApi>>) {
        self.replace_api(api, |_| {});
    }

    /// [`Self::set_api`], with `between` run after the API is replaced and
    /// before the epoch moves: tests look at the state a concurrent
    /// [`Self::whois`] can see there.
    fn replace_api(&self, api: Option<Arc<dyn LocalApi>>, between: impl FnOnce(&Self)) {
        let current = self.api.load();
        let same = match (current.as_ref(), api.as_ref()) {
            (None, None) => true,
            (Some(slot), Some(api)) => Arc::ptr_eq(&slot.0, api),
            _ => false,
        };
        if same {
            return;
        }
        // The new API is in place, and the old answers gone, before the
        // epoch moves (D11). A `whois` that reads the new epoch (Acquire,
        // pairing with this Release) therefore also reads the new API; one
        // that read the old epoch is refused when it rechecks it, or caches
        // under the old epoch, which nothing uses once it moved.
        self.api.store(api.map(|api| Arc::new(ApiSlot(api))));
        self.cache.store(Arc::new(HashMap::new()));
        between(self);
        self.epoch.fetch_add(1, Ordering::Release);
    }

    /// The epoch, then the API to ask in it, in that order: see
    /// [`Self::replace_api`].
    fn snapshot(&self) -> (u64, Option<Arc<ApiSlot>>) {
        let epoch = self.epoch.load(Ordering::Acquire);
        (epoch, self.api.load_full())
    }

    /// `whois` of `ip`, cached. `Ok(None)` when tailscaled does not know it.
    async fn whois(&self, ip: IpAddr) -> Result<Option<Visitor>, String> {
        let (epoch, api) = self.snapshot();
        if let Some(cached) = self.cache.load().get(&ip) {
            if cached.epoch == epoch && cached.at.elapsed() < WHOIS_TTL {
                return Ok(cached.visitor.clone());
            }
        }
        let Some(api) = api else {
            return Err("this worker's owner has not allowed asking tailscaled".to_string());
        };
        let who = api
            .0
            .whois(&ip.to_string())
            .await
            .map_err(|e| e.to_string())?;
        if self.epoch.load(Ordering::Acquire) != epoch {
            // Consent changed while asking: this answer is not to be used.
            return Err("tailscaled access changed while asking".to_string());
        }
        let visitor = who.map(|who| Visitor {
            login: who.login().map(str::to_ascii_lowercase),
            tags: if who.shared_in() {
                Vec::new()
            } else {
                who.tags().iter().map(|t| t.to_ascii_lowercase()).collect()
            },
            shared_tagged: who.shared_in() && !who.tags().is_empty(),
            node_id: who.node.stable_id.clone(),
        });
        let entry = Cached {
            visitor: visitor.clone(),
            at: Instant::now(),
            epoch,
        };
        self.cache.rcu(|map| {
            let mut next: HashMap<IpAddr, Cached> = map
                .iter()
                .filter(|(_, c)| c.epoch == epoch && c.at.elapsed() < WHOIS_TTL)
                .map(|(ip, c)| (*ip, c.clone()))
                .collect();
            if next.len() < MAX_CACHED {
                next.insert(ip, entry.clone());
            }
            next
        });
        Ok(visitor)
    }

    /// A request on a Serve socket, from `visitor` (tailscaled's
    /// `X-Forwarded-For`), with tailscaled's `Tailscale-User-Login`.
    pub async fn admit_serve(
        &self,
        principals: &[TailnetPrincipal],
        login_header: Option<&HeaderValue>,
        visitor: IpAddr,
    ) -> Verdict {
        if principals.is_empty() {
            return Verdict::Refuse(messages::NOBODY_LISTED.to_string());
        }
        match self.is_the_worker(visitor, None) {
            None => return Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string()),
            Some(true) => return Verdict::Refuse(messages::SELF_NOT_ADMITTED.to_string()),
            Some(false) => {}
        }
        if let Some(raw) = login_header {
            let Some(login) = tailnet::decode_login_header(raw.as_bytes()) else {
                return Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string());
            };
            return if tailnet::admits(principals, Some(&login), &[]) {
                Verdict::Admit {
                    login: Some(login.to_ascii_lowercase()),
                }
            } else {
                Verdict::Refuse(messages::user_not_allowed(&login))
            };
        }
        // No user: a tagged device, admitted only by an explicit tag entry.
        match self.whois(visitor).await {
            Ok(Some(found)) if self.is_the_worker(visitor, Some(&found.node_id)) != Some(false) => {
                Verdict::Refuse(messages::SELF_NOT_ADMITTED.to_string())
            }
            Ok(Some(found)) if found.shared_tagged => {
                Verdict::Refuse(messages::SHARED_TAGGED_NOT_ADMITTED.to_string())
            }
            Ok(Some(found)) if found.login.is_none() && !found.tags.is_empty() => {
                let listed = principals.iter().any(|p| p.kind == PrincipalKind::Tag);
                if listed && tailnet::admits(principals, None, &found.tags) {
                    Verdict::Admit { login: None }
                } else {
                    Verdict::Refuse(messages::tagged_not_allowed(&found.tags))
                }
            }
            Ok(_) => Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string()),
            Err(error) => {
                debug!(%visitor, %error, "whois for a tagged visitor failed; refusing");
                Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
            }
        }
    }

    /// A request on the private listener from the TCP peer `peer`.
    pub async fn admit_peer(&self, principals: &[TailnetPrincipal], peer: IpAddr) -> Verdict {
        if principals.is_empty() {
            return Verdict::Refuse(messages::NOBODY_LISTED.to_string());
        }
        match self.is_the_worker(peer, None) {
            None => return Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string()),
            Some(true) => return Verdict::Refuse(messages::SELF_NOT_ADMITTED.to_string()),
            Some(false) => {}
        }
        match self.whois(peer).await {
            Ok(Some(found)) if self.is_the_worker(peer, Some(&found.node_id)) != Some(false) => {
                Verdict::Refuse(messages::SELF_NOT_ADMITTED.to_string())
            }
            Ok(Some(found)) => {
                if tailnet::admits(principals, found.login.as_deref(), &found.tags) {
                    Verdict::Admit { login: found.login }
                } else if let Some(login) = &found.login {
                    Verdict::Refuse(messages::user_not_allowed(login))
                } else if found.shared_tagged {
                    Verdict::Refuse(messages::SHARED_TAGGED_NOT_ADMITTED.to_string())
                } else {
                    Verdict::Refuse(messages::tagged_not_allowed(&found.tags))
                }
            }
            Ok(None) => Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string()),
            Err(error) => {
                debug!(%peer, %error, "whois for a private listener peer failed; refusing");
                Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tailscale_api::fake;
    use crate::tailscale_api::UnixLocalApi;
    use serde_json::json;

    fn list(values: &[&str]) -> Vec<TailnetPrincipal> {
        values
            .iter()
            .map(|v| TailnetPrincipal::parse(v).unwrap())
            .collect()
    }

    fn user(login: &str) -> serde_json::Value {
        json!({"Node": {"Name": "laptop.example.ts.net."}, "UserProfile": {"LoginName": login}})
    }

    fn tagged(tags: &[&str]) -> serde_json::Value {
        json!({"Node": {"Name": "ci.example.ts.net.", "Tags": tags}, "UserProfile": {"LoginName": "tagged-devices"}})
    }

    async fn identity_with(
        whois: &[(&str, serde_json::Value)],
    ) -> (tempfile::TempDir, TailnetIdentity, fake::Shared) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = fake::running();
        for (ip, answer) in whois {
            state.whois.insert(ip.to_string(), answer.clone());
        }
        let (socket, shared) = fake::start(dir.path(), state).await;
        let identity = TailnetIdentity::new();
        identity.set_api(Some(Arc::new(UnixLocalApi::new(socket))));
        identity.set_self(Some(worker()));
        (dir, identity, shared)
    }

    /// The worker, as the recorded `status` names `Self`.
    fn worker() -> SelfNode {
        SelfNode {
            stable_id: "n123CNTRL".into(),
            ips: vec![ip("100.101.102.103"), ip("fd7a:115c:a1e0::5")],
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[tokio::test]
    async fn the_socket_admits_a_listed_login_from_its_header_only() {
        let (_dir, identity, _) = identity_with(&[]).await;
        let principals = list(&["alice@example.com"]);
        let visitor = ip("100.64.0.7");
        assert_eq!(
            identity
                .admit_serve(
                    &principals,
                    Some(&HeaderValue::from_static("Alice@Example.com")),
                    visitor
                )
                .await,
            Verdict::Admit {
                login: Some("alice@example.com".into())
            }
        );
        let refused = identity
            .admit_serve(
                &principals,
                Some(&HeaderValue::from_static("bob@example.com")),
                visitor,
            )
            .await;
        assert!(matches!(refused, Verdict::Refuse(m) if m.contains("`bob@example.com`")));
        // A header that does not decode is refused, never guessed at.
        assert_eq!(
            identity
                .admit_serve(
                    &principals,
                    Some(&HeaderValue::from_static("=?utf-8?b?YWxpY2U=?=")),
                    visitor
                )
                .await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
    }

    #[tokio::test]
    async fn an_empty_list_admits_nobody() {
        let (_dir, identity, _) = identity_with(&[("100.64.0.7", user("alice@example.com"))]).await;
        let header = HeaderValue::from_static("alice@example.com");
        for verdict in [
            identity
                .admit_serve(&[], Some(&header), ip("100.64.0.7"))
                .await,
            identity.admit_peer(&[], ip("100.64.0.7")).await,
        ] {
            assert_eq!(
                verdict,
                Verdict::Refuse(messages::NOBODY_LISTED.to_string())
            );
        }
    }

    #[tokio::test]
    async fn a_tagged_device_needs_its_tag_listed() {
        let (_dir, identity, _) = identity_with(&[
            ("100.64.0.8", tagged(&["tag:ci"])),
            ("100.64.0.9", tagged(&["tag:server"])),
            ("100.64.0.10", user("alice@example.com")),
        ])
        .await;
        let principals = list(&["alice@example.com", "tag:ci"]);
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.8"))
                .await,
            Verdict::Admit { login: None }
        );
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.9"))
                .await,
            Verdict::Refuse(
                "This app admits only listed tailnet users. Requests from tagged devices \
                 (`tag:server`) carry no user; ask the owner to add the tag."
                    .to_string()
            )
        );
        // A user's device with no header did not come through Serve as one:
        // whois is not used to read its login on the socket.
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.10"))
                .await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
        // An unknown node, and a list with no tag, are refused too.
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.99"))
                .await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
        assert!(matches!(
            identity
                .admit_serve(&list(&["alice@example.com"]), None, ip("100.64.0.8"))
                .await,
            Verdict::Refuse(_)
        ));
    }

    #[tokio::test]
    async fn the_listener_asks_whois_of_the_peer_and_caches_it() {
        let (_dir, identity, shared) = identity_with(&[
            ("100.64.0.10", user("alice@example.com")),
            ("100.64.0.8", tagged(&["tag:ci"])),
        ])
        .await;
        let principals = list(&["alice@example.com", "tag:ci"]);
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.10")).await,
            Verdict::Admit {
                login: Some("alice@example.com".into())
            }
        );
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.8")).await,
            Verdict::Admit { login: None }
        );
        // Cached: tailscaled's answer changing is not seen within 30 s.
        shared
            .lock()
            .whois
            .insert("100.64.0.10".into(), user("bob@example.com"));
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.10")).await,
            Verdict::Admit {
                login: Some("alice@example.com".into())
            }
        );
        assert!(matches!(
            identity.admit_peer(&list(&["bob@example.com"]), ip("100.64.0.8")).await,
            Verdict::Refuse(m) if m.contains("`tag:ci`")
        ));
    }

    #[tokio::test]
    async fn without_the_owners_consent_nothing_is_admitted() {
        let (_dir, identity, _) =
            identity_with(&[("100.64.0.10", user("alice@example.com"))]).await;
        identity.set_api(None);
        let principals = list(&["alice@example.com", "tag:ci"]);
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.10")).await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.10"))
                .await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
    }

    /// A process on the worker, or a host-network workload, reaching the
    /// worker's own tailnet address arrives as the worker's node: on an
    /// untagged worker that is its owner's login. It is never admitted.
    #[tokio::test]
    async fn the_worker_itself_is_never_admitted() {
        let owner = json!({"Node": {"StableID": "n123CNTRL", "Name": "master.example.ts.net."}, "UserProfile": {"LoginName": "owner@example.com"}});
        let (_dir, identity, _) = identity_with(&[
            ("100.101.102.103", owner.clone()),
            // The same node by an address `status` did not list.
            ("100.101.102.104", owner),
        ])
        .await;
        let principals = list(&["owner@example.com"]);
        let refused = Verdict::Refuse(messages::SELF_NOT_ADMITTED.to_string());
        assert_eq!(
            identity
                .admit_peer(&principals, ip("100.101.102.103"))
                .await,
            refused
        );
        assert_eq!(
            identity
                .admit_peer(&principals, ip("100.101.102.104"))
                .await,
            refused
        );
        assert_eq!(
            identity
                .admit_serve(
                    &principals,
                    Some(&HeaderValue::from_static("owner@example.com")),
                    ip("100.101.102.103")
                )
                .await,
            refused
        );
        assert_eq!(
            identity
                .admit_peer(&principals, ip("fd7a:115c:a1e0::5"))
                .await,
            refused
        );
    }

    /// Until tailscaled said who the worker is, nobody is admitted.
    #[tokio::test]
    async fn nobody_is_admitted_before_the_worker_knows_itself() {
        let (_dir, identity, _) =
            identity_with(&[("100.64.0.10", user("alice@example.com"))]).await;
        identity.set_self(None);
        let principals = list(&["alice@example.com"]);
        let unknown = Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string());
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.10")).await,
            unknown
        );
        assert_eq!(
            identity
                .admit_serve(
                    &principals,
                    Some(&HeaderValue::from_static("alice@example.com")),
                    ip("100.64.0.10")
                )
                .await,
            unknown
        );
    }

    /// A node shared in from another tailnet carries that tailnet's tags;
    /// they never match here. Its user's login still can.
    #[tokio::test]
    async fn tags_of_a_node_shared_in_never_match() {
        let shared_tagged = json!({"Node": {"Name": "ci.other.ts.net.", "Tags": ["tag:ci"], "Sharer": 42}, "UserProfile": {"LoginName": "tagged-devices"}});
        let shared_user = json!({"Node": {"Name": "laptop.other.ts.net.", "Sharer": 42}, "UserProfile": {"LoginName": "carol@example.org"}});
        let (_dir, identity, _) =
            identity_with(&[("100.64.0.20", shared_tagged), ("100.64.0.21", shared_user)]).await;
        let principals = list(&["tag:ci", "carol@example.org"]);
        // Refused with why, not with advice to list a tag that cannot work.
        let shared = Verdict::Refuse(messages::SHARED_TAGGED_NOT_ADMITTED.to_string());
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.20")).await,
            shared
        );
        assert_eq!(
            identity
                .admit_serve(&principals, None, ip("100.64.0.20"))
                .await,
            shared
        );
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.21")).await,
            Verdict::Admit {
                login: Some("carol@example.org".into())
            }
        );
    }

    /// Every point a `whois` can read the identity at while consent is
    /// withdrawn: before, between the API and the epoch moving, and after.
    /// Whatever it reads, an answer it may use (one asked in the epoch that
    /// is current once withdrawal finished) never comes from the withdrawn
    /// API. Bumping the epoch first left a point where it read the new epoch
    /// and the old API.
    #[tokio::test]
    async fn no_point_of_a_withdrawal_pairs_the_new_epoch_with_the_old_api() {
        let (_dir, identity, _) = identity_with(&[]).await;
        let (_, old) = identity.snapshot();
        let old = old.expect("consent was given");
        let before = identity.snapshot();
        let mut between = None;
        identity.replace_api(None, |identity| between = Some(identity.snapshot()));
        let between = between.expect("ran between");
        let after = identity.snapshot();
        let final_epoch = after.0;
        assert_ne!(final_epoch, before.0, "a withdrawal starts a new epoch");
        for (point, (epoch, api)) in [("before", before), ("between", between), ("after", after)] {
            if epoch == final_epoch {
                assert!(
                    api.as_ref().is_none_or(|api| !Arc::ptr_eq(api, &old)),
                    "{point}: the current epoch carries the withdrawn API"
                );
            }
        }
    }

    /// A whois asked before consent was withdrawn, answered after, is not
    /// used and not cached.
    #[tokio::test]
    async fn an_answer_to_a_question_asked_before_consent_was_withdrawn_is_dropped() {
        let (_dir, identity, shared) =
            identity_with(&[("100.64.0.10", user("alice@example.com"))]).await;
        let gate = Arc::new(tokio::sync::Notify::new());
        shared.lock().whois_gate = Some(Arc::clone(&gate));
        let identity = Arc::new(identity);
        let principals = list(&["alice@example.com"]);
        let asking = {
            let identity = Arc::clone(&identity);
            let principals = principals.clone();
            tokio::spawn(async move { identity.admit_peer(&principals, ip("100.64.0.10")).await })
        };
        // Let the question reach the fake, then withdraw consent.
        tokio::time::sleep(Duration::from_millis(50)).await;
        identity.set_api(None);
        gate.notify_one();
        assert_eq!(
            asking.await.unwrap(),
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
        assert!(identity.cache.load().is_empty());
        assert_eq!(
            identity.admit_peer(&principals, ip("100.64.0.10")).await,
            Verdict::Refuse(messages::IDENTITY_UNREADABLE.to_string())
        );
    }
}
