// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What planning and applying ask of Tailscale (ADR 065 D2, D4, D5): the
//! cluster's OAuth client, its scope check, the Service a plan names and the
//! hosts advertising it. The engine holds one [`Tailnet`] and calls it by
//! method; everything Tailscale-shaped stays here (ADR-116 D1).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sea_orm::DatabaseConnection;
use temps_core::tailnet::TailscaleReport;

use super::plan::{service_of, service_tag, service_worker};
use super::{
    ScopeCheck, ServiceHost, TailnetApi, TailscaleClient, TailscaleCredential, DEFAULT_TAG,
    FIELD_CLIENT_ID, FIELD_CLIENT_SECRET, FIELD_TAILNET, KIND,
};
use crate::reachability::compute::ServiceHostsState;
use crate::reachability::plan::{
    PlanFacts, ServiceSeen, TailnetWorkerFact, TailscaleCredentialFact,
};
use crate::reachability::run::{unrecorded_hosts, worker_approval};
use crate::reachability::serve_entries;
use crate::reachability::service::ReachabilityError;
use crate::service::IngressConnectorService;

/// How long a scope check of the Tailscale credential is reused.
const SCOPE_CHECK_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a Service's host read is reused by the outside check.
const SERVICE_HOSTS_TTL: Duration = Duration::from_secs(30);
/// At most this many Services' host reads are kept.
const SERVICE_HOSTS_CACHED: usize = 256;

/// One Service's host read, and when it was made.
type CachedHosts = (Instant, Result<Vec<ServiceHost>, String>);

/// The cluster's Tailscale credential (ADR 065 D2): `(row id, name, the
/// OAuth client)`, decrypted for the call.
pub(crate) async fn load_credential(
    connectors: &IngressConnectorService,
) -> Result<Option<(i32, String, TailscaleCredential)>, String> {
    let Some((row, settings, secrets)) = connectors
        .credential(KIND)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let credential = TailscaleCredential::from_answers(&settings, &secrets)
        .map_err(|e| format!("Tailscale OAuth client {} ('{}'): {e}", row.id, row.name))?;
    Ok(Some((row.id, row.name, credential)))
}

/// The Service an address names, and the worker that will host it.
pub(crate) struct ServiceHostChoice<'a> {
    pub(crate) service: String,
    pub(crate) tailnet: String,
    pub(crate) worker: &'a TailnetWorkerFact,
    /// The tag the Service's hosts carry.
    pub(crate) tag: String,
}

/// Tailscale's API, its access tokens in memory only (ADR 065 D2), and the
/// short-lived reads planning reuses.
pub(crate) struct Tailnet {
    pub(crate) api: Arc<dyn TailnetApi>,
    /// The last scope check of the cluster's Tailscale credential, by row
    /// id: planning proves the scopes without probing on every plan.
    scopes: Mutex<Option<(i32, Instant, ScopeCheck)>>,
    /// Recent host reads of each Service, for the outside check.
    hosts: Mutex<HashMap<String, CachedHosts>>,
}

impl Tailnet {
    pub(crate) fn new() -> Self {
        Self::with_api(Arc::new(TailscaleClient::new()))
    }

    fn with_api(api: Arc<dyn TailnetApi>) -> Self {
        Self {
            api,
            scopes: Mutex::new(None),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// The same API with nothing cached: a check made after a run's
    /// changes must not reuse a read made before them.
    pub(crate) fn uncached(&self) -> Self {
        Self::with_api(self.api.clone())
    }

    /// The cluster's Tailscale credential as a plan sees it, its scopes
    /// proven at most every [`SCOPE_CHECK_TTL`] (ADR 065 D2).
    pub(crate) async fn credential_fact(
        &self,
        connectors: &IngressConnectorService,
    ) -> Option<TailscaleCredentialFact> {
        let (id, name, credential) = match load_credential(connectors).await {
            Ok(Some(found)) => found,
            Ok(None) => return None,
            Err(error) => {
                return Some(TailscaleCredentialFact {
                    id: 0,
                    name: String::new(),
                    refusal: Some(format!(
                        "Temps could not read the cluster's Tailscale OAuth client: {error}"
                    )),
                })
            }
        };
        let cached = self.scopes.lock().ok().and_then(|cache| {
            cache
                .as_ref()
                .filter(|(cached, at, _)| *cached == id && at.elapsed() < SCOPE_CHECK_TTL)
                .map(|(_, _, check)| check.clone())
        });
        let check = match cached {
            Some(check) => check,
            None => {
                let check = self.api.check_scopes(&credential).await;
                if let Ok(mut cache) = self.scopes.lock() {
                    *cache = Some((id, Instant::now(), check.clone()));
                }
                check
            }
        };
        Some(TailscaleCredentialFact {
            id,
            name,
            refusal: check.refusal(),
        })
    }

    /// The hosts advertising `service`, read at most every
    /// [`SERVICE_HOSTS_TTL`] for the outside check (ADR 065 D5).
    async fn cached_service_hosts(
        &self,
        connectors: &IngressConnectorService,
        service: &str,
    ) -> Result<Vec<ServiceHost>, String> {
        if let Some(hit) = self.hosts.lock().ok().and_then(|cache| {
            cache
                .get(service)
                .filter(|(at, _)| at.elapsed() < SERVICE_HOSTS_TTL)
                .map(|(_, hosts)| hosts.clone())
        }) {
            return hit;
        }
        let read = match load_credential(connectors).await {
            Ok(Some((_, _, credential))) => self
                .api
                .service_hosts(&credential, service)
                .await
                .map_err(|e| e.to_string()),
            Ok(None) => Err(
                "This cluster has no Tailscale OAuth client any more, so Temps cannot check which \
                 machines host it."
                    .to_string(),
            ),
            Err(error) => Err(error),
        };
        if let Ok(mut cache) = self.hosts.lock() {
            if cache.len() >= SERVICE_HOSTS_CACHED {
                cache.retain(|_, (at, _)| at.elapsed() < SERVICE_HOSTS_TTL);
            }
            if cache.len() < SERVICE_HOSTS_CACHED {
                cache.insert(service.to_string(), (Instant::now(), read.clone()));
            }
        }
        read
    }

    /// What the outside check knows of `service`'s hosts (ADR 065 D5).
    pub(crate) async fn service_hosts_state(
        &self,
        connectors: &IngressConnectorService,
        db: &DatabaseConnection,
        service: &str,
        node_id: i32,
        address_id: &str,
        report: &TailscaleReport,
    ) -> ServiceHostsState {
        let tag = report
            .tags
            .iter()
            .find(|t| t.as_str() == DEFAULT_TAG)
            .or_else(|| report.tags.first())
            .cloned()
            .unwrap_or_else(|| DEFAULT_TAG.to_string());
        let hosts = match self.cached_service_hosts(connectors, service).await {
            Ok(hosts) => hosts,
            Err(error) => {
                return ServiceHostsState {
                    name: service.to_string(),
                    tag,
                    unrecorded: Err(error),
                    approved: None,
                }
            }
        };
        let entries = match serve_entries::for_service(db, service).await {
            Ok(entries) => entries,
            Err(error) => {
                return ServiceHostsState {
                    name: service.to_string(),
                    tag,
                    unrecorded: Err(format!(
                        "the Serve entries naming it could not be read: {error}"
                    )),
                    approved: None,
                }
            }
        };
        let approved = worker_approval(&hosts, &entries, node_id, address_id);
        ServiceHostsState {
            name: service.to_string(),
            tag,
            unrecorded: Ok(unrecorded_hosts(&hosts, &entries, Some(address_id))),
            approved,
        }
    }

    /// ADR 065 D4: the Service `address` names, read before planning so the
    /// run is held to what was found. `None` when the address names no
    /// Service or the cluster's credential cannot be used.
    pub(crate) async fn service_seen(
        &self,
        connectors: &IngressConnectorService,
        facts: &PlanFacts,
        address: &str,
    ) -> Option<(String, ServiceSeen)> {
        let (service, _) = service_of(facts, address)?;
        let credential = facts
            .tailscale_credential
            .as_ref()
            .filter(|c| c.refusal.is_none())?;
        let seen = match load_credential(connectors).await {
            Ok(Some((id, _, oauth))) if id == credential.id => {
                match self.api.get_service(&oauth, &service).await {
                    Ok(None) => ServiceSeen::Absent,
                    Ok(Some(definition)) => ServiceSeen::Found(definition),
                    Err(error) => ServiceSeen::Unreadable(error.to_string()),
                }
            }
            Ok(_) => ServiceSeen::Unreadable(
                "the cluster's Tailscale OAuth client changed while planning".into(),
            ),
            Err(error) => ServiceSeen::Unreadable(error),
        };
        Some((service, seen))
    }

    /// ADR 065 D1, D4: the Service `address` names, on a worker that can
    /// host it now.
    pub(crate) fn service_host<'a>(
        facts: &'a PlanFacts,
        address: &str,
    ) -> Result<ServiceHostChoice<'a>, ReachabilityError> {
        let (service, tailnet) = service_of(facts, address).ok_or_else(|| {
            ReachabilityError::Conflict(format!(
                "{address} is not a Tailscale Service name in your tailnet any more."
            ))
        })?;
        let worker = service_worker(facts, &tailnet).ok_or_else(|| {
            ReachabilityError::Conflict(format!(
                "No worker on {tailnet} can host `{service}` any more."
            ))
        })?;
        Ok(ServiceHostChoice {
            tag: service_tag(worker),
            service,
            tailnet,
            worker,
        })
    }

    /// The OAuth client pasted into the answers, every scope proven before
    /// anything starts (ADR 065 D2). A resume needs it pasted again: Temps
    /// never stores it until the run saves it.
    pub(crate) async fn pasted_credential(
        &self,
        answer: impl Fn(&str) -> Option<String>,
        resuming: bool,
    ) -> Result<TailscaleCredential, ReachabilityError> {
        let mut settings = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for key in [FIELD_CLIENT_ID, FIELD_TAILNET] {
            if let Some(value) = answer(key) {
                settings.insert(key.to_string(), value);
            }
        }
        match answer(FIELD_CLIENT_SECRET) {
            Some(value) => {
                secrets.insert(FIELD_CLIENT_SECRET.to_string(), value);
            }
            None if resuming => {
                return Err(ReachabilityError::Validation(
                    "Paste the Tailscale OAuth client again: Temps never stores it until the \
                     run saves it."
                        .into(),
                ))
            }
            None => {}
        }
        let credential = TailscaleCredential::from_answers(&settings, &secrets)?;
        if let Some(refusal) = self.api.check_scopes(&credential).await.refusal() {
            return Err(ReachabilityError::Validation(refusal));
        }
        Ok(credential)
    }

    /// ADR 065 3b: the worker that will host `service` must be a device of
    /// the tailnet the credential creates it in. A credential's tailnet
    /// `-` is whichever tailnet the OAuth client belongs to, so the worker is
    /// looked up through it by its stable node id rather than by comparing
    /// names. Not found, or not answerable, refuses before anything is made.
    pub(crate) async fn check_worker(
        &self,
        connectors: &IngressConnectorService,
        entered: Option<&TailscaleCredential>,
        worker: &TailnetWorkerFact,
        service: &str,
    ) -> Result<(), ReachabilityError> {
        let credential = match entered {
            Some(credential) => credential.clone(),
            None => match load_credential(connectors).await {
                Ok(Some((_, _, credential))) => credential,
                Ok(None) => {
                    return Err(ReachabilityError::Conflict(format!(
                        "The cluster's Tailscale OAuth client is gone; plan `{service}` again."
                    )))
                }
                Err(error) => return Err(ReachabilityError::Validation(error)),
            },
        };
        let Some(node_id) = worker.report.node_id.as_deref().filter(|id| !id.is_empty()) else {
            return Err(ReachabilityError::Validation(format!(
                "{} has not reported its Tailscale node id yet, so Temps cannot check it is in \
                 the tailnet `{service}` would be created in. Try again once its agent has \
                 reported.",
                worker.name
            )));
        };
        match self.api.has_device(&credential, node_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(ReachabilityError::Validation(format!(
                "{} is not a device of the tailnet this Tailscale OAuth client manages \
                 (`{}`), so `{service}` would be created where {} cannot host it. Use an OAuth \
                 client of {}'s own tailnet.",
                worker.name,
                credential.tailnet,
                worker.name,
                worker.report.tailnet.as_deref().unwrap_or("the worker's"),
            ))),
            Err(error) => Err(ReachabilityError::Validation(format!(
                "Temps could not check that {} is in the tailnet `{service}` would be created \
                 in: {error}",
                worker.name
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use temps_core::EncryptionService;
    use temps_dns::{DnsProviderService, ManagedDnsRecordService};
    use temps_routes::CachedPeerTable;

    use super::*;
    use crate::providers::tailscale::{
        ScopeState, ServiceDefinition, TailscaleApiError, DEFAULT_TAILNET,
    };

    /// A tailnet whose answers each test sets, counting what was asked.
    struct FakeApi {
        scopes_ready: bool,
        device: Result<bool, ()>,
        scope_checks: AtomicUsize,
        device_checks: AtomicUsize,
    }

    impl FakeApi {
        fn new(scopes_ready: bool, device: Result<bool, ()>) -> Arc<Self> {
            Arc::new(Self {
                scopes_ready,
                device,
                scope_checks: AtomicUsize::new(0),
                device_checks: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl TailnetApi for FakeApi {
        async fn check_scopes(&self, _: &TailscaleCredential) -> ScopeCheck {
            self.scope_checks.fetch_add(1, Ordering::SeqCst);
            let state = if self.scopes_ready {
                ScopeState::Present
            } else {
                ScopeState::Missing
            };
            ScopeCheck {
                valid: true,
                devices_read: state,
                services: state,
                service_hosts: state,
                error: None,
            }
        }
        async fn get_service(
            &self,
            _: &TailscaleCredential,
            _: &str,
        ) -> Result<Option<ServiceDefinition>, TailscaleApiError> {
            unreachable!("not asked in these tests")
        }
        async fn create_service(
            &self,
            _: &TailscaleCredential,
            _: &ServiceDefinition,
        ) -> Result<(), TailscaleApiError> {
            unreachable!("never created while planning")
        }
        async fn delete_service(
            &self,
            _: &TailscaleCredential,
            _: &str,
        ) -> Result<(), TailscaleApiError> {
            unreachable!("never deleted while planning")
        }
        async fn service_hosts(
            &self,
            _: &TailscaleCredential,
            _: &str,
        ) -> Result<Vec<ServiceHost>, TailscaleApiError> {
            unreachable!("not asked in these tests")
        }
        async fn has_device(
            &self,
            _: &TailscaleCredential,
            _: &str,
        ) -> Result<bool, TailscaleApiError> {
            self.device_checks.fetch_add(1, Ordering::SeqCst);
            self.device.map_err(|()| TailscaleApiError::NotUnderstood {
                operation: "device lookup".into(),
                reason: "no answer".into(),
            })
        }
    }

    /// Connectors over no database: every credential read fails.
    fn unreadable_connectors() -> IngressConnectorService {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_errors((0..4).map(|_| sea_orm::DbErr::Custom("down".into())))
                .into_connection(),
        );
        let encryption =
            Arc::new(EncryptionService::new("0123456789abcdef0123456789abcdef").expect("key"));
        let dns = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
        let records = Arc::new(ManagedDnsRecordService::new(
            db.clone(),
            dns.clone(),
            encryption.clone(),
        ));
        IngressConnectorService::new(
            db.clone(),
            encryption,
            dns,
            records,
            Arc::new(CachedPeerTable::new(db)),
        )
    }

    fn answers(secret: bool) -> impl Fn(&str) -> Option<String> {
        move |key: &str| match key {
            FIELD_CLIENT_ID => Some("client".into()),
            FIELD_CLIENT_SECRET if secret => Some("tskey-client-secret".into()),
            _ => None,
        }
    }

    fn credential() -> TailscaleCredential {
        TailscaleCredential {
            client_id: "client".into(),
            client_secret: "tskey-client-secret".into(),
            tailnet: "-".into(),
        }
    }

    fn worker(node_id: Option<&str>) -> TailnetWorkerFact {
        TailnetWorkerFact {
            id: 7,
            name: "worker-1".into(),
            report: TailscaleReport {
                node_id: node_id.map(str::to_string),
                tailnet: Some("example.ts.net".into()),
                ..TailscaleReport::default()
            },
            minutes_ago: 0,
            listener_on_tailscale: None,
        }
    }

    #[tokio::test]
    async fn a_pasted_credential_is_used_only_once_every_scope_is_proven() {
        let refused = FakeApi::new(false, Ok(true));
        let tailnet = Tailnet::with_api(refused.clone());
        let error = tailnet
            .pasted_credential(answers(true), false)
            .await
            .expect_err("missing scopes refuse");
        assert!(matches!(error, ReachabilityError::Validation(_)), "{error}");
        assert_eq!(refused.scope_checks.load(Ordering::SeqCst), 1);

        let ready = FakeApi::new(true, Ok(true));
        let tailnet = Tailnet::with_api(ready.clone());
        let got = tailnet
            .pasted_credential(answers(true), false)
            .await
            .expect("proven");
        assert_eq!(got.client_id, "client");
        assert_eq!(got.tailnet, DEFAULT_TAILNET);
        assert_eq!(ready.scope_checks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_resume_needs_the_secret_pasted_again_and_asks_nothing_without_it() {
        let api = FakeApi::new(true, Ok(true));
        let tailnet = Tailnet::with_api(api.clone());
        let error = tailnet
            .pasted_credential(answers(false), true)
            .await
            .expect_err("no secret");
        assert!(
            error
                .to_string()
                .contains("Paste the Tailscale OAuth client again"),
            "{error}"
        );
        // Planning without a secret fails on the answers, before any call.
        let error = tailnet
            .pasted_credential(answers(false), false)
            .await
            .expect_err("no secret");
        assert!(!error.to_string().contains("again"), "{error}");
        assert_eq!(api.scope_checks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_hosting_worker_must_be_a_device_of_the_credentials_tailnet() {
        let connectors = unreadable_connectors();
        let entered = credential();
        for (device, ok) in [(Ok(true), true), (Ok(false), false), (Err(()), false)] {
            let api = FakeApi::new(true, device);
            let tailnet = Tailnet::with_api(api.clone());
            let got = tailnet
                .check_worker(&connectors, Some(&entered), &worker(Some("n1")), "svc:app")
                .await;
            assert_eq!(got.is_ok(), ok, "{device:?}: {got:?}");
            assert_eq!(api.device_checks.load(Ordering::SeqCst), 1);
        }
        // A worker that has not said which node it is is never assumed in.
        let api = FakeApi::new(true, Ok(true));
        let tailnet = Tailnet::with_api(api.clone());
        let got = tailnet
            .check_worker(&connectors, Some(&entered), &worker(None), "svc:app")
            .await;
        assert!(
            matches!(got, Err(ReachabilityError::Validation(_))),
            "{got:?}"
        );
        assert_eq!(api.device_checks.load(Ordering::SeqCst), 0);
        // Nor when the stored credential cannot be read.
        let got = tailnet
            .check_worker(&connectors, None, &worker(Some("n1")), "svc:app")
            .await;
        assert!(
            matches!(got, Err(ReachabilityError::Validation(_))),
            "{got:?}"
        );
        assert_eq!(api.device_checks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_unreadable_credential_is_a_refusal_not_an_absence() {
        let tailnet = Tailnet::with_api(FakeApi::new(true, Ok(true)));
        let fact = tailnet
            .credential_fact(&unreadable_connectors())
            .await
            .expect("a fact");
        assert!(
            fact.refusal
                .as_deref()
                .is_some_and(|r| r.contains("could not read")),
            "{fact:?}"
        );
    }

    #[test]
    fn a_run_s_check_shares_the_api_but_none_of_the_reads() {
        let tailnet = Tailnet::new();
        tailnet
            .hosts
            .lock()
            .expect("lock")
            .insert("svc:app".into(), (Instant::now(), Ok(Vec::new())));
        *tailnet.scopes.lock().expect("lock") = Some((
            1,
            Instant::now(),
            ScopeCheck {
                valid: true,
                devices_read: ScopeState::Present,
                services: ScopeState::Present,
                service_hosts: ScopeState::Present,
                error: None,
            },
        ));
        let fresh = tailnet.uncached();
        assert!(Arc::ptr_eq(&fresh.api, &tailnet.api));
        assert!(fresh.hosts.lock().expect("lock").is_empty());
        assert!(fresh.scopes.lock().expect("lock").is_none());
    }
}
