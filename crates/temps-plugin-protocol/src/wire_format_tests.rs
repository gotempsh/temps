// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pins the exact bytes of the external-plugin protocol.
//!
//! Plugin binaries ship separately from the platform and are built against an
//! older SDK, so these bytes are a compatibility contract. Each case compares
//! a whole serialized message with a literal, and parses the literal back: a
//! renamed field, a reordered field, a changed tag or a new required field
//! fails here. Changing a literal means breaking every installed plugin, so
//! it also means bumping `EXTERNAL_PLUGIN_PROTOCOL_VERSION`.

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::channel::*;
use super::headers;
use super::manifest::*;

/// `value` serializes to exactly `expected`, and `expected` parses and
/// re-serializes to the same bytes.
#[track_caller]
fn assert_wire<T: Serialize + DeserializeOwned>(value: &T, expected: &str) {
    assert_eq!(serde_json::to_string(value).unwrap(), expected);
    let parsed: T = serde_json::from_str(expected).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), expected);
}

fn project() -> ProjectInfo {
    ProjectInfo {
        id: 42,
        name: "demo".into(),
        slug: "demo-slug".into(),
        repo_name: "repo".into(),
        repo_owner: "owner".into(),
        main_branch: "main".into(),
        preset: "node".into(),
        source_type: "github".into(),
        created_at: "2026-01-02T03:04:05Z".into(),
        updated_at: "2026-01-03T03:04:05Z".into(),
        last_deployment: None,
        enable_preview_environments: true,
    }
}
const PROJECT: &str = r#"{"id":42,"name":"demo","slug":"demo-slug","repo_name":"repo","repo_owner":"owner","main_branch":"main","preset":"node","source_type":"github","created_at":"2026-01-02T03:04:05Z","updated_at":"2026-01-03T03:04:05Z","last_deployment":null,"enable_preview_environments":true}"#;

fn environment() -> EnvironmentInfo {
    EnvironmentInfo {
        id: 3,
        project_id: 42,
        name: "Production".into(),
        slug: "production".into(),
        branch: Some("main".into()),
        is_preview: false,
        current_deployment_id: Some(9),
        created_at: "2026-01-02T03:04:05Z".into(),
        updated_at: "2026-01-03T03:04:05Z".into(),
    }
}
const ENVIRONMENT: &str = r#"{"id":3,"project_id":42,"name":"Production","slug":"production","branch":"main","is_preview":false,"current_deployment_id":9,"created_at":"2026-01-02T03:04:05Z","updated_at":"2026-01-03T03:04:05Z"}"#;

fn deployment() -> DeploymentInfo {
    DeploymentInfo {
        id: 9,
        project_id: 42,
        environment_id: 3,
        state: "completed".into(),
        branch: Some("main".into()),
        tag: None,
        commit_sha: Some("abc123".into()),
        commit_message: Some("message".into()),
        commit_author: None,
        created_at: "2026-01-02T03:04:05Z".into(),
        started_at: None,
        finished_at: Some("2026-01-03T03:04:05Z".into()),
    }
}
const DEPLOYMENT: &str = r#"{"id":9,"project_id":42,"environment_id":3,"state":"completed","branch":"main","tag":null,"commit_sha":"abc123","commit_message":"message","commit_author":null,"created_at":"2026-01-02T03:04:05Z","started_at":null,"finished_at":"2026-01-03T03:04:05Z"}"#;

/// Every call the plugin can make, in its request envelope.
#[test]
fn channel_requests_are_pinned() {
    let cases: Vec<(PlatformCallRequest, &str)> = vec![
        (
            GetProject { project_id: 42 }.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"get_project","params":{"project_id":42}}}"#,
        ),
        (
            ListProjects {}.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"list_projects","params":{}}}"#,
        ),
        (
            GetEnvironment { environment_id: 3 }.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"get_environment","params":{"environment_id":3}}}"#,
        ),
        (
            ListEnvironments { project_id: 42 }.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"list_environments","params":{"project_id":42}}}"#,
        ),
        (
            GetDeployment { deployment_id: 9 }.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"get_deployment","params":{"deployment_id":9}}}"#,
        ),
        (
            GetLastDeployment {
                project_id: 42,
                environment_id: None,
            }
            .into_request(),
            r#"{"type":"request","id":1,"call":{"method":"get_last_deployment","params":{"project_id":42}}}"#,
        ),
        (
            ListDeployments {
                project_id: 42,
                environment_id: Some(3),
                limit: Some(5),
            }
            .into_request(),
            r#"{"type":"request","id":1,"call":{"method":"list_deployments","params":{"project_id":42,"environment_id":3,"limit":5}}}"#,
        ),
        (
            ApiCall {
                method: HttpMethod::Post,
                path: "/projects".into(),
                query: Some("a=1".into()),
                body: Some(CallBody::Json(JsonBody::from_raw(r#"{"name":"demo"}"#))),
                actor: ActorToken::new("signed-token"),
            }
            .into_request(),
            r#"{"type":"request","id":1,"call":{"method":"api_call","params":{"method":"POST","path":"/projects","query":"a=1","body":{"kind":"json","body":"{\"name\":\"demo\"}"},"actor":"signed-token"}}}"#,
        ),
        (
            ApiCall {
                method: HttpMethod::Delete,
                path: "/projects/1/upload".into(),
                query: None,
                body: Some(CallBody::Multipart(vec![
                    MultipartPart {
                        name: "file".into(),
                        filename: Some("a.tgz".into()),
                        content_type: Some("application/gzip".into()),
                        content: PartContent::Binary(BinaryPayload::new(vec![0, 1, 254, 255])),
                    },
                    MultipartPart {
                        name: "note".into(),
                        filename: None,
                        content_type: None,
                        content: PartContent::Text("hello".into()),
                    },
                ])),
                actor: ActorToken::new("signed-token"),
            }
            .into_request(),
            r#"{"type":"request","id":1,"call":{"method":"api_call","params":{"method":"DELETE","path":"/projects/1/upload","body":{"kind":"multipart","body":[{"name":"file","filename":"a.tgz","content_type":"application/gzip","content":{"kind":"binary","value":"AAH+/w=="}},{"name":"note","content":{"kind":"text","value":"hello"}}]},"actor":"signed-token"}}}"#,
        ),
        (
            GetHostCapabilities {}.into_request(),
            r#"{"type":"request","id":1,"call":{"method":"get_host_capabilities","params":{}}}"#,
        ),
        (
            GenerateAi {
                purpose: "summary".into(),
                prompt: "hello".into(),
                system: Some("be brief".into()),
                max_tokens: Some(100),
                temperature: Some(0.5),
            }
            .into_request(),
            r#"{"type":"request","id":1,"call":{"method":"generate_ai","params":{"purpose":"summary","prompt":"hello","system":"be brief","max_tokens":100,"temperature":0.5}}}"#,
        ),
    ];
    for (call, expected) in cases {
        assert_wire(
            &ChannelMessage::Request(ChannelRequest { id: 1, call }),
            expected,
        );
    }
}

/// Every reply the platform can send, in its response envelope.
#[test]
fn channel_responses_are_pinned() {
    let cases: Vec<(PlatformCallResponse, String)> = vec![
        (
            PlatformCallResponse::GetProject(project()),
            format!(r#"{{"method":"get_project","result":{PROJECT}}}"#),
        ),
        (
            PlatformCallResponse::ListProjects(vec![project()]),
            format!(r#"{{"method":"list_projects","result":[{PROJECT}]}}"#),
        ),
        (
            PlatformCallResponse::GetEnvironment(environment()),
            format!(r#"{{"method":"get_environment","result":{ENVIRONMENT}}}"#),
        ),
        (
            PlatformCallResponse::ListEnvironments(vec![environment()]),
            format!(r#"{{"method":"list_environments","result":[{ENVIRONMENT}]}}"#),
        ),
        (
            PlatformCallResponse::GetDeployment(deployment()),
            format!(r#"{{"method":"get_deployment","result":{DEPLOYMENT}}}"#),
        ),
        (
            PlatformCallResponse::GetLastDeployment(deployment()),
            format!(r#"{{"method":"get_last_deployment","result":{DEPLOYMENT}}}"#),
        ),
        (
            PlatformCallResponse::ListDeployments(vec![deployment()]),
            format!(r#"{{"method":"list_deployments","result":[{DEPLOYMENT}]}}"#),
        ),
        (
            PlatformCallResponse::ApiCall(ApiCallResult {
                status: 201,
                body: JsonBody::from_raw(r#"{"id":1}"#),
            }),
            r#"{"method":"api_call","result":{"status":201,"body":"{\"id\":1}"}}"#.to_string(),
        ),
        (
            PlatformCallResponse::GetHostCapabilities(HostCapabilities {
                actor: PluginActorInfo {
                    id: "actor-1".into(),
                    name: "demo".into(),
                    active: true,
                },
                permissions: vec![
                    PluginHostPermission::AiGenerate,
                    PluginHostPermission::ProjectsRead,
                    PluginHostPermission::EnvironmentsRead,
                    PluginHostPermission::DeploymentsRead,
                    PluginHostPermission::ApiRead,
                    PluginHostPermission::ApiWrite,
                    PluginHostPermission::EventsRead,
                ],
                ai: PluginAiCapability {
                    configured: false,
                    reason: Some("No AI provider".into()),
                    setup_path: "/settings/ai".into(),
                    daily_call_limit: 10,
                    max_output_tokens: 20,
                    max_prompt_bytes: 30,
                },
            }),
            r#"{"method":"get_host_capabilities","result":{"actor":{"id":"actor-1","name":"demo","active":true},"permissions":["ai_generate","projects_read","environments_read","deployments_read","api_read","api_write","events_read"],"ai":{"configured":false,"reason":"No AI provider","setup_path":"/settings/ai","daily_call_limit":10,"max_output_tokens":20,"max_prompt_bytes":30}}}"#.to_string(),
        ),
        (
            PlatformCallResponse::GenerateAi(GenerateAiResult {
                text: "hi".into(),
                model: "model-a".into(),
            }),
            r#"{"method":"generate_ai","result":{"text":"hi","model":"model-a"}}"#.to_string(),
        ),
    ];
    for (result, payload) in cases {
        assert_wire(
            &ChannelMessage::Response(ChannelResponse::ok(7, result)),
            &format!(r#"{{"type":"response","id":7,"outcome":{{"ok":{payload}}}}}"#),
        );
    }
}

#[test]
fn channel_errors_are_pinned() {
    for (code, expected) in [
        (ChannelErrorCode::MethodNotFound, "method_not_found"),
        (ChannelErrorCode::InvalidParams, "invalid_params"),
        (ChannelErrorCode::PermissionDenied, "permission_denied"),
        (ChannelErrorCode::Unauthenticated, "unauthenticated"),
        (ChannelErrorCode::NotFound, "not_found"),
        (ChannelErrorCode::Internal, "internal"),
    ] {
        assert_wire(
            &ChannelMessage::Response(ChannelResponse::err(2, code, "why")),
            &format!(
                r#"{{"type":"response","id":2,"outcome":{{"err":{{"code":"{expected}","message":"why"}}}}}}"#
            ),
        );
    }
}

fn event() -> PluginEvent {
    PluginEvent {
        id: "evt-1".into(),
        event_type: "deployment.succeeded".into(),
        timestamp: "2026-01-02T03:04:05Z".parse().unwrap(),
        project_id: Some(7),
        data: serde_json::json!({"deployment_id": 9}),
    }
}
const EVENT: &str = r#"{"id":"evt-1","event_type":"deployment.succeeded","timestamp":"2026-01-02T03:04:05Z","project_id":7,"data":{"deployment_id":9}}"#;

/// The same event body goes to `/_events` and down the channel.
#[test]
fn events_are_pinned() {
    assert_wire(&event(), EVENT);
    assert_wire(
        &ChannelMessage::Event(ChannelEvent { event: event() }),
        &format!(r#"{{"type":"event","event":{EVENT}}}"#),
    );
}

const FULL_MANIFEST: &str = r#"{"name":"demo-plugin","version":"1.2.3","display_name":"Demo","description":"A demo","nav":[{"label":"Demo","icon":"puzzle","section":"project","path":"/demo","order":5}],"ui":{"entry_js":"main.js","css":["main.css"],"routes":[{"path":"/demo","title":"Demo"}]},"requires_db":true,"requires_host_data_access":true,"health_path":"/healthz","hide_header":true,"public_paths":["/webhooks"],"capabilities":["api_read","api_write"],"host_permissions":["events_read"],"events":["project.created"]}"#;

fn full_manifest() -> PluginManifest {
    PluginManifest::builder("demo-plugin", "1.2.3")
        .display_name("Demo")
        .description("A demo")
        .nav(NavEntry {
            label: "Demo".into(),
            icon: "puzzle".into(),
            section: NavSection::Project,
            path: "/demo".into(),
            order: 5,
        })
        .ui(UiManifest {
            entry_js: "main.js".into(),
            css: vec!["main.css".into()],
            routes: vec![UiRoute {
                path: "/demo".into(),
                title: "Demo".into(),
            }],
        })
        .requires_db(true)
        .requires_host_data_access(true)
        .health_path("/healthz")
        .hide_header(true)
        .public_path("/webhooks")
        .capability(PluginCapability::ApiRead)
        .capability(PluginCapability::ApiWrite)
        .host_permission(PluginHostPermission::EventsRead)
        .event("project.created")
        .build()
}

/// What the plugin prints on stdout while starting up.
#[test]
fn handshake_messages_are_pinned() {
    assert_wire(
        &HandshakeMessage::Hello(PluginHello {
            protocol_version: 2,
            manifest: Box::new(full_manifest()),
        }),
        &format!(r#"{{"type":"hello","protocol_version":2,"manifest":{FULL_MANIFEST}}}"#),
    );
    assert_wire(
        &HandshakeMessage::Ready(PluginReady {
            ready: true,
            has_ui: true,
            protocol_version: 2,
            openapi: None,
        }),
        r#"{"type":"ready","ready":true,"has_ui":true,"protocol_version":2}"#,
    );
    assert_wire(
        &HandshakeMessage::Ready(PluginReady {
            ready: true,
            has_ui: false,
            protocol_version: 2,
            openapi: Some(serde_json::json!({"openapi": "3.1.0"})),
        }),
        r#"{"type":"ready","ready":true,"has_ui":false,"protocol_version":2,"openapi":{"openapi":"3.1.0"}}"#,
    );
}

/// A legacy `manifest` handshake must keep parsing, so the manager can tell
/// its author to upgrade instead of failing on opaque JSON.
#[test]
fn legacy_manifest_handshake_still_parses() {
    let parsed: HandshakeMessage =
        serde_json::from_str(r#"{"type":"manifest","name":"old","version":"0.1.0"}"#).unwrap();
    let HandshakeMessage::Manifest(manifest) = parsed else {
        panic!("expected the legacy manifest variant");
    };
    assert_eq!(manifest.name, "old");
    // Optional fields keep their defaults, so an older plugin stays parseable.
    assert_eq!(manifest.health_path, "/health");
    assert!(manifest.nav.is_empty());
    assert!(manifest.host_permissions.is_empty());
    assert!(!manifest.requires_host_data_access);
}

/// What the platform writes to the plugin's stdin. The `Debug`-less type keeps
/// the secret out of logs; the bytes still have to match.
#[test]
fn launch_config_is_pinned() {
    let config = PluginLaunchConfig {
        protocol_version: 2,
        auth_secret: "not-a-real-secret".into(),
        database_url: None,
        host_data_dir: Some("/data".into()),
    };
    assert_wire(
        &config,
        r#"{"protocol_version":2,"auth_secret":"not-a-real-secret","database_url":null,"host_data_dir":"/data"}"#,
    );
}

/// Names plugins and the platform both hard-code.
#[test]
fn well_known_names_are_pinned() {
    assert_eq!(EXTERNAL_PLUGIN_PROTOCOL_VERSION, 2);
    assert_eq!(PLUGIN_EVENTS_PATH, "/_events");
    assert_eq!(PLUGIN_CHANNEL_PATH, "/_temps/channel");
    assert_eq!(MAX_CALL_BODY_BYTES, 32 * 1024 * 1024);
    assert_eq!(headers::PREFIX, "x-temps-");
    assert_eq!(headers::PLUGIN_NAME, "x-temps-plugin");
    assert_eq!(headers::USER_ID, "x-temps-user-id");
    assert_eq!(headers::USER_EMAIL, "x-temps-user-email");
    assert_eq!(headers::USER_ROLE, "x-temps-user-role");
    assert_eq!(headers::USER_PERMISSIONS, "x-temps-user-permissions");
    assert_eq!(headers::REQUEST_ID, "x-temps-request-id");
    assert_eq!(headers::AUTH_SIGNATURE, "x-temps-auth-signature");
    assert_eq!(headers::ACTOR_TOKEN, "x-temps-actor-token");
}
