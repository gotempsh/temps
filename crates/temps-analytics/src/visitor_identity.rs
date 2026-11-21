// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Identify a visitor: attach the app's user id and traits to it (ADR 109 D4).
//!
//! Two callers share this service:
//!
//! - **The browser**, through the public ingest endpoint `POST /_temps/identify`.
//!   It can only label the visitor the request already resolves to (its
//!   cookie or the tracker's stored id); it never names a visitor. What it
//!   sends is a claim, stored with source `browser`.
//! - **The app's backend**, through
//!   `PUT /projects/{project_id}/analytics/visitors/{visitor_id}/identify`
//!   with a deployment token or API key. Stored with source `server`.
//!
//! Trust rules, applied in [`plan_identify`]:
//!
//! - A browser cannot replace a user id the backend set, nor a trait the
//!   backend set (through identify or enrichment); those keys are skipped.
//! - A browser sending the same user id the backend set keeps it `server`.
//! - A project can refuse browser identify altogether
//!   (`project_analytics_settings.accept_browser_identify`).
//!
//! A cookieless project (ADR 109 D3) stores nothing: its visitor id changes
//! every day by design, and a user id or trait on it (an email, an account id)
//! would link those days back together. Identify answers
//! [`IdentifyOutcome::CookielessProject`] and writes no row.
//!
//! Volume: identify runs at sign-in, not per event. The row is written only
//! when something changes, under a row lock so two concurrent calls cannot
//! lose each other's traits.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use temps_entities::visitor;
use temps_entities::visitor_identity_source::VisitorIdentitySource;
use thiserror::Error;
use utoipa::ToSchema;

/// Longest accepted user id, in characters. Ids are the app's own keys
/// (database ids, UUIDs, emails); anything longer is a mistake or abuse.
pub const MAX_USER_ID_CHARS: usize = 256;

/// What an identify call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentifyOutcome {
    /// The visitor now carries the user id and traits.
    Updated,
    /// The visitor already carried exactly this; nothing was written.
    Unchanged,
    /// No visitor of this project matches the request (it has not sent an
    /// event yet, or the id is unknown). Nothing was written.
    VisitorNotFound,
    /// The project counts visitors without cookies; identify stores nothing
    /// there, so days stay unlinked.
    CookielessProject,
    /// The project accepts identify only from its backend.
    BrowserIdentifyRefused,
    /// The backend already identified this visitor as another user; a
    /// browser claim does not replace it.
    ServerIdentityKept,
}

impl IdentifyOutcome {
    /// Whether the visitor carries the requested identity afterwards.
    pub fn applied(self) -> bool {
        matches!(self, IdentifyOutcome::Updated | IdentifyOutcome::Unchanged)
    }

    pub fn message(self) -> &'static str {
        match self {
            IdentifyOutcome::Updated => "Visitor identified",
            IdentifyOutcome::Unchanged => "Visitor already identified with this data",
            IdentifyOutcome::VisitorNotFound => {
                "No visitor of this project matches the request; nothing was stored"
            }
            IdentifyOutcome::CookielessProject => {
                "This project uses cookieless tracking, which stores no user identity"
            }
            IdentifyOutcome::BrowserIdentifyRefused => {
                "This project accepts identify only from its backend"
            }
            IdentifyOutcome::ServerIdentityKept => {
                "The app's backend identified this visitor as another user; kept"
            }
        }
    }
}

/// The answer of both identify endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IdentifyVisitorResponse {
    /// Whether the visitor carries the requested identity now
    /// (`updated` or `unchanged`).
    pub applied: bool,
    pub outcome: IdentifyOutcome,
    pub message: String,
}

impl From<IdentifyOutcome> for IdentifyVisitorResponse {
    fn from(outcome: IdentifyOutcome) -> Self {
        Self {
            applied: outcome.applied(),
            outcome,
            message: outcome.message().to_string(),
        }
    }
}

/// Body of both identify endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IdentifyVisitorRequest {
    /// The app's own id for the person (database id, UUID, email). At most
    /// 256 characters, no control characters.
    pub user_id: String,
    /// Traits merged into the visitor's `custom_data` at the top level; a
    /// key set to `null` is removed. At most 32 keys and 8 KB.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub traits: Option<Value>,
}

/// A validated identify call.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentifyInput {
    pub user_id: String,
    /// Merged into `custom_data` at the top level; `null` removes a key.
    pub traits: Map<String, Value>,
}

/// The result, with what the audit entry of the server path needs.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentifyResult {
    pub outcome: IdentifyOutcome,
    pub visitor_row_id: Option<i32>,
    /// Trait keys this call set and removed (names only).
    pub set_keys: Vec<String>,
    pub removed_keys: Vec<String>,
}

impl IdentifyResult {
    fn only(outcome: IdentifyOutcome) -> Self {
        Self {
            outcome,
            visitor_row_id: None,
            set_keys: Vec::new(),
            removed_keys: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum VisitorIdentityError {
    #[error("Invalid identify call for project {project_id}: {reason}")]
    Validation { project_id: i32, reason: String },

    #[error("Project {project_id} not found while {operation}")]
    ProjectNotFound {
        project_id: i32,
        operation: &'static str,
    },

    #[error("Database error while {operation} for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

fn db_error(
    project_id: i32,
    operation: &'static str,
) -> impl FnOnce(DbErr) -> VisitorIdentityError {
    move |source| VisitorIdentityError::Database {
        project_id,
        operation,
        source,
    }
}

/// Check a user id and traits before anything is read.
pub fn validate_identify_input(
    project_id: i32,
    user_id: &str,
    traits: Option<&Value>,
) -> Result<IdentifyInput, VisitorIdentityError> {
    let invalid = |reason: String| VisitorIdentityError::Validation { project_id, reason };
    if user_id.trim().is_empty() {
        return Err(invalid("user_id must not be empty".to_string()));
    }
    let chars = user_id.chars().count();
    if chars > MAX_USER_ID_CHARS {
        return Err(invalid(format!(
            "user_id is {chars} characters; at most {MAX_USER_ID_CHARS} are allowed"
        )));
    }
    if user_id.chars().any(char::is_control) {
        return Err(invalid(
            "user_id must not contain control characters".to_string(),
        ));
    }
    let traits = match traits {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(invalid("traits must be a JSON object".to_string())),
    };
    // The tighter limits deployment tokens have apply to both paths.
    crate::analytics::validate_scoped_enrichment(&Value::Object(traits.clone()))
        .map_err(|error| invalid(format!("traits: {error}")))?;
    Ok(IdentifyInput {
        user_id: user_id.to_string(),
        traits,
    })
}

/// The identity part of a visitor row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CurrentIdentity {
    pub user_id: Option<String>,
    pub source: Option<VisitorIdentitySource>,
    pub custom_data: Option<Value>,
    pub trait_sources: Option<Value>,
}

/// What [`plan_identify`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum IdentifyPlan {
    /// Refused or nothing to do; write nothing.
    Skip(IdentifyOutcome),
    /// Boxed: the write is much larger than a skip.
    Write(Box<IdentifyWrite>),
}

/// The visitor row update an identify call makes.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentifyWrite {
    pub user_id: String,
    pub source: VisitorIdentitySource,
    /// `user_id` or its source changed (so `identified_at` moves).
    pub identity_changed: bool,
    pub custom_data: Option<Value>,
    pub trait_sources: Option<Value>,
    pub set_keys: Vec<String>,
    pub removed_keys: Vec<String>,
}

fn source_of(
    trait_sources: Option<&Map<String, Value>>,
    key: &str,
) -> Option<VisitorIdentitySource> {
    trait_sources
        .and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .and_then(VisitorIdentitySource::from_stored)
}

/// Whether the backend owns `key`: it marked it `server`, or the key holds a
/// value with no recorded source. Every browser write records its source, so
/// an unmarked value came from backend enrichment, including everything a row
/// held before sources were recorded.
fn backend_owns(
    data: Option<&Map<String, Value>>,
    trait_sources: Option<&Map<String, Value>>,
    key: &str,
) -> bool {
    match source_of(trait_sources, key) {
        Some(source) => source == VisitorIdentitySource::Server,
        None => data.is_some_and(|data| data.contains_key(key)),
    }
}

/// Apply the trust rules to one call. Pure, so every rule is unit-tested.
pub fn plan_identify(
    current: &CurrentIdentity,
    source: VisitorIdentitySource,
    input: &IdentifyInput,
) -> IdentifyPlan {
    use VisitorIdentitySource::{Browser, Server};

    let server_owned_id = current.source == Some(Server);
    let same_user = current.user_id.as_deref() == Some(input.user_id.as_str());
    if source == Browser && server_owned_id && !same_user {
        return IdentifyPlan::Skip(IdentifyOutcome::ServerIdentityKept);
    }
    // A browser repeating the backend's id does not downgrade it.
    let new_source = if server_owned_id && same_user {
        Server
    } else {
        source
    };

    let current_data = current.custom_data.as_ref().and_then(Value::as_object);
    let current_sources = current.trait_sources.as_ref().and_then(Value::as_object);
    let mut data = current_data.cloned().unwrap_or_default();
    let mut sources = current_sources.cloned().unwrap_or_default();
    let mut set_keys = Vec::new();
    let mut removed_keys = Vec::new();
    for (key, value) in &input.traits {
        if source == Browser && backend_owns(current_data, current_sources, key) {
            continue;
        }
        if value.is_null() {
            if data.remove(key).is_some() {
                removed_keys.push(key.clone());
            }
            sources.remove(key);
        } else {
            if data.get(key) != Some(value) {
                set_keys.push(key.clone());
            }
            data.insert(key.clone(), value.clone());
            // A trait is a claim of whoever sent it, even when the browser
            // repeats the backend's user id.
            sources.insert(key.clone(), Value::String(source.as_str().to_string()));
        }
    }

    let normalise = |map: Map<String, Value>, was: Option<&Value>| {
        if map.is_empty() && was.is_none() {
            None
        } else {
            Some(Value::Object(map))
        }
    };
    let custom_data = normalise(data, current.custom_data.as_ref());
    let trait_sources = normalise(sources, current.trait_sources.as_ref());

    let identity_changed = !same_user || current.source != Some(new_source);
    if !identity_changed
        && custom_data == current.custom_data
        && trait_sources == current.trait_sources
    {
        return IdentifyPlan::Skip(IdentifyOutcome::Unchanged);
    }
    IdentifyPlan::Write(Box::new(IdentifyWrite {
        user_id: input.user_id.clone(),
        source: new_source,
        identity_changed,
        custom_data,
        trait_sources,
        set_keys,
        removed_keys,
    }))
}

/// Mark the keys an enrichment call set as backend-owned, so a later browser
/// identify cannot overwrite them; removed keys lose their mark.
pub fn mark_enriched_keys(trait_sources: Option<&Value>, incoming: &Value) -> Option<Value> {
    let incoming = incoming.as_object()?;
    let mut sources = trait_sources
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in incoming {
        if value.is_null() {
            sources.remove(key);
        } else {
            sources.insert(
                key.clone(),
                Value::String(VisitorIdentitySource::Server.as_str().to_string()),
            );
        }
    }
    if sources.is_empty() && trait_sources.is_none() {
        None
    } else {
        Some(Value::Object(sources))
    }
}

/// How a project treats identify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentifyPolicy {
    pub mode: AnalyticsIdentityMode,
    pub accept_browser_identify: bool,
}

pub struct VisitorIdentityService {
    db: Arc<DatabaseConnection>,
    cookie_crypto: Arc<temps_core::CookieCrypto>,
}

impl VisitorIdentityService {
    pub fn new(db: Arc<DatabaseConnection>, cookie_crypto: Arc<temps_core::CookieCrypto>) -> Self {
        Self { db, cookie_crypto }
    }

    /// The plaintext visitor id of a reference the backend sends: the sealed
    /// `enc_...` cookie value, or (for user and API-key callers) a raw id.
    /// `None` when a sealed value does not decrypt, which callers answer like
    /// an unknown visitor so the cookie key cannot be probed.
    pub fn visitor_guid(&self, reference: &str) -> Option<String> {
        match reference.strip_prefix("enc_") {
            Some(sealed) => self.cookie_crypto.decrypt(sealed).ok(),
            None => Some(reference.to_string()),
        }
    }

    /// The project's identity mode and browser identify setting, in one
    /// query. A project without a settings row accepts browser identify.
    pub async fn policy(&self, project_id: i32) -> Result<IdentifyPolicy, VisitorIdentityError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT p.analytics_identity_mode, \
                        COALESCE(s.accept_browser_identify, TRUE) AS accept_browser_identify \
                 FROM projects p \
                 LEFT JOIN project_analytics_settings s ON s.project_id = p.id \
                 WHERE p.id = $1 AND p.is_deleted = false",
                [project_id.into()],
            ))
            .await
            .map_err(db_error(project_id, "reading the identify policy"))?
            .ok_or(VisitorIdentityError::ProjectNotFound {
                project_id,
                operation: "reading the identify policy",
            })?;
        let mode: String = row
            .try_get("", "analytics_identity_mode")
            .map_err(db_error(project_id, "decoding the identity mode"))?;
        let accept_browser_identify: bool =
            row.try_get("", "accept_browser_identify")
                .map_err(db_error(
                    project_id,
                    "decoding the browser identify setting",
                ))?;
        Ok(IdentifyPolicy {
            mode: crate::cookieless::mode_from_stored(Some(&mode)),
            accept_browser_identify,
        })
    }

    /// Identify `visitor_guid` (the plaintext visitor id) of `project_id`.
    pub async fn identify(
        &self,
        project_id: i32,
        visitor_guid: &str,
        source: VisitorIdentitySource,
        input: &IdentifyInput,
    ) -> Result<IdentifyResult, VisitorIdentityError> {
        let policy = self.policy(project_id).await?;
        if policy.mode.is_cookieless() {
            return Ok(IdentifyResult::only(IdentifyOutcome::CookielessProject));
        }
        if source == VisitorIdentitySource::Browser && !policy.accept_browser_identify {
            return Ok(IdentifyResult::only(
                IdentifyOutcome::BrowserIdentifyRefused,
            ));
        }

        let txn = self
            .db
            .begin()
            .await
            .map_err(db_error(project_id, "starting the identify transaction"))?;
        let Some(row) = visitor::Entity::find()
            .filter(visitor::Column::ProjectId.eq(project_id))
            .filter(visitor::Column::VisitorId.eq(visitor_guid))
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(db_error(project_id, "loading the visitor to identify"))?
        else {
            return Ok(IdentifyResult::only(IdentifyOutcome::VisitorNotFound));
        };
        let visitor_row_id = row.id;
        let current = CurrentIdentity {
            user_id: row.user_id.clone(),
            source: row.identity_source,
            custom_data: row.custom_data.clone(),
            trait_sources: row.trait_sources.clone(),
        };

        let (
            user_id,
            new_source,
            identity_changed,
            custom_data,
            trait_sources,
            set_keys,
            removed_keys,
        ) = match plan_identify(&current, source, input) {
            IdentifyPlan::Skip(outcome) => {
                return Ok(IdentifyResult {
                    outcome,
                    visitor_row_id: Some(visitor_row_id),
                    set_keys: Vec::new(),
                    removed_keys: Vec::new(),
                })
            }
            IdentifyPlan::Write(write) => {
                let IdentifyWrite {
                    user_id,
                    source,
                    identity_changed,
                    custom_data,
                    trait_sources,
                    set_keys,
                    removed_keys,
                } = *write;
                (
                    user_id,
                    source,
                    identity_changed,
                    custom_data,
                    trait_sources,
                    set_keys,
                    removed_keys,
                )
            }
        };
        if let Some(merged) = custom_data.as_ref() {
            crate::analytics::validate_merge(current.custom_data.as_ref(), merged, true).map_err(
                |error| VisitorIdentityError::Validation {
                    project_id,
                    reason: format!("traits: {error}"),
                },
            )?;
        }

        let mut active: visitor::ActiveModel = row.into();
        active.user_id = Set(Some(user_id));
        active.identity_source = Set(Some(new_source));
        if identity_changed {
            active.identified_at = Set(Some(Utc::now()));
        }
        active.custom_data = Set(custom_data);
        active.trait_sources = Set(trait_sources);
        active
            .update(&txn)
            .await
            .map_err(db_error(project_id, "saving the visitor identity"))?;
        txn.commit()
            .await
            .map_err(db_error(project_id, "committing the visitor identity"))?;

        Ok(IdentifyResult {
            outcome: IdentifyOutcome::Updated,
            visitor_row_id: Some(visitor_row_id),
            set_keys,
            removed_keys,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use VisitorIdentitySource::{Browser, Server};

    fn input(user_id: &str, traits: Value) -> IdentifyInput {
        validate_identify_input(1, user_id, Some(&traits)).expect("valid input")
    }

    fn write_parts(
        plan: IdentifyPlan,
    ) -> (
        String,
        VisitorIdentitySource,
        bool,
        Option<Value>,
        Option<Value>,
    ) {
        match plan {
            IdentifyPlan::Write(write) => (
                write.user_id,
                write.source,
                write.identity_changed,
                write.custom_data,
                write.trait_sources,
            ),
            IdentifyPlan::Skip(outcome) => panic!("expected a write, got {outcome:?}"),
        }
    }

    #[test]
    fn a_first_identify_sets_the_user_and_marks_each_trait() {
        let plan = plan_identify(
            &CurrentIdentity::default(),
            Browser,
            &input("u_42", json!({"plan": "pro"})),
        );
        let (user, source, changed, data, sources) = write_parts(plan);
        assert_eq!(user, "u_42");
        assert_eq!(source, Browser);
        assert!(changed);
        assert_eq!(data, Some(json!({"plan": "pro"})));
        assert_eq!(sources, Some(json!({"plan": "browser"})));
    }

    #[test]
    fn repeating_the_same_identify_writes_nothing() {
        let current = CurrentIdentity {
            user_id: Some("u_42".into()),
            source: Some(Browser),
            custom_data: Some(json!({"plan": "pro"})),
            trait_sources: Some(json!({"plan": "browser"})),
        };
        assert_eq!(
            plan_identify(&current, Browser, &input("u_42", json!({"plan": "pro"}))),
            IdentifyPlan::Skip(IdentifyOutcome::Unchanged)
        );
    }

    #[test]
    fn a_browser_cannot_replace_the_backends_user_id() {
        let current = CurrentIdentity {
            user_id: Some("u_42".into()),
            source: Some(Server),
            ..Default::default()
        };
        assert_eq!(
            plan_identify(&current, Browser, &input("u_attacker", json!({}))),
            IdentifyPlan::Skip(IdentifyOutcome::ServerIdentityKept)
        );
    }

    #[test]
    fn a_browser_repeating_the_backends_id_keeps_it_vouched_and_cannot_touch_its_traits() {
        let current = CurrentIdentity {
            user_id: Some("u_42".into()),
            source: Some(Server),
            custom_data: Some(json!({"email": "a@example.com"})),
            trait_sources: Some(json!({"email": "server"})),
        };
        let plan = plan_identify(
            &current,
            Browser,
            &input("u_42", json!({"email": "b@example.com", "theme": "dark"})),
        );
        let (_, source, changed, data, sources) = write_parts(plan);
        assert_eq!(source, Server);
        assert!(!changed);
        assert_eq!(
            data,
            Some(json!({"email": "a@example.com", "theme": "dark"}))
        );
        assert_eq!(
            sources,
            Some(json!({"email": "server", "theme": "browser"}))
        );
    }

    #[test]
    fn a_browser_cannot_change_or_remove_traits_enriched_before_sources_were_recorded() {
        // A row enriched before this release: data, but no sources at all.
        let current = CurrentIdentity {
            custom_data: Some(json!({"plan": "enterprise", "company": "Example Co"})),
            ..Default::default()
        };
        let plan = plan_identify(
            &current,
            Browser,
            &input(
                "u_42",
                json!({"plan": "free", "company": null, "theme": "dark"}),
            ),
        );
        let (_, _, _, data, sources) = write_parts(plan);
        assert_eq!(
            data,
            Some(json!({"plan": "enterprise", "company": "Example Co", "theme": "dark"}))
        );
        assert_eq!(sources, Some(json!({"theme": "browser"})));

        // The backend can still change them.
        let plan = plan_identify(&current, Server, &input("u_42", json!({"plan": "free"})));
        let (_, _, _, data, _) = write_parts(plan);
        assert_eq!(data, Some(json!({"plan": "free", "company": "Example Co"})));
    }

    #[test]
    fn the_backend_takes_over_a_browser_claim() {
        let current = CurrentIdentity {
            user_id: Some("u_claimed".into()),
            source: Some(Browser),
            custom_data: Some(json!({"plan": "free"})),
            trait_sources: Some(json!({"plan": "browser"})),
        };
        let (user, source, changed, data, sources) = write_parts(plan_identify(
            &current,
            Server,
            &input("u_42", json!({"plan": "pro"})),
        ));
        assert_eq!(user, "u_42");
        assert_eq!(source, Server);
        assert!(changed);
        assert_eq!(data, Some(json!({"plan": "pro"})));
        assert_eq!(sources, Some(json!({"plan": "server"})));
    }

    #[test]
    fn a_null_trait_removes_the_key_and_its_mark() {
        let current = CurrentIdentity {
            user_id: Some("u_42".into()),
            source: Some(Server),
            custom_data: Some(json!({"plan": "pro", "beta": true})),
            trait_sources: Some(json!({"plan": "server", "beta": "server"})),
        };
        let plan = plan_identify(&current, Server, &input("u_42", json!({"beta": null})));
        let IdentifyPlan::Write(write) = plan else {
            panic!("expected a write");
        };
        let IdentifyWrite {
            custom_data,
            trait_sources,
            removed_keys,
            ..
        } = *write;
        assert_eq!(custom_data, Some(json!({"plan": "pro"})));
        assert_eq!(trait_sources, Some(json!({"plan": "server"})));
        assert_eq!(removed_keys, vec!["beta".to_string()]);
    }

    #[test]
    fn enrichment_marks_its_keys_as_backend_owned() {
        let marked = mark_enriched_keys(
            Some(&json!({"theme": "browser", "old": "server"})),
            &json!({"email": "a@example.com", "old": null}),
        );
        assert_eq!(marked, Some(json!({"theme": "browser", "email": "server"})));
        assert_eq!(mark_enriched_keys(None, &json!({})), None);
    }

    #[test]
    fn user_ids_and_traits_are_validated_with_the_project_named() {
        for bad in ["", "   ", "a\nb"] {
            let error = validate_identify_input(7, bad, None).expect_err("rejected");
            assert!(error.to_string().contains("project 7"), "{error}");
        }
        let long = "x".repeat(MAX_USER_ID_CHARS + 1);
        assert!(validate_identify_input(7, &long, None).is_err());
        assert!(validate_identify_input(7, "u", Some(&json!(["not", "an", "object"]))).is_err());
        let many: Map<String, Value> = (0..40).map(|i| (format!("k{i}"), json!(i))).collect();
        assert!(validate_identify_input(7, "u", Some(&Value::Object(many))).is_err());
        assert!(validate_identify_input(7, "u_42", None).is_ok());
    }

    #[test]
    fn outcomes_say_whether_the_identity_was_applied() {
        assert!(IdentifyOutcome::Updated.applied());
        assert!(IdentifyOutcome::Unchanged.applied());
        for refused in [
            IdentifyOutcome::VisitorNotFound,
            IdentifyOutcome::CookielessProject,
            IdentifyOutcome::BrowserIdentifyRefused,
            IdentifyOutcome::ServerIdentityKept,
        ] {
            let response = IdentifyVisitorResponse::from(refused);
            assert!(!response.applied);
            assert!(!response.message.is_empty());
        }
    }

    #[tokio::test]
    async fn identify_against_the_database_honours_mode_policy_and_trust() -> anyhow::Result<()> {
        use sea_orm::EntityTrait;
        let (analytics, db, _container) = crate::create_test_analytics_service!(
            "identify_against_the_database_honours_mode_policy_and_trust"
        );
        let row = visitor::Entity::find()
            .one(db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("the seeded visitor exists"))?;
        let project_id = row.project_id;
        let crypto = Arc::new(
            temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests")
                .map_err(|e| anyhow::anyhow!("test cookie key: {e}"))?,
        );
        let service = VisitorIdentityService::new(db.clone(), crypto.clone());
        let sealed = format!(
            "enc_{}",
            crypto
                .encrypt(&row.visitor_id)
                .map_err(|e| anyhow::anyhow!("seal: {e}"))?
        );
        assert_eq!(
            service.visitor_guid(&sealed).as_deref(),
            Some(row.visitor_id.as_str())
        );
        assert_eq!(service.visitor_guid("enc_not-sealed"), None);

        // An unknown visitor: nothing written.
        let missing = service
            .identify(
                project_id,
                "no-such-visitor",
                Browser,
                &input("u_1", json!({})),
            )
            .await?;
        assert_eq!(missing.outcome, IdentifyOutcome::VisitorNotFound);

        // The browser claims an identity; the backend then vouches for another.
        let claimed = service
            .identify(
                project_id,
                &row.visitor_id,
                Browser,
                &input("u_1", json!({"plan": "free"})),
            )
            .await?;
        assert_eq!(claimed.outcome, IdentifyOutcome::Updated);
        let vouched = service
            .identify(
                project_id,
                &row.visitor_id,
                Server,
                &input("u_2", json!({"plan": "pro"})),
            )
            .await?;
        assert_eq!(vouched.outcome, IdentifyOutcome::Updated);
        let refused = service
            .identify(
                project_id,
                &row.visitor_id,
                Browser,
                &input("u_1", json!({})),
            )
            .await?;
        assert_eq!(refused.outcome, IdentifyOutcome::ServerIdentityKept);
        let stored = visitor::Entity::find_by_id(row.id)
            .one(db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("visitor still exists"))?;
        assert_eq!(stored.user_id.as_deref(), Some("u_2"));
        assert_eq!(stored.identity_source, Some(Server));
        assert!(stored.identified_at.is_some());
        assert_eq!(stored.custom_data, Some(json!({"plan": "pro"})));
        assert_eq!(stored.trait_sources, Some(json!({"plan": "server"})));

        // The read side: the visitor page and the "same user" filter.
        {
            use crate::Analytics;
            let details = analytics
                .get_visitor_details_by_id(row.id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("visitor details"))?;
            assert_eq!(details.user_id.as_deref(), Some("u_2"));
            assert_eq!(details.identity_source.as_deref(), Some("server"));
            assert!(details.identified_at.is_some());
            assert_eq!(details.trait_sources, Some(json!({"plan": "server"})));

            let list = |user_id: &str| {
                let segment = crate::types::requests::VisitorSegmentFilters {
                    filter_user_id: Some(user_id.to_string()),
                    ..Default::default()
                };
                analytics.get_visitors(
                    row.last_seen - chrono::Duration::days(1),
                    Utc::now() + chrono::Duration::days(1),
                    project_id,
                    None,
                    None,
                    None,
                    None,
                    None,
                    segment,
                )
            };
            let same_user = list("u_2").await?;
            assert_eq!(same_user.total_count, 1);
            assert_eq!(same_user.visitors[0].id, row.id);
            assert_eq!(same_user.visitors[0].user_id.as_deref(), Some("u_2"));
            assert_eq!(
                same_user.visitors[0].identity_source.as_deref(),
                Some("server")
            );
            assert_eq!(list("someone-else").await?.total_count, 0);
        }

        // Refusing browser identify stops the browser path only.
        db.execute_unprepared(&format!(
            "INSERT INTO project_analytics_settings (project_id, exclude_datacenter_traffic, \
             exclude_spam_referrers, accept_browser_identify, created_at, updated_at) \
             VALUES ({project_id}, true, true, false, now(), now())"
        ))
        .await?;
        let off = service
            .identify(
                project_id,
                &row.visitor_id,
                Browser,
                &input("u_2", json!({"x": 1})),
            )
            .await?;
        assert_eq!(off.outcome, IdentifyOutcome::BrowserIdentifyRefused);

        // A cookieless project stores nothing, from either side.
        db.execute_unprepared(&format!(
            "UPDATE projects SET analytics_identity_mode = 'cookieless' WHERE id = {project_id}"
        ))
        .await?;
        let cookieless = service
            .identify(
                project_id,
                &row.visitor_id,
                Server,
                &input("u_3", json!({"y": 2})),
            )
            .await?;
        assert_eq!(cookieless.outcome, IdentifyOutcome::CookielessProject);
        let after = visitor::Entity::find_by_id(row.id)
            .one(db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("visitor still exists"))?;
        assert_eq!(after.user_id.as_deref(), Some("u_2"));
        assert_eq!(after.custom_data, Some(json!({"plan": "pro"})));
        Ok(())
    }
}
