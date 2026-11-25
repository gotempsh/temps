// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! First-run onboarding state, per install.
//!
//! The welcome flow (`/welcome` in the console) asks one question per screen
//! and records here whether it was finished, which steps the operator left
//! for later and why, and the install's default way in. Stored as its own
//! sub-document of the singleton `settings` row under [`ONBOARDING_KEY`], so
//! every admin sees the same state and an unrelated `PUT /settings` never
//! resets it ([`crate::AppSettings::to_json_merged`] preserves keys it does
//! not own).

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QuerySelect, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

/// Key of the onboarding sub-document on the `settings` row.
pub const ONBOARDING_KEY: &str = "onboarding";

/// Longest reason kept for a step left for later.
pub const MAX_LATER_REASON_CHARS: usize = 200;

/// A step of the welcome flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStep {
    /// Check the box: read-only facts about the machine.
    Host,
    /// Reach your apps: who reaches them, and how.
    Reach,
    /// Wake your agents: the model they think with.
    Agents,
    /// Ship something: a starter or the operator's repository.
    App,
}

impl OnboardingStep {
    pub const ALL: [OnboardingStep; 4] = [
        OnboardingStep::Host,
        OnboardingStep::Reach,
        OnboardingStep::Agents,
        OnboardingStep::App,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            OnboardingStep::Host => "host",
            OnboardingStep::Reach => "reach",
            OnboardingStep::Agents => "agents",
            OnboardingStep::App => "app",
        }
    }
}

/// A step the operator chose to leave for later, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OnboardingLaterStep {
    pub step: OnboardingStep,
    /// What the operator said, or the screen's own words ("No domain yet").
    pub reason: String,
    /// When it was left for later.
    #[schema(value_type = String, format = DateTime, example = "2026-11-24T10:00:00Z")]
    pub at: DateTime<Utc>,
}

/// Who should reach the apps on this install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachAudience {
    Public,
    Private,
}

/// How traffic gets in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachWay {
    /// A Cloudflare Tunnel connector; no port is opened.
    Tunnel,
    /// Ports 80 and 443 open to the internet.
    Direct,
    /// The operator's domain, pointed at the box's Tailscale address.
    TailnetDomain,
    /// The machine's tailnet name, through Tailscale Serve.
    TailnetName,
}

impl ReachWay {
    /// Whether the way is public or private.
    pub fn audience(self) -> ReachAudience {
        match self {
            ReachWay::Tunnel | ReachWay::Direct => ReachAudience::Public,
            ReachWay::TailnetDomain | ReachWay::TailnetName => ReachAudience::Private,
        }
    }

    /// Whether the way needs a domain at all.
    pub fn needs_domain(self) -> bool {
        !matches!(self, ReachWay::TailnetName)
    }
}

/// How apps are named under a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachNaming {
    /// All apps under one wildcard: `shop.apps.example.com`.
    Wildcard,
    /// A full name per app, chosen when the app is added.
    PerApp,
}

/// The install's default way in, which a new project's "Make it reachable"
/// starts from so the answer is not asked again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DefaultReach {
    pub audience: ReachAudience,
    pub way: ReachWay,
    pub naming: ReachNaming,
    /// Base domain for a wildcard; `null` per app and for the tailnet name.
    #[serde(default)]
    pub domain: Option<String>,
    /// The tunnel connector, when the way is a tunnel and one exists.
    #[serde(default)]
    pub connector_id: Option<i32>,
}

/// The stored onboarding record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OnboardingState {
    /// When the welcome flow was finished or skipped as a whole. `null` until
    /// then: an instance admin with no projects is sent to `/welcome`.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime, example = "2026-11-24T10:00:00Z")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Steps the operator finished in the welcome flow.
    #[serde(default)]
    pub done: Vec<OnboardingStep>,
    /// Steps left for later, with why. The Setup page lists them.
    #[serde(default)]
    pub later: Vec<OnboardingLaterStep>,
    /// The default way in, once chosen.
    #[serde(default)]
    pub default_reach: Option<DefaultReach>,
}

/// A step to leave for later, as sent by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OnboardingLaterInput {
    pub step: OnboardingStep,
    pub reason: String,
}

/// A partial update. Absent fields are left as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OnboardingPatch {
    /// `true` marks the flow finished (keeping the first time it was);
    /// `false` reopens it.
    #[serde(default)]
    pub completed: Option<bool>,
    /// Mark these steps done. A step done is no longer "later".
    #[serde(default)]
    pub done: Option<Vec<OnboardingStep>>,
    /// Leave these steps for later, replacing an earlier reason for the same
    /// step.
    #[serde(default)]
    pub later: Option<Vec<OnboardingLaterInput>>,
    /// Set the default way in.
    #[serde(default)]
    pub default_reach: Option<DefaultReach>,
}

#[derive(Debug, Error)]
pub enum OnboardingError {
    #[error("Onboarding field '{field}' is invalid: {reason}")]
    Validation { field: &'static str, reason: String },

    #[error("Failed to read or write the onboarding record on settings row 1: {0}")]
    Database(#[from] DbErr),

    #[error("Failed to serialize the onboarding record for settings row 1: {0}")]
    Serialization(#[from] serde_json::Error),
}

fn invalid(field: &'static str, reason: impl Into<String>) -> OnboardingError {
    OnboardingError::Validation {
        field,
        reason: reason.into(),
    }
}

/// Null when `input` is a usable DNS name (`apps.example.com`); otherwise why
/// not. Same rules as the console's domain field.
pub fn domain_problem(input: &str) -> Option<String> {
    let domain = input.trim();
    if domain.is_empty() {
        return Some("enter a domain you control, such as apps.example.com".into());
    }
    if domain.contains("://") || domain.contains('/') || domain.starts_with("*.") {
        return Some("just the domain: no scheme, path or wildcard".into());
    }
    if domain.len() > 253 {
        return Some("a domain name is at most 253 characters".into());
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return Some(format!(
            "'{domain}' has no dot; use a name like apps.example.com"
        ));
    }
    for label in &labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Some(format!(
                "'{label}' is not a valid label: use lowercase letters, digits and dashes"
            ));
        }
    }
    let tld = labels.last().copied().unwrap_or_default();
    if tld.len() < 2 || tld.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("'{domain}' does not end in a top-level domain"));
    }
    None
}

impl DefaultReach {
    /// Check the combination is one Temps can set up. Returns it normalized.
    pub fn validated(mut self) -> Result<Self, OnboardingError> {
        if self.way.audience() != self.audience {
            return Err(invalid(
                "default_reach.way",
                format!(
                    "{:?} is a {:?} way in, but the audience is {:?}",
                    self.way,
                    self.way.audience(),
                    self.audience
                ),
            ));
        }
        self.domain = self
            .domain
            .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|d| !d.is_empty());
        match (self.way.needs_domain(), self.naming, self.domain.as_deref()) {
            (false, _, Some(_)) => {
                return Err(invalid(
                    "default_reach.domain",
                    "the tailnet name uses the machine's own name; leave the domain empty",
                ))
            }
            (false, ReachNaming::Wildcard, None) => {
                return Err(invalid(
                    "default_reach.naming",
                    "the tailnet name serves one app per machine name; use per_app",
                ))
            }
            (true, ReachNaming::Wildcard, None) => {
                return Err(invalid(
                    "default_reach.domain",
                    "all apps under one domain needs that domain",
                ))
            }
            (true, _, Some(domain)) => {
                if let Some(problem) = domain_problem(domain) {
                    return Err(invalid("default_reach.domain", problem));
                }
            }
            _ => {}
        }
        if self.connector_id.is_some() && self.way != ReachWay::Tunnel {
            return Err(invalid(
                "default_reach.connector_id",
                "only a tunnel has a connector",
            ));
        }
        if matches!(self.connector_id, Some(id) if id <= 0) {
            return Err(invalid(
                "default_reach.connector_id",
                "must be a positive connector id",
            ));
        }
        Ok(self)
    }
}

impl OnboardingState {
    /// Read the record from the `settings.data` document. A missing key is
    /// the empty record; a malformed one is logged and read as empty rather
    /// than failing every console load.
    pub fn from_settings_document(data: &serde_json::Value) -> Self {
        match data.get(ONBOARDING_KEY) {
            None | Some(serde_json::Value::Null) => Self::default(),
            Some(value) => match serde_json::from_value(value.clone()) {
                Ok(state) => state,
                Err(error) => {
                    warn!(
                        %error,
                        key = ONBOARDING_KEY,
                        "Stored onboarding record is malformed; reading it as not started"
                    );
                    Self::default()
                }
            },
        }
    }

    /// The step left for later, when it is.
    pub fn later_reason(&self, step: OnboardingStep) -> Option<&str> {
        self.later
            .iter()
            .find(|l| l.step == step)
            .map(|l| l.reason.as_str())
    }

    /// Apply a patch, validating it first. Pure, so it is tested without a
    /// database.
    pub fn apply(
        mut self,
        patch: OnboardingPatch,
        now: DateTime<Utc>,
    ) -> Result<Self, OnboardingError> {
        let default_reach = patch
            .default_reach
            .map(DefaultReach::validated)
            .transpose()?;

        let later = match patch.later {
            Some(items) => {
                let mut seen = std::collections::HashSet::new();
                let mut checked = Vec::with_capacity(items.len());
                for item in items {
                    let reason = item.reason.trim().to_string();
                    if reason.is_empty() {
                        return Err(invalid(
                            "later.reason",
                            format!("say why step '{}' is left for later", item.step.as_str()),
                        ));
                    }
                    if reason.chars().count() > MAX_LATER_REASON_CHARS {
                        return Err(invalid(
                            "later.reason",
                            format!(
                                "the reason for step '{}' is longer than {MAX_LATER_REASON_CHARS} characters",
                                item.step.as_str()
                            ),
                        ));
                    }
                    if !seen.insert(item.step) {
                        return Err(invalid(
                            "later.step",
                            format!("step '{}' is listed twice", item.step.as_str()),
                        ));
                    }
                    checked.push((item.step, reason));
                }
                Some(checked)
            }
            None => None,
        };

        if let (Some(done), Some(later)) = (&patch.done, &later) {
            if let Some((step, _)) = later.iter().find(|(step, _)| done.contains(step)) {
                return Err(invalid(
                    "later.step",
                    format!(
                        "step '{}' cannot be both done and left for later",
                        step.as_str()
                    ),
                ));
            }
        }

        if let Some(later) = later {
            for (step, reason) in later {
                self.done.retain(|d| *d != step);
                match self.later.iter_mut().find(|l| l.step == step) {
                    Some(existing) => {
                        existing.reason = reason;
                        existing.at = now;
                    }
                    None => self.later.push(OnboardingLaterStep {
                        step,
                        reason,
                        at: now,
                    }),
                }
            }
        }
        if let Some(done) = patch.done {
            for step in done {
                self.later.retain(|l| l.step != step);
                if !self.done.contains(&step) {
                    self.done.push(step);
                }
            }
        }
        // Keep a stable order, the flow's, whatever order steps arrived in.
        self.done
            .sort_by_key(|s| OnboardingStep::ALL.iter().position(|a| a == s));
        self.later
            .sort_by_key(|l| OnboardingStep::ALL.iter().position(|a| *a == l.step));

        if let Some(reach) = default_reach {
            self.default_reach = Some(reach);
        }
        match patch.completed {
            Some(true) => {
                self.completed_at.get_or_insert(now);
            }
            Some(false) => self.completed_at = None,
            None => {}
        }
        Ok(self)
    }
}

/// Reads and writes the onboarding record.
pub struct OnboardingService {
    db: Arc<DatabaseConnection>,
}

impl OnboardingService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get(&self) -> Result<OnboardingState, OnboardingError> {
        let row = temps_entities::settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;
        Ok(row
            .map(|r| OnboardingState::from_settings_document(&r.data))
            .unwrap_or_default())
    }

    /// Apply `patch` and return the record before and after.
    ///
    /// A read-modify-write of a document shared with `AppSettings` and other
    /// subsystems, so the row is locked `FOR UPDATE` inside a transaction;
    /// only the onboarding key is replaced.
    pub async fn update(
        &self,
        patch: OnboardingPatch,
    ) -> Result<(OnboardingState, OnboardingState), OnboardingError> {
        let now = Utc::now();
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
            .map(|r| OnboardingState::from_settings_document(&r.data))
            .unwrap_or_default();
        let after = before.clone().apply(patch, now)?;
        let value = serde_json::to_value(&after)?;

        match row {
            Some(existing) => {
                let mut data = existing.data.clone();
                match data.as_object_mut() {
                    Some(map) => {
                        map.insert(ONBOARDING_KEY.to_string(), value);
                    }
                    None => data = serde_json::json!({ ONBOARDING_KEY: value }),
                }
                let mut am: temps_entities::settings::ActiveModel = existing.into();
                am.data = Set(data);
                am.updated_at = Set(now);
                am.update(&txn).await?;
            }
            None => {
                temps_entities::settings::ActiveModel {
                    id: Set(1),
                    data: Set(serde_json::json!({ ONBOARDING_KEY: value })),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&txn)
                .await?;
            }
        }
        txn.commit().await?;
        Ok((before, after))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-24T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn reach(way: ReachWay, naming: ReachNaming, domain: Option<&str>) -> DefaultReach {
        DefaultReach {
            audience: way.audience(),
            way,
            naming,
            domain: domain.map(str::to_string),
            connector_id: None,
        }
    }

    #[test]
    fn missing_or_malformed_record_reads_as_not_started() {
        assert_eq!(
            OnboardingState::from_settings_document(&serde_json::json!({})),
            OnboardingState::default()
        );
        assert_eq!(
            OnboardingState::from_settings_document(&serde_json::json!({"onboarding": 7})),
            OnboardingState::default()
        );
    }

    #[test]
    fn completing_keeps_the_first_time() {
        let first = OnboardingState::default()
            .apply(
                OnboardingPatch {
                    completed: Some(true),
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        let later = now() + chrono::Duration::hours(1);
        let again = first
            .clone()
            .apply(
                OnboardingPatch {
                    completed: Some(true),
                    ..Default::default()
                },
                later,
            )
            .unwrap();
        assert_eq!(again.completed_at, Some(now()));
        let reopened = again
            .apply(
                OnboardingPatch {
                    completed: Some(false),
                    ..Default::default()
                },
                later,
            )
            .unwrap();
        assert_eq!(reopened.completed_at, None);
    }

    #[test]
    fn a_step_done_is_no_longer_later_and_the_reverse() {
        let state = OnboardingState::default()
            .apply(
                OnboardingPatch {
                    later: Some(vec![OnboardingLaterInput {
                        step: OnboardingStep::Reach,
                        reason: "No domain yet".into(),
                    }]),
                    done: Some(vec![OnboardingStep::App, OnboardingStep::Host]),
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        assert_eq!(state.done, vec![OnboardingStep::Host, OnboardingStep::App]);
        assert_eq!(
            state.later_reason(OnboardingStep::Reach),
            Some("No domain yet")
        );

        let state = state
            .apply(
                OnboardingPatch {
                    done: Some(vec![OnboardingStep::Reach]),
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        assert_eq!(state.later_reason(OnboardingStep::Reach), None);
        assert!(state.done.contains(&OnboardingStep::Reach));

        let state = state
            .apply(
                OnboardingPatch {
                    later: Some(vec![OnboardingLaterInput {
                        step: OnboardingStep::App,
                        reason: "Not now".into(),
                    }]),
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        assert!(!state.done.contains(&OnboardingStep::App));
        assert_eq!(state.later_reason(OnboardingStep::App), Some("Not now"));
    }

    #[test]
    fn later_needs_a_short_reason_and_each_step_once() {
        let blank = OnboardingState::default().apply(
            OnboardingPatch {
                later: Some(vec![OnboardingLaterInput {
                    step: OnboardingStep::Agents,
                    reason: "   ".into(),
                }]),
                ..Default::default()
            },
            now(),
        );
        assert!(matches!(
            blank,
            Err(OnboardingError::Validation {
                field: "later.reason",
                ..
            })
        ));

        let long = OnboardingState::default().apply(
            OnboardingPatch {
                later: Some(vec![OnboardingLaterInput {
                    step: OnboardingStep::Agents,
                    reason: "x".repeat(MAX_LATER_REASON_CHARS + 1),
                }]),
                ..Default::default()
            },
            now(),
        );
        assert!(long.is_err());

        let twice = OnboardingState::default().apply(
            OnboardingPatch {
                later: Some(vec![
                    OnboardingLaterInput {
                        step: OnboardingStep::Agents,
                        reason: "a".into(),
                    },
                    OnboardingLaterInput {
                        step: OnboardingStep::Agents,
                        reason: "b".into(),
                    },
                ]),
                ..Default::default()
            },
            now(),
        );
        assert!(matches!(
            twice,
            Err(OnboardingError::Validation {
                field: "later.step",
                ..
            })
        ));

        let both = OnboardingState::default().apply(
            OnboardingPatch {
                done: Some(vec![OnboardingStep::Agents]),
                later: Some(vec![OnboardingLaterInput {
                    step: OnboardingStep::Agents,
                    reason: "a".into(),
                }]),
                ..Default::default()
            },
            now(),
        );
        assert!(both.is_err());
    }

    #[test]
    fn default_reach_is_normalized_and_checked() {
        let ok = reach(
            ReachWay::Direct,
            ReachNaming::Wildcard,
            Some(" Apps.Example.COM. "),
        )
        .validated()
        .unwrap();
        assert_eq!(ok.domain.as_deref(), Some("apps.example.com"));

        // A wildcard needs its domain.
        assert!(reach(ReachWay::Tunnel, ReachNaming::Wildcard, None)
            .validated()
            .is_err());
        // Per app may leave it empty.
        assert!(reach(ReachWay::Tunnel, ReachNaming::PerApp, None)
            .validated()
            .is_ok());
        // The tailnet name takes no domain and is one app per name.
        assert!(reach(
            ReachWay::TailnetName,
            ReachNaming::PerApp,
            Some("a.example.com")
        )
        .validated()
        .is_err());
        assert!(reach(ReachWay::TailnetName, ReachNaming::Wildcard, None)
            .validated()
            .is_err());
        assert!(reach(ReachWay::TailnetName, ReachNaming::PerApp, None)
            .validated()
            .is_ok());

        // Audience has to match the way.
        let mut mismatched = reach(ReachWay::Tunnel, ReachNaming::PerApp, None);
        mismatched.audience = ReachAudience::Private;
        assert!(mismatched.validated().is_err());

        // Only a tunnel has a connector.
        let mut connector = reach(ReachWay::Direct, ReachNaming::PerApp, None);
        connector.connector_id = Some(3);
        assert!(connector.validated().is_err());
    }

    #[test]
    fn domain_problems_say_what_is_wrong() {
        assert_eq!(domain_problem("apps.example.com"), None);
        assert!(domain_problem("").is_some());
        assert!(domain_problem("https://apps.example.com").is_some());
        assert!(domain_problem("*.example.com").is_some());
        assert!(domain_problem("localhost").is_some());
        assert!(domain_problem("bad_label.example.com").is_some());
        assert!(domain_problem("example.123").is_some());
    }

    #[tokio::test]
    async fn get_reads_the_sub_document_and_ignores_the_rest() {
        let row = temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({
                "external_url": "https://temps.example.com",
                "onboarding": { "completed_at": "2026-11-24T10:00:00Z", "done": ["host"] }
            }),
            created_at: now(),
            updated_at: now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();
        let state = OnboardingService::new(Arc::new(db)).get().await.unwrap();
        assert_eq!(state.completed_at, Some(now()));
        assert_eq!(state.done, vec![OnboardingStep::Host]);
    }

    #[tokio::test]
    async fn get_on_a_fresh_install_is_not_started() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::settings::Model>::new()])
            .into_connection();
        let state = OnboardingService::new(Arc::new(db)).get().await.unwrap();
        assert_eq!(state, OnboardingState::default());
    }

    #[tokio::test]
    async fn get_surfaces_a_database_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".into())])
            .into_connection();
        let error = OnboardingService::new(Arc::new(db))
            .get()
            .await
            .unwrap_err();
        assert!(matches!(error, OnboardingError::Database(_)));
        assert!(error.to_string().contains("settings row 1"));
    }
}
