---
name: temps-add-plugin
description: Add a TempsPlugin so a crate's services, routes and OpenAPI schema are wired into the server - the trait in temps-core, register_services with require_service vs get_service, the two-phase initialisation, configure_routes/configure_public_routes/openapi_schema, and where the plugin list lives in the console startup. Use when creating a new feature crate, or when a crate needs to provide or consume a shared service.
---
<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Add a plugin

Every feature crate plugs into the server through `TempsPlugin`. The plugin
builds the crate's services from shared ones, publishes its own services for
other plugins, and returns its router and OpenAPI document.

- Trait and runtime: `crates/temps-core/src/plugin.rs` (`TempsPlugin`,
  `ServiceRegistrationContext`, `PluginContext`, `PluginRoutes`,
  `RequiredService`, `PluginManager`).
- Smallest complete example: `crates/temps-flags/src/plugin.rs` (one service, one
  router, one OpenAPI doc, no background work).
- Example with declared requirements: `crates/temps-error-tracking/src/plugin.rs`.
- Example with deferred initialisation: `crates/temps-blob/src/plugin.rs`.
- Where plugins are registered: `start_console_api` in
  `crates/temps-cli/src/commands/serve/console.rs`.

## 1. The crate

A new crate under `crates/`:

- Add it to `members` in the root `Cargo.toml`.
- End its `Cargo.toml` with `[lints]` / `workspace = true`.
- Typical layout, as in temps-flags: `lib.rs`, `error.rs`, `services/`,
  `handlers/{mod,handler,types,audit}.rs`, `plugin.rs`. Re-export the plugin from
  `lib.rs` (`pub use plugin::FlagsPlugin;`).
- Add a `description` to `[package]`.

## 2. Implement `TempsPlugin`

Shape of `crates/temps-flags/src/plugin.rs`:

```rust
pub struct FlagsPlugin;

impl TempsPlugin for FlagsPlugin {
    fn name(&self) -> &'static str {
        "flags"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            context.register_service(Arc::new(FlagService::new(db)));
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let flag_service = context.require_service::<FlagService>();
        let audit_service = context.require_service::<dyn AuditLogger>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let app_state = Arc::new(FlagsAppState { flag_service, audit_service, project_access_checker });
        Some(PluginRoutes::new(configure_routes().with_state(app_state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<FlagsApiDoc as OpenApiTrait>::openapi())
    }
}
```

Also provide `new()` and `Default`.

### Services: `require_service` vs `get_service`

- Services are keyed by type and returned as `Arc<T>`:
  `require_service::<DatabaseConnection>()` yields `Arc<DatabaseConnection>`.
  Trait objects use `dyn`: `require_service::<dyn AuditLogger>()`.
- **`require_service`** for anything the plugin cannot work without. It fails
  fast at startup with the missing type's name. Never wrap a required
  dependency in `Option`.
- **`get_service`** only for genuinely optional collaborators, where `None` has a
  defined meaning. The canonical case is `dyn temps_core::ProjectAccessChecker`,
  which is `None` unless another plugin provides team-based access, and which
  the guard macros treat as a no-op.
- Give each component its own `Arc` clone. Do not add accessors such as
  `service.db()` to hand dependencies around.
- Publish with `context.register_service(Arc::new(MyService::new(..)))`. If other
  plugins should depend on an abstraction, register an `Arc<dyn Trait>`.

### Two-phase initialisation

`PluginManager::initialize_plugins` runs:

1. **Phase 1, `register_services`**, for every plugin in registration order.
   `require_service` here only sees services registered by plugins **earlier**
   in the list, so register providers before consumers.
2. **`verify_required_services`**: every type listed by a plugin's
   `required_services()` must now be registered; all gaps are reported in one
   error. Declare cross-plugin requirements there, as
   `crates/temps-error-tracking/src/plugin.rs` does with
   `RequiredService::of::<temps_otel::OtelService>()`.
3. **Phase 2, `initialize_plugin_services(&PluginContext)`**: every service is
   registered, so a plugin can look up others and load configuration from the
   database. Do not block startup on slow work (pulling an image, starting a
   container); spawn it, as `crates/temps-blob/src/plugin.rs` does.

Routes, middleware and OpenAPI are collected after both phases.

### Routes and OpenAPI

- `configure_routes` returns authenticated routes (the auth middleware puts an
  `AuthContext` in the request; handlers take `RequireAuth`). Return `None` if
  the crate has no endpoints.
- `configure_public_routes` is for endpoints without a user session (tracking
  pixels, inbound webhooks, token-authenticated delivery). Each one still
  authenticates its caller somehow; say how in the handler doc.
- Paths have no `/api` prefix; `build_application` adds it. Use `{param}`.
- Two plugins must not register the same method and path. Replacing an existing
  route is done with `PluginRoutes::with_override`, not a second `.route()`.
- `openapi_schema` returns the crate's `#[derive(OpenApi)]` doc. A route missing
  from it is missing from both generated clients.
- `configure_middleware` exists for the rare plugin that adds middleware.

Handlers themselves follow the `temps-add-handler` skill.

## 3. Wire it into the server

In `start_console_api` (`crates/temps-cli/src/commands/serve/console.rs`):

```rust
// 5.3. FlagsPlugin - provides feature flags (depends on database only)
debug!("Registering FlagsPlugin");
let flags_plugin = Box::new(FlagsPlugin::new());
plugin_manager.register_plugin(flags_plugin);
```

- Insert it after every plugin whose services it requires in phase 1, with a
  numbered comment that names those dependencies.
- Add the crate to `crates/temps-cli/Cargo.toml`
  (`temps-flags = { path = "../temps-flags" }`) and the `use` at the top of
  `console.rs`.
- This is the console process. The proxy, agent and stateless processes wire
  their services separately; if the feature must run there too, wire it there
  explicitly.
- Background loops are bounded: O(changes) per tick, bounded channels, bounded
  memory (`AGENTS.md` → Scalability & Efficiency).
- An optional dependency (Docker, an external provider) never makes the plugin
  disappear: register it, report "not configured" through a status endpoint, and
  let the UI onboard (`AGENTS.md` → Feature Discoverability).

## 4. Tests

- Plugin-level: assert the OpenAPI document contains the routes you expect.
  `openapi_schema_exposes_deployment_token_routes` in
  `crates/temps-deployments/src/plugin.rs` is the pattern.
- Registration behaviour: `crates/temps-core/src/plugin.rs` tests build a
  `PluginManager`, `register_plugin` a provider and a consumer, and call
  `initialize_plugins()` (`satisfied_requirements_pass`,
  `missing_requirement_fails_before_phase_two`). Copy that shape when the plugin
  declares `required_services()`.
- Services and handlers are tested in their own modules (see `temps-add-handler`).
- Start the server once (`start-temps` skill) and check the startup log shows
  the plugin registered and initialised without errors.

## Verification

```bash
cargo check --lib -p <new-crate>
cargo check --lib -p temps-cli
cargo test --lib -p <new-crate>
cargo clippy -p <new-crate> -p temps-cli --all-targets -- -D warnings
python3 scripts/rule_drift.py
python3 scripts/source_attribution.py check
```

If the plugin adds or changes endpoints, regenerate both OpenAPI clients as
described in the `temps-add-handler` skill.

New files get the SPDX header (`python3 scripts/source_attribution.py annotate
<file>`). Commit with `git commit -s -m "feat(<crate>): <capability>"`.

## Checklist

- [ ] Crate in workspace `members`, `[lints] workspace = true`, `description` set
- [ ] `require_service` for required deps, `get_service` only for optional ones
- [ ] Cross-plugin needs declared in `required_services()`
- [ ] Slow initialisation spawned, not awaited, in phase 2
- [ ] Routes in `configure_routes` (or `configure_public_routes` with its own auth)
- [ ] `openapi_schema` returns the crate's `ApiDoc`
- [ ] Registered in `start_console_api` after its providers
- [ ] OpenAPI test, service tests and handler tests pass
