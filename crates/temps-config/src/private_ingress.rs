// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The operator's consent for the control plane to serve private addresses
//! (ADR-115 D2).
//!
//! On a worker the machine's owner consents in `agent.json`. On the control
//! plane the owner and the operator are the same person, so the consent is
//! a record: the `private_ingress` sub-document of the singleton `settings`
//! row, absent by default, written only through the audited
//! `PUT`/`DELETE /settings/private-ingress`. An unrelated `PUT /settings`
//! never resets it ([`crate::AppSettings::to_json_merged`] keeps keys it does
//! not own).

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QuerySelect, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

/// Key of the consent sub-document on the `settings` row.
pub const PRIVATE_INGRESS_KEY: &str = "private_ingress";

/// What `listen` takes to mean the Tailscale interface's address.
pub const LISTEN_TAILSCALE: &str = "tailscale";

/// Where the control plane serves private addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateListen {
    /// The Tailscale interface's address, whatever it is now.
    Tailscale,
    /// One private address of this machine.
    Address(IpAddr),
}

impl PrivateListen {
    /// Parse what an operator typed: `tailscale`, or a private address.
    /// A public address, `0.0.0.0`, `::` and loopback are refused with why.
    pub fn parse(input: &str) -> Result<Self, PrivateIngressError> {
        let text = input.trim();
        if text.eq_ignore_ascii_case(LISTEN_TAILSCALE) {
            return Ok(Self::Tailscale);
        }
        let ip: IpAddr = text.parse().map_err(|_| PrivateIngressError::Validation {
            field: "listen",
            reason: format!(
                "'{text}' is neither `tailscale` nor an IP address. Use `tailscale`, or one of \
                 this machine's private addresses."
            ),
        })?;
        temps_core::worker_ingress::check_private_listen(ip).map_err(|reason| {
            PrivateIngressError::Validation {
                field: "listen",
                reason: reason.replace(
                    "`temps agent ingress fixed` or `delegated`",
                    "a public way in",
                ),
            }
        })?;
        Ok(Self::Address(ip))
    }

    /// How it is stored and shown: `tailscale` or the address.
    pub fn as_stored(&self) -> String {
        match self {
            Self::Tailscale => LISTEN_TAILSCALE.to_string(),
            Self::Address(ip) => ip.to_string(),
        }
    }
}

/// The stored consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivateIngressConsent {
    /// `tailscale`, or the private address to serve on.
    pub listen: String,
    /// Who counts as inside (ADR-115 D8): private ranges such as
    /// `10.8.0.0/24`. Empty means the listen address's own network (the
    /// interface's subnet, or Tailscale's ranges).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admit: Vec<String>,
    /// Temps may write its own entries in tailscaled's Serve configuration
    /// (ADR 065 D2). Without it Temps never opens the LocalAPI for writing.
    #[serde(default)]
    pub tailscale_serve: bool,
    /// When the consent was last given or changed.
    #[schema(value_type = String, format = DateTime, example = "2026-11-24T10:00:00Z")]
    pub updated_at: DateTime<Utc>,
}

impl PrivateIngressConsent {
    /// Where to listen, or `None` for a stored value this version cannot
    /// read (it then serves nothing rather than guess).
    pub fn listen(&self) -> Option<PrivateListen> {
        PrivateListen::parse(&self.listen).ok()
    }

    /// The ranges named, or `None` for a stored value this version cannot
    /// read (it then serves nothing rather than admit more than asked).
    pub fn admit(&self) -> Option<Vec<temps_core::private_network::IpNet>> {
        temps_core::private_network::parse_admits(&self.admit).ok()
    }
}

/// What a client sends to give or change the consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivateIngressRequest {
    /// `tailscale`, or a private address of this machine: one in
    /// `100.64.0.0/10`, RFC 1918 or `fd00::/8`.
    pub listen: String,
    /// Who counts as inside: private ranges such as `10.8.0.0/24`. Leave
    /// empty to admit the listen address's own network.
    #[serde(default)]
    pub admit: Vec<String>,
    #[serde(default)]
    pub tailscale_serve: bool,
}

/// The consent and what the control plane made of it, as it last reported
/// in `node_public_ingress` row `0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivateIngressStatus {
    /// Whether a process of this control plane applies a consent here.
    /// `false` where the console runs without the proxy (split roles): no
    /// private address is served, a consent cannot be given, and `error`
    /// says why and what to run instead.
    pub available: bool,
    /// `null` until the operator consents.
    pub consent: Option<PrivateIngressConsent>,
    /// The private address served on now (`100.101.102.103`), `null` when
    /// none is.
    pub serving: Option<String>,
    /// The ranges admitted now (`10.8.0.0/24`), empty when nothing is
    /// served.
    pub admit: Vec<String>,
    /// Why nothing is served although consent was given, with the fix.
    pub error: Option<String>,
    /// This machine's private networks, with what serving on each would
    /// admit, for choosing one (ADR-115 D8). Empty where no private-ingress
    /// loop runs in this process.
    pub networks: Vec<temps_core::private_network::PrivateNetworkOption>,
    /// When the control plane last applied the consent.
    #[schema(value_type = Option<String>, format = DateTime, example = "2026-11-24T10:00:00Z")]
    pub reported_at: Option<DateTime<Utc>>,
}

impl PrivateIngressStatus {
    /// The status from the consent and row `0`, if there is one. A report
    /// that answers another consent (an earlier one, however late it was
    /// written) is not about this one, so it is left out.
    pub fn from_report(
        consent: Option<PrivateIngressConsent>,
        row: Option<&temps_entities::node_public_ingress::Model>,
    ) -> Self {
        let row = row.filter(|row| {
            consent.as_ref().is_some_and(|c| {
                temps_core::private_ingress::report_answers_consent(
                    row.listening.as_ref(),
                    c.updated_at,
                )
            })
        });
        let listening = row.and_then(|r| r.listening.as_ref());
        let text = |key: &str| {
            listening
                .and_then(|l| l.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let serving = text("https").and_then(|https| {
            temps_core::worker_ingress::parse_reported_ip(&https).map(|ip| ip.to_string())
        });
        let admit = if serving.is_some() {
            listening
                .and_then(|l| l.get("admit"))
                .and_then(serde_json::Value::as_array)
                .map(|ranges| {
                    ranges
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            available: true,
            serving,
            admit,
            networks: Vec::new(),
            error: text("error"),
            reported_at: row.map(|r| r.updated_at),
            consent,
        }
    }
}

#[derive(Debug, Error)]
pub enum PrivateIngressError {
    #[error("Private ingress field '{field}' is invalid: {reason}")]
    Validation { field: &'static str, reason: String },

    #[error("Failed to read or write the private ingress consent on settings row 1: {0}")]
    Database(#[from] DbErr),

    #[error("Failed to serialize the private ingress consent for settings row 1: {0}")]
    Serialization(#[from] serde_json::Error),

    /// No process of this control plane applies a consent (ADR-115 D7).
    #[error("{SPLIT_ROLE_UNAVAILABLE}")]
    Unavailable,
}

/// Why node `0` serves no private address when its console and proxy run
/// as separate processes: the loop runs only beside the proxy, in
/// `temps serve` with both (ADR-115 D7).
pub const SPLIT_ROLE_UNAVAILABLE: &str = "This control plane runs its console and proxy as separate processes, so it serves no private address itself. Run `temps serve` with both in one process to serve them here, or serve them from a worker (`temps agent ingress private`).";

/// Why node `0`'s consent refuses Tailscale Serve: node `0` does not write
/// its tailnet machine name's Serve entries; a worker's agent does.
pub const NODE_ZERO_TAILSCALE_SERVE: &str = "the control plane does not serve its tailnet machine name through Tailscale Serve; give the consent without it, or serve the machine name from a worker (`temps agent ingress private --tailscale-serve`)";

/// Read the consent from the `settings.data` document. A missing key is no
/// consent; a malformed one is logged and read as none, which serves
/// nothing.
pub fn consent_from_settings_document(data: &serde_json::Value) -> Option<PrivateIngressConsent> {
    match data.get(PRIVATE_INGRESS_KEY) {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(consent) => Some(consent),
            Err(error) => {
                warn!(
                    %error,
                    key = PRIVATE_INGRESS_KEY,
                    "Stored private ingress consent is malformed; serving no private address"
                );
                None
            }
        },
    }
}

/// Reads and writes the consent.
pub struct PrivateIngressService {
    db: Arc<DatabaseConnection>,
    /// Woken after every change, so the control plane applies it at once.
    /// `None` where no private-ingress loop runs in this process.
    runtime: Option<Arc<temps_core::private_ingress::ControlPlanePrivateIngress>>,
}

impl PrivateIngressService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        runtime: Option<Arc<temps_core::private_ingress::ControlPlanePrivateIngress>>,
    ) -> Self {
        Self { db, runtime }
    }

    pub async fn get(&self) -> Result<Option<PrivateIngressConsent>, PrivateIngressError> {
        let row = temps_entities::settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;
        Ok(row.and_then(|r| consent_from_settings_document(&r.data)))
    }

    /// The consent and the control plane's last report on it.
    pub async fn status(&self) -> Result<PrivateIngressStatus, PrivateIngressError> {
        let consent = self.get().await?;
        let row = match consent {
            Some(_) => {
                temps_entities::node_public_ingress::Entity::find_by_id(
                    temps_core::CONTROL_PLANE_NODE_ID,
                )
                .one(self.db.as_ref())
                .await?
            }
            None => None,
        };
        let mut status = PrivateIngressStatus::from_report(consent, row.as_ref());
        match &self.runtime {
            Some(runtime) => status.networks = runtime.networks(),
            // Nothing here applies the consent, so no report is current:
            // one left by an earlier single-process run is not served now.
            None => {
                status.available = false;
                status.serving = None;
                status.admit = Vec::new();
                status.reported_at = None;
                status.error = Some(SPLIT_ROLE_UNAVAILABLE.to_string());
            }
        }
        Ok(status)
    }

    /// Give or change the consent. Returns it before and after.
    pub async fn put(
        &self,
        request: PrivateIngressRequest,
    ) -> Result<(Option<PrivateIngressConsent>, PrivateIngressConsent), PrivateIngressError> {
        if self.runtime.is_none() {
            return Err(PrivateIngressError::Unavailable);
        }
        if request.tailscale_serve {
            return Err(PrivateIngressError::Validation {
                field: "tailscale_serve",
                reason: NODE_ZERO_TAILSCALE_SERVE.to_string(),
            });
        }
        let listen = PrivateListen::parse(&request.listen)?;
        let admit =
            temps_core::private_network::parse_admits(&request.admit).map_err(|reason| {
                PrivateIngressError::Validation {
                    field: "admit",
                    reason,
                }
            })?;
        let after = PrivateIngressConsent {
            listen: listen.as_stored(),
            admit: admit.iter().map(ToString::to_string).collect(),
            tailscale_serve: request.tailscale_serve,
            updated_at: Utc::now(),
        };
        let before = self.write(Some(&after)).await?;
        Ok((before, after))
    }

    /// Withdraw the consent. Returns what it was.
    pub async fn delete(&self) -> Result<Option<PrivateIngressConsent>, PrivateIngressError> {
        self.write(None).await
    }

    /// Replace the consent key on the settings row, locked `FOR UPDATE`
    /// inside a transaction; every other key is kept.
    async fn write(
        &self,
        consent: Option<&PrivateIngressConsent>,
    ) -> Result<Option<PrivateIngressConsent>, PrivateIngressError> {
        let now = Utc::now();
        let value = consent.map(serde_json::to_value).transpose()?;
        let is_postgres = matches!(
            self.db.get_database_backend(),
            sea_orm::DatabaseBackend::Postgres
        );
        let txn = self.db.begin().await?;
        let query = temps_entities::settings::Entity::find_by_id(1);
        let query = if is_postgres {
            query.lock_exclusive()
        } else {
            query
        };
        let row = query.one(&txn).await?;
        let before = row
            .as_ref()
            .and_then(|r| consent_from_settings_document(&r.data));
        match (row, value) {
            (Some(existing), value) => {
                let mut data = existing.data.clone();
                if !data.is_object() {
                    data = serde_json::json!({});
                }
                if let Some(map) = data.as_object_mut() {
                    match value {
                        Some(value) => {
                            map.insert(PRIVATE_INGRESS_KEY.to_string(), value);
                        }
                        None => {
                            map.remove(PRIVATE_INGRESS_KEY);
                        }
                    }
                }
                let mut am: temps_entities::settings::ActiveModel = existing.into();
                am.data = Set(data);
                am.updated_at = Set(now);
                am.update(&txn).await?;
            }
            (None, Some(value)) => {
                temps_entities::settings::ActiveModel {
                    id: Set(1),
                    data: Set(serde_json::json!({ PRIVATE_INGRESS_KEY: value })),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&txn)
                .await?;
            }
            (None, None) => {}
        }
        txn.commit().await?;
        if let Some(runtime) = &self.runtime {
            // A withdrawn consent stops serving now, not when the loop next
            // gets to it (it may be in the middle of inside checks), and a
            // pass that read the consent before this write can no longer
            // publish what it decided from it.
            runtime.consent_written(consent.is_none());
        }
        Ok(before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn settings_row(data: serde_json::Value) -> temps_entities::settings::Model {
        temps_entities::settings::Model {
            id: 1,
            data,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn listen_takes_tailscale_or_a_private_address_and_says_why_not() {
        assert_eq!(
            PrivateListen::parse(" Tailscale ").unwrap(),
            PrivateListen::Tailscale
        );
        assert_eq!(
            PrivateListen::parse("100.101.102.103").unwrap(),
            PrivateListen::Address("100.101.102.103".parse().unwrap())
        );
        assert!(PrivateListen::parse("fd00::7").is_ok());
        for (input, why) in [
            ("0.0.0.0", "every address of this machine"),
            ("::", "every address of this machine"),
            ("127.0.0.1", "loopback"),
            ("203.0.113.7", "not a private address"),
            ("my-box", "neither `tailscale` nor an IP address"),
        ] {
            let error = PrivateListen::parse(input).unwrap_err().to_string();
            assert!(error.contains("'listen'"), "{input}: {error}");
            assert!(error.contains(why), "{input}: {error}");
            assert!(!error.contains("temps agent ingress fixed"), "{error}");
        }
    }

    #[test]
    fn a_missing_or_malformed_record_is_no_consent() {
        assert!(consent_from_settings_document(&serde_json::json!({})).is_none());
        assert!(
            consent_from_settings_document(&serde_json::json!({"private_ingress": null})).is_none()
        );
        assert!(consent_from_settings_document(
            &serde_json::json!({"private_ingress": {"listen": 7}})
        )
        .is_none());
        let consent = consent_from_settings_document(&serde_json::json!({
            "private_ingress": {"listen": "tailscale", "updated_at": "2026-11-24T10:00:00Z"}
        }))
        .unwrap();
        assert!(!consent.tailscale_serve);
        assert_eq!(consent.listen(), Some(PrivateListen::Tailscale));
        // A stored value this version does not accept serves nothing.
        let odd = PrivateIngressConsent {
            listen: "203.0.113.7".into(),
            ..consent
        };
        assert_eq!(odd.listen(), None);
    }

    fn report(
        listening: serde_json::Value,
        at: &str,
    ) -> temps_entities::node_public_ingress::Model {
        let at = DateTime::parse_from_rfc3339(at)
            .unwrap()
            .with_timezone(&Utc);
        temps_entities::node_public_ingress::Model {
            node_id: 0,
            listen_address: None,
            http_port: 80,
            https_port: 443,
            public_address: None,
            interfaces: None,
            listening: Some(listening),
            mode: Some("private".into()),
            last_check: None,
            private_checks: None,
            tailscale: None,
            checked_at: None,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn the_status_reads_the_report_that_answers_the_consent() {
        let consent = PrivateIngressConsent {
            listen: "tailscale".into(),
            admit: vec![],
            tailscale_serve: false,
            updated_at: DateTime::parse_from_rfc3339("2026-11-24T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let answers = consent.updated_at;
        let served = report(
            serde_json::json!({
                "https": "100.101.102.103:443",
                "error": null,
                "admit": ["100.64.0.0/10", "fd7a:115c:a1e0::/48"],
                "consent_at": answers,
            }),
            "2026-11-24T10:00:05Z",
        );
        let status = PrivateIngressStatus::from_report(Some(consent.clone()), Some(&served));
        assert_eq!(status.serving.as_deref(), Some("100.101.102.103"));
        assert_eq!(status.admit, ["100.64.0.0/10", "fd7a:115c:a1e0::/48"]);
        assert!(status.available);
        assert_eq!(status.error, None);

        let refused = report(
            serde_json::json!({
                "https": null,
                "error": "The proxy listens on 203.0.113.7:443 only.",
                "consent_at": answers,
            }),
            "2026-11-24T10:00:05Z",
        );
        let status = PrivateIngressStatus::from_report(Some(consent.clone()), Some(&refused));
        assert_eq!(status.serving, None);
        assert!(status.error.unwrap().contains("203.0.113.7:443"));

        // A report of an earlier consent says nothing about this one, even
        // written after it by a pass that read the earlier one.
        let earlier = serde_json::json!({
            "https": "100.101.102.103:443",
            "consent_at": "2026-11-24T09:00:00Z",
        });
        for written in ["2026-11-24T09:00:01Z", "2026-11-24T10:00:05Z"] {
            let stale = report(earlier.clone(), written);
            let status = PrivateIngressStatus::from_report(Some(consent.clone()), Some(&stale));
            assert_eq!(status.serving, None);
            assert_eq!(status.reported_at, None);
        }

        let status = PrivateIngressStatus::from_report(None, Some(&served));
        assert_eq!(status.serving, None);
        assert!(status.consent.is_none());
    }

    /// ADR-115 D7: with the console and proxy in separate processes nothing
    /// applies a consent. Saving one is refused, saying why, and the status
    /// says so instead of the address an earlier single-process run served
    /// or waiting forever.
    #[tokio::test]
    async fn without_a_loop_in_this_process_consent_is_unavailable() {
        let consent = serde_json::json!({
            "private_ingress": {"listen": "10.8.0.1", "updated_at": "2026-11-26T10:00:00Z"}
        });
        let settings = temps_entities::settings::Model {
            id: 1,
            data: consent,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // An earlier single-process run's report on this same consent.
        let report = temps_entities::node_public_ingress::Model {
            node_id: 0,
            listen_address: None,
            http_port: 80,
            https_port: 443,
            public_address: None,
            interfaces: None,
            listening: Some(serde_json::json!({
                "https": "10.8.0.1:443",
                "admit": ["10.8.0.0/24"],
                "consent_at": "2026-11-26T10:00:00Z"
            })),
            mode: Some("private".to_string()),
            last_check: None,
            private_checks: None,
            tailscale: None,
            checked_at: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings]])
            .append_query_results([vec![report]])
            .into_connection();
        let service = PrivateIngressService::new(Arc::new(db), None);
        let status = service.status().await.unwrap();
        assert!(!status.available);
        assert_eq!(status.serving, None);
        assert!(status.admit.is_empty());
        assert_eq!(status.error.as_deref(), Some(SPLIT_ROLE_UNAVAILABLE));

        let error = service
            .put(PrivateIngressRequest {
                listen: "10.8.0.1".into(),
                admit: vec![],
                tailscale_serve: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, PrivateIngressError::Unavailable));
    }

    /// Node 0 does not write Tailscale Serve entries, so a consent asking
    /// for them is refused instead of stored and shown as enabled.
    #[tokio::test]
    async fn the_control_plane_refuses_tailscale_serve() {
        let service = PrivateIngressService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Some(temps_core::private_ingress::ControlPlanePrivateIngress::new()),
        );
        let error = service
            .put(PrivateIngressRequest {
                listen: "tailscale".into(),
                admit: vec![],
                tailscale_serve: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PrivateIngressError::Validation {
                field: "tailscale_serve",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn put_validates_before_touching_the_database() {
        let service = PrivateIngressService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Some(temps_core::private_ingress::ControlPlanePrivateIngress::new()),
        );
        let error = service
            .put(PrivateIngressRequest {
                listen: "0.0.0.0".into(),
                admit: vec![],
                tailscale_serve: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, PrivateIngressError::Validation { .. }));
        // ADR-115 D8: only private ranges can be admitted.
        let error = service
            .put(PrivateIngressRequest {
                listen: "10.8.0.1".into(),
                admit: vec!["10.8.0.0/24".into(), "0.0.0.0/0".into()],
                tailscale_serve: false,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                PrivateIngressError::Validation { field: "admit", .. }
            ),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("0.0.0.0/0 is not a private range"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn put_keeps_other_keys_and_wakes_the_loop() {
        let stored = settings_row(serde_json::json!({"external_url": "https://temps.example.com"}));
        let mut updated = stored.clone();
        updated.data["private_ingress"] =
            serde_json::json!({"listen": "tailscale", "updated_at": "2026-11-24T10:00:00Z"});
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .append_query_results([vec![updated]])
            .into_connection();
        let runtime = temps_core::private_ingress::ControlPlanePrivateIngress::new();
        let service = PrivateIngressService::new(Arc::new(db), Some(runtime.clone()));
        let (before, after) = service
            .put(PrivateIngressRequest {
                listen: "TAILSCALE".into(),
                admit: vec![" 100.64.0.7/10 ".into()],
                tailscale_serve: false,
            })
            .await
            .unwrap();
        assert!(before.is_none());
        assert_eq!(after.listen, "tailscale");
        // Stored normalized.
        assert_eq!(after.admit, ["100.64.0.0/10"]);
        assert!(!after.tailscale_serve);
        // The wake-up is stored until the loop waits for it.
        tokio::time::timeout(std::time::Duration::from_secs(1), runtime.changed())
            .await
            .expect("the loop is woken");
    }

    /// Withdrawing consent stops serving as soon as it commits: the loop may
    /// be inside checks that take a DNS timeout per host, and the old
    /// networks must not stay admitted until they finish.
    #[tokio::test]
    async fn withdrawing_consent_stops_serving_at_once_and_wakes_the_loop() {
        let stored = settings_row(serde_json::json!({
            "external_url": "https://temps.example.com",
            "private_ingress": {"listen": "10.8.0.1", "updated_at": "2026-11-24T10:00:00Z"},
        }));
        let mut updated = stored.clone();
        updated.data = serde_json::json!({"external_url": "https://temps.example.com"});
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stored]])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .append_query_results([vec![updated]])
            .into_connection();
        let runtime = temps_core::private_ingress::ControlPlanePrivateIngress::new();
        let serving = temps_core::private_ingress::PrivateIngressServing::new(
            "10.8.0.1".parse().unwrap(),
            vec!["10.8.0.0/24".parse().unwrap()],
        )
        .unwrap();
        runtime.set_serving(Some(serving.clone()));
        // A loop pass read the consent and is awaiting the machine's
        // networks when the withdrawal commits.
        let pass_read = runtime.consent_generation();
        let service = PrivateIngressService::new(Arc::new(db), Some(runtime.clone()));
        let before = service.delete().await.unwrap();
        assert_eq!(before.map(|c| c.listen), Some("10.8.0.1".to_string()));
        assert_eq!(runtime.serving(), None);
        // That pass decided from the withdrawn consent: it cannot serve it again.
        assert!(!runtime.set_serving_for(pass_read, Some(serving)));
        assert_eq!(runtime.serving(), None);
        tokio::time::timeout(std::time::Duration::from_secs(1), runtime.changed())
            .await
            .expect("the loop is woken");
    }

    #[tokio::test]
    async fn a_database_failure_is_reported_as_such() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = PrivateIngressService::new(Arc::new(db), None);
        let error = service.get().await.unwrap_err();
        assert!(matches!(error, PrivateIngressError::Database(_)));
        assert!(error.to_string().contains("settings row 1"));
    }
}
