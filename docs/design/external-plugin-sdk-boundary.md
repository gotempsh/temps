# External Plugin Authentication Boundary Plan

Status: Implemented
Branch: `feat/external-plugin-auth-boundary`

## Objective

Make `temps-plugin-sdk` the mandatory authentication and caller-delegation
boundary for external plugins. Trusted first-party plugins may still link
internal runtime crates for in-process platform services, but that trust must
not allow them to bypass caller authentication or authorization.

## Security contract

Temps owns:

- Authentication of sessions, API keys, and CLI tokens.
- Resolution of the caller's effective permission set.
- Removal of caller-supplied `x-temps-*`, `cookie`, `authorization`, and
  `proxy-authorization` headers.
- Delivery of identity over the plugin's private Unix socket, authenticated
  with the per-process handshake secret.
- Authorization of caller-scoped platform API operations.

The SDK exposes:

- `AuthenticatedCaller` and `OptionalCaller` Axum extractors.
- SDK-owned role and permission wire types.
- `PluginContext::api_as_caller` for delegated platform calls.
- Typed platform projections rather than entity models.

The plugin owns authorization for its own resources after extraction. It must
not parse forwarded headers itself or reconstruct permissions from role names.

## Runtime trust model

The SDK is not intended to replace every internal Rust service for first-party
plugins. Every installed external plugin is fully trusted host code: plugin
processes currently run as the Temps operating-system user and are not
sandboxed from the instance filesystem or sibling processes. The caller and
capability checks in this document protect user requests and prevent accidental
authority widening; they are not containment against a malicious plugin.

A private plugin may link internal crates when it needs in-process access to
configured services. Such a plugin is explicitly trusted and version-locked to
Temps, but it still receives no database credential or host-data path in its
launch configuration by default.

Direct control-plane database access and host-data access are independent,
privileged manifest disclosures. `requires_db(true)` asks Temps to supply the
database URL. `requires_host_data_access(true)` asks it to supply the instance
data root, which can contain encryption and authentication keys. These flags
minimize ambient launch data and make privilege visible to operators; they do
not create an OS security boundary. Ordinary plugins should request neither.

Raw host services and secrets are not part of the general third-party plugin
contract. Untrusted third-party plugins require a separate UID, container, or
sandbox with a private filesystem and plugin-scoped database credentials; that
containment is not provided by the current runtime.

## Implemented work

- Added the resolved permission header to the host/plugin protocol.
- Added typed authenticated and optional caller extractors.
- Verified proxy assertions in SDK middleware before inserting caller state.
- Authenticated the internal WebSocket channel and HTTP event fallback before
  either endpoint consumes plugin state.
- Restricted the socket directory and socket itself to the host process user.
- Added a versioned, staged startup handshake. One process publishes its
  manifest and then receives typed launch configuration through stdin; legacy
  or mismatched binaries get an actionable rebuild error.
- Defaulted database and host-data launch values off and supplied them
  independently from the declared manifest.
- Caller-scoped channel clients are captured from the initiating request and
  carried into background work; there is no mutable user-ID token registry.
- Added caller-scoped project listing through the platform HTTP handler.
- Stripped browser credentials and spoofed Temps headers before proxying.
- Added regression tests for spoofing, narrowed API-key permissions, anonymous
  requests, and caller extraction.

## Remaining work

- Add explicit expiry reporting for background delegations so a long-running
  operation can explain when its initiating actor token has expired.
- Add multi-user integration coverage proving project isolation.
- Add generic host APIs only where a concrete plugin requirement justifies
  them; do not mirror the entire Temps service graph preemptively.
- Replace unrestricted host-table operations that handle user-scoped resources
  with authorized SDK calls as concrete requirements arise.

## Verification and release gates

- `cargo check --lib -p temps-plugin-sdk`
- `cargo check --lib -p temps-external-plugins`
- Focused unit suites for both crates.
- Local external-plugin E2E covering SDK caller extraction, delegated platform
  calls, and generated public URLs.
- Security-auditor approval before merge.
- Rebuild external plugin binaries against the matching SDK protocol before a
  host release; legacy binaries fail secure startup preflight.
- OSS Temps changes merge through a PR with runtime evidence.
