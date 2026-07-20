//! Vercel `@vercel/sandbox` SDK compatibility test suite.
//!
//! This file pins the `/v1/sandboxes/*` contract to what the
//! `@vercel/sandbox` npm SDK expects, so a drive-by refactor cannot
//! silently break drop-in clients. It validates:
//!
//! 1. **Path coverage** — every method the SDK calls has a matching
//!    OpenAPI path declared in `SandboxApiDoc`.
//! 2. **Request DTO shape** — request bodies the SDK sends deserialize
//!    into our typed DTOs, including SDK-only fields like `projectId`,
//!    `resources`, and `timeout` (ms) that we accept but don't require.
//! 3. **Response DTO shape** — response DTOs serialize to the envelope
//!    shapes the SDK's zod validators read: `{ sandbox, routes }` for
//!    single-sandbox endpoints and epoch-ms timestamps.
//!
//! These tests don't need a running server. They reflect on the
//! compiled OpenAPI document and exercise the DTOs directly, which is
//! intentional: running containers per test adds flake surface without
//! catching contract drift.
//!
//! **Sync rule:** if you add a route in `sandboxes::routes()`, add the
//! path below too. CI must fail here before it fails at the SDK.

use temps_sandbox::handlers::sandboxes::{
    CmdBody, CmdInner, CmdResponse, CreateSandboxBody, ExecBody, ExecResponse, ExtendTimeoutBody,
    KillJobBody, MkdirBody, ReadFileResponse, SandboxInner, SandboxResponse, SourceBody,
    StatResponse, WriteFileBody, WriteFilesBody,
};
use temps_sandbox::handlers::SandboxApiDoc;
use utoipa::OpenApi;

/// Every Vercel SDK operation maps to a single HTTP path. Keeping this
/// list explicit rather than derived makes contract drift a test failure
/// instead of a silent "the SDK returns 404" in production.
fn expected_sdk_paths() -> Vec<&'static str> {
    vec![
        // Sandbox lifecycle (Sandbox.create, Sandbox.get, Sandbox.stop)
        "/v1/sandboxes",
        "/v1/sandboxes/{id}",
        "/v1/sandboxes/{id}/stop",
        "/v1/sandboxes/{id}/extend-timeout",
        // Exec (Command.run, Command.runDetached) — temps-native shape.
        "/v1/sandboxes/{id}/exec",
        "/v1/sandboxes/{id}/exec-detached",
        // Jobs (temps-native). The SDK doesn't use these — it speaks the
        // `/cmd` surface below.
        "/v1/sandboxes/{id}/jobs/{job_id}",
        "/v1/sandboxes/{id}/jobs/{job_id}/logs",
        "/v1/sandboxes/{id}/jobs/{job_id}/kill",
        // `@vercel/sandbox` command surface. `sandbox.runCommand()` posts
        // to `/cmd` with `{command, args, cwd, env, sudo, wait}`;
        // `getCommand` / `logs` / `kill` mirror SDK paths exactly.
        "/v1/sandboxes/{id}/cmd",
        "/v1/sandboxes/{id}/cmd/{cmd_id}",
        "/v1/sandboxes/{id}/cmd/{cmd_id}/logs",
        "/v1/sandboxes/{id}/{cmd_id}/kill",
        // Filesystem (sandbox.readFile, writeFile, writeFiles, stat, mkdir)
        "/v1/sandboxes/{id}/fs/read",
        "/v1/sandboxes/{id}/fs/write",
        "/v1/sandboxes/{id}/fs/write-batch",
        "/v1/sandboxes/{id}/fs/stat",
        "/v1/sandboxes/{id}/fs/mkdir",
        // Preview URLs (sandbox.domain(port))
        "/v1/sandboxes/{id}/domain",
        // ── Temps extensions (not in the Vercel SDK) ────────────────────
        // Non-destructive lifecycle verbs. `/stop` stays SDK-compatible
        // (stop + destroy) — these add pause/resume/restart/destroy with
        // explicit semantics for users who don't want `/stop` to also
        // tear the sandbox down.
        "/v1/sandboxes/{id}/pause",
        "/v1/sandboxes/{id}/resume",
        "/v1/sandboxes/{id}/restart",
        "/v1/sandboxes/{id}/destroy",
        // Post-create source seeding (git clone / tarball extract).
        "/v1/sandboxes/{id}/source",
        // List detached jobs for a sandbox. The Vercel SDK has no
        // equivalent — it gives you a Command handle at detach time and
        // expects you to track it yourself. Useful for our dashboard
        // where a human returns after the page reloaded.
        "/v1/sandboxes/{id}/jobs",
        // Client-generated preview password (temps extension).
        "/v1/sandboxes/{id}/preview-password",
        // Firecracker backend extensions (ADR-029): operations timeline,
        // live disk resize, and host-global rootfs inventory/GC.
        "/v1/sandboxes/{id}/events",
        "/v1/sandboxes/{id}/resize",
        "/v1/sandboxes/rootfs",
        "/v1/sandboxes/rootfs/gc",
    ]
}

#[test]
fn openapi_covers_every_sdk_path() {
    let api = SandboxApiDoc::openapi();
    let got = &api.paths.paths;
    for expected in expected_sdk_paths() {
        assert!(
            got.contains_key(expected),
            "OpenAPI doc missing SDK path '{}' — the `@vercel/sandbox` SDK calls this endpoint; \
             if you removed the route, also drop it from `expected_sdk_paths`.",
            expected
        );
    }
}

/// Surface-area regression guard. The SDK is stable today — any *new* path
/// surfacing here means someone added functionality not in the SDK; that
/// needs a conscious decision (do we want to advertise it in our
/// Vercel-compat doc or document it separately?), not an implicit one.
#[test]
fn openapi_does_not_advertise_unknown_paths() {
    let api = SandboxApiDoc::openapi();
    let expected: std::collections::HashSet<_> = expected_sdk_paths().into_iter().collect();
    let got = &api.paths.paths;

    for path in got.keys() {
        assert!(
            expected.contains(path.as_str()),
            "OpenAPI doc exposes path '{}' not in the Vercel SDK surface — \
             add it to `expected_sdk_paths` with a comment, or remove it from `SandboxApiDoc::paths`.",
            path
        );
    }
}

// ── DTO shape: request bodies the SDK sends ─────────────────────────────────

/// `Sandbox.create({ image, timeoutSecs, env })` — the SDK omits optional
/// fields. Our DTO must accept a bare object and still deserialize.
#[test]
fn create_sandbox_body_accepts_empty_object() {
    let body: CreateSandboxBody =
        serde_json::from_str("{}").expect("empty object must deserialize as defaults");
    assert!(body.image.is_none());
    assert!(body.name.is_none());
    assert!(body.env.is_empty());
    assert!(body.source.is_none());
}

#[test]
fn create_sandbox_body_accepts_full_payload() {
    // Shape taken from @vercel/sandbox 1.x docs.
    let json = serde_json::json!({
        "image": "node:20",
        "name": "demo",
        "timeout_secs": 600,
        "env": { "DEBUG": "1" },
    });
    let body: CreateSandboxBody =
        serde_json::from_value(json).expect("full temps payload must deserialize");
    assert_eq!(body.image.as_deref(), Some("node:20"));
    assert_eq!(body.timeout_secs, Some(600));
    assert_eq!(body.env.get("DEBUG").map(String::as_str), Some("1"));
}

/// The real `@vercel/sandbox` SDK sends `timeout` in milliseconds, a
/// nested `resources` object, and a `projectId` we don't care about.
/// None of these should cause a deserialization failure — the whole
/// point of drop-in compat.
#[test]
fn create_sandbox_body_accepts_vercel_sdk_payload() {
    let json = serde_json::json!({
        "projectId": "ignored",
        "timeout": 600_000,
        "resources": { "memory": 2048, "vcpus": 2.0 },
        "runtime": "node24",
        "ports": [3000],
        "networkPolicy": { "mode": "allow-all" },
    });
    let body: CreateSandboxBody =
        serde_json::from_value(json).expect("SDK payload with SDK-only fields must deserialize");
    assert_eq!(body.timeout, Some(600_000));
    assert!(body.resources.is_some());
    let resources = body.resources.unwrap();
    assert_eq!(resources.memory, Some(2048));
    assert_eq!(resources.vcpus, Some(2.0));
    // `ports` drives the SDK's `sandbox.domain(port)` resolution — it
    // must round-trip through `CreateSandboxBody` into actual routes.
    assert_eq!(body.ports, vec![3000u16]);
}

#[test]
fn create_sandbox_body_accepts_git_source() {
    let json = serde_json::json!({
        "source": { "type": "git", "url": "https://github.com/example/repo" }
    });
    let body: CreateSandboxBody = serde_json::from_value(json).expect("git source deserializes");
    assert!(matches!(body.source, Some(SourceBody::Git { .. })));
}

#[test]
fn exec_body_accepts_minimal_command() {
    // Matches `sandbox.runCommand({ cmd: ['ls', '-la'] })`.
    let body: ExecBody =
        serde_json::from_str(r#"{"cmd":["ls","-la"]}"#).expect("minimal cmd deserializes");
    assert_eq!(body.cmd, vec!["ls".to_string(), "-la".to_string()]);
    assert!(body.env.is_empty());
    assert!(body.cwd.is_none());
}

#[test]
fn write_file_body_requires_path_and_contents() {
    // Missing `contents_b64` should fail deserialization. The SDK always
    // sends content, so accepting a body without it would mask SDK bugs.
    let err = serde_json::from_str::<WriteFileBody>(r#"{"path":"/a"}"#).expect_err("must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("contents_b64") || msg.contains("missing field"),
        "expected missing-field error, got: {}",
        msg
    );
}

#[test]
fn write_files_body_accepts_empty_batch() {
    // `sandbox.writeFiles([])` is legal — maps to a zero-entry batch.
    let body: WriteFilesBody =
        serde_json::from_str(r#"{"files":[]}"#).expect("empty batch deserializes");
    assert!(body.files.is_empty());
}

/// Both the native `extra_secs` (seconds) and the SDK-compat `duration`
/// (milliseconds) are accepted. The handler resolves one or the other via
/// `resolve_secs()`. An empty body is now legal at deserialize time — the
/// handler rejects missing values with a 400.
#[test]
fn extend_timeout_body_accepts_extra_secs() {
    let body: ExtendTimeoutBody =
        serde_json::from_str(r#"{"extra_secs":60}"#).expect("extra_secs deserializes");
    assert_eq!(body.resolve_secs(), Some(60));
}

#[test]
fn extend_timeout_body_accepts_sdk_duration_ms() {
    let body: ExtendTimeoutBody =
        serde_json::from_str(r#"{"duration":90000}"#).expect("duration (ms) deserializes");
    assert_eq!(body.resolve_secs(), Some(90));
}

#[test]
fn extend_timeout_body_without_value_resolves_none() {
    // Empty body must parse; the handler converts `None` into a 400.
    let body: ExtendTimeoutBody = serde_json::from_str("{}").expect("empty body deserializes");
    assert_eq!(body.resolve_secs(), None);
}

#[test]
fn mkdir_body_requires_path() {
    let err = serde_json::from_str::<MkdirBody>("{}").expect_err("must fail");
    assert!(err.to_string().contains("path"));
}

#[test]
fn kill_job_body_defaults_force_false() {
    // `Command.kill()` (no args) must produce a valid body; our DTO
    // defaults `force` to false. The SDK also accepts `{}` for a graceful
    // SIGTERM.
    let body: KillJobBody = serde_json::from_str("{}").expect("empty body deserializes");
    assert!(!body.force);
}

/// The real shape `@vercel/sandbox` sends to `POST /cmd`:
/// `{command, args, cwd, env, sudo, wait}`. `command` is argv[0], not a
/// full argv array — the SDK keeps them separate.
#[test]
fn cmd_body_accepts_sdk_payload() {
    let json = serde_json::json!({
        "command": "node",
        "args": ["--version"],
        "cwd": "/workspace",
        "env": { "DEBUG": "1" },
        "sudo": false,
        "wait": true,
    });
    let body: CmdBody = serde_json::from_value(json).expect("SDK cmd payload deserializes");
    assert_eq!(body.command, "node");
    assert_eq!(body.args, vec!["--version".to_string()]);
    assert_eq!(body.cwd.as_deref(), Some("/workspace"));
    assert!(body.wait);
}

#[test]
fn cmd_body_accepts_minimal_payload() {
    // `sandbox.runCommand({ command: 'ls' })` — everything else default.
    let body: CmdBody =
        serde_json::from_str(r#"{"command":"ls"}"#).expect("minimal cmd body deserializes");
    assert_eq!(body.command, "ls");
    assert!(body.args.is_empty());
    assert!(!body.wait);
    assert!(!body.sudo);
}

/// The SDK's zod validator pins the inner command shape. Any rename
/// here silently breaks the client.
#[test]
fn cmd_response_matches_sdk_envelope() {
    let r = CmdResponse {
        command: CmdInner {
            id: "job_abc".into(),
            name: "node".into(),
            args: vec!["--version".into()],
            cwd: "/workspace".into(),
            sandbox_id: "sbx_xyz".into(),
            exit_code: None,
            started_at: 1_700_000_000_000,
        },
    };
    let v = serde_json::to_value(&r).expect("serializes");
    assert!(v.get("command").is_some(), "missing `command` envelope key");
    let c = &v["command"];
    for field in [
        "id",
        "name",
        "args",
        "cwd",
        "sandboxId",
        "exitCode",
        "startedAt",
    ] {
        assert!(
            c.get(field).is_some(),
            "CmdInner missing SDK field '{}' — the SDK's zod validator will reject this",
            field
        );
    }
    // `exitCode` is `null` while running — the SDK reads it as a nullable
    // number, not an absent key.
    assert!(c["exitCode"].is_null());
    // `startedAt` must be a number (epoch ms).
    assert!(c["startedAt"].is_number());
}

#[test]
fn cmd_response_serializes_finished_with_exit_code() {
    let r = CmdResponse {
        command: CmdInner {
            id: "job_abc".into(),
            name: "ls".into(),
            args: Vec::new(),
            cwd: String::new(),
            sandbox_id: "sbx_xyz".into(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
        },
    };
    let v = serde_json::to_value(&r).expect("serializes");
    assert_eq!(v["command"]["exitCode"], 0);
}

// ── DTO shape: response bodies the SDK reads ────────────────────────────────

/// The SDK's zod validator wraps every single-sandbox endpoint in
/// `{ sandbox: {...}, routes: [] }`. `sandbox` must carry the strict
/// SDK-required field set: id, memory, vcpus, region, runtime, timeout,
/// status, requestedAt, createdAt, updatedAt, cwd. Timestamps are epoch
/// milliseconds (numbers), not ISO strings.
#[test]
fn sandbox_response_matches_sdk_envelope() {
    let r = SandboxResponse {
        sandbox: SandboxInner {
            id: "sbx_abc".into(),
            memory: 2048,
            vcpus: 2.0,
            region: "local".into(),
            runtime: "node24".into(),
            timeout: 600_000,
            status: "running".into(),
            requested_at: 1_700_000_000_000,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_001_000,
            cwd: "/workspace".into(),
            name: "demo".into(),
            image: Some("node:20".into()),
            backend: None,
            disk_size_mb: None,
            preview_url_template: String::new(),
            preview_password_hint: None,
        },
        routes: Vec::new(),
    };
    let v = serde_json::to_value(&r).expect("serializes");

    // Envelope shape.
    assert!(v.get("sandbox").is_some(), "missing `sandbox` envelope key");
    assert!(v.get("routes").is_some(), "missing `routes` array");
    assert!(v["routes"].is_array());

    let s = &v["sandbox"];
    for field in [
        "id",
        "memory",
        "vcpus",
        "region",
        "runtime",
        "timeout",
        "status",
        "requestedAt",
        "createdAt",
        "updatedAt",
        "cwd",
    ] {
        assert!(
            s.get(field).is_some(),
            "SandboxInner is missing SDK field '{}' — the SDK's zod validator will reject this",
            field
        );
    }

    // Timestamps must be numbers (epoch ms), not strings.
    assert!(
        s["createdAt"].is_number(),
        "createdAt must be a number (epoch ms), got: {:?}",
        s["createdAt"]
    );
    assert!(s["requestedAt"].is_number());
    assert!(s["updatedAt"].is_number());
}

/// The SDK's status enum is a closed set. Any value we emit outside this
/// set is rejected by the zod validator at the client.
#[test]
fn sandbox_status_uses_sdk_enum_values() {
    let valid = [
        "pending",
        "running",
        "stopping",
        "stopped",
        "failed",
        "aborted",
        "snapshotting",
    ];
    for status in valid {
        let r = SandboxResponse {
            sandbox: SandboxInner {
                id: "x".into(),
                memory: 0,
                vcpus: 0.0,
                region: "local".into(),
                runtime: "node24".into(),
                timeout: 0,
                status: status.into(),
                requested_at: 0,
                created_at: 0,
                updated_at: 0,
                cwd: "/".into(),
                name: String::new(),
                image: None,
                backend: None,
                disk_size_mb: None,
                preview_url_template: String::new(),
                preview_password_hint: None,
            },
            routes: Vec::new(),
        };
        let v = serde_json::to_value(&r).expect("serializes");
        assert_eq!(v["sandbox"]["status"], status);
    }
}

#[test]
fn exec_response_exposes_exit_code_and_streams() {
    let r = ExecResponse {
        exit_code: 0,
        stdout: "hi\n".into(),
        stderr: String::new(),
    };
    let v = serde_json::to_value(&r).expect("serializes");
    for field in ["exit_code", "stdout", "stderr"] {
        assert!(v.get(field).is_some(), "ExecResponse missing '{}'", field);
    }
}

#[test]
fn read_file_response_uses_base64() {
    let r = ReadFileResponse {
        path: "/a".into(),
        contents_b64: "aGk=".into(),
        size: 2,
    };
    let v = serde_json::to_value(&r).expect("serializes");
    // `contents_b64` (not `contents`) is the field name. The SDK mirrors
    // this — renaming breaks round-tripping binary data through JSON.
    assert!(v.get("contents_b64").is_some());
    assert_eq!(v["size"], 2);
}

#[test]
fn stat_response_distinguishes_missing_from_error() {
    // The SDK relies on `exists=false` rather than 404 when a file is
    // absent. Verify the field is present and both type fields coexist.
    let r = StatResponse {
        path: "/missing".into(),
        exists: false,
        is_dir: false,
        is_file: false,
        size: 0,
    };
    let v = serde_json::to_value(&r).expect("serializes");
    assert_eq!(v["exists"], false);
    assert_eq!(v["is_dir"], false);
    assert_eq!(v["is_file"], false);
}

// ── Strict rejection where still required ───────────────────────────────────

/// Bodies where we still enforce strict shape. The SDK-facing request
/// DTOs (`CreateSandboxBody`, `ExtendTimeoutBody`) deliberately drop
/// `deny_unknown_fields` to tolerate SDK-only extras like `projectId` or
/// `networkPolicy`. But bodies the SDK doesn't send or sends with a
/// fixed shape (exec, write, mkdir, kill) stay strict — a typo means a
/// client bug we want to surface.
#[test]
fn strict_request_dtos_still_reject_unknown_fields() {
    fn reject<T: for<'de> serde::Deserialize<'de>>(label: &str, json: &str) {
        match serde_json::from_str::<T>(json) {
            Ok(_) => panic!(
                "{} accepted unknown field: {}. This weakens contract guarantees.",
                label, json
            ),
            Err(e) => assert!(
                e.to_string().contains("unknown field"),
                "{}: expected 'unknown field' error, got: {}",
                label,
                e
            ),
        }
    }

    reject::<ExecBody>("ExecBody", r#"{"cmd":["ls"],"xxx":1}"#);
    reject::<WriteFileBody>(
        "WriteFileBody",
        r#"{"path":"/a","contents_b64":"aA==","xxx":1}"#,
    );
    reject::<WriteFilesBody>("WriteFilesBody", r#"{"files":[],"xxx":1}"#);
    reject::<MkdirBody>("MkdirBody", r#"{"path":"/a","xxx":1}"#);
    reject::<KillJobBody>("KillJobBody", r#"{"xxx":1}"#);
}

/// Counterpart: the SDK-facing create/extend bodies MUST tolerate unknown
/// fields so forward-compat additions to `@vercel/sandbox` don't take
/// us out until we catch up.
#[test]
fn sdk_facing_request_dtos_tolerate_unknown_fields() {
    // CreateSandboxBody — an SDK that adds a new optional field should
    // keep working against us.
    let body: CreateSandboxBody = serde_json::from_str(r#"{"unknownFutureField": 42}"#)
        .expect("CreateSandboxBody must tolerate unknown SDK fields for forward-compat");
    assert!(body.image.is_none());

    // ExtendTimeoutBody — same deal.
    let body: ExtendTimeoutBody = serde_json::from_str(r#"{"unknownFutureField": 42}"#)
        .expect("ExtendTimeoutBody must tolerate unknown SDK fields for forward-compat");
    assert_eq!(body.resolve_secs(), None);
}

// ── OpenAPI schema registration ─────────────────────────────────────────────

#[test]
fn openapi_registers_core_request_schemas() {
    let api = SandboxApiDoc::openapi();
    let components = api.components.expect("components present");
    for schema in [
        "CreateSandboxBody",
        "ExecBody",
        "ExtendTimeoutBody",
        "WriteFileBody",
        "WriteFilesBody",
        "MkdirBody",
        "KillJobBody",
    ] {
        assert!(
            components.schemas.contains_key(schema),
            "OpenAPI is missing request schema '{}' — external SDK generators won't see it",
            schema
        );
    }
}

#[test]
fn openapi_registers_core_response_schemas() {
    let api = SandboxApiDoc::openapi();
    let components = api.components.expect("components present");
    for schema in [
        "SandboxResponse",
        "ExecResponse",
        "ReadFileResponse",
        "StatResponse",
        "SandboxDomainResponse",
    ] {
        assert!(
            components.schemas.contains_key(schema),
            "OpenAPI is missing response schema '{}'",
            schema
        );
    }
}
