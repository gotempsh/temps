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
plugins. A private plugin may link internal crates when it needs in-process
access to configured services. Such a plugin is explicitly trusted and
version-locked to Temps.

Raw host services and secrets are not part of the general third-party plugin
contract. Future third-party plugins require narrower host capabilities or a
separate sandboxed execution model.

## Implemented work

- Added the resolved permission header to the host/plugin protocol.
- Added typed authenticated and optional caller extractors.
- Verified proxy assertions in SDK middleware before inserting caller state.
- Caller-scoped channel clients are captured from the initiating request and
  carried into background work; there is no mutable user-ID token registry.
- Added caller-scoped project listing through the platform HTTP handler.
- Stripped browser credentials and spoofed Temps headers before proxying.
- Added regression tests for spoofing, narrowed API-key permissions, anonymous
  requests, and caller extraction.

## Remaining work

- Add protocol-version negotiation and actionable compatibility errors.
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
- OSS Temps changes merge through a PR with runtime evidence.
