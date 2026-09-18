# Runtime and verification

## TypeScript

Use the starter's pinned SDK and lockfile. Do not copy an SDK's unbuilt workspace source
into a published dependency or bypass a protocol mismatch with ad-hoc auth patches.
Check the selected SDK types before using newer APIs; a released SDK can differ from the
source checkout's version label.

Typical entry points are `runPlugin`, `createManifest`, `handler`, `onEvent`,
`embeddedUiAssets`, and `onShutdown`. Keep `temps.name`, manifest name, versions and UI
routes aligned. Return structured errors with the failed operation and useful recovery.
Use the SDK's verified auth extraction; trusting raw `x-temps-user-*` headers allows
spoofing. Instance-wide administrative reports should remain admin-only unless the
plugin implements project/tenant authorization itself. Do not assume host proxy auth
also authorizes every action inside the plugin.

Protocol v2 bootstraps through the SDK handshake. Never hardcode an auth secret or put it
in command-line arguments or logs. Reserve stdout for protocol messages. Do not register
SDK-owned health/channel/event endpoints yourself. The Node request bridge may not be a
full `IncomingMessage`; use the API the installed SDK actually supports.

Persist plugin settings and work in `ctx.dataDir` (SQLite is appropriate), not the host's
application database. Keep work bounded, write state atomically, and distinguish an
interrupted job after restart from a completed job. Graceful shutdown cancels/drains
owned jobs before closing storage.

### Host permissions and AI

Request only permissions used by the feature, such as `events_read` for events and
`ai_generate` for host-brokered generation. Inspect current manifest/capability types for
exact names. Discover runtime permissions/configuration instead of assuming declaration
means approval. Missing AI configuration should show an explanation and settings link.
Do not require AI for deterministic checks that work without it.

For AI, use the host broker; do not expose provider credentials to the plugin or browser.
Respect configured quotas, concurrency, timeouts and output bounds. Test limits and
revocation with mocks before any separately authorized provider calls. Preserve plugin
actor attribution when using host APIs. Same-source upgrades and replacement/reinstall
identity behavior must be verified on the target host, not implemented by copying actor
IDs into plugin configuration.

### Deployment events

Subscribe to `deployment.succeeded`; relevant data includes `deployment_id`, `project_id`,
`environment_id`, `environment_name`, and optionally `url`. Validate fields and handle a
missing public URL. A success event should enqueue work, not block event delivery for a
long crawl. Record its deployment/project/environment attribution. Deduplicate deliveries,
bound queues and concurrency, and expose saturation/skipped work. Test non-production
filtering, cancellation, restart and repeated events. Crawling a mutable deployment URL
observes it at crawl time, not necessarily the original immutable release.

Networked plugins must validate destinations and each redirect, defend against private-IP
and DNS rebinding access where only public sites are supported, and bound request time,
response size, parser depth and discovered URLs. A resource-limit failure means incomplete
inspection, not proof the target is broken. Native runtime permissions remain those of
the host OS account regardless of host API grants.

## Local simulator

First verify these subcommands with the installed CLI's `--help`. If unavailable, use an
explicitly identified compatible CLI checkout/version or a real development Temps host;
do not silently substitute a naked UI server and claim protocol coverage.

```sh
bun install --frozen-lockfile
bun run check
bun test
bun run build

# Use the actual binary path printed by the project's build.
bunx --bun @temps-sdk/cli plugin dev ./dist/my-plugin --session authoring --grant events_read
```

In another terminal:

```sh
bunx --bun @temps-sdk/cli plugin dev status --session authoring --json
bunx --bun @temps-sdk/cli plugin dev events
bunx --bun @temps-sdk/cli plugin dev emit deployment.succeeded --session authoring --url https://example.com --repeat 2
bunx --bun @temps-sdk/cli plugin dev grants set --session authoring --clear
bunx --bun @temps-sdk/cli plugin dev grants set --session authoring --grant events_read
bunx --bun @temps-sdk/cli plugin dev logs --session authoring --json
```

Use the runner's printed loopback preview URL; do not invent a port or credentials.
`--fixtures` supports host/mock-AI data; inspect its schema in the matching CLI source.
Test `--role reader` as well as admin. Read the report/state after emitting an event:
receipts only prove delivery. Stop the owned runner with Ctrl-C; retain only intended
plugin data. Keep session auth, fixture secrets and generated databases out of commits.

## Rust alternative

Use a compatible pinned `temps-plugin-sdk` release/revision and the official Rust examples
in `gotempsh/plugins`. Implement `ExternalPlugin`; let `temps_plugin_sdk::main!` own the
runtime. Do not register duplicate SDK health routes or block a running Tokio executor
with `block_on`. Follow the SDK's async initialization pattern. Use contextual `thiserror`
errors, compile the UI before embedding, and check/test the affected crate.

The GitHub TypeScript source installer is not a general Rust build service. For Rust,
verify the target host's supported binary installation/distribution path rather than
claiming the TypeScript template's catalog build validates a Cargo project.

## Evidence to retain

Record commit and SDK/CLI/host versions, commands and pass/fail results, tested platforms,
screenshots at mobile/tablet/desktop in light/dark mode, permission and update scenarios,
and observed job results. Separate local simulation, real host installation, mock external
services, and real external calls. Review networking, auth, secrets and persistence changes
before distributing the executable. Cross-compilation alone is not runtime verification.
