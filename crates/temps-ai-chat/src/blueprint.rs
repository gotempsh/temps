// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The architecture blueprint of an AI workspace (ADR-053).
//!
//! A blueprint is the agreement the user and the model build against: the
//! parts of the application and the flows of data between them. The model
//! proposes a version, the user accepts or rejects it, and every chat of the
//! workspace sees the accepted one in its context. The shape is open where the
//! user decides (any stack, any topics and routes) and closed where Temps acts
//! (an app runs from a preset, a Dockerfile or an image; data lives in an
//! engine Temps operates).
//!
//! Versions are immutable; deciding one only changes its status. Which project
//! or service realises a part is a binding on the blueprint, read live: its
//! status is resolved from the project's latest deployment or the service's
//! state on every read and never stored.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_entities::{ai_blueprint_versions, ai_blueprints, external_services, projects};
use thiserror::Error;
use utoipa::ToSchema;

/// Most parts one blueprint may have.
pub const MAX_COMPONENTS: usize = 40;
/// Most flows one blueprint may have.
pub const MAX_FLOWS: usize = 120;
const MAX_SUMMARY_CHARS: usize = 2_000;
const MAX_CHANGE_SUMMARY_CHARS: usize = 500;
const MAX_LABEL_CHARS: usize = 200;
const MAX_NAME_CHARS: usize = 80;
const MAX_PURPOSE_CHARS: usize = 300;
const MAX_ID_CHARS: usize = 40;
const MAX_HOLDS: usize = 20;
/// Problems reported at once; a document with more is mostly wrong anyway.
const MAX_REPORTED_PROBLEMS: usize = 20;

/// What a part of the application is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Who or what uses the application from outside: a browser, a phone, a CLI.
    Client,
    /// Code Temps builds and runs.
    App,
    /// State Temps runs as a managed service.
    Data,
    /// Something the application talks to that Temps does not run.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientSurface {
    Browser,
    Mobile,
    Cli,
    ThirdParty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppRole {
    /// Serves pages people look at.
    Web,
    /// Serves an API other parts call.
    Api,
    /// Runs in the background, consuming jobs or messages.
    Worker,
    /// Runs on a schedule.
    Cron,
    /// Static files only, no server code.
    Static,
}

/// How Temps runs an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppRuntime {
    /// A detected framework preset (Next.js, Vite, Bun, Python…).
    Preset,
    /// The app's own Dockerfile.
    Dockerfile,
    /// A ready-made image, which is also how a broker such as NATS runs.
    Image,
}

/// The engines Temps operates as managed services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataEngine {
    Postgres,
    Redis,
    Mongodb,
    Mariadb,
    S3,
}

impl DataEngine {
    fn label(self) -> &'static str {
        match self {
            DataEngine::Postgres => "postgres",
            DataEngine::Redis => "redis",
            DataEngine::Mongodb => "mongodb",
            DataEngine::Mariadb => "mariadb",
            DataEngine::S3 => "s3",
        }
    }
}

/// What travels along a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    /// Requests and responses over HTTP.
    Http,
    /// A long-lived WebSocket connection.
    Websocket,
    /// Reads and writes to a database (Postgres, MariaDB or MongoDB).
    Query,
    /// Messages published to a topic or stream.
    Publish,
    /// Messages consumed from a topic or stream.
    Subscribe,
    /// Cached values read and written in Redis.
    Cache,
    /// Files stored in an S3 bucket.
    Blob,
    /// An HTTP callback from or to an outside service.
    Webhook,
}

impl FlowKind {
    fn label(self) -> &'static str {
        match self {
            FlowKind::Http => "http",
            FlowKind::Websocket => "websocket",
            FlowKind::Query => "query",
            FlowKind::Publish => "publish",
            FlowKind::Subscribe => "subscribe",
            FlowKind::Cache => "cache",
            FlowKind::Blob => "blob",
            FlowKind::Webhook => "webhook",
        }
    }
}

/// One part of the application. Which fields apply depends on `kind`:
/// a client has `surface`; an app has `role`, `stack` and `runtime` (and
/// `image` when it runs an image); data has `engine` and `holds`; an external
/// service has `provider`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Component {
    /// Stable slug, unique in the document: `web`, `api`, `orders-worker`.
    pub id: String,
    pub name: String,
    /// Why it exists, in one sentence.
    pub purpose: String,
    pub kind: ComponentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<ClientSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AppRole>,
    /// Free text, as the user wants it: "Next.js", "Bun + Hono", "FastAPI".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<AppRuntime>,
    /// The image an app runs, when `runtime` is `image`: `nats:2.10`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<DataEngine>,
    /// What lives in a data part: tables, key prefixes, buckets, streams.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
    /// The outside service: "Stripe", "an LLM API", "GitHub OAuth".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Data moving from one part to another. `from` is the part that starts it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Flow {
    pub from: String,
    pub to: String,
    pub kind: FlowKind,
    /// What travels: "POST /api/tasks", "topic task.created", "tables tasks, users".
    pub label: String,
}

/// The design of an application: what it is for, its parts and its flows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BlueprintDocument {
    /// One paragraph: what the application does and for whom.
    pub summary: String,
    pub components: Vec<Component>,
    #[serde(default)]
    pub flows: Vec<Flow>,
}

/// Where a version stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// Waiting for the user.
    Proposed,
    /// What the workspace builds against.
    Accepted,
    /// The user turned it down.
    Rejected,
    /// A later version replaced it before or after it was accepted.
    Superseded,
}

impl VersionStatus {
    fn as_str(self) -> &'static str {
        match self {
            VersionStatus::Proposed => "proposed",
            VersionStatus::Accepted => "accepted",
            VersionStatus::Rejected => "rejected",
            VersionStatus::Superseded => "superseded",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(VersionStatus::Proposed),
            "accepted" => Some(VersionStatus::Accepted),
            "rejected" => Some(VersionStatus::Rejected),
            "superseded" => Some(VersionStatus::Superseded),
            _ => None,
        }
    }
}

/// The user's answer to a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Reject,
}

/// The project or service that realises one part.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComponentBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<i32>,
}

/// One stored version, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct BlueprintVersion {
    pub version: i32,
    pub document: BlueprintDocument,
    pub change_summary: String,
    pub status: VersionStatus,
    /// The chat that proposed it; `None` when the user wrote it.
    pub proposed_in: Option<i64>,
    pub created_by: i32,
    pub decided_by: Option<i32>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A workspace's blueprint as the console and the model see it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlueprintState {
    pub accepted: Option<BlueprintVersion>,
    pub proposal: Option<BlueprintVersion>,
    pub bindings: BTreeMap<String, ComponentBinding>,
}

/// How a bound part is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    /// Bound, but nothing has run yet.
    Created,
    /// A deployment is in progress, or the service is starting.
    Building,
    /// The latest deployment finished, or the service is running.
    Live,
    /// The latest deployment or the service failed.
    Failed,
    /// The service is stopped.
    Stopped,
    /// The bound project or service no longer exists.
    Missing,
}

/// The live state of one bound part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ComponentLive {
    pub status: LiveStatus,
    /// The project's or the service's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The project's slug, for links into the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_slug: Option<String>,
    /// The project's latest deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<i32>,
    /// The service's engine as Temps names it (`postgres`, `redis`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_type: Option<String>,
}

#[derive(Error, Debug)]
pub enum BlueprintError {
    #[error("The blueprint of workspace {application_id} is not valid: {}", problems.join("; "))]
    Invalid {
        application_id: i64,
        problems: Vec<String>,
    },

    #[error("Workspace {application_id} has no blueprint version {version}")]
    VersionNotFound { application_id: i64, version: i32 },

    #[error(
        "Version {version} of the blueprint of workspace {application_id} is {status}, so it can no longer be accepted or rejected"
    )]
    NotOpen {
        application_id: i64,
        version: i32,
        status: String,
    },

    #[error(
        "Workspace {application_id} has no accepted blueprint with a part '{component_id}' to bind"
    )]
    UnknownComponent {
        application_id: i64,
        component_id: String,
    },

    #[error("Version {version} of the blueprint of workspace {application_id} is stored in a shape this server cannot read: {reason}")]
    Corrupt {
        application_id: i64,
        version: i32,
        reason: String,
    },

    #[error(
        "Database error while {operation} the blueprint of workspace {application_id}: {source}"
    )]
    Database {
        application_id: i64,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

impl BlueprintError {
    fn db(application_id: i64, operation: &'static str) -> impl FnOnce(DbErr) -> Self {
        move |source| BlueprintError::Database {
            application_id,
            operation,
            source,
        }
    }
}

fn is_slug(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && id.chars().count() <= MAX_ID_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.ends_with('-')
}

/// A URL carrying a user and password (`postgres://user:pass@host`) is a
/// credential, whatever field it sits in.
fn contains_credential_url(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let after = &rest[at + 3..];
        let authority_end = after
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(after.len());
        let authority = &after[..authority_end];
        if let Some(userinfo_end) = authority.rfind('@') {
            if authority[..userinfo_end].contains(':') {
                return true;
            }
        }
        rest = after;
    }
    false
}

fn check_text(
    problems: &mut Vec<String>,
    what: &str,
    value: &str,
    max_chars: usize,
    required: bool,
) {
    let trimmed = value.trim();
    if required && trimmed.is_empty() {
        problems.push(format!("{what} is empty"));
    }
    if trimmed.chars().count() > max_chars {
        problems.push(format!("{what} is longer than {max_chars} characters"));
    }
    if contains_credential_url(trimmed) {
        problems.push(format!(
            "{what} contains a URL with a password; a blueprint names parts and never holds credentials"
        ));
    }
}

fn check_component(problems: &mut Vec<String>, component: &Component) {
    let id = &component.id;
    let who = format!("part '{id}'");
    if !is_slug(id) {
        problems.push(format!(
            "{who}: the id must be a lowercase slug of at most {MAX_ID_CHARS} characters, starting with a letter (for example `orders-worker`)"
        ));
    }
    check_text(
        problems,
        &format!("{who} name"),
        &component.name,
        MAX_NAME_CHARS,
        true,
    );
    check_text(
        problems,
        &format!("{who} purpose"),
        &component.purpose,
        MAX_PURPOSE_CHARS,
        true,
    );

    let mut misplaced: Vec<&str> = Vec::new();
    let has = |present: bool, field: &'static str, allowed: bool, misplaced: &mut Vec<&str>| {
        if present && !allowed {
            misplaced.push(field);
        }
    };
    let kind = component.kind;
    has(
        component.surface.is_some(),
        "surface",
        kind == ComponentKind::Client,
        &mut misplaced,
    );
    has(
        component.role.is_some(),
        "role",
        kind == ComponentKind::App,
        &mut misplaced,
    );
    has(
        component.stack.is_some(),
        "stack",
        kind == ComponentKind::App,
        &mut misplaced,
    );
    has(
        component.runtime.is_some(),
        "runtime",
        kind == ComponentKind::App,
        &mut misplaced,
    );
    has(
        component.image.is_some(),
        "image",
        kind == ComponentKind::App,
        &mut misplaced,
    );
    has(
        component.engine.is_some(),
        "engine",
        kind == ComponentKind::Data,
        &mut misplaced,
    );
    has(
        !component.holds.is_empty(),
        "holds",
        kind == ComponentKind::Data,
        &mut misplaced,
    );
    has(
        component.provider.is_some(),
        "provider",
        kind == ComponentKind::External,
        &mut misplaced,
    );
    if !misplaced.is_empty() {
        problems.push(format!(
            "{who} is {}, so it has no {}",
            kind_phrase(kind),
            misplaced.join(", ")
        ));
    }

    match kind {
        ComponentKind::Client => {
            if component.surface.is_none() {
                problems.push(format!(
                    "{who} is a client and needs a `surface` (browser, mobile, cli or third_party)"
                ));
            }
        }
        ComponentKind::App => {
            if component.role.is_none() {
                problems.push(format!(
                    "{who} is an app and needs a `role` (web, api, worker, cron or static)"
                ));
            }
            match component.stack.as_deref() {
                Some(stack) => check_text(
                    problems,
                    &format!("{who} stack"),
                    stack,
                    MAX_NAME_CHARS,
                    true,
                ),
                None => problems.push(format!(
                    "{who} is an app and needs a `stack`, for example \"Next.js\" or \"Bun + Hono\""
                )),
            }
            match (component.runtime, component.image.as_deref()) {
                (None, _) => problems.push(format!(
                    "{who} is an app and needs a `runtime` (preset, dockerfile or image)"
                )),
                (Some(AppRuntime::Image), None) => problems.push(format!(
                    "{who} runs an image, so it needs the `image`, for example \"nats:2.10\""
                )),
                (Some(AppRuntime::Image), Some(image)) => check_text(
                    problems,
                    &format!("{who} image"),
                    image,
                    MAX_LABEL_CHARS,
                    true,
                ),
                (Some(_), Some(_)) => problems.push(format!(
                    "{who} has an `image` but its runtime is not `image`"
                )),
                (Some(_), None) => {}
            }
        }
        ComponentKind::Data => {
            if component.engine.is_none() {
                problems.push(format!(
                    "{who} is data and needs an `engine` (postgres, redis, mongodb, mariadb or s3)"
                ));
            }
            if component.holds.len() > MAX_HOLDS {
                problems.push(format!("{who} lists more than {MAX_HOLDS} things it holds"));
            }
            for (index, held) in component.holds.iter().enumerate() {
                check_text(
                    problems,
                    &format!("{who} holds[{index}]"),
                    held,
                    MAX_NAME_CHARS,
                    true,
                );
            }
        }
        ComponentKind::External => match component.provider.as_deref() {
            Some(provider) => check_text(
                problems,
                &format!("{who} provider"),
                provider,
                MAX_NAME_CHARS,
                true,
            ),
            None => problems.push(format!(
                "{who} is an external service and needs a `provider`, for example \"Stripe\""
            )),
        },
    }
}

fn kind_phrase(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Client => "a client",
        ComponentKind::App => "an app",
        ComponentKind::Data => "data",
        ComponentKind::External => "an external service",
    }
}

fn check_flow(
    problems: &mut Vec<String>,
    index: usize,
    flow: &Flow,
    parts: &HashMap<&str, &Component>,
) {
    let who = format!("flow {} ({} → {})", index + 1, flow.from, flow.to);
    check_text(
        problems,
        &format!("{who} label"),
        &flow.label,
        MAX_LABEL_CHARS,
        true,
    );
    let (Some(from), Some(to)) = (parts.get(flow.from.as_str()), parts.get(flow.to.as_str()))
    else {
        for end in [&flow.from, &flow.to] {
            if !parts.contains_key(end.as_str()) {
                problems.push(format!(
                    "{who} names '{end}', which is not a part of the design"
                ));
            }
        }
        return;
    };
    if flow.from == flow.to {
        problems.push(format!("{who} starts and ends at the same part"));
        return;
    }
    if from.kind == ComponentKind::Data {
        problems.push(format!(
            "{who} starts at data; data never starts a flow, so draw it from the part that reads or writes"
        ));
    }
    if to.kind == ComponentKind::Client {
        problems.push(format!(
            "{who} ends at a client; draw it from the client to the part it calls"
        ));
    }
    let engine = to.engine;
    let kind = flow.kind.label();
    match flow.kind {
        FlowKind::Query => {
            if !matches!(
                engine,
                Some(DataEngine::Postgres | DataEngine::Mariadb | DataEngine::Mongodb)
            ) {
                problems.push(format!(
                    "{who} is a `query` flow, so it must end at a postgres, mariadb or mongodb part"
                ));
            }
        }
        FlowKind::Cache => {
            if engine != Some(DataEngine::Redis) {
                problems.push(format!(
                    "{who} is a `cache` flow, so it must end at a redis part"
                ));
            }
        }
        FlowKind::Blob => {
            if engine != Some(DataEngine::S3) {
                problems.push(format!(
                    "{who} is a `blob` flow, so it must end at an s3 part"
                ));
            }
        }
        FlowKind::Publish | FlowKind::Subscribe => {
            let broker = engine == Some(DataEngine::Redis) || to.kind == ComponentKind::App;
            if !broker {
                problems.push(format!(
                    "{who} is a `{kind}` flow, so it must end at the broker: a redis part, or an app running one (for example NATS from an image)"
                ));
            }
        }
        FlowKind::Http | FlowKind::Websocket | FlowKind::Webhook => {
            if to.kind == ComponentKind::Data {
                let engine = engine.map(DataEngine::label).unwrap_or("data");
                problems.push(format!(
                    "{who} is a `{kind}` flow to {engine}; use `query`, `cache` or `blob` for data"
                ));
            }
        }
    }
}

/// Check a document against the rules every stored version follows. Every
/// problem is reported at once, naming the part or flow, so a model can fix
/// them all in one go.
pub fn validate(application_id: i64, document: &BlueprintDocument) -> Result<(), BlueprintError> {
    let mut problems = Vec::new();
    check_text(
        &mut problems,
        "the summary",
        &document.summary,
        MAX_SUMMARY_CHARS,
        true,
    );
    if document.components.is_empty() {
        problems.push("the design has no parts".to_string());
    }
    if document.components.len() > MAX_COMPONENTS {
        problems.push(format!("the design has more than {MAX_COMPONENTS} parts"));
    }
    if document.flows.len() > MAX_FLOWS {
        problems.push(format!("the design has more than {MAX_FLOWS} flows"));
    }
    let mut parts: HashMap<&str, &Component> = HashMap::new();
    for component in &document.components {
        check_component(&mut problems, component);
        if parts.insert(component.id.as_str(), component).is_some() {
            problems.push(format!("two parts share the id '{}'", component.id));
        }
    }
    let mut seen_flows = HashSet::new();
    for (index, flow) in document.flows.iter().enumerate() {
        check_flow(&mut problems, index, flow, &parts);
        if !seen_flows.insert((
            flow.from.as_str(),
            flow.to.as_str(),
            flow.kind,
            flow.label.trim(),
        )) {
            problems.push(format!(
                "flow {} repeats an earlier flow ({} → {} {})",
                index + 1,
                flow.from,
                flow.to,
                flow.kind.label()
            ));
        }
    }
    // Field names that hold credentials are refused anywhere in the document,
    // the same rule as every other artifact the model writes.
    if let Ok(value) = serde_json::to_value(document) {
        if let Err(crate::applications::ApplicationError::SecretValue { path }) =
            crate::applications::validate_secret_free_payload(&value, "$")
        {
            problems.push(format!(
                "{path} looks like a credential; leave secrets out of the design"
            ));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    problems.truncate(MAX_REPORTED_PROBLEMS);
    Err(BlueprintError::Invalid {
        application_id,
        problems,
    })
}

fn check_change_summary(
    application_id: i64,
    change_summary: &str,
) -> Result<String, BlueprintError> {
    let mut problems = Vec::new();
    check_text(
        &mut problems,
        "the change summary",
        change_summary,
        MAX_CHANGE_SUMMARY_CHARS,
        true,
    );
    if problems.is_empty() {
        Ok(change_summary.trim().to_string())
    } else {
        Err(BlueprintError::Invalid {
            application_id,
            problems,
        })
    }
}

fn decode_version(
    application_id: i64,
    row: ai_blueprint_versions::Model,
) -> Result<BlueprintVersion, BlueprintError> {
    let document =
        serde_json::from_value(row.document).map_err(|error| BlueprintError::Corrupt {
            application_id,
            version: row.version,
            reason: error.to_string(),
        })?;
    let status = VersionStatus::parse(&row.status).ok_or_else(|| BlueprintError::Corrupt {
        application_id,
        version: row.version,
        reason: format!("unknown status '{}'", row.status),
    })?;
    Ok(BlueprintVersion {
        version: row.version,
        document,
        change_summary: row.change_summary,
        status,
        proposed_in: row.proposed_in,
        created_by: row.created_by,
        decided_by: row.decided_by,
        decided_at: row.decided_at,
        created_at: row.created_at,
    })
}

fn decode_bindings(value: &Value) -> BTreeMap<String, ComponentBinding> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// A part of the design and what it currently runs as, for the prompt.
fn binding_phrase(binding: Option<&ComponentBinding>) -> String {
    match binding {
        Some(ComponentBinding {
            project_id: Some(project_id),
            ..
        }) => format!(" Built as project {project_id}."),
        Some(ComponentBinding {
            service_id: Some(service_id),
            ..
        }) => format!(" Runs as service {service_id}."),
        _ => " Not created yet.".to_string(),
    }
}

fn component_line(component: &Component, binding: Option<&ComponentBinding>) -> String {
    let detail = match component.kind {
        ComponentKind::Client => component
            .surface
            .map(|surface| format!("client, {}", enum_label(&surface)))
            .unwrap_or_else(|| "client".to_string()),
        ComponentKind::App => {
            let role = component
                .role
                .map(|role| enum_label(&role))
                .unwrap_or_default();
            let stack = component.stack.clone().unwrap_or_default();
            let runtime = match (component.runtime, component.image.as_deref()) {
                (Some(AppRuntime::Image), Some(image)) => format!("image {image}"),
                (Some(runtime), _) => enum_label(&runtime),
                (None, _) => String::new(),
            };
            format!("app, {role}, {stack}, runs from {runtime}")
        }
        ComponentKind::Data => {
            let engine = component.engine.map(DataEngine::label).unwrap_or("data");
            if component.holds.is_empty() {
                format!("data, {engine}")
            } else {
                format!("data, {engine}, holds {}", component.holds.join(", "))
            }
        }
        ComponentKind::External => format!(
            "external, {}",
            component.provider.clone().unwrap_or_default()
        ),
    };
    let bound = match component.kind {
        ComponentKind::App | ComponentKind::Data => binding_phrase(binding),
        ComponentKind::Client | ComponentKind::External => String::new(),
    };
    format!(
        "- `{}` {} ({detail}): {}{bound}",
        component.id, component.name, component.purpose
    )
}

fn enum_label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The design as compact text: the summary, one line per part, one per flow.
pub fn render_document(
    document: &BlueprintDocument,
    bindings: &BTreeMap<String, ComponentBinding>,
) -> String {
    let mut out = document.summary.trim().to_string();
    out.push_str("\n\nParts:\n");
    for component in &document.components {
        out.push_str(&component_line(component, bindings.get(&component.id)));
        out.push('\n');
    }
    if !document.flows.is_empty() {
        out.push_str("\nFlows:\n");
        for flow in &document.flows {
            out.push_str(&format!(
                "- {} → {} ({}): {}\n",
                flow.from,
                flow.to,
                flow.kind.label(),
                flow.label.trim()
            ));
        }
    }
    out
}

/// The section every turn of a workspace chat carries: the accepted design,
/// whether a proposal waits, and the rules for building against it.
pub fn prompt_section(workspace_name: &str, state: &BlueprintState) -> String {
    let mut out = format!(
        "## Workspace \"{workspace_name}\"\nThis chat is one of the chats of this workspace. They share its sandbox, its projects and services, and the architecture below, which the user agreed to.\n\n"
    );
    match &state.accepted {
        Some(accepted) => {
            out.push_str(&format!(
                "### Accepted architecture (version {})\n{}\n",
                accepted.version,
                render_document(&accepted.document, &state.bindings)
            ));
        }
        None => out.push_str(
            "### Architecture\nNothing has been accepted yet. Before you build, design the application with `temps_propose_blueprint` and wait for the user to accept it.\n",
        ),
    }
    if let Some(proposal) = &state.proposal {
        out.push_str(&format!(
            "\nVersion {} is waiting for the user's decision (\"{}\"). Do not build what it adds or changes until they accept it; if they ask for changes, propose a new version.\n",
            proposal.version,
            proposal.change_summary
        ));
    }
    out.push_str(
        "\n### Building against it\n- Build what the accepted architecture describes, with the stacks it names. Do not add a part, a flow, a database or a broker it does not have.\n- When the work needs something it does not have, or the user asks for a different design, call `temps_propose_blueprint` with the whole new document and a one-line change summary, then stop and wait for the user to accept it.\n- After you create the project or service that realises a part, call `temps_bind_blueprint` with the part's id, so the architecture shows it live.\n",
    );
    out
}

/// What the console needs to draw one version.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BlueprintVersionResponse {
    pub version: i32,
    pub document: BlueprintDocument,
    pub change_summary: String,
    pub status: VersionStatus,
    /// Whether the model proposed it (in a chat) rather than the user writing it.
    pub proposed_by_assistant: bool,
    /// ISO 8601.
    pub created_at: String,
    /// ISO 8601, once accepted or rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
}

impl From<BlueprintVersion> for BlueprintVersionResponse {
    fn from(version: BlueprintVersion) -> Self {
        Self {
            version: version.version,
            document: version.document,
            change_summary: version.change_summary,
            status: version.status,
            proposed_by_assistant: version.proposed_in.is_some(),
            created_at: version
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            decided_at: version
                .decided_at
                .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct LatestDeployment {
    project_id: i32,
    id: i32,
    state: String,
}

fn deployment_status(state: &str) -> LiveStatus {
    match state {
        "completed" | "succeeded" | "success" => LiveStatus::Live,
        "failed" | "cancelled" | "error" => LiveStatus::Failed,
        _ => LiveStatus::Building,
    }
}

fn service_status(status: &str) -> LiveStatus {
    match status {
        "running" | "healthy" | "active" => LiveStatus::Live,
        "failed" | "error" | "unhealthy" => LiveStatus::Failed,
        "stopped" | "paused" => LiveStatus::Stopped,
        _ => LiveStatus::Building,
    }
}

/// Stores and decides a workspace's blueprint versions.
#[derive(Clone)]
pub struct BlueprintService {
    db: Arc<DatabaseConnection>,
}

impl BlueprintService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn blueprint_row<C: ConnectionTrait>(
        conn: &C,
        application_id: i64,
    ) -> Result<Option<ai_blueprints::Model>, BlueprintError> {
        ai_blueprints::Entity::find()
            .filter(ai_blueprints::Column::ApplicationId.eq(application_id))
            .one(conn)
            .await
            .map_err(BlueprintError::db(application_id, "reading"))
    }

    /// The accepted version, the open proposal and the bindings.
    pub async fn state(&self, application_id: i64) -> Result<BlueprintState, BlueprintError> {
        let Some(blueprint) = Self::blueprint_row(self.db.as_ref(), application_id).await? else {
            return Ok(BlueprintState::default());
        };
        let rows = ai_blueprint_versions::Entity::find()
            .filter(ai_blueprint_versions::Column::BlueprintId.eq(blueprint.id))
            .filter(ai_blueprint_versions::Column::Status.is_in([
                VersionStatus::Proposed.as_str(),
                VersionStatus::Accepted.as_str(),
            ]))
            .all(self.db.as_ref())
            .await
            .map_err(BlueprintError::db(application_id, "reading"))?;
        let mut state = BlueprintState {
            bindings: decode_bindings(&blueprint.bindings),
            ..Default::default()
        };
        for row in rows {
            let version = decode_version(application_id, row)?;
            match version.status {
                VersionStatus::Accepted => state.accepted = Some(version),
                VersionStatus::Proposed => state.proposal = Some(version),
                VersionStatus::Rejected | VersionStatus::Superseded => {}
            }
        }
        Ok(state)
    }

    /// Every version, newest first.
    pub async fn versions(
        &self,
        application_id: i64,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<BlueprintVersion>, u64), BlueprintError> {
        let Some(blueprint) = Self::blueprint_row(self.db.as_ref(), application_id).await? else {
            return Ok((Vec::new(), 0));
        };
        let paginator = ai_blueprint_versions::Entity::find()
            .filter(ai_blueprint_versions::Column::BlueprintId.eq(blueprint.id))
            .order_by_desc(ai_blueprint_versions::Column::Version)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await.map_err(BlueprintError::db(
            application_id,
            "listing the versions of",
        ))?;
        let rows = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await
            .map_err(BlueprintError::db(
                application_id,
                "listing the versions of",
            ))?;
        let versions = rows
            .into_iter()
            .map(|row| decode_version(application_id, row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((versions, total))
    }

    /// Lock the workspace's blueprint row for a write, creating it first.
    async fn lock_blueprint<C: ConnectionTrait>(
        txn: &C,
        application_id: i64,
    ) -> Result<ai_blueprints::Model, BlueprintError> {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO ai_blueprints (application_id) VALUES ($1) \
             ON CONFLICT (application_id) DO NOTHING",
            [application_id.into()],
        ))
        .await
        .map_err(BlueprintError::db(application_id, "creating"))?;
        ai_blueprints::Entity::find()
            .filter(ai_blueprints::Column::ApplicationId.eq(application_id))
            .lock_exclusive()
            .one(txn)
            .await
            .map_err(BlueprintError::db(application_id, "locking"))?
            .ok_or_else(|| BlueprintError::Database {
                application_id,
                operation: "locking",
                source: DbErr::RecordNotFound(format!(
                    "blueprint of workspace {application_id} vanished while it was being written"
                )),
            })
    }

    async fn next_version<C: ConnectionTrait>(
        txn: &C,
        application_id: i64,
        blueprint_id: i64,
    ) -> Result<i32, BlueprintError> {
        let latest = ai_blueprint_versions::Entity::find()
            .filter(ai_blueprint_versions::Column::BlueprintId.eq(blueprint_id))
            .order_by_desc(ai_blueprint_versions::Column::Version)
            .one(txn)
            .await
            .map_err(BlueprintError::db(application_id, "numbering a version of"))?;
        Ok(latest.map_or(1, |row| row.version + 1))
    }

    /// Mark every version in `from` status as superseded.
    async fn supersede<C: ConnectionTrait>(
        txn: &C,
        application_id: i64,
        blueprint_id: i64,
        from: VersionStatus,
    ) -> Result<(), BlueprintError> {
        ai_blueprint_versions::Entity::update_many()
            .col_expr(
                ai_blueprint_versions::Column::Status,
                sea_orm::sea_query::Expr::value(VersionStatus::Superseded.as_str()),
            )
            .filter(ai_blueprint_versions::Column::BlueprintId.eq(blueprint_id))
            .filter(ai_blueprint_versions::Column::Status.eq(from.as_str()))
            .exec(txn)
            .await
            .map_err(BlueprintError::db(
                application_id,
                "superseding a version of",
            ))?;
        Ok(())
    }

    /// Store a proposal from the model. An open proposal is superseded: the
    /// user only ever decides on the newest one.
    pub async fn propose(
        &self,
        application_id: i64,
        user_id: i32,
        conversation_id: Option<i64>,
        document: BlueprintDocument,
        change_summary: &str,
    ) -> Result<BlueprintVersion, BlueprintError> {
        validate(application_id, &document)?;
        let change_summary = check_change_summary(application_id, change_summary)?;
        self.write_version(
            application_id,
            user_id,
            conversation_id,
            document,
            change_summary,
            VersionStatus::Proposed,
        )
        .await
    }

    /// Store a version the user wrote themselves. It is accepted as it is
    /// saved, replacing the accepted version and any open proposal.
    pub async fn save_user_version(
        &self,
        application_id: i64,
        user_id: i32,
        document: BlueprintDocument,
        change_summary: &str,
    ) -> Result<BlueprintVersion, BlueprintError> {
        validate(application_id, &document)?;
        let change_summary = check_change_summary(application_id, change_summary)?;
        self.write_version(
            application_id,
            user_id,
            None,
            document,
            change_summary,
            VersionStatus::Accepted,
        )
        .await
    }

    async fn write_version(
        &self,
        application_id: i64,
        user_id: i32,
        conversation_id: Option<i64>,
        document: BlueprintDocument,
        change_summary: String,
        status: VersionStatus,
    ) -> Result<BlueprintVersion, BlueprintError> {
        let document_value =
            serde_json::to_value(&document).map_err(|error| BlueprintError::Invalid {
                application_id,
                problems: vec![format!("the document could not be encoded: {error}")],
            })?;
        let txn = self
            .db
            .begin()
            .await
            .map_err(BlueprintError::db(application_id, "writing"))?;
        let blueprint = Self::lock_blueprint(&txn, application_id).await?;
        Self::supersede(&txn, application_id, blueprint.id, VersionStatus::Proposed).await?;
        if status == VersionStatus::Accepted {
            Self::supersede(&txn, application_id, blueprint.id, VersionStatus::Accepted).await?;
        }
        let version = Self::next_version(&txn, application_id, blueprint.id).await?;
        let now = Utc::now();
        let decided = status == VersionStatus::Accepted;
        let row = ai_blueprint_versions::ActiveModel {
            blueprint_id: Set(blueprint.id),
            version: Set(version),
            document: Set(document_value),
            change_summary: Set(change_summary),
            status: Set(status.as_str().to_string()),
            proposed_in: Set(conversation_id),
            created_by: Set(user_id),
            decided_by: Set(decided.then_some(user_id)),
            decided_at: Set(decided.then_some(now)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(BlueprintError::db(application_id, "storing a version of"))?;
        let mut active: ai_blueprints::ActiveModel = blueprint.into();
        if decided {
            active.accepted_version = Set(Some(version));
        }
        active.updated_at = Set(now);
        active
            .update(&txn)
            .await
            .map_err(BlueprintError::db(application_id, "updating"))?;
        txn.commit()
            .await
            .map_err(BlueprintError::db(application_id, "writing"))?;
        decode_version(application_id, row)
    }

    /// Accept or reject the open proposal `version`.
    pub async fn decide(
        &self,
        application_id: i64,
        version: i32,
        user_id: i32,
        decision: Decision,
    ) -> Result<BlueprintVersion, BlueprintError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(BlueprintError::db(application_id, "deciding"))?;
        let blueprint = Self::lock_blueprint(&txn, application_id).await?;
        let row = ai_blueprint_versions::Entity::find()
            .filter(ai_blueprint_versions::Column::BlueprintId.eq(blueprint.id))
            .filter(ai_blueprint_versions::Column::Version.eq(version))
            .one(&txn)
            .await
            .map_err(BlueprintError::db(application_id, "reading"))?
            .ok_or(BlueprintError::VersionNotFound {
                application_id,
                version,
            })?;
        if row.status != VersionStatus::Proposed.as_str() {
            return Err(BlueprintError::NotOpen {
                application_id,
                version,
                status: row.status,
            });
        }
        let now = Utc::now();
        let status = match decision {
            Decision::Accept => {
                Self::supersede(&txn, application_id, blueprint.id, VersionStatus::Accepted)
                    .await?;
                VersionStatus::Accepted
            }
            Decision::Reject => VersionStatus::Rejected,
        };
        let mut active: ai_blueprint_versions::ActiveModel = row.into();
        active.status = Set(status.as_str().to_string());
        active.decided_by = Set(Some(user_id));
        active.decided_at = Set(Some(now));
        let row = active
            .update(&txn)
            .await
            .map_err(BlueprintError::db(application_id, "deciding a version of"))?;
        if decision == Decision::Accept {
            let mut blueprint: ai_blueprints::ActiveModel = blueprint.into();
            blueprint.accepted_version = Set(Some(version));
            blueprint.updated_at = Set(now);
            blueprint
                .update(&txn)
                .await
                .map_err(BlueprintError::db(application_id, "updating"))?;
        }
        txn.commit()
            .await
            .map_err(BlueprintError::db(application_id, "deciding"))?;
        decode_version(application_id, row)
    }

    /// Record which project or service realises a part of the accepted design.
    /// `None` removes the binding.
    pub async fn bind(
        &self,
        application_id: i64,
        component_id: &str,
        binding: Option<ComponentBinding>,
    ) -> Result<BTreeMap<String, ComponentBinding>, BlueprintError> {
        let accepted = self.state(application_id).await?.accepted;
        let component = accepted
            .as_ref()
            .and_then(|version| {
                version
                    .document
                    .components
                    .iter()
                    .find(|component| component.id == component_id)
            })
            .ok_or_else(|| BlueprintError::UnknownComponent {
                application_id,
                component_id: component_id.to_string(),
            })?;
        if let Some(binding) = &binding {
            let problem = match (component.kind, binding.project_id, binding.service_id) {
                (ComponentKind::App, Some(_), None) | (ComponentKind::Data, None, Some(_)) => None,
                (ComponentKind::App, _, _) => {
                    Some(format!("part '{component_id}' is an app, so it binds to a project"))
                }
                (ComponentKind::Data, _, _) => {
                    Some(format!("part '{component_id}' is data, so it binds to a service"))
                }
                (kind, _, _) => Some(format!(
                    "part '{component_id}' is {}, which Temps does not run, so it has nothing to bind",
                    kind_phrase(kind)
                )),
            };
            if let Some(problem) = problem {
                return Err(BlueprintError::Invalid {
                    application_id,
                    problems: vec![problem],
                });
            }
        }
        let txn = self
            .db
            .begin()
            .await
            .map_err(BlueprintError::db(application_id, "binding"))?;
        let blueprint = Self::lock_blueprint(&txn, application_id).await?;
        let mut bindings = decode_bindings(&blueprint.bindings);
        match binding {
            Some(binding) => {
                bindings.insert(component_id.to_string(), binding);
            }
            None => {
                bindings.remove(component_id);
            }
        }
        let value = serde_json::to_value(&bindings).map_err(|error| BlueprintError::Invalid {
            application_id,
            problems: vec![format!("the bindings could not be encoded: {error}")],
        })?;
        let mut active: ai_blueprints::ActiveModel = blueprint.into();
        active.bindings = Set(value);
        active.updated_at = Set(Utc::now());
        active
            .update(&txn)
            .await
            .map_err(BlueprintError::db(application_id, "binding"))?;
        txn.commit()
            .await
            .map_err(BlueprintError::db(application_id, "binding"))?;
        Ok(bindings)
    }

    /// The live state of every bound part, in three queries whatever the
    /// number of parts: the projects, their latest deployments, the services.
    pub async fn live(
        &self,
        application_id: i64,
        bindings: &BTreeMap<String, ComponentBinding>,
    ) -> Result<BTreeMap<String, ComponentLive>, BlueprintError> {
        let project_ids: Vec<i32> = bindings.values().filter_map(|b| b.project_id).collect();
        let service_ids: Vec<i32> = bindings.values().filter_map(|b| b.service_id).collect();
        let projects: HashMap<i32, projects::Model> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids.clone()))
                .filter(projects::Column::IsDeleted.eq(false))
                .all(self.db.as_ref())
                .await
                .map_err(BlueprintError::db(
                    application_id,
                    "reading the projects of",
                ))?
                .into_iter()
                .map(|project| (project.id, project))
                .collect()
        };
        let deployments: HashMap<i32, LatestDeployment> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            LatestDeployment::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT ON (project_id) project_id, id, state \
                 FROM deployments WHERE project_id = ANY($1) \
                 ORDER BY project_id, created_at DESC",
                [project_ids.into()],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(BlueprintError::db(
                application_id,
                "reading the deployments of",
            ))?
            .into_iter()
            .map(|deployment| (deployment.project_id, deployment))
            .collect()
        };
        let services: HashMap<i32, external_services::Model> = if service_ids.is_empty() {
            HashMap::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids))
                .all(self.db.as_ref())
                .await
                .map_err(BlueprintError::db(
                    application_id,
                    "reading the services of",
                ))?
                .into_iter()
                .map(|service| (service.id, service))
                .collect()
        };
        let mut live = BTreeMap::new();
        for (component_id, binding) in bindings {
            let state = if let Some(project_id) = binding.project_id {
                match projects.get(&project_id) {
                    Some(project) => {
                        let deployment = deployments.get(&project_id);
                        ComponentLive {
                            status: deployment
                                .map_or(LiveStatus::Created, |d| deployment_status(&d.state)),
                            name: Some(project.name.clone()),
                            project_slug: Some(project.slug.clone()),
                            deployment_id: deployment.map(|d| d.id),
                            service_type: None,
                        }
                    }
                    None => missing(),
                }
            } else if let Some(service_id) = binding.service_id {
                match services.get(&service_id) {
                    Some(service) => ComponentLive {
                        status: service_status(&service.status),
                        name: Some(service.name.clone()),
                        project_slug: None,
                        deployment_id: None,
                        service_type: Some(service.service_type.clone()),
                    },
                    None => missing(),
                }
            } else {
                continue;
            };
            live.insert(component_id.clone(), state);
        }
        Ok(live)
    }
}

fn missing() -> ComponentLive {
    ComponentLive {
        status: LiveStatus::Missing,
        name: None,
        project_slug: None,
        deployment_id: None,
        service_type: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(value: Value) -> BlueprintDocument {
        serde_json::from_value(value).expect("the fixture is a document")
    }

    /// A small full-stack design: a browser, a web app, an API, a worker fed
    /// by Redis streams, Postgres and an S3 bucket.
    fn taskboard() -> BlueprintDocument {
        document(json!({
            "summary": "A task board for small teams: tasks, assignees and file attachments.",
            "components": [
                { "id": "browser", "name": "Browser", "purpose": "Where people use the board.", "kind": "client", "surface": "browser" },
                { "id": "web", "name": "Web app", "purpose": "Serves the board.", "kind": "app", "role": "web", "stack": "Next.js", "runtime": "preset" },
                { "id": "api", "name": "API", "purpose": "Reads and writes tasks.", "kind": "app", "role": "api", "stack": "Bun + Hono", "runtime": "dockerfile" },
                { "id": "worker", "name": "Notifier", "purpose": "Sends notifications when tasks change.", "kind": "app", "role": "worker", "stack": "Bun", "runtime": "preset" },
                { "id": "db", "name": "Database", "purpose": "Keeps tasks and users.", "kind": "data", "engine": "postgres", "holds": ["tasks", "users"] },
                { "id": "events", "name": "Events", "purpose": "Carries task changes.", "kind": "data", "engine": "redis", "holds": ["stream task.events"] },
                { "id": "files", "name": "Attachments", "purpose": "Keeps uploaded files.", "kind": "data", "engine": "s3", "holds": ["bucket attachments"] },
                { "id": "mail", "name": "Email", "purpose": "Delivers notifications.", "kind": "external", "provider": "an email API" }
            ],
            "flows": [
                { "from": "browser", "to": "web", "kind": "http", "label": "pages" },
                { "from": "web", "to": "api", "kind": "http", "label": "GET/POST /api/tasks" },
                { "from": "api", "to": "db", "kind": "query", "label": "tables tasks, users" },
                { "from": "api", "to": "events", "kind": "publish", "label": "stream task.events" },
                { "from": "worker", "to": "events", "kind": "subscribe", "label": "stream task.events" },
                { "from": "api", "to": "files", "kind": "blob", "label": "bucket attachments" },
                { "from": "worker", "to": "mail", "kind": "http", "label": "POST /send" }
            ]
        }))
    }

    fn problems(document: &BlueprintDocument) -> Vec<String> {
        match validate(7, document) {
            Ok(()) => Vec::new(),
            Err(BlueprintError::Invalid { problems, .. }) => problems,
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn a_full_stack_design_with_pub_sub_is_valid() {
        assert_eq!(problems(&taskboard()), Vec::<String>::new());
    }

    #[test]
    fn a_broker_can_be_an_app_running_an_image() {
        let mut design = taskboard();
        design.components.push(document(json!({
            "summary": "x",
            "components": [{ "id": "nats", "name": "NATS", "purpose": "Carries events.", "kind": "app", "role": "worker", "stack": "NATS", "runtime": "image", "image": "nats:2.10" }]
        })).components.remove(0));
        design.flows.push(Flow {
            from: "api".into(),
            to: "nats".into(),
            kind: FlowKind::Publish,
            label: "subject task.created".into(),
        });
        assert_eq!(problems(&design), Vec::<String>::new());
    }

    #[test]
    fn every_problem_is_reported_at_once_and_names_the_part() {
        let design = document(json!({
            "summary": "",
            "components": [
                { "id": "Web App", "name": "Web", "purpose": "Pages.", "kind": "app", "role": "web", "runtime": "preset" },
                { "id": "db", "name": "DB", "purpose": "Data.", "kind": "data", "stack": "Postgres" },
                { "id": "db", "name": "Again", "purpose": "Twice.", "kind": "external", "provider": "x" }
            ],
            "flows": [
                { "from": "db", "to": "nowhere", "kind": "http", "label": "?" }
            ]
        }));
        let found = problems(&design);
        let text = found.join("\n");
        assert!(text.contains("the summary is empty"), "{text}");
        assert!(
            text.contains("part 'Web App': the id must be a lowercase slug"),
            "{text}"
        );
        assert!(
            text.contains("part 'Web App' is an app and needs a `stack`"),
            "{text}"
        );
        assert!(
            text.contains("part 'db' is data, so it has no stack"),
            "{text}"
        );
        assert!(
            text.contains("part 'db' is data and needs an `engine`"),
            "{text}"
        );
        assert!(text.contains("two parts share the id 'db'"), "{text}");
        assert!(
            text.contains("names 'nowhere', which is not a part"),
            "{text}"
        );
    }

    #[test]
    fn flows_must_end_where_their_kind_lives() {
        let mut design = taskboard();
        design.flows = vec![
            Flow {
                from: "api".into(),
                to: "events".into(),
                kind: FlowKind::Query,
                label: "tasks".into(),
            },
            Flow {
                from: "api".into(),
                to: "db".into(),
                kind: FlowKind::Cache,
                label: "sessions".into(),
            },
            Flow {
                from: "api".into(),
                to: "db".into(),
                kind: FlowKind::Blob,
                label: "files".into(),
            },
            Flow {
                from: "api".into(),
                to: "mail".into(),
                kind: FlowKind::Publish,
                label: "topic".into(),
            },
            Flow {
                from: "web".into(),
                to: "db".into(),
                kind: FlowKind::Http,
                label: "rest".into(),
            },
            Flow {
                from: "db".into(),
                to: "api".into(),
                kind: FlowKind::Http,
                label: "push".into(),
            },
            Flow {
                from: "api".into(),
                to: "browser".into(),
                kind: FlowKind::Websocket,
                label: "updates".into(),
            },
        ];
        let text = problems(&design).join("\n");
        assert!(
            text.contains("`query` flow, so it must end at a postgres"),
            "{text}"
        );
        assert!(
            text.contains("`cache` flow, so it must end at a redis part"),
            "{text}"
        );
        assert!(
            text.contains("`blob` flow, so it must end at an s3 part"),
            "{text}"
        );
        assert!(
            text.contains("`publish` flow, so it must end at the broker"),
            "{text}"
        );
        assert!(
            text.contains("`http` flow to postgres; use `query`"),
            "{text}"
        );
        assert!(text.contains("starts at data"), "{text}");
        assert!(text.contains("ends at a client"), "{text}");
    }

    #[test]
    fn a_design_never_holds_a_credential() {
        let mut design = taskboard();
        design.flows[2].label = "postgres://app:hunter22@db:5432/app".into();
        let text = problems(&design).join("\n");
        assert!(text.contains("contains a URL with a password"), "{text}");

        let mut design = taskboard();
        design.components[2].stack = Some("Bun".into());
        assert!(problems(&design).is_empty());
        assert!(!contains_credential_url(
            "https://api.example.test/v1/tasks"
        ));
        assert!(contains_credential_url(
            "see redis://:secretpass@cache:6379"
        ));
    }

    #[test]
    fn a_design_is_bounded() {
        let mut design = taskboard();
        let extra = design.components[1].clone();
        for index in 0..MAX_COMPONENTS {
            let mut part = extra.clone();
            part.id = format!("web-{index}");
            design.components.push(part);
        }
        let text = problems(&design).join("\n");
        assert!(
            text.contains(&format!("more than {MAX_COMPONENTS} parts")),
            "{text}"
        );
    }

    #[test]
    fn the_prompt_lists_parts_flows_bindings_and_the_rules() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "api".to_string(),
            ComponentBinding {
                project_id: Some(12),
                service_id: None,
            },
        );
        bindings.insert(
            "db".to_string(),
            ComponentBinding {
                project_id: None,
                service_id: Some(4),
            },
        );
        let state = BlueprintState {
            accepted: Some(BlueprintVersion {
                version: 3,
                document: taskboard(),
                change_summary: "Add the notifier".into(),
                status: VersionStatus::Accepted,
                proposed_in: Some(1),
                created_by: 1,
                decided_by: Some(1),
                decided_at: None,
                created_at: Utc::now(),
            }),
            proposal: Some(BlueprintVersion {
                version: 4,
                document: taskboard(),
                change_summary: "Cache sessions in Redis".into(),
                status: VersionStatus::Proposed,
                proposed_in: Some(1),
                created_by: 1,
                decided_by: None,
                decided_at: None,
                created_at: Utc::now(),
            }),
            bindings,
        };
        let section = prompt_section("Taskboard", &state);
        assert!(section.contains("## Workspace \"Taskboard\""));
        assert!(section.contains("### Accepted architecture (version 3)"));
        assert!(section.contains(
            "- `api` API (app, api, Bun + Hono, runs from dockerfile): Reads and writes tasks. Built as project 12."
        ));
        assert!(section.contains("- `db` Database (data, postgres, holds tasks, users): Keeps tasks and users. Runs as service 4."));
        assert!(section.contains("- `worker` Notifier (app, worker, Bun, runs from preset): Sends notifications when tasks change. Not created yet."));
        assert!(section.contains("- api → events (publish): stream task.events"));
        assert!(section.contains(
            "Version 4 is waiting for the user's decision (\"Cache sessions in Redis\")"
        ));
        assert!(section.contains("`temps_propose_blueprint`"));
        assert!(section.contains("`temps_bind_blueprint`"));
    }

    #[test]
    fn an_empty_workspace_is_told_to_design_first() {
        let section = prompt_section("Shop", &BlueprintState::default());
        assert!(section.contains("Nothing has been accepted yet"));
        assert!(section.contains("wait for the user to accept it"));
    }

    #[test]
    fn statuses_map_from_deployments_and_services() {
        assert_eq!(deployment_status("completed"), LiveStatus::Live);
        assert_eq!(deployment_status("failed"), LiveStatus::Failed);
        assert_eq!(deployment_status("running"), LiveStatus::Building);
        assert_eq!(service_status("running"), LiveStatus::Live);
        assert_eq!(service_status("stopped"), LiveStatus::Stopped);
        assert_eq!(service_status("creating"), LiveStatus::Building);
    }

    #[test]
    fn the_document_round_trips_through_its_stored_shape() {
        let design = taskboard();
        let value = serde_json::to_value(&design).unwrap();
        // Fields of other kinds are not written out, so the stored JSON stays
        // as small as what the model sent.
        assert!(value["components"][0].get("engine").is_none());
        assert_eq!(
            serde_json::from_value::<BlueprintDocument>(value).unwrap(),
            design
        );
    }
}
